// TUI 本体。左に 3270 の画面、右にガイド、下に状態行とキーのボタンを出す。
// マウスを 3270 の画面やガイドの用語に乗せると、説明をツールチップで出す。
// Ctrl+G でガイドを消すと、3270 の画面と状態行だけの普通の端末になる。

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use unicode_width::UnicodeWidthChar;

use crate::guide::{Guide, Note, Sources};
use crate::screen::{Aid, COLS, ROWS, Screen};
use crate::session::Session;

/// 3270 の画面の枠を含む大きさ
const TERM_W: u16 = COLS as u16 + 2;
const TERM_H: u16 = ROWS as u16 + 2;
/// 案内欄の幅 (最小より狭ければ案内欄を下に置く)
const SIDE_MIN_W: u16 = 34;
const SIDE_MAX_W: u16 = 72;

/// ボタン (キーの代わりにクリックで押せるもの)
#[derive(Clone)]
enum Button {
    Key(Step),
    Action(usize),
    Quit,
}

/// OS のマウスポインタの形 (OSC 22 対応端末で切り替える)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointer {
    Default,
    Hand,
    Text,
}

impl Pointer {
    fn osc22(self) -> &'static str {
        match self {
            Pointer::Default => "\x1b]22;default\x07",
            Pointer::Hand => "\x1b]22;pointer\x07",
            Pointer::Text => "\x1b]22;text\x07",
        }
    }
}

/// 操作の 1 手
#[derive(Clone, Debug)]
enum Step {
    Text(String),
    Aid(AidKey),
    Tab,
    Home,
    EraseEof,
    Reset,
}

#[derive(Clone, Copy, Debug)]
enum AidKey {
    Enter,
    Pf(u8),
    Pa(u8),
    Clear,
}

impl AidKey {
    fn aid(self) -> Aid {
        match self {
            AidKey::Enter => Aid::Enter,
            AidKey::Pf(n) => Aid::Pf(n),
            AidKey::Pa(n) => Aid::Pa(n),
            AidKey::Clear => Aid::Clear,
        }
    }
}

