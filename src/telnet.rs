// TN3270 (RFC 1576) の telnet 層。
// 端末型の交渉 (TERMINAL-TYPE)・BINARY・EOR を済ませ、IAC EOR で区切られた 3270 のレコードを取り出す。
// TN3270E は使わない (Hercules の 3270 端末は素の TN3270 で足りる)。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const EOR: u8 = 239;

const OPT_BINARY: u8 = 0;
const OPT_TTYPE: u8 = 24;
const OPT_EOR: u8 = 25;

/// 端末型。3278 モデル 2 (24 行 80 桁) の拡張属性付き (-E)。
/// -E を名乗らないと、Hercules は WSF (端末への問い合わせ) を受け付けずエラーにし、TSO のログオンがエラー回復 (IKT00405I) になる
pub const TERMINAL_TYPE: &str = "IBM-3278-2-E";

pub enum NetEvent {
    /// 3270 のレコード 1 件 (IAC の重ねは解いてある)
    Record(Vec<u8>),
    /// 接続が切れた
    Closed(String),
}

#[derive(Clone)]
pub struct Sender3270 {
    stream: Arc<Mutex<TcpStream>>,
}

impl Sender3270 {
    /// 3270 のレコードを送る。0xFF は IAC IAC に重ね、末尾に IAC EOR を付ける
    pub fn send_record(&self, data: &[u8]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(data.len() + 2);
        for &b in data {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            }
        }
        out.extend_from_slice(&[IAC, EOR]);
        self.stream.lock().unwrap().write_all(&out)
    }
}

/// 接続して受信スレッドを起こす。受け取ったレコードは tx に流す
pub fn connect(addr: &str, tx: Sender<NetEvent>) -> anyhow::Result<Sender3270> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    let reader = stream.try_clone()?;
    let sender = Sender3270 {
        stream: Arc::new(Mutex::new(stream)),
    };
    let writer = sender.clone();
    thread::spawn(move || receive_loop(reader, writer, tx));
    Ok(sender)
}

fn receive_loop(mut reader: TcpStream, writer: Sender3270, tx: Sender<NetEvent>) {
    let mut parser = Parser::default();
    let mut buf = [0u8; 4096];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(NetEvent::Closed("ホストが接続を閉じた".into()));
                return;
            }
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(NetEvent::Closed(e.to_string()));
                return;
            }
        };
        for &b in &buf[..n] {
            match parser.feed(b) {
                Some(Output::Reply(bytes)) => {
                    if writer.stream.lock().unwrap().write_all(&bytes).is_err() {
                        return;
                    }
                }
                Some(Output::Record(rec)) => {
                    if tx.send(NetEvent::Record(rec)).is_err() {
                        return;
                    }
                }
                None => {}
            }
        }
    }
}

enum Output {
    Reply(Vec<u8>),
    Record(Vec<u8>),
}

#[derive(Default)]
enum State {
    #[default]
    Data,
    Iac,
    Option(u8),
    Sub,
    SubIac,
}

#[derive(Default)]
struct Parser {
    state: State,
    record: Vec<u8>,
    sub: Vec<u8>,
}

impl Parser {
    fn feed(&mut self, b: u8) -> Option<Output> {
        match self.state {
            State::Data => {
                if b == IAC {
                    self.state = State::Iac;
                } else {
                    self.record.push(b);
                }
                None
            }
            State::Iac => {
                self.state = State::Data;
                match b {
                    IAC => {
                        self.record.push(IAC);
                        None
                    }
                    EOR => Some(Output::Record(std::mem::take(&mut self.record))),
                    DO | DONT | WILL | WONT => {
                        self.state = State::Option(b);
                        None
                    }
                    SB => {
                        self.sub.clear();
                        self.state = State::Sub;
                        None
                    }
                    _ => None,
                }
            }
            State::Option(verb) => {
                self.state = State::Data;
                let supported = matches!(b, OPT_BINARY | OPT_TTYPE | OPT_EOR);
                let reply = match verb {
                    DO if supported => WILL,
                    DO => WONT,
                    WILL if b == OPT_BINARY || b == OPT_EOR => DO,
                    WILL => DONT,
                    // DONT / WONT は受け入れるだけ
                    _ => return None,
                };
                Some(Output::Reply(vec![IAC, reply, b]))
            }
            State::Sub => {
                if b == IAC {
                    self.state = State::SubIac;
                } else {
                    self.sub.push(b);
                }
                None
            }
            State::SubIac => {
                if b == SE {
                    self.state = State::Data;
                    // IAC SB TERMINAL-TYPE SEND IAC SE に端末型を答える
                    if self.sub.first() == Some(&OPT_TTYPE) && self.sub.get(1) == Some(&1) {
                        let mut reply = vec![IAC, SB, OPT_TTYPE, 0];
                        reply.extend_from_slice(TERMINAL_TYPE.as_bytes());
                        reply.extend_from_slice(&[IAC, SE]);
                        return Some(Output::Reply(reply));
                    }
                } else {
                    self.sub.push(b);
                    self.state = State::Sub;
                }
                None
            }
        }
    }
}
