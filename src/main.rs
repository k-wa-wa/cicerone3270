mod app;
mod ebcdic;
mod guide;
mod screen;
mod session;
mod telnet;

use std::path::PathBuf;
use std::time::Duration;

use guide::{Guide, Sources};

use screen::{Aid, COLS, ROWS};
use session::Session;

const USAGE: &str = "\
使い方: ci3270 [オプション] [ホスト:ポート]

  ホスト:ポート      接続先 (既定: localhost:3270)
  --guide ディレクトリ  サイトの辞書 (*.toml) を足す。何度でも指定でき、先のものほど優先する。
                     環境変数 CI3270_GUIDE_PATH (: 区切り) でも足せる (存在しないディレクトリは無視する)
  --no-builtin       組み込みの辞書を読まない (組み込みの辞書そのものを書くときに --guide guide と使う)
                     辞書は Ctrl+L で読み直せる
  --plain            ガイドを消した状態 (普通の 3270 端末と同じ画面) で始める。Ctrl+G で切り替えられる
  --script 手順      画面を出さずに手順を流し、各 dump で画面を標準出力に出す (動作確認用)
                     手順は ; 区切り: wait / waitfor 文字列 / type 文字列 / enter / pf1..pf24 / pa1..pa3 /
                     clear / tab / cursor 行 桁 / logoff / dump / explain 行 桁 (その位置の説明)
";

fn main() -> anyhow::Result<()> {
    let mut host = "localhost:3270".to_string();
    let mut script = None;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut builtin = true;
    let mut guided = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--script" => script = args.next(),
            "--guide" => dirs.extend(args.next().map(PathBuf::from)),
            "--no-builtin" => builtin = false,
            "--plain" => guided = false,
            _ if a.starts_with('-') => anyhow::bail!("不明なオプション: {a}\n\n{USAGE}"),
            _ => host = a,
        }
    }

    if let Some(path) = std::env::var_os("CI3270_GUIDE_PATH") {
        dirs.extend(std::env::split_paths(&path).filter(|d| d.is_dir()));
    }
    let sources = Sources { dirs, builtin };
    let guide = Guide::load(&sources)?;
    let mut session =
        Session::connect(&host).map_err(|e| anyhow::anyhow!("{host} に接続できない: {e}"))?;
    match script {
        Some(s) => run_script(&mut session, &guide, &s),
        None => app::App::new(session, guide, sources, guided).run(),
    }
}

fn run_script(session: &mut Session, guide: &Guide, script: &str) -> anyhow::Result<()> {
    for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (cmd, arg) = step.split_once(' ').unwrap_or((step, ""));
        match cmd.to_ascii_lowercase().as_str() {
            "wait" => session.wait_settle(Duration::from_millis(700), Duration::from_secs(15)),
            "waitfor" => {
                if !session.wait_for(arg, Duration::from_secs(60)) {
                    dump(session);
                    anyhow::bail!("waitfor {arg}: 現れなかった");
                }
            }
            "type" => {
                for c in arg.chars() {
                    if let Err(e) = session.screen.type_char(c) {
                        anyhow::bail!("type {arg}: {e}");
                    }
                }
            }
            "enter" => session.press(Aid::Enter),
            "logoff" => {
                // READY が出るまで PF3 で戻ってから LOGOFF する (セッションを残さないため)
                for _ in 0..8 {
                    if session.screen.text().trim_end().ends_with("READY") {
                        break;
                    }
                    session.press(Aid::Pf(3));
                    session.wait_settle(Duration::from_millis(500), Duration::from_secs(10));
                }
                for c in "LOGOFF".chars() {
                    let _ = session.screen.type_char(c);
                }
                session.press(Aid::Enter);
                if !session.wait_for("TSO Logon", Duration::from_secs(20)) {
                    dump(session);
                    anyhow::bail!("LOGOFF できなかった");
                }
            }
            "clear" => session.press(Aid::Clear),
            "tab" => session.screen.tab(),
            "cursor" => {
                let (r, c) = arg.split_once(' ').unwrap_or((arg, "1"));
                session.screen.cursor = (r.parse::<usize>()? - 1) * COLS + c.parse::<usize>()? - 1;
            }
            "dump" => {
                dump(session);
                let info = guide.screen_info(&session.screen);
                println!("  screen: {}", info.title);
                for a in info.actions {
                    println!("  action: {} ({})", a.label, a.keys);
                }
            }
            "explain" => {
                let (r, c) = arg.split_once(' ').unwrap_or((arg, "1"));
                for n in guide.notes_at(
                    &session.screen,
                    r.parse::<usize>()? - 1,
                    c.parse::<usize>()? - 1,
                ) {
                    println!("  [{}] {}", n.title, n.body.replace('\n', " / "));
                }
            }
            c if c.starts_with("pf") => session.press(Aid::Pf(c[2..].parse()?)),
            c if c.starts_with("pa") => session.press(Aid::Pa(c[2..].parse()?)),
            _ => anyhow::bail!("不明な手順: {step}"),
        }
        if let Some(why) = &session.closed {
            anyhow::bail!("切断された: {why}");
        }
    }
    Ok(())
}

/// 画面の文字と、入力欄・カーソルの位置を出す
fn dump(session: &Session) {
    let s = &session.screen;
    println!(
        "---- cursor=({},{}) locked={} formatted={}",
        s.cursor / COLS + 1,
        s.cursor % COLS + 1,
        s.locked,
        s.formatted()
    );
    for r in 0..ROWS {
        println!("{:2}|{}|", r + 1, s.row_text(r));
    }
    for p in s.input_fields() {
        let a = s.cells[p].attr.unwrap();
        println!(
            "  input field at ({},{}) len={} attr={:02X}{}{}",
            (p + 1) % screen::SIZE / COLS + 1,
            (p + 1) % COLS + 1,
            s.field_span(p).len(),
            a.byte,
            if a.hidden() { " hidden" } else { "" },
            if a.numeric() { " numeric" } else { "" },
        );
    }
}
