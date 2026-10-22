// 3270 の画面バッファ。ホストからのデータストリーム (Write 系コマンドとオーダー) を解釈し、
// キー入力を受けて、ホストへ送るレコード (AID・カーソル位置・変更された入力欄) を組み立てる。

use crate::ebcdic::{E2A, a2e};

pub const ROWS: usize = 24;
pub const COLS: usize = 80;
pub const SIZE: usize = ROWS * COLS;

/// 拡張色 (SFE / SA の色属性)。0 は既定
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Ext {
    pub color: u8,
    pub highlight: u8,
}

/// フィールド属性 (属性バイトと拡張属性)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FieldAttr {
    pub byte: u8,
    pub ext: Ext,
}

impl FieldAttr {
    pub fn protected(&self) -> bool {
        self.byte & 0x20 != 0
    }
    pub fn numeric(&self) -> bool {
        self.byte & 0x10 != 0
    }
    pub fn intensified(&self) -> bool {
        self.byte & 0x0C == 0x08
    }
    pub fn hidden(&self) -> bool {
        self.byte & 0x0C == 0x0C
    }
    pub fn modified(&self) -> bool {
        self.byte & 0x01 != 0
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Cell {
    /// EBCDIC の文字 (属性の位置では 0)
    pub ch: u8,
    /// この位置が属性バイトなら Some
    pub attr: Option<FieldAttr>,
    /// SA で付けた文字ごとの拡張属性
    pub ext: Ext,
}

pub enum Aid {
    Enter,
    Pf(u8),
    Pa(u8),
    Clear,
}

impl Aid {
    fn byte(&self) -> u8 {
        const PF: [u8; 24] = [
            0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x7B, 0x7C, 0xC1, 0xC2,
            0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0x4A, 0x4B, 0x4C,
        ];
        match self {
            Aid::Enter => 0x7D,
            Aid::Pf(n) => PF[(*n as usize - 1).min(23)],
            Aid::Pa(1) => 0x6C,
            Aid::Pa(2) => 0x6E,
            Aid::Pa(_) => 0x6B,
            Aid::Clear => 0x6D,
        }
    }
}

/// 6 ビットずつのバッファアドレスの符号表 (12 ビットアドレス)
const ADDR_CODE: [u8; 64] = [
    0x40, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
    0x50, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
    0x60, 0x61, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
];

fn decode_addr(b1: u8, b2: u8) -> usize {
    let a = if b1 & 0xC0 == 0 {
        // 14 ビットアドレス
        ((b1 as usize & 0x3F) << 8) | b2 as usize
    } else {
        ((b1 as usize & 0x3F) << 6) | (b2 as usize & 0x3F)
    };
    a % SIZE
}

fn encode_addr(a: usize) -> [u8; 2] {
    [ADDR_CODE[(a >> 6) & 0x3F], ADDR_CODE[a & 0x3F]]
}

pub struct Screen {
    pub cells: Vec<Cell>,
    pub cursor: usize,
    /// キーボードが禁止されている (AID を送ってからホストが解除するまで)
    pub locked: bool,
    pub insert: bool,
    /// 最後に送った AID (Read Modified に答えるときに使う)
    last_aid: u8,
    /// ホストが画面を書き換えるたびに増える
    pub generation: u64,
}

impl Default for Screen {
    fn default() -> Self {
        Screen {
            cells: vec![Cell::default(); SIZE],
            cursor: 0,
            locked: true,
            insert: false,
            last_aid: 0x60,
            generation: 0,
        }
    }
}

impl Screen {
    pub fn formatted(&self) -> bool {
        self.cells.iter().any(|c| c.attr.is_some())
    }

    /// pos を含むフィールドの属性の位置。未フォーマットなら None
    pub fn field_start(&self, pos: usize) -> Option<usize> {
        (0..SIZE)
            .map(|i| (pos + SIZE - i) % SIZE)
            .find(|&p| self.cells[p].attr.is_some())
    }

    pub fn field_at(&self, pos: usize) -> Option<FieldAttr> {
        self.field_start(pos).and_then(|p| self.cells[p].attr)
    }

    /// フィールドの中身の範囲 (属性の次から次の属性の手前まで。折り返しあり)
    pub fn field_span(&self, start: usize) -> Vec<usize> {
        (1..SIZE)
            .map(|i| (start + i) % SIZE)
            .take_while(|&p| self.cells[p].attr.is_none())
            .collect()
    }

    /// 画面に表示する文字。属性の位置と非表示フィールドは空白
    pub fn display_char(&self, pos: usize) -> char {
        let cell = &self.cells[pos];
        if cell.attr.is_some() {
            return ' ';
        }
        if self.field_at(pos).is_some_and(|f| f.hidden()) {
            return ' ';
        }
        E2A[cell.ch as usize]
    }

    pub fn row_text(&self, row: usize) -> String {
        (row * COLS..(row + 1) * COLS)
            .map(|p| self.display_char(p))
            .collect()
    }

    pub fn text(&self) -> String {
        (0..ROWS)
            .map(|r| self.row_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 入力欄 (保護されていないフィールド) の先頭位置の一覧
    pub fn input_fields(&self) -> Vec<usize> {
        (0..SIZE)
            .filter(|&p| self.cells[p].attr.is_some_and(|a| !a.protected()))
            .collect()
    }

    // ---- ホストからのデータストリーム ----

    /// ホストからのレコード 1 件を処理する。ホストへ返すレコード (Read 系コマンドへの応答) があれば返す
    pub fn process(&mut self, rec: &[u8]) -> Option<Vec<u8>> {
        let &cmd = rec.first()?;
        self.generation += 1;
        match cmd {
            // Write
            0x01 | 0xF1 => self.write(&rec[1..], false),
            // Erase/Write, Erase/Write Alternate
            0x05 | 0xF5 | 0x0D | 0x7E => self.write(&rec[1..], true),
            // Erase All Unprotected
            0x0F | 0x6F => {
                for p in 0..SIZE {
                    if self.cells[p].attr.is_none()
                        && self.field_at(p).is_none_or(|f| !f.protected())
                    {
                        self.cells[p].ch = 0;
                    }
                }
                self.reset_mdt();
                self.cursor = self.next_input(SIZE - 1).unwrap_or(0);
                self.locked = false;
            }
            // Read Buffer
            0x02 | 0xF2 => return Some(self.read_buffer()),
            // Read Modified, Read Modified All
            0x06 | 0xF6 => return Some(self.read_modified(self.last_aid, false)),
            0x0E | 0x6E => return Some(self.read_modified(self.last_aid, true)),
            // Write Structured Field: Read Partition (Query / Query List) にだけ答える。
            // TSO はログオン時に端末の能力を問い合わせ、答えが無いとエラー回復 (IKT00405I) で画面を消す
            0x11 | 0xF3 => {
                let mut i = 1;
                while i + 2 < rec.len() {
                    let len = (rec[i] as usize) << 8 | rec[i + 1] as usize;
                    let len = if len == 0 { rec.len() - i } else { len };
                    if rec[i + 2] == 0x01 && matches!(rec.get(i + 4), Some(0x02 | 0x03)) {
                        return Some(query_reply());
                    }
                    i += len.max(3);
                }
            }
            _ => {}
        }
        None
    }

    fn reset_mdt(&mut self) {
        for c in self.cells.iter_mut() {
            if let Some(a) = c.attr.as_mut() {
                a.byte &= !0x01;
            }
        }
    }

    fn write(&mut self, data: &[u8], erase: bool) {
        if erase {
            self.cells = vec![Cell::default(); SIZE];
            self.cursor = 0;
        }
        let Some(&wcc) = data.first() else {
            return;
        };
        if wcc & 0x01 != 0 {
            self.reset_mdt();
        }
        let mut addr = if erase { 0 } else { self.cursor };
        let mut char_ext = Ext::default();
        let mut i = 1;
        let mut last_was_data = false;
        while i < data.len() {
            let b = data[i];
            i += 1;
            let mut is_data = false;
            match b {
                // SF: 属性バイト 1 つ
                0x1D if i < data.len() => {
                    self.cells[addr] = Cell {
                        ch: 0,
                        attr: Some(FieldAttr {
                            byte: data[i],
                            ext: Ext::default(),
                        }),
                        ext: Ext::default(),
                    };
                    i += 1;
                    addr = (addr + 1) % SIZE;
                }
                // SFE / MF: 型と値の組が n 個
                0x29 | 0x2C if i < data.len() => {
                    let n = data[i] as usize;
                    i += 1;
                    let mut fa = if b == 0x2C {
                        self.cells[addr].attr.unwrap_or(FieldAttr {
                            byte: 0,
                            ext: Ext::default(),
                        })
                    } else {
                        FieldAttr {
                            byte: 0,
                            ext: Ext::default(),
                        }
                    };
                    for _ in 0..n {
                        if i + 1 >= data.len() {
                            break;
                        }
                        let (t, v) = (data[i], data[i + 1]);
                        i += 2;
                        match t {
                            0xC0 => fa.byte = v,
                            0x41 => fa.ext.highlight = v,
                            0x42 => fa.ext.color = v,
                            _ => {}
                        }
                    }
                    if b == 0x29 || self.cells[addr].attr.is_some() {
                        self.cells[addr] = Cell {
                            ch: 0,
                            attr: Some(fa),
                            ext: Ext::default(),
                        };
                    }
                    addr = (addr + 1) % SIZE;
                }
                // SBA
                0x11 if i + 1 < data.len() => {
                    addr = decode_addr(data[i], data[i + 1]);
                    i += 2;
                }
                // SA: 以降の文字の拡張属性
                0x28 if i + 1 < data.len() => {
                    match data[i] {
                        0x00 => char_ext = Ext::default(),
                        0x41 => char_ext.highlight = data[i + 1],
                        0x42 => char_ext.color = data[i + 1],
                        _ => {}
                    }
                    i += 2;
                }
                // IC
                0x13 => self.cursor = addr,
                // PT: 次の入力欄の先頭へ。文字の直後なら、そこまでを NUL で埋める
                0x05 => {
                    let next = self.next_input(addr);
                    if last_was_data {
                        let mut p = addr;
                        while self.cells[p].attr.is_none() {
                            self.cells[p].ch = 0;
                            p = (p + 1) % SIZE;
                            if p == 0 {
                                break;
                            }
                        }
                    }
                    addr = next.unwrap_or(0);
                }
                // RA: 指定位置の手前まで同じ文字で埋める
                0x3C if i + 2 < data.len() => {
                    let stop = decode_addr(data[i], data[i + 1]);
                    let mut ch = data[i + 2];
                    i += 3;
                    if ch == 0x08 && i < data.len() {
                        ch = data[i];
                        i += 1;
                    }
                    loop {
                        self.cells[addr] = Cell {
                            ch,
                            attr: None,
                            ext: char_ext,
                        };
                        addr = (addr + 1) % SIZE;
                        if addr == stop {
                            break;
                        }
                    }
                }
                // EUA: 指定位置の手前まで入力欄を消す
                0x12 if i + 1 < data.len() => {
                    let stop = decode_addr(data[i], data[i + 1]);
                    i += 2;
                    loop {
                        if self.cells[addr].attr.is_none()
                            && self.field_at(addr).is_none_or(|f| !f.protected())
                        {
                            self.cells[addr].ch = 0;
                        }
                        addr = (addr + 1) % SIZE;
                        if addr == stop {
                            break;
                        }
                    }
                }
                // GE: 次の 1 バイトは別の文字集合の文字。表示は通常の文字として扱う
                0x08 if i < data.len() => {
                    self.cells[addr] = Cell {
                        ch: data[i],
                        attr: None,
                        ext: char_ext,
                    };
                    i += 1;
                    addr = (addr + 1) % SIZE;
                    is_data = true;
                }
                _ => {
                    self.cells[addr] = Cell {
                        ch: b,
                        attr: None,
                        ext: char_ext,
                    };
                    addr = (addr + 1) % SIZE;
                    is_data = true;
                }
            }
            last_was_data = is_data;
        }
        // キーボードの復元
        if wcc & 0x02 != 0 {
            self.locked = false;
        }
    }

    // ---- ホストへ送るレコード ----

    fn header(&self, aid: u8) -> Vec<u8> {
        let mut out = vec![aid];
        out.extend_from_slice(&encode_addr(self.cursor));
        out
    }

    fn read_modified(&self, aid: u8, all: bool) -> Vec<u8> {
        // PA と CLEAR は AID だけを送る (short read)
        // 0x6B PA3、0x6C PA1、0x6D CLEAR、0x6E PA2
        if !all && matches!(aid, 0x6B..=0x6E) {
            return vec![aid];
        }
        let mut out = self.header(aid);
        if !self.formatted() {
            out.extend(self.cells.iter().map(|c| c.ch).filter(|&c| c != 0));
            return out;
        }
        for p in 0..SIZE {
            if let Some(a) = self.cells[p].attr
                && a.modified()
            {
                let start = (p + 1) % SIZE;
                out.push(0x11);
                out.extend_from_slice(&encode_addr(start));
                out.extend(
                    self.field_span(p)
                        .into_iter()
                        .map(|q| self.cells[q].ch)
                        .filter(|&c| c != 0),
                );
            }
        }
        out
    }

    fn read_buffer(&self) -> Vec<u8> {
        let mut out = self.header(self.last_aid);
        for c in &self.cells {
            match c.attr {
                Some(a) => {
                    out.push(0x1D);
                    out.push(a.byte);
                }
                None => out.push(c.ch),
            }
        }
        out
    }

    /// AID キーを押す。ホストへ送るレコードを返す
    pub fn press(&mut self, aid: Aid) -> Vec<u8> {
        let b = aid.byte();
        self.last_aid = b;
        let rec = self.read_modified(b, false);
        if matches!(aid, Aid::Clear) {
            self.cells = vec![Cell::default(); SIZE];
            self.cursor = 0;
        }
        self.locked = true;
        rec
    }

    // ---- キー入力 (ローカルの編集) ----

    /// pos より後ろの最初の入力欄の先頭 (属性の次)
    pub fn next_input(&self, pos: usize) -> Option<usize> {
        (1..=SIZE)
            .map(|i| (pos + i) % SIZE)
            .find(|&p| self.cells[p].attr.is_some_and(|a| !a.protected()))
            .map(|p| (p + 1) % SIZE)
    }

    pub fn tab(&mut self) {
        if let Some(p) = self.next_input(self.cursor) {
            self.cursor = p;
        }
    }

    pub fn backtab(&mut self) {
        let starts: Vec<usize> = self
            .input_fields()
            .into_iter()
            .map(|p| (p + 1) % SIZE)
            .collect();
        if starts.is_empty() {
            return;
        }
        // カーソルより前の入力欄の先頭のうち一番近いもの。欄の途中なら、その欄の先頭
        let back = |p: usize| (self.cursor + SIZE - p) % SIZE;
        if let Some(&p) = starts
            .iter()
            .filter(|&&p| back(p) > 0)
            .min_by_key(|&&p| back(p))
        {
            self.cursor = p;
        }
    }

    pub fn move_cursor(&mut self, drow: isize, dcol: isize) {
        let p = self.cursor as isize + drow * COLS as isize + dcol;
        self.cursor = p.rem_euclid(SIZE as isize) as usize;
    }

    pub fn home(&mut self) {
        self.cursor = self.next_input(SIZE - 1).unwrap_or(0);
    }

    /// 入力できない理由。入力できるなら None
    fn input_error(&self, pos: usize, c: Option<char>) -> Option<&'static str> {
        if self.locked {
            return Some("応答待ちでロック中 (Ctrl+R で解除)");
        }
        if self.cells[pos].attr.is_some() {
            return Some("属性位置には入力できない (Tab で入力欄へ)");
        }
        if let Some(f) = self.field_at(pos) {
            if f.protected() {
                return Some("表示専用の欄である (Tab で入力欄へ)");
            }
            if let Some(c) = c
                && f.numeric()
                && !(c.is_ascii_digit() || c == '.' || c == '-')
            {
                return Some("数字専用の入力欄である");
            }
        }
        None
    }

    fn set_mdt(&mut self, pos: usize) {
        if let Some(s) = self.field_start(pos)
            && let Some(a) = self.cells[s].attr.as_mut()
        {
            a.byte |= 0x01;
        }
    }

    /// 入力欄でカーソル位置から欄の終わりまで
    fn rest_of_field(&self, pos: usize) -> Vec<usize> {
        let mut v = Vec::new();
        let mut p = pos;
        loop {
            if self.cells[p].attr.is_some() {
                break;
            }
            v.push(p);
            p = (p + 1) % SIZE;
            if p == pos {
                break;
            }
        }
        v
    }

    pub fn type_char(&mut self, c: char) -> Result<(), &'static str> {
        let pos = self.cursor;
        if let Some(e) = self.input_error(pos, Some(c)) {
            return Err(e);
        }
        let Some(e) = a2e(c) else {
            return Err("3270 で使えない文字である");
        };
        if self.insert {
            let rest = self.rest_of_field(pos);
            let last = *rest.last().unwrap();
            if self.cells[last].ch != 0 && self.cells[last].ch != 0x40 {
                return Err("挿入できない (欄の末尾が埋まっている)");
            }
            for w in rest.windows(2).rev() {
                self.cells[w[1]].ch = self.cells[w[0]].ch;
            }
        }
        self.cells[pos].ch = e;
        self.set_mdt(pos);
        self.cursor = (pos + 1) % SIZE;
        // 欄の終わりに達したら次の入力欄へ (自動スキップ)
        if self.cells[self.cursor].attr.is_some()
            && let Some(p) = self.next_input(pos)
        {
            self.cursor = p;
        }
        Ok(())
    }

    /// カーソル位置の文字を消し、欄の残りを左へ詰める
    pub fn delete(&mut self) -> Result<(), &'static str> {
        let pos = self.cursor;
        if let Some(e) = self.input_error(pos, None) {
            return Err(e);
        }
        let rest = self.rest_of_field(pos);
        for w in rest.windows(2) {
            self.cells[w[0]].ch = self.cells[w[1]].ch;
        }
        self.cells[*rest.last().unwrap()].ch = 0;
        self.set_mdt(pos);
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<(), &'static str> {
        let prev = (self.cursor + SIZE - 1) % SIZE;
        if self.input_error(prev, None).is_some() {
            return Err("ここより前は消せない");
        }
        self.cursor = prev;
        self.delete()
    }

    /// カーソル位置から欄の終わりまで消す (Erase EOF)
    pub fn erase_eof(&mut self) -> Result<(), &'static str> {
        let pos = self.cursor;
        if let Some(e) = self.input_error(pos, None) {
            return Err(e);
        }
        for p in self.rest_of_field(pos) {
            self.cells[p].ch = 0;
        }
        self.set_mdt(pos);
        Ok(())
    }
}