/// 辞書の keys ("LOGON IBMUSER{enter}") を手順にする
fn parse_keys(keys: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut rest = keys;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('{')
            && let Some(end) = stripped.find('}')
        {
            let name = stripped[..end].to_ascii_lowercase();
            rest = &stripped[end + 1..];
            let step = match name.as_str() {
                "enter" => Step::Aid(AidKey::Enter),
                "clear" => Step::Aid(AidKey::Clear),
                "tab" => Step::Tab,
                "home" => Step::Home,
                "eraseeof" => Step::EraseEof,
                n if n.starts_with("pf") => Step::Aid(AidKey::Pf(n[2..].parse().unwrap_or(1))),
                n if n.starts_with("pa") => Step::Aid(AidKey::Pa(n[2..].parse().unwrap_or(1))),
                _ => continue,
            };
            steps.push(step);
        } else {
            let end = rest.find('{').unwrap_or(rest.len()).max(1);
            steps.push(Step::Text(rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
    steps
}

pub struct App {
    session: Session,
    guide: Guide,
    sources: Sources,
    /// マウスが乗っている端末上の座標 (列, 行)
    hover: Option<(u16, u16)>,
    /// 一時的なお知らせ (入力エラーなど)
    message: Option<(String, Instant)>,
    /// 実行待ちの手順 (辞書の「できること」を押したとき)
    pending: VecDeque<Step>,
    /// 前回描いたボタンの位置
    buttons: Vec<(Rect, Button)>,
    /// 前回描いたガイド欄の各行 (開始 x, 行 y, 文字列)
    side_lines: Vec<(u16, u16, String)>,
    /// 前回描いた 3270 の画面の文字の範囲 (枠の内側)
    term_inner: Rect,
    /// ガイド (ガイド欄・ツールチップ・ボタン) を出すか。消すと普通の 3270 端末として振る舞う
    guided: bool,
    pointer: Pointer,
    quit_armed: Option<Instant>,
    quit: bool,
}

impl App {
    pub fn new(session: Session, guide: Guide, sources: Sources, guided: bool) -> App {
        App {
            session,
            guide,
            sources,
            hover: None,
            message: None,
            pending: VecDeque::new(),
            buttons: Vec::new(),
            side_lines: Vec::new(),
            term_inner: Rect::default(),
            guided,
            pointer: Pointer::Default,
            quit_armed: None,
            quit: false,
        }
    }

    pub fn run(mut self) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        let _ = io::Write::write_all(&mut io::stdout(), Pointer::Default.osc22().as_bytes());
        let result = self.event_loop(&mut terminal);
        let _ = io::Write::write_all(&mut io::stdout(), b"\x1b]22;\x07");
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
        result
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
        let mut dirty = true;
        while !self.quit {
            if self.session.poll() {
                dirty = true;
            }
            if self.run_pending() {
                dirty = true;
            }
            if self
                .message
                .as_ref()
                .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
            {
                self.message = None;
                dirty = true;
            }
            if dirty {
                terminal.draw(|f| self.draw(f))?;
                self.update_pointer();
                dirty = false;
            }
            if event::poll(Duration::from_millis(30))? {
                // 溜まっているイベントをまとめて処理してから描く (マウス移動の連続で描画が追いつかなくならないように)
                loop {
                    self.handle_event(event::read()?);
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
                dirty = true;
            }
        }
        Ok(())
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    /// 実行待ちの手順を、キーボードが使える間だけ進める
    fn run_pending(&mut self) -> bool {
        let mut progressed = false;
        while !self.session.screen.locked
            && let Some(step) = self.pending.pop_front()
        {
            progressed = true;
            self.do_step(step);
        }
        progressed
    }

    fn do_step(&mut self, step: Step) {
        let screen = &mut self.session.screen;
        let result = match step {
            Step::Text(t) => t.chars().try_for_each(|c| screen.type_char(c)),
            Step::Aid(k) => {
                if screen.locked {
                    Err("応答待ちでロック中 (Ctrl+R で解除)")
                } else {
                    self.session.press(k.aid());
                    Ok(())
                }
            }
            Step::Tab => {
                screen.tab();
                Ok(())
            }
            Step::Home => {
                screen.home();
                Ok(())
            }
            Step::EraseEof => screen.erase_eof(),
            Step::Reset => {
                screen.locked = false;
                Ok(())
            }
        };
        if let Err(e) = result {
            self.pending.clear();
            self.notify(e);
        }
    }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) => {
                let text: String = s.chars().filter(|c| !c.is_control()).collect();
                self.do_step(Step::Text(text));
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if !matches!(k.code, KeyCode::Char('q')) || !ctrl {
            self.quit_armed = None;
        }
        let s = &mut self.session.screen;
        let step = match k.code {
            KeyCode::Char('q') if ctrl => {
                self.request_quit();
                return;
            }
            KeyCode::Char('c') if ctrl => Step::Aid(AidKey::Clear),
            KeyCode::Char('r') if ctrl => Step::Reset,
            KeyCode::Char('e') if ctrl => Step::EraseEof,
            KeyCode::Char('p') if ctrl => Step::Aid(AidKey::Pa(1)),
            KeyCode::Char('l') if ctrl => {
                self.reload_guide();
                return;
            }
            KeyCode::Char('g') if ctrl => {
                self.guided = !self.guided;
                return;
            }
            KeyCode::Char(c) if !ctrl => Step::Text(c.to_string()),
            KeyCode::Enter => Step::Aid(AidKey::Enter),
            KeyCode::Tab => Step::Tab,
            KeyCode::BackTab => {
                s.backtab();
                return;
            }
            KeyCode::Home => Step::Home,
            KeyCode::Left => {
                s.move_cursor(0, -1);
                return;
            }
            KeyCode::Right => {
                s.move_cursor(0, 1);
                return;
            }
            KeyCode::Up => {
                s.move_cursor(-1, 0);
                return;
            }
            KeyCode::Down => {
                s.move_cursor(1, 0);
                return;
            }
            KeyCode::Backspace => {
                if let Err(e) = s.backspace() {
                    self.notify(e);
                }
                return;
            }
            KeyCode::Delete => {
                if let Err(e) = s.delete() {
                    self.notify(e);
                }
                return;
            }
            KeyCode::Insert => {
                s.insert = !s.insert;
                return;
            }
            KeyCode::F(n) if (1..=12).contains(&n) => {
                Step::Aid(AidKey::Pf(if shift { n + 12 } else { n }))
            }
            KeyCode::F(n) if (13..=24).contains(&n) => Step::Aid(AidKey::Pf(n)),
            _ => return,
        };
        self.do_step(step);
    }

    fn request_quit(&mut self) {
        // ログオン前の画面なら、そのまま終える
        let id = self.guide.screen_id(&self.session.screen);
        let safe =
            self.session.closed.is_some() || matches!(id, Some("hercules-logo" | "vtam-logon"));
        if safe
            || self
                .quit_armed
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            self.quit = true;
        } else {
            self.quit_armed = Some(Instant::now());
            self.notify("LOGOFF しないとセッションが残る。もう一度押すと終了");
        }
    }

    fn reload_guide(&mut self) {
        match Guide::load(&self.sources) {
            Ok(g) => {
                self.guide = g;
                self.notify("辞書を読み直した");
            }
            Err(e) => self.notify(format!("辞書の読み込みに失敗: {e}")),
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let inner = self.term_inner;
        let cell = inner
            .contains(pos)
            .then(|| ((m.row - inner.y) as usize, (m.column - inner.x) as usize));
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = Some((m.column, m.row));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((r, c)) = cell {
                    // クリックした位置へカーソルを移す
                    self.session.screen.cursor = r * COLS + c;
                    return;
                }
                let hit = self
                    .buttons
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .map(|(_, b)| b.clone());
                match hit {
                    Some(Button::Key(step)) => self.do_step(step),
                    Some(Button::Action(i)) => {
                        let info = self.guide.screen_info(&self.session.screen);
                        if let Some(a) = info.actions.get(i) {
                            self.pending = parse_keys(&a.keys).into();
                        }
                    }
                    Some(Button::Quit) => self.request_quit(),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn update_pointer(&mut self) {
        let next = self.desired_pointer();
        if next != self.pointer {
            self.pointer = next;
            let _ = io::Write::write_all(&mut io::stdout(), next.osc22().as_bytes());
            let _ = io::Write::flush(&mut io::stdout());
        }
    }

    fn desired_pointer(&self) -> Pointer {
        let Some((mx, my)) = self.hover else {
            return Pointer::Default;
        };
        let pos = Position::new(mx, my);
        if self.buttons.iter().any(|(r, _)| r.contains(pos)) {
            return Pointer::Hand;
        }
        let term_inner = self.term_inner;
        if term_inner.contains(pos) {
            let r = (my - term_inner.y) as usize;
            let c = (mx - term_inner.x) as usize;
            if self.session.screen.is_input_box(r * COLS + c) {
                return Pointer::Text;
            }
        }
        Pointer::Default
    }

    // ---- 描画 ----

    fn draw(&mut self, f: &mut Frame) {
        self.buttons.clear();
        self.side_lines.clear();
        let area = f.area();
        if !self.guided {
            self.draw_plain(f, area);
            return;
        }
        // 左に 3270 の画面・状態行・ボタン、右にガイド。狭ければガイドを下に置く
        let (left, side) = if area.width >= TERM_W + SIDE_MIN_W {
            let [l, s] = Layout::horizontal([
                Constraint::Length(TERM_W),
                Constraint::Length((area.width - TERM_W).min(SIDE_MAX_W)),
            ])
            .areas(area);
            (l, Some(s))
        } else {
            (area, None)
        };
        let status_lines = self.status_lines(left.width as usize);
        let [term, status, keys, rest] = Layout::vertical([
            Constraint::Length(TERM_H),
            Constraint::Length(status_lines.len() as u16),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .areas(left);
        self.draw_terminal(f, term);
        f.render_widget(Paragraph::new(status_lines), status);
        self.draw_keys(f, keys);
        self.draw_side(f, side.unwrap_or(rest));
        self.draw_tooltip(f);
    }

    /// ガイドを消したときの画面。普通の 3270 端末と同じく、24 行 80 桁の画面と、その下の状態行だけを出す
    fn draw_plain(&mut self, f: &mut Frame, area: Rect) {
        let [term, status] =
            Layout::vertical([Constraint::Length(ROWS as u16), Constraint::Min(0)]).areas(area);
        let term = Rect {
            width: term.width.min(COLS as u16),
            ..term
        };
        self.term_inner = term;
        self.draw_cells(f, term);
        let mut lines = self.status_lines(area.width as usize);
        if let Some(first) = lines.first_mut() {
            first.spans.push(Span::styled(
                "  Ctrl+G ガイド",
                Style::default().fg(Color::DarkGray),
            ));
        }
        f.render_widget(Paragraph::new(lines), status);
    }

    fn draw_terminal(&mut self, f: &mut Frame, area: Rect) {
        let title = format!(" 3270 端末  {} ", self.session.host);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title);
        f.render_widget(block, area);
        let inner = inner(area);
        self.term_inner = inner;
        self.draw_cells(f, inner);
    }

    /// 3270 の画面の文字を area に描き、カーソルを置く。ガイドを出しているときは、マウスの乗った文字を反転する
    fn draw_cells(&self, f: &mut Frame, inner: Rect) {
        let s = &self.session.screen;
        let hover_cell = self.hover.filter(|_| self.guided).and_then(|(mx, my)| {
            inner
                .contains(Position::new(mx, my))
                .then(|| ((my - inner.y) as usize, (mx - inner.x) as usize))
        });
        let buf = f.buffer_mut();
        for r in 0..ROWS.min(inner.height as usize) {
            for c in 0..COLS.min(inner.width as usize) {
                let p = r * COLS + c;
                let mut style = cell_style(s, p);
                if hover_cell == Some((r, c)) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some(cell) =
                    buf.cell_mut(Position::new(inner.x + c as u16, inner.y + r as u16))
                {
                    cell.set_char(s.display_char(p)).set_style(style);
                }
            }
        }
        let cur = s.cursor;
        f.set_cursor_position(Position::new(
            inner.x + (cur % COLS) as u16,
            inner.y + (cur / COLS) as u16,
        ));
    }

    fn draw_side(&mut self, f: &mut Frame, area: Rect) {
        if area.height < 3 {
            return;
        }
        let info = self.guide.screen_info(&self.session.screen);
        let title_str = info.title.to_string();
        let desc_str = info.description.to_string();
        let actions: Vec<_> = info.actions.iter().map(|a| a.label.clone()).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" ガイド ");
        let inner = block.inner(area);
        f.render_widget(block, area);

        let width = inner.width as usize;
        let mut lines = styled_lines(
            &title_str,
            width,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        lines.push(Line::raw(""));
        lines.extend(desc_lines(&self.guide, &desc_str, width));
        let desc_h = (lines.len() as u16).min(inner.height);
        self.render_side_lines(
            f,
            Rect {
                height: desc_h,
                ..inner
            },
            lines,
        );

        let mut y = inner.y + desc_h + 1;
        let bottom = inner.y + inner.height;
        if !actions.is_empty() && y < bottom {
            self.render_side_lines(
                f,
                Rect {
                    y,
                    height: 1,
                    ..inner
                },
                vec![Line::from(Span::styled(
                    "できること (クリックで実行)",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))],
            );
            y += 1;
            for (i, label) in actions.iter().enumerate() {
                if y >= bottom {
                    break;
                }
                let rect = Rect {
                    y,
                    height: 1,
                    ..inner
                };
                let hovered = self
                    .hover
                    .is_some_and(|(mx, my)| rect.contains(Position::new(mx, my)));
                let label_style = if hovered {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().add_modifier(Modifier::UNDERLINED)
                };
                self.render_side_lines(
                    f,
                    rect,
                    vec![Line::from(vec![
                        Span::styled(" ▶ ", Style::default().fg(Color::Cyan)),
                        Span::styled(label.clone(), label_style),
                    ])],
                );
                self.buttons.push((rect, Button::Action(i)));
                y += 1;
            }
            y += 1;
        }

        if y + 1 < bottom {
            let lines = key_help_lines(width);
            self.render_side_lines(
                f,
                Rect {
                    y,
                    height: bottom - y,
                    ..inner
                },
                lines,
            );
        }

        // ガイド欄の用語にマウスが乗っていれば反転させる
        if let Some((mx, my)) = self.hover
            && let Some((lx, _, text)) = self
                .side_lines
                .iter()
                .find(|(lx, ly, _)| *ly == my && mx >= *lx)
            && let (_, Some((sc, ec))) = self.guide.term_notes_at(text, (mx - lx) as usize)
        {
            let buf = f.buffer_mut();
            for x in (lx + sc as u16)..(lx + ec as u16).min(inner.x + inner.width) {
                if let Some(cell) = buf.cell_mut(Position::new(x, my)) {
                    cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }

    fn render_side_lines(&mut self, f: &mut Frame, rect: Rect, lines: Vec<Line<'static>>) {
        for (i, line) in lines.iter().enumerate() {
            let y = rect.y + i as u16;
            if y >= rect.y + rect.height {
                break;
            }
            let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if !plain.is_empty() {
                self.side_lines.push((rect.x, y, plain));
            }
        }
        f.render_widget(Paragraph::new(lines), rect);
    }

    fn status_lines(&self, width: usize) -> Vec<Line<'static>> {
        let s = &self.session.screen;
        let mut spans = Vec::new();
        if let Some(why) = &self.session.closed {
            spans.push(Span::styled(
                format!(" 切断: {why} "),
                Style::default().fg(Color::White).bg(Color::Red),
            ));
        } else if s.locked {
            spans.push(Span::styled(
                " X 応答待ち ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ));
        } else {
            spans.push(Span::styled(
                " 入力できる ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ));
        }
        spans.push(Span::raw(format!(
            "  {:02}行{:02}桁",
            s.cursor / COLS + 1,
            s.cursor % COLS + 1
        )));
        if s.insert {
            spans.push(Span::styled("  挿入", Style::default().fg(Color::Yellow)));
        }
        if !self.pending.is_empty() {
            spans.push(Span::styled("  実行中…", Style::default().fg(Color::Cyan)));
        }
        let prefix_w: usize = spans.iter().map(|sp| disp_width(sp.content.as_ref())).sum();
        let mut lines = Vec::new();
        if let Some((m, _)) = &self.message {
            let inline = format!("  {m}");
            if prefix_w + disp_width(&inline) <= width {
                spans.push(Span::styled(inline, Style::default().fg(Color::LightRed)));
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(spans));
                lines.extend(styled_lines(m, width, Style::default().fg(Color::LightRed)));
            }
        } else {
            lines.push(Line::from(spans));
        }
        lines
    }

    fn draw_keys(&mut self, f: &mut Frame, area: Rect) {
        let mut buttons: Vec<(String, Button)> = vec![
            ("Enter".into(), Button::Key(Step::Aid(AidKey::Enter))),
            ("PF3 戻る".into(), Button::Key(Step::Aid(AidKey::Pf(3)))),
            ("PF7 上".into(), Button::Key(Step::Aid(AidKey::Pf(7)))),
            ("PF8 下".into(), Button::Key(Step::Aid(AidKey::Pf(8)))),
            ("PF10 左".into(), Button::Key(Step::Aid(AidKey::Pf(10)))),
            ("PF11 右".into(), Button::Key(Step::Aid(AidKey::Pf(11)))),
            ("PA1 中断".into(), Button::Key(Step::Aid(AidKey::Pa(1)))),
            ("PA2".into(), Button::Key(Step::Aid(AidKey::Pa(2)))),
            ("CLEAR".into(), Button::Key(Step::Aid(AidKey::Clear))),
            ("Reset".into(), Button::Key(Step::Reset)),
        ];
        buttons.push(("終了".into(), Button::Quit));
        let (mut x, mut y) = (area.x, area.y);
        for (label, b) in buttons {
            let w = ratatui::text::Line::raw(format!("[{label}]")).width() as u16;
            if x + w > area.x + area.width {
                x = area.x;
                y += 1;
            }
            if y >= area.y + area.height {
                break;
            }
            let rect = Rect {
                x,
                y,
                width: w,
                height: 1,
            };
            let hovered = self
                .hover
                .is_some_and(|(mx, my)| rect.contains(Position::new(mx, my)));
            let style = if hovered {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Black).bg(Color::Gray)
            };
            f.render_widget(
                Paragraph::new(Span::styled(format!("[{label}]"), style)),
                rect,
            );
            self.buttons.push((rect, b));
            x += w + 1;
        }
    }

    fn draw_tooltip(&self, f: &mut Frame) {
        let Some((mx, my)) = self.hover else {
            return;
        };
        let term_inner = self.term_inner;
        let notes = if term_inner.contains(Position::new(mx, my)) {
            let r = (my - term_inner.y) as usize;
            let c = (mx - term_inner.x) as usize;
            self.guide.notes_at(&self.session.screen, r, c)
        } else if let Some((lx, _, text)) = self
            .side_lines
            .iter()
            .find(|(lx, ly, _)| *ly == my && mx >= *lx)
        {
            self.guide.term_notes_at(text, (mx - lx) as usize).0
        } else {
            Vec::new()
        };
        if notes.is_empty() {
            return;
        }
        let area = f.area();
        let width = 54.min(area.width.saturating_sub(2)).max(20);
        let lines = note_lines(&notes, width as usize - 2);
        let height = (lines.len() as u16 + 2).min(area.height);
        let para = Paragraph::new(lines);
        // マウスの右下に出し、はみ出すなら反対側へ
        let x = if mx + 2 + width <= area.width {
            mx + 2
        } else {
            mx.saturating_sub(width + 1)
        };
        let y = if my + 1 + height <= area.height {
            my + 1
        } else {
            my.saturating_sub(height)
        };
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        f.render_widget(Clear, rect);
        f.render_widget(
            para.block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow))
                    .style(Style::default().bg(Color::Black)),
            ),
            rect,
        );
    }
}

/// 表示幅 width で折り返す。日本語の文は空白で区切れないため文字単位で折り、
/// 英数字の並び (LOGON など) は途中で切らずに語の前で折る。
/// 行頭に来てはいけない句読点・閉じ括弧は前の行に残す
fn wrap(text: &str, width: usize) -> Vec<String> {
    const NO_HEAD: &str = "。、，．）」』】)]!?！？";
    let width = width.max(4);
    let mut out = Vec::new();
    for para in text.lines() {
        let bullet_indent = if para.starts_with("- ") { "  " } else { "" };
        // 英数字・記号の並び (空白以外の ASCII) と、それ以外の 1 文字ずつに分ける
        let mut tokens: Vec<String> = Vec::new();
        for c in para.chars() {
            let ascii_word = c.is_ascii() && c != ' ';
            match tokens.last_mut() {
                Some(t) if ascii_word && t.chars().all(|x| x.is_ascii() && x != ' ') => t.push(c),
                _ => tokens.push(c.to_string()),
            }
        }
        let mut line = String::new();
        let mut w = 0;
        for t in tokens {
            let tw = disp_width(&t);
            let head = t.chars().next().is_some_and(|c| NO_HEAD.contains(c));
            if w + tw > width && !line.is_empty() && !head {
                out.push(std::mem::take(&mut line).trim_end().to_string());
                line = bullet_indent.to_string();
                w = bullet_indent.len();
                if t == " " {
                    continue;
                }
            }
            // 1 行に収まらない長い語は文字単位で折る
            for c in t.chars() {
                let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                if w + cw > width && !line.is_empty() && !head {
                    out.push(std::mem::take(&mut line));
                    line = bullet_indent.to_string();
                    w = bullet_indent.len();
                }
                line.push(c);
                w += cw;
            }
        }
        out.push(line.trim_end().to_string());
    }
    out
}

fn disp_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

fn styled_lines(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, style)))
        .collect()
}

/// 画面の説明文を折り返し、箇条書きの記号と辞書にある用語を色分けする
fn desc_lines(guide: &Guide, text: &str, width: usize) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|l| {
            let mut spans = Vec::new();
            let rest = if let Some(stripped) = l.strip_prefix("- ") {
                spans.push(Span::styled("- ", Style::default().fg(Color::Cyan)));
                stripped
            } else {
                l.as_str()
            };
            let mut pos = 0;
            for (start, end) in guide.term_byte_spans(rest) {
                if pos < start {
                    spans.push(Span::raw(rest[pos..start].to_string()));
                }
                spans.push(Span::styled(
                    rest[start..end].to_string(),
                    Style::default().fg(Color::LightCyan),
                ));
                pos = end;
            }
            if pos < rest.len() {
                spans.push(Span::raw(rest[pos..].to_string()));
            }
            Line::from(spans)
        })
        .collect()
}

const KEY_HELP_LEFT: &[(&str, &str)] = &[
    ("Enter", "送信"),
    ("Tab / S-Tab", "入力欄の移動"),
    ("Home", "コマンド欄へ"),
    ("Ctrl+E", "欄の残りを消す"),
    ("Insert", "挿入モード切替"),
    ("Ctrl+L", "辞書の読み直し"),
    ("ホバー", "用語・欄の説明"),
    ("Ctrl+G", "ガイドを消す"),
];

const KEY_HELP_RIGHT: &[(&str, &str)] = &[
    ("F1〜F12", "PF1〜12"),
    ("Shift+F1〜12", "PF13〜24"),
    ("Ctrl+C", "CLEAR (画面消去)"),
    ("Ctrl+P", "PA1 (中断)"),
    ("Ctrl+R", "ロック解除"),
    ("Ctrl+Q", "終了"),
    ("クリック", "カーソル移動"),
    ("BS / Delete", "1 文字消す"),
];

fn pad_right(s: &str, target_w: usize) -> String {
    let w = disp_width(s);
    if w >= target_w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(target_w - w))
    }
}

