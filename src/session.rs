// 接続 1 本分の状態。画面バッファとホストとの送受信をまとめる。

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::screen::{Aid, Screen};
use crate::telnet::{self, NetEvent, Sender3270};

pub struct Session {
    pub host: String,
    pub screen: Screen,
    sender: Sender3270,
    rx: Receiver<NetEvent>,
    /// 切断されたら理由
    pub closed: Option<String>,
}

impl Session {
    pub fn connect(host: &str) -> anyhow::Result<Session> {
        let (tx, rx) = channel();
        let sender = telnet::connect(host, tx)?;
        Ok(Session {
            host: host.to_string(),
            screen: Screen::default(),
            sender,
            rx,
            closed: None,
        })
    }

    #[cfg(test)]
    pub fn for_test(screen: Screen) -> Session {
        let (_tx, rx) = channel();
        Session {
            host: "127.0.0.1:3270".to_string(),
            screen,
            sender: Sender3270::dummy(),
            rx,
            closed: None,
        }
    }

    /// 届いているレコードをすべて処理する。画面が変わったら true
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            changed = true;
            self.handle(ev);
        }
        changed
    }

    fn handle(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Record(rec) => {
                trace("<<", &rec);
                if let Some(reply) = self.screen.process(&rec) {
                    trace(">>", &reply);
                    let _ = self.sender.send_record(&reply);
                }
            }
            NetEvent::Closed(why) => self.closed = Some(why),
        }
    }

    /// ホストから画面が届き、更新が timeout の間止まるまで待つ (スクリプト実行用)
    pub fn wait_settle(&mut self, quiet: Duration, timeout: Duration) {
        let start = Instant::now();
        let mut last = Instant::now();
        let gen0 = self.screen.generation;
        while start.elapsed() < timeout && self.closed.is_none() {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(ev) => {
                    self.handle(ev);
                    last = Instant::now();
                }
                Err(_) => {
                    let updated = self.screen.generation != gen0;
                    if updated && !self.screen.locked && last.elapsed() >= quiet {
                        return;
                    }
                }
            }
        }
    }

    /// 画面に text が現れ、キーボードが使えるようになるまで待つ (スクリプト実行用)
    pub fn wait_for(&mut self, text: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout && self.closed.is_none() {
            if !self.screen.locked && self.screen.text().contains(text) {
                return true;
            }
            if let Ok(ev) = self.rx.recv_timeout(Duration::from_millis(50)) {
                self.handle(ev);
            }
        }
        false
    }

    pub fn press(&mut self, aid: Aid) {
        let rec = self.screen.press(aid);
        trace(">>", &rec);
        if let Err(e) = self.sender.send_record(&rec) {
            self.closed = Some(e.to_string());
        }
    }
}

/// 環境変数 CI3270_TRACE が設定されていれば、送受信したレコードを 16 進で標準エラーに出す
fn trace(dir: &str, rec: &[u8]) {
    if std::env::var_os("CI3270_TRACE").is_some() {
        let hex: Vec<String> = rec.iter().map(|b| format!("{b:02X}")).collect();
        eprintln!("{dir} {}", hex.join(" "));
    }
}