/// Read Partition Query への応答。x3270 の応答を元に、モデル 2 (24 行 80 桁) の値にしたもの
fn query_reply() -> Vec<u8> {
    const FIELDS: &[&[u8]] = &[
        // Summary: 以下の応答の一覧
        &[0x81, 0x80, 0x80, 0x81, 0x84, 0x85, 0x86, 0x87, 0x88, 0xA6],
        // Usable Area: 12/14 ビットアドレス、80 x 24、バッファ 1920 バイト
        &[
            0x81, 0x81, 0x01, 0x00, 0x00, 0x50, 0x00, 0x18, 0x01, 0x00, 0x0A, 0x02, 0xE5, 0x00,
            0x02, 0x00, 0x6F, 0x09, 0x0C, 0x07, 0x80,
        ],
        // Alphanumeric Partitions
        &[0x81, 0x84, 0x00, 0x07, 0x80, 0x00],
        // Character Sets
        &[
            0x81, 0x85, 0x82, 0x00, 0x09, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x10, 0x00,
            0x02, 0xB9, 0x00, 0x25, 0x01, 0x00, 0xF1, 0x03, 0xC3, 0x01, 0x36,
        ],
        // Color: 8 色
        &[
            0x81, 0x86, 0x00, 0x10, 0x00, 0xF4, 0xF1, 0xF1, 0xF2, 0xF2, 0xF3, 0xF3, 0xF4, 0xF4,
            0xF5, 0xF5, 0xF6, 0xF6, 0xF7, 0xF7, 0xF8, 0xF8, 0xF9, 0xF9, 0xFA, 0xFA, 0xFB, 0xFB,
            0xFC, 0xFC, 0xFD, 0xFD, 0xFE, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF,
        ],
        // Highlighting: 点滅・反転・下線・強調
        &[
            0x81, 0x87, 0x05, 0x00, 0xF0, 0xF1, 0xF1, 0xF2, 0xF2, 0xF4, 0xF4, 0xF8, 0xF8,
        ],
        // Reply Modes
        &[0x81, 0x88, 0x00, 0x01, 0x02],
        // Implicit Partition: 既定も代替も 80 x 24
        &[
            0x81, 0xA6, 0x00, 0x00, 0x0B, 0x01, 0x00, 0x00, 0x50, 0x00, 0x18, 0x00, 0x50, 0x00,
            0x18,
        ],
    ];
    let mut out = vec![0x88];
    for f in FIELDS {
        let len = f.len() + 2;
        out.push((len >> 8) as u8);
        out.push(len as u8);
        out.extend_from_slice(f);
    }
    out
}