fn key_cell(key: &str, desc: &str, key_w: usize, desc_w: usize) -> Vec<Span<'static>> {
    let key_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Gray);
    vec![
        Span::styled(pad_right(key, key_w), key_style),
        Span::styled(pad_right(desc, desc_w), desc_style),
    ]
}

fn key_help_lines(width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "キー操作",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    if width >= 56 {
        for (&(lk, ld), &(rk, rd)) in KEY_HELP_LEFT.iter().zip(KEY_HELP_RIGHT.iter()) {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(key_cell(lk, ld, 12, 16));
            spans.extend(key_cell(rk, rd, 13, 0));
            lines.push(Line::from(spans));
        }
    } else {
        for &(k, d) in KEY_HELP_LEFT.iter().chain(KEY_HELP_RIGHT.iter()) {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(key_cell(k, d, 13, 0));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn note_lines(notes: &[Note], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, n) in notes.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.extend(styled_lines(
            &n.title,
            width,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        let body: Vec<&str> = n.body.lines().filter(|l| !l.is_empty()).collect();
        lines.extend(styled_lines(
            &body.join("\n"),
            width,
            Style::default().fg(Color::White),
        ));
    }
    lines
}

fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// 3270 の色。拡張色が無ければ基本の 4 色 (保護/非保護 × 通常/強調) で塗る
fn cell_style(s: &Screen, pos: usize) -> Style {
    let Some(start) = s.field_start(pos) else {
        return Style::default().fg(Color::Green);
    };
    let fa = s.cells[start].attr.unwrap();
    let cell = &s.cells[pos];
    let ext_color = if cell.ext.color != 0 {
        cell.ext.color
    } else {
        fa.ext.color
    };
    let fg = match ext_color {
        0xF1 => Color::LightBlue,
        0xF2 => Color::LightRed,
        0xF3 => Color::LightMagenta,
        0xF4 => Color::LightGreen,
        0xF5 => Color::LightCyan,
        0xF6 => Color::LightYellow,
        0xF7 => Color::White,
        _ => match (fa.protected(), fa.intensified()) {
            (false, false) => Color::LightGreen,
            (false, true) => Color::LightRed,
            (true, false) => Color::LightBlue,
            (true, true) => Color::White,
        },
    };
    let mut style = Style::default().fg(fg);
    let hl = if cell.ext.highlight != 0 {
        cell.ext.highlight
    } else {
        fa.ext.highlight
    };
    match hl {
        0xF2 => style = style.add_modifier(Modifier::REVERSED),
        0xF4 => style = style.add_modifier(Modifier::UNDERLINED),
        0xF8 => style = style.add_modifier(Modifier::BOLD),
        _ => {}
    }
    // 短い入力欄は下線で示す (画面の残り全体が入力欄になる TSO の行モードでは引かない)
    if s.is_input_box(pos) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn test_app(screen: Screen) -> App {
        let sources = Sources {
            dirs: vec![],
            builtin: true,
        };
        let guide = Guide::load(&sources).unwrap();
        App::new(Session::for_test(screen), guide, sources, true)
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            let mut line = String::new();
            let mut x = 0;
            while x < area.width {
                let sym = buf[(x, y)].symbol();
                line.push_str(sym);
                x += disp_width(sym).max(1) as u16;
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn wraps_japanese_by_width() {
        let lines = wrap("あいうえお。かきくけこ", 10);
        assert_eq!(lines, vec!["あいうえお。", "かきくけこ"]);
        assert_eq!(wrap("LOGON IBMUSER", 6), vec!["LOGON", "IBMUSE", "R"]);
        assert_eq!(
            wrap("ここで LOGON と打つ", 10),
            vec!["ここで", "LOGON と打", "つ"]
        );
        // 箇条書きの折り返しは 2 文字字下げする
        assert_eq!(
            wrap("- あいうえおかきく", 10),
            vec!["- あいうえ", "  おかきく"]
        );
    }

    #[test]
    fn keys_are_parsed() {
        let steps = parse_keys("{home}{eraseeof}HEX ON{enter}");
        assert!(matches!(steps[0], Step::Home));
        assert!(matches!(steps[1], Step::EraseEof));
        assert!(matches!(&steps[2], Step::Text(t) if t == "HEX ON"));
        assert!(matches!(steps[3], Step::Aid(AidKey::Enter)));
        let steps = parse_keys("{pf11}");
        assert!(matches!(steps[0], Step::Aid(AidKey::Pf(11))));
    }

    #[test]
    fn scenario_vtam_logon_guide_and_term_hover() {
        let mut screen = Screen::default();
        screen.locked = false;
        screen.put_field(23, 1, 0x20);
        screen.put_str(23, 2, "TSO Logon ===>");
        screen.put_field(23, 17, 0x00);
        screen.put_field(23, 40, 0x20);
        screen.home();

        let mut app = test_app(screen);
        let mut terminal = Terminal::new(TestBackend::new(154, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();

        let rendered = buffer_text(&terminal);
        // 右ペインのタイトルが「ガイド」であり、説明文と 2 列のキー操作が出ていること
        assert!(rendered.contains(" ガイド "));
        assert!(!rendered.contains(" 案内 "));
        assert!(rendered.contains("TSO のログオン画面"));
        assert!(rendered.contains("- MVS の通信制御 VTAM が出す最初の画面 (USS 画面)"));
        assert!(rendered.contains("Tab / S-Tab"));
        assert!(rendered.contains("Shift+F1〜12"));

        // ガイド欄の中の「VTAM」にマウスを乗せると、VTAM の正式名称と説明がツールチップで出る
        let (lx, ly, line) = app
            .side_lines
            .iter()
            .find(|(_, _, t)| t.contains("VTAM"))
            .unwrap()
            .clone();
        let vtam_col = disp_width(&line[..line.find("VTAM").unwrap()]) as u16;
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: lx + vtam_col + 1,
            row: ly,
            modifiers: KeyModifiers::NONE,
        }));
        terminal.draw(|f| app.draw(f)).unwrap();

        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("Virtual Telecommunications Access Method"));
        assert!(rendered.contains("仮想通信アクセス方式"));
        // ガイド欄の「VTAM」自身が反転ハイライトされていること
        let cell = &terminal.backend().buffer()[(lx + vtam_col, ly)];
        assert!(cell.modifier.contains(Modifier::REVERSED));

        // ガイド欄の中の「MVS」にマウスを乗せると、MVS の説明が出る
        let mvs_col = disp_width(&line[..line.find("MVS").unwrap()]) as u16;
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: lx + mvs_col,
            row: ly,
            modifiers: KeyModifiers::NONE,
        }));
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("Multiple Virtual Storage"));

        // 3270 端末画面の「Logon」(23 行 6 桁) にマウスを乗せると、用語と入力欄の説明が出る
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1 + 6,
            row: 1 + 22,
            modifiers: KeyModifiers::NONE,
        }));
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("LOGON (ログオン)"));
        assert!(rendered.contains("23行7桁"));
    }

    #[test]
    fn scenario_pointer_shape_and_button_click() {
        let mut screen = Screen::default();
        screen.locked = false;
        screen.put_field(23, 1, 0x20);
        screen.put_str(23, 2, "TSO Logon ===>");
        screen.put_field(23, 17, 0x00);
        screen.put_field(23, 40, 0x20);
        screen.home();

        let mut app = test_app(screen);
        let mut terminal = Terminal::new(TestBackend::new(154, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();

        // 初期状態および保護領域の上では通常の矢印 (Default)
        assert_eq!(app.desired_pointer(), Pointer::Default);
        app.hover = Some((2, 2));
        assert_eq!(app.desired_pointer(), Pointer::Default);

        // 3270 の下線付き入力欄 (23 行 18 桁) の上ではテキスト入力 (Text)
        app.hover = Some((1 + 17, 1 + 22));
        assert_eq!(app.desired_pointer(), Pointer::Text);

        // 「▶ IBMUSER でログオン」ボタンの上では指差し (Hand) になり、クリックで入力される
        let action_rect = app
            .buttons
            .iter()
            .find_map(|(r, b)| matches!(b, Button::Action(0)).then_some(*r))
            .unwrap();
        app.hover = Some((action_rect.x + 2, action_rect.y));
        assert_eq!(app.desired_pointer(), Pointer::Hand);

        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: action_rect.x + 2,
            row: action_rect.y,
            modifiers: KeyModifiers::NONE,
        }));
        app.run_pending();
        assert!(app.session.screen.row_text(22).contains("LOGON IBMUSER"));
        assert!(app.session.screen.locked);
    }

    #[test]
    fn scenario_plain_mode_hides_guide() {
        let mut screen = Screen::default();
        screen.locked = false;
        screen.put_field(23, 1, 0x20);
        screen.put_str(23, 2, "TSO Logon ===>");
        screen.put_field(23, 17, 0x00);
        screen.put_field(23, 40, 0x20);
        screen.home();

        let mut app = test_app(screen);
        let mut terminal = Terminal::new(TestBackend::new(154, 30)).unwrap();

        // Ctrl+G でガイドを消すと、枠もガイド欄もボタンも無く、3270 の画面が左上から始まり、その下に状態行が出る
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(!rendered.contains(" ガイド "));
        assert!(!rendered.contains("3270 端末"));
        assert!(!rendered.contains("[Enter]"));
        let rows: Vec<&str> = rendered.lines().collect();
        assert!(rows[22].starts_with(" TSO Logon ===>"));
        assert!(rows[24].contains("入力できる"));
        assert!(rows[24].contains("Ctrl+G ガイド"));
        assert!(app.buttons.is_empty());

        // 用語の上にマウスを乗せても、ツールチップも反転も出ない
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 6,
            row: 22,
            modifiers: KeyModifiers::NONE,
        }));
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(!rendered.contains("LOGON (ログオン)"));
        let cell = &terminal.backend().buffer()[(6, 22)];
        assert!(!cell.modifier.contains(Modifier::REVERSED));

        // クリックでは、枠の無い位置のままカーソルが移る (23 行 20 桁)
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 22,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.session.screen.cursor, 22 * COLS + 19);

        // もう一度 Ctrl+G でガイドが戻る
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(buffer_text(&terminal).contains(" ガイド "));
    }

    #[test]
    fn scenario_quit_confirmation_and_long_message_wrapping() {
        let mut screen = Screen::default();
        screen.locked = false;
        screen.put_str(1, 1, "READY");

        let mut app = test_app(screen);
        let mut terminal = Terminal::new(TestBackend::new(154, 30)).unwrap();

        // ログオン中に Ctrl+Q を 1 回押すと、終了せず警告が 1 行に収まって全体表示される
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.quit);
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("LOGOFF しないとセッションが残る。もう一度押すと終了"));

        // 別のキーを押すと確認状態が解除される
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        )));
        assert!(!app.quit);

        // 続けてもう一度 Ctrl+Q を押すと終了する
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.quit);

        // 82 桁を超える長いエラー通知でも、ステータス行が自動で折り返されて末尾まで表示される
        app.notify("辞書の読み込みに失敗: /very/long/path/to/site/guide/screens.toml: TOML parse error at line 42, column 10 (末尾確認)");
        terminal.draw(|f| app.draw(f)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("(末尾確認)"));
    }
}
