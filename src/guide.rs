// 画面の辞書。*.toml を読み、いまの画面がどれかを判定し、マウス位置の説明を組み立てる。
//
// - screen: 画面の判定条件 (正規表現) と、説明・できること・領域ごとの注釈
// - term:   画面のどこに出ても説明する語 (メッセージ ID・データセット名など)
// - layout: データセットのレコード定義。ブラウズ画面で桁の意味と値を出す
//
// 辞書は「サイトの辞書 (外から渡すディレクトリ)」→「組み込みの辞書 (MVS 3.8j・MVS/CE の一般的な内容)」の順に読み、
// 先に読んだものを優先する。サイトの辞書は extends で組み込みの画面に「できること」と注釈を足せる。

use std::path::PathBuf;

use regex::Regex;
use serde::Deserialize;
use unicode_width::UnicodeWidthChar;

use crate::screen::{COLS, Screen};

/// バイナリに埋め込む辞書
const BUILTIN: &[(&str, &str)] = &[
    (
        "(組み込み) screens.toml",
        include_str!("../guide/screens.toml"),
    ),
    ("(組み込み) terms.toml", include_str!("../guide/terms.toml")),
];

/// 辞書の読み込み元。Ctrl+L で読み直すために持っておく
#[derive(Clone)]
pub struct Sources {
    /// サイトの辞書のディレクトリ (先のものほど優先)
    pub dirs: Vec<PathBuf>,
    /// 組み込みの辞書も読む
    pub builtin: bool,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    screen: Vec<ScreenDef>,
    #[serde(default)]
    term: Vec<TermDef>,
    #[serde(default)]
    layout: Vec<LayoutDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenDef {
    id: String,
    /// 別の画面 (id) に「できること」・注釈・説明 (末尾に足す) を足す。このとき title と match は書かない
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    title: String,
    /// 画面の文字 (行を改行でつないだもの) にすべて当てはまれば、この画面とみなす
    #[serde(rename = "match", default)]
    matches: Vec<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    action: Vec<Action>,
    #[serde(default)]
    region: Vec<RegionDef>,
    /// ブラウズ画面なら、レコード定義 (layout) で桁を説明する
    #[serde(default)]
    browse: Option<BrowseDef>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub label: String,
    /// 送るキー。文字はそのまま入力し、{enter} {pf3} {clear} {tab} {home} {eraseeof} {pa1} は対応するキー
    pub keys: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionDef {
    /// 行の範囲 (1 始まり、両端を含む)
    rows: [usize; 2],
    /// 桁の範囲 (1 始まり、両端を含む)。省略すると行全体
    #[serde(default = "full_cols")]
    cols: [usize; 2],
    title: String,
    note: String,
}

fn full_cols() -> [usize; 2] {
    [1, COLS]
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct BrowseDef {
    /// データの行の範囲 (1 始まり)
    rows: [usize; 2],
    /// 16 進表示 (1 バイトを 2 桁で表示する)
    #[serde(default)]
    hex: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TermDef {
    pattern: String,
    title: String,
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutDef {
    /// データセット名の正規表現
    dataset: String,
    title: String,
    field: Vec<LayoutField>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct LayoutField {
    /// 桁の範囲 (1 始まり、両端を含む)
    cols: [usize; 2],
    name: String,
    /// char (文字) / zoned (数字の文字) / packed (パック 10 進数)
    #[serde(default = "char_kind")]
    kind: String,
    #[serde(default)]
    note: String,
}

fn char_kind() -> String {
    "char".into()
}

struct CompiledScreen {
    def: ScreenDef,
    matches: Vec<Regex>,
}

pub struct Guide {
    screens: Vec<CompiledScreen>,
    terms: Vec<(Regex, TermDef)>,
    layouts: Vec<(Regex, LayoutDef)>,
}

/// いまの画面の説明
pub struct ScreenInfo<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub actions: &'a [Action],
}

/// 説明の 1 項目 (ツールチップの 1 段落)
pub struct Note {
    pub title: String,
    pub body: String,
}

impl Guide {
    pub fn load(src: &Sources) -> anyhow::Result<Guide> {
        let mut sources = Vec::new();
        for dir in &src.dirs {
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            paths.sort();
            for p in paths {
                sources.push((p.display().to_string(), std::fs::read_to_string(&p)?));
            }
        }
        if src.builtin {
            sources.extend(BUILTIN.iter().map(|(n, s)| (n.to_string(), s.to_string())));
        }
        Self::from_sources(sources)
    }

    fn from_sources(sources: Vec<(String, String)>) -> anyhow::Result<Guide> {
        let mut g = Guide {
            screens: Vec::new(),
            terms: Vec::new(),
            layouts: Vec::new(),
        };
        let mut extensions = Vec::new();
        for (name, text) in sources {
            let f: File = toml::from_str(&text).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
            for def in f.screen {
                if def.extends.is_some() {
                    extensions.push((name.clone(), def));
                    continue;
                }
                if def.matches.is_empty() || def.title.is_empty() {
                    anyhow::bail!("{name}: screen {}: title と match が要る", def.id);
                }
                let matches = def
                    .matches
                    .iter()
                    .map(|m| Regex::new(m))
                    .collect::<Result<_, _>>()
                    .map_err(|e| anyhow::anyhow!("{name}: screen {}: {e}", def.id))?;
                g.screens.push(CompiledScreen { def, matches });
            }
            for t in f.term {
                let re = Regex::new(&t.pattern)
                    .map_err(|e| anyhow::anyhow!("{name}: term {}: {e}", t.pattern))?;
                g.terms.push((re, t));
            }
            for l in f.layout {
                let re = Regex::new(&format!("^(?:{})$", l.dataset))
                    .map_err(|e| anyhow::anyhow!("{name}: layout {}: {e}", l.dataset))?;
                g.layouts.push((re, l));
            }
        }
        // 拡張は、足す先の画面の「できること」と注釈の前に入れる (サイトのものを先に見せる)
        for (name, ext) in extensions {
            let target = ext.extends.as_deref().unwrap_or_default();
            let Some(base) = g.screens.iter_mut().find(|s| s.def.id == target) else {
                anyhow::bail!("{name}: screen {}: extends の先 {target} が無い", ext.id);
            };
            base.def.action.splice(0..0, ext.action);
            base.def.region.splice(0..0, ext.region);
            if !ext.description.is_empty() {
                base.def.description.push('\n');
                base.def.description.push_str(ext.description.trim());
            }
        }
        Ok(g)
    }

    fn find(&self, text: &str) -> Option<&ScreenDef> {
        self.screens
            .iter()
            .find(|s| s.matches.iter().all(|m| m.is_match(text)))
            .map(|s| &s.def)
    }

    pub fn screen_id(&self, screen: &Screen) -> Option<&str> {
        self.find(&screen.text()).map(|d| d.id.as_str())
    }

    pub fn screen_info(&self, screen: &Screen) -> ScreenInfo<'_> {
        match self.find(&screen.text()) {
            Some(d) => ScreenInfo {
                title: &d.title,
                description: &d.description,
                actions: &d.action,
            },
            None => ScreenInfo {
                title: "(辞書に無い画面)",
                description: "- この画面の説明はまだ辞書に無い\n\
                    - マウスを文字に乗せると用語や入力欄の説明が出る",
                actions: &[],
            },
        }
    }

    /// 1 行の文字列の中で、表示桁 col (0 始まり) の位置に当たる語の説明と、
    /// 最初に当たった語の表示桁の範囲 (start_col, end_col) を返す
    pub fn term_notes_at(&self, line: &str, col: usize) -> (Vec<Note>, Option<(usize, usize)>) {
        let mut notes = Vec::new();
        let mut span = None;
        let mut covered: Vec<(usize, usize)> = Vec::new();
        for (re, t) in &self.terms {
            for m in re.find_iter(line) {
                if covered.iter().any(|&(s, e)| m.start() < e && s < m.end()) {
                    continue;
                }
                let start_col = disp_width(&line[..m.start()]);
                let end_col = start_col + disp_width(m.as_str());
                if start_col <= col && col < end_col {
                    covered.push((m.start(), m.end()));
                    if span.is_none() {
                        span = Some((start_col, end_col));
                    }
                    let matched = m.as_str().trim();
                    let title = if t.title.contains(matched) {
                        t.title.clone()
                    } else {
                        format!("{} ({matched})", t.title)
                    };
                    notes.push(Note {
                        title,
                        body: t.note.clone(),
                    });
                }
            }
        }
        (notes, span)
    }

    /// 1 行の文字列の中で、辞書に当たる語のバイト範囲 (start, end) を先頭から順に返す
    pub fn term_byte_spans(&self, line: &str) -> Vec<(usize, usize)> {
        let mut covered: Vec<(usize, usize)> = Vec::new();
        for (re, _) in &self.terms {
            for m in re.find_iter(line) {
                if !covered.iter().any(|&(s, e)| m.start() < e && s < m.end()) {
                    covered.push((m.start(), m.end()));
                }
            }
        }
        covered.sort_by_key(|&(s, _)| s);
        covered
    }

    /// 画面上の (row, col) (0 始まり) の説明
    pub fn notes_at(&self, screen: &Screen, row: usize, col: usize) -> Vec<Note> {
        let text = screen.text();
        let line = screen.row_text(row);
        let def = self.find(&text);
        let (mut notes, _) = self.term_notes_at(&line, col);

        if let Some(def) = def {
            for r in &def.region {
                if (r.rows[0]..=r.rows[1]).contains(&(row + 1))
                    && (r.cols[0]..=r.cols[1]).contains(&(col + 1))
                {
                    notes.push(Note {
                        title: r.title.clone(),
                        body: r.note.clone(),
                    });
                }
            }
            if let Some(b) = def.browse
                && (b.rows[0]..=b.rows[1]).contains(&(row + 1))
                && let Some(n) = self.layout_note(screen, b, row, col)
            {
                notes.push(n);
            }
        }

        // 欄の種類だけでは説明にならないので、ほかの説明があるときと入力欄の上でだけ付ける
        let pos = row * COLS + col;
        if !notes.is_empty() || screen.is_input_box(pos) {
            notes.push(field_note(screen, pos));
        }
        notes
    }

    /// ブラウズ画面のデータ行で、マウス位置がレコードのどの項目かを説明する
    fn layout_note(&self, screen: &Screen, b: BrowseDef, row: usize, col: usize) -> Option<Note> {
        let head = screen.row_text(0);
        let dsn = head.split_whitespace().next()?;
        // メンバー名・世代を除いた名前でも探す (NUAGE.USAGE.MASTER.G0003V00 → NUAGE.USAGE.MASTER)
        let base = Regex::new(r"(\.G\d{4}V\d{2}|\(.*\))$")
            .unwrap()
            .replace(dsn, "");
        let (_, layout) = self
            .layouts
            .iter()
            .find(|(re, _)| re.is_match(dsn) || re.is_match(&base))?;
        let first = Regex::new(r"Col (\d+)")
            .unwrap()
            .captures(&head)?
            .get(1)?
            .as_str()
            .parse::<usize>()
            .ok()?;
        let line = screen.row_text(row);
        let (rec_col, bytes) = if b.hex {
            (first + col / 2, decode_hex(&line))
        } else {
            (first + col, Vec::new())
        };
        let f = layout
            .field
            .iter()
            .find(|f| (f.cols[0]..=f.cols[1]).contains(&rec_col))?;
        let mut body = format!(
            "{} の {}〜{} 桁目 ({})。",
            layout.title,
            f.cols[0],
            f.cols[1],
            kind_name(&f.kind)
        );
        // 16 進表示なら、画面に出ている範囲の値を読める
        let visible = first..first + bytes.len();
        if b.hex && visible.contains(&f.cols[0]) && visible.contains(&f.cols[1]) {
            let raw = &bytes[f.cols[0] - first..=f.cols[1] - first];
            body.push_str(&format!("\n値: {}", decode_value(&f.kind, raw)));
        } else if f.kind == "packed" && !b.hex {
            body.push_str("\nパック 10 進数は文字として読めない。コマンド欄に HEX ON と打つと 16 進で読める。");
        } else if b.hex {
            // 項目の先頭が画面の左端に来るように動かす手順
            let hint = if f.cols[0] >= first {
                format!(
                    "コマンド欄に {} と打って PF11 を押すと (右へ {0} 桁)",
                    f.cols[0] - first
                )
            } else {
                format!(
                    "コマンド欄に {} と打って PF10 を押すと (左へ {0} 桁)",
                    first - f.cols[0]
                )
            };
            body.push_str(&format!(
                "\nこの項目は画面からはみ出している。{hint}、項目の先頭が左端に来て値が読める。"
            ));
        }
        if !f.note.is_empty() {
            body.push('\n');
            body.push_str(&f.note);
        }
        Some(Note {
            title: format!("項目: {}", f.name),
            body,
        })
    }
}

fn disp_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

fn kind_name(kind: &str) -> &str {
    match kind {
        "packed" => "パック 10 進数",
        "zoned" => "数字 (文字)",
        _ => "文字",
    }
}

/// 16 進表示の行 (2 桁で 1 バイト) をバイト列にする。16 進でない文字が出たらそこまで
fn decode_hex(line: &str) -> Vec<u8> {
    let chars: Vec<char> = line.chars().collect();
    chars
        .chunks(2)
        .map_while(|p| {
            let s: String = p.iter().collect();
            u8::from_str_radix(&s, 16).ok()
        })
        .collect()
}

fn decode_value(kind: &str, raw: &[u8]) -> String {
    match kind {
        "packed" => {
            // 1 バイトに 2 桁ずつ。最後の半バイトが符号 (C/F = 正、D = 負)
            let digits: String = raw.iter().map(|b| format!("{b:02X}")).collect();
            let (num, sign) = digits.split_at(digits.len() - 1);
            let value: i128 = num.parse().unwrap_or(0);
            let value = if sign == "D" { -value } else { value };
            format!(
                "{} (16 進 {digits}: 数字 {num} と符号 {sign})",
                group(value)
            )
        }
        _ => {
            let s: String = raw
                .iter()
                .map(|&b| crate::ebcdic::E2A[b as usize])
                .collect();
            format!("\"{}\"", s.trim_end())
        }
    }
}

/// 3 桁ごとにカンマを入れる
fn group(v: i128) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0 { format!("-{out}") } else { out }
}

/// その位置の入力欄の種類
fn field_note(screen: &Screen, pos: usize) -> Note {
    if !screen.formatted() {
        return Note {
            title: "未フォーマット画面".into(),
            body: "画面全体が 1 つの入力欄になっている。どこにでも入力できる。".into(),
        };
    }
    let cell = &screen.cells[pos];
    if let Some(a) = cell.attr {
        return Note {
            title: "属性バイト".into(),
            body: format!(
                "次の欄の性質 (保護・数字・明るさ) を決める位置で、画面上は空白に見える。値は X'{:02X}'。",
                a.byte
            ),
        };
    }
    let Some(a) = screen.field_at(pos) else {
        return Note {
            title: "欄の外".into(),
            body: String::new(),
        };
    };
    let mut kinds = Vec::new();
    if a.protected() {
        kinds.push("表示専用 (入力できない)");
    } else {
        kinds.push("入力欄 (Tab で欄から欄へ移れる)");
    }
    if a.numeric() && !a.protected() {
        kinds.push("数字のみ");
    }
    if a.intensified() {
        kinds.push("強調表示");
    }
    if a.hidden() {
        kinds.push("非表示 (パスワードなど。打った文字は見えない)");
    }
    if a.modified() && !a.protected() {
        kinds.push("変更済み (Enter で送られる)");
    }
    Note {
        title: format!("{}行{}桁", pos / COLS + 1, pos % COLS + 1),
        body: kinds.join("、"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Guide {
        Guide::load(&Sources {
            dirs: vec![],
            builtin: true,
        })
        .unwrap()
    }

    #[test]
    fn builtin_guide_loads() {
        assert!(!builtin().screens.is_empty());
    }

    #[test]
    fn site_guide_extends_builtin() {
        let site = r#"
[[screen]]
id = "site-dslist"
extends = "dslist-entry"
[[screen.action]]
label = "SITE を一覧"
keys = "SITE{enter}"
"#;
        let mut sources = vec![("site.toml".to_string(), site.to_string())];
        sources.extend(BUILTIN.iter().map(|(n, s)| (n.to_string(), s.to_string())));
        let g = Guide::from_sources(sources).unwrap();
        let s = g
            .screens
            .iter()
            .find(|s| s.def.id == "dslist-entry")
            .unwrap();
        assert_eq!(s.def.action[0].label, "SITE を一覧");
        assert!(s.def.action.len() > 1);
    }

    #[test]
    fn packed_decimal() {
        assert_eq!(group(19227), "19,227");
        assert!(
            decode_value("packed", &[0x00, 0x00, 0x00, 0x19, 0x22, 0x7C]).starts_with("19,227 ")
        );
        assert!(decode_value("packed", &[0x00, 0x00, 0x00, 0x00, 0x12, 0x3D]).starts_with("-123 "));
    }

    #[test]
    fn term_notes_on_japanese_line() {
        let g = builtin();
        let line = "- MVS の通信制御 VTAM が出す最初の画面 (USS 画面)";
        // "- " は幅 2、"MVS" は桁 2..5
        let (notes, span) = g.term_notes_at(line, 3);
        assert_eq!(span, Some((2, 5)));
        assert!(notes[0].title.contains("Multiple Virtual Storage"));
        // "VTAM" は桁 17..21
        let (notes, span) = g.term_notes_at(line, 18);
        assert_eq!(span, Some((17, 21)));
        assert!(
            notes[0]
                .title
                .contains("Virtual Telecommunications Access Method")
        );
    }

    #[test]
    fn screen_descriptions_are_compact() {
        let g = builtin();
        for s in &g.screens {
            let lines: Vec<_> = s.def.description.lines().collect();
            assert!(
                lines.len() <= 3,
                "screen {} has {} lines",
                s.def.id,
                lines.len()
            );
            for l in lines {
                assert!(
                    disp_width(l) <= 60,
                    "screen {} line too wide ({}): {l}",
                    s.def.id,
                    disp_width(l)
                );
            }
        }
    }

    #[test]
    fn all_builtin_screens_match() {
        let g = builtin();
        let cases: &[(&str, &[(usize, &str)])] = &[
            (
                "hercules-logo",
                &[
                    (1, "Hercules Version 4.7"),
                    (24, "CLEAR the screen or hit ENTER"),
                ],
            ),
            ("vtam-logon", &[(23, "TSO Logon ===>")]),
            (
                "logon-password",
                &[(2, "ENTER CURRENT PASSWORD FOR IBMUSER-")],
            ),
            ("logon-in-use", &[(1, "IKJ56425I LOGON REJECTED")]),
            (
                "reconnected",
                &[(1, "IKT00300I LOGON RECONNECT SUCCESSFUL")],
            ),
            ("logon-prompt", &[(1, "IKJ56400A ENTER LOGON OR LOGOFF-")]),
            ("erp", &[(1, "IKT00405I SCREEN ERASURE CAUSED BY ERROR")]),
            (
                "tso-welcome",
                &[(1, "WELCOME TO MVS COMMUNITY EDITION"), (12, "READY")],
            ),
            (
                "kicks-startup",
                &[
                    (1, " KICKS version 1.5.0(0) startup using SIT=1$,"),
                    (3, " loading KIKSIT1$ - 80 bytes at AF130, ep AF130"),
                    (24, " ***"),
                ],
            ),
            (
                "kicks-ksgm",
                &[
                    (1, " KSGM for tso user IBMUSER  at terminal U400"),
                    (24, " Press PF1 for help, CLEAR to continue..."),
                ],
            ),
            ("kicks-shutdown", &[(1, " KICKS is shutting down...")]),
            ("tso-more", &[(24, " ***")]),
            ("tso-ready", &[(1, "TIME-08:00:00"), (2, "READY")]),
            ("tso-blank", &[]),
            ("ispf-primary", &[(1, "  ISPF Primary Option Menu")]),
            ("rfe-primary", &[(3, "  REVIEW FRONT END")]),
            (
                "dslist-entry",
                &[(1, "RFE DSLIST"), (6, "Data set name prefix ==> SYS1")],
            ),
            ("dslist", &[(1, "RFE DSLIST"), (2, "S DATA-SET-NAME")]),
            (
                "browse-hex",
                &[
                    (1, " SYS1.PARMLIB on MVS000 ---- Line 1 Col 1 40"),
                    (4, &"----/\\\\\\".repeat(10)),
                ],
            ),
            (
                "browse",
                &[
                    (1, " SYS1.PARMLIB on MVS000 ---- Line 1 Col 1 80"),
                    (4, &"----+---".repeat(10)),
                ],
            ),
        ];

        for &(expected_id, rows) in cases {
            let mut s = Screen::default();
            for &(r, text) in rows {
                s.put_str(r, 1, text);
            }
            assert_eq!(
                g.screen_id(&s),
                Some(expected_id),
                "failed to match screen {expected_id}"
            );
        }
    }

    #[test]
    fn abbreviations_and_terms_resolve() {
        let g = builtin();
        let cases = [
            ("MVS", "Multiple Virtual Storage"),
            ("MVS/CE", "MVS Community Edition"),
            ("VTAM", "Virtual Telecommunications Access Method"),
            ("USS", "Unformatted System Services"),
            ("TSO", "Time Sharing Option"),
            ("ISPF", "Interactive System Productivity Facility"),
            ("RFE", "Review Front End"),
            ("Hercules", "エミュレータ"),
            ("JES2", "Job Entry Subsystem"),
            ("JCL", "Job Control Language"),
            ("DASD", "Direct Access Storage Device"),
            ("VSAM", "Virtual Storage Access Method"),
            ("EBCDIC", "Extended Binary Coded Decimal Interchange Code"),
            ("CCSID", "Coded Character Set Identifier"),
            ("PF3", "Program Function Key"),
            ("PA1", "Program Attention Key"),
            ("CLEAR", "画面消去"),
            ("KICKS", "KICKS for TSO"),
            ("KSSF", "サインオフ"),
            ("KIKSIT1$", "KICKS の表"),
            ("LOGON", "ログオン"),
            ("LOGOFF", "ログオフ"),
            ("RECONNECT", "再接続"),
            ("LISTCAT", "List Catalog"),
            ("HEX ON", "16 進表示"),
            ("COMP-3", "Packed Decimal"),
        ];
        for (word, expected_in_title) in cases {
            let (notes, span) = g.term_notes_at(word, 0);
            assert!(
                !notes.is_empty(),
                "term '{word}' should match in builtin guide"
            );
            assert_eq!(span, Some((0, disp_width(word))));
            assert!(
                notes[0].title.contains(expected_in_title),
                "term '{word}' title '{}' did not contain '{expected_in_title}'",
                notes[0].title
            );
        }
    }

    #[test]
    fn field_note_needs_other_notes_or_input_box() {
        let g = builtin();
        let mut s = Screen::default();
        s.put_field(23, 1, 0x20);
        s.put_str(23, 2, "    Logon ===>");
        s.put_field(23, 17, 0x00);
        s.put_field(23, 40, 0x20);

        // 説明の無い保護領域 (空白) では何も出さない
        assert_eq!(g.screen_id(&s), None);
        assert!(g.notes_at(&s, 4, 10).is_empty());
        // 用語の上では、用語と欄の種類を出す
        let notes = g.notes_at(&s, 22, 6);
        assert!(notes.len() >= 2);
        assert_eq!(notes.last().unwrap().title, "23行7桁");
        // 入力欄の上では、欄の種類だけでも出す
        let notes = g.notes_at(&s, 22, 17);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].body.contains("入力欄"));
    }
}