#[cfg(test)]
impl Screen {
    /// テスト用: 1 始まりの (row, col) から文字列を書く
    pub fn put_str(&mut self, row: usize, col: usize, s: &str) {
        let start = (row - 1) * COLS + (col - 1);
        for (i, c) in s.chars().enumerate() {
            if let Some(e) = a2e(c) {
                self.cells[(start + i) % SIZE].ch = e;
            }
        }
    }

    /// テスト用: 1 始まりの (row, col) に属性バイトを置く
    pub fn put_field(&mut self, row: usize, col: usize, byte: u8) {
        let pos = (row - 1) * COLS + (col - 1);
        self.cells[pos] = Cell {
            ch: 0,
            attr: Some(FieldAttr {
                byte,
                ext: Ext::default(),
            }),
            ext: Ext::default(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_field_editing_and_read_modified() {
        let mut s = Screen {
            locked: false,
            ..Screen::default()
        };
        // 1 行 1 桁: 保護属性、2〜15 桁: "TSO Logon ===>"
        s.put_field(1, 1, 0x20);
        s.put_str(1, 2, "TSO Logon ===>");
        // 1 行 16 桁: 非保護の入力欄 (長さ 10)、27 桁: 保護属性
        s.put_field(1, 16, 0x00);
        s.put_field(1, 27, 0x20);

        s.home();
        assert_eq!(s.cursor, 16);
        for c in "IBMUSER".chars() {
            s.type_char(c).unwrap();
        }
        assert!(s.row_text(0).contains("TSO Logon ===> IBMUSER"));
        s.backspace().unwrap();
        assert!(s.row_text(0).contains("TSO Logon ===> IBMUSE "));
        s.erase_eof().unwrap();

        s.home();
        for c in "LOGON".chars() {
            s.type_char(c).unwrap();
        }
        let rec = s.press(Aid::Enter);
        assert_eq!(rec[0], 0x7D); // Enter AID
        assert!(s.locked);
    }

    #[test]
    fn input_errors_are_reported() {
        let mut s = Screen::default();
        assert!(s.type_char('A').unwrap_err().contains("ロック中"));

        s.locked = false;
        s.put_field(1, 1, 0x20); // 保護欄
        s.put_field(1, 10, 0x10); // 数字専用欄
        s.put_field(1, 20, 0x20);

        s.cursor = 0; // 属性バイトの位置
        assert!(s.type_char('A').unwrap_err().contains("属性位置"));

        s.cursor = 1; // 保護欄の中
        assert!(s.type_char('A').unwrap_err().contains("表示専用"));

        s.cursor = 10; // 数字専用欄の中
        assert!(s.type_char('A').unwrap_err().contains("数字専用"));
        assert!(s.type_char('9').is_ok());
    }
}
