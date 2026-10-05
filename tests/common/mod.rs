//! Mock usenet server shared by the integration tests.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use atlas::config::UsenetServer;
use atlas::indexer::Indexer;

pub const GROUP: &str = "alt.binaries.test";

#[derive(Clone)]
pub struct Post {
    pub number: u64,
    pub subject: String,
    pub message_id: String,
    pub bytes: u64,
    pub body: Vec<Vec<u8>>,
}

pub struct Server {
    pub posts: Mutex<Vec<Post>>,
    pub password: &'static str,
    /// false = BODY always 430s, like a provider missing the article
    pub bodies: bool,
    pub xover_delay: Duration,
    /// answer XFEATURE COMPRESS GZIP and send zlib compressed XOVER
    pub compress: bool,
    /// says yes to compression but sends junk
    pub compress_broken: bool,
    pub compressed_sent: AtomicUsize,
    pub open: AtomicUsize,
    pub peak: AtomicUsize,
    /// serve the same posts for any alt.binaries.* group, not just GROUP
    pub any_group: bool,
    /// XOVERs answered, and the most answered at the same moment
    pub xovers: AtomicUsize,
    pub xovers_in_flight: AtomicUsize,
    pub xover_peak: AtomicUsize,
    /// groups that saw at least one XOVER
    pub groups_seen: Mutex<std::collections::BTreeSet<String>>,
    /// refuse logins past this many open connections (0 = no limit)
    pub max_conns: usize,
    pub refused: AtomicUsize,
    /// answer GROUP with 501, like an article only (fill / bonus) server
    pub no_group: bool,
    /// the next `stalls_left` XOVERs sit on their reply for `stall`, like a
    /// provider that stops answering mid request; `stalled` counts the ones waiting
    pub stall: Duration,
    pub stalls_left: AtomicUsize,
    pub stalled: AtomicUsize,
    /// connections ever accepted
    pub accepted: AtomicUsize,
}

impl Server {
    pub fn new(posts: Vec<Post>) -> Arc<Server> {
        Arc::new(Server {
            posts: Mutex::new(posts),
            password: "secret",
            bodies: true,
            xover_delay: Duration::ZERO,
            compress: false,
            compress_broken: false,
            compressed_sent: AtomicUsize::new(0),
            open: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            any_group: false,
            xovers: AtomicUsize::new(0),
            xovers_in_flight: AtomicUsize::new(0),
            xover_peak: AtomicUsize::new(0),
            groups_seen: Mutex::new(Default::default()),
            max_conns: 0,
            refused: AtomicUsize::new(0),
            no_group: false,
            stall: Duration::ZERO,
            stalls_left: AtomicUsize::new(0),
            stalled: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
        })
    }
}

pub fn mock(host_port: u16, password: &str, connections: u32, priority: i64) -> UsenetServer {
    let mut s = UsenetServer::new("127.0.0.1", "bob", password, host_port);
    s.connections = Some(connections);
    s.priority = priority;
    s
}

pub fn yenc_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in data {
        let e = b.wrapping_add(42);
        if matches!(e, 0 | b'\n' | b'\r' | b'=') {
            out.push(b'=');
            out.push(e.wrapping_add(64));
        } else {
            out.push(e);
        }
    }
    out
}

pub fn yenc_body(name: &str, data: &[u8]) -> Vec<Vec<u8>> {
    vec![
        format!("=ybegin line=128 size={} name={name}", data.len()).into_bytes(),
        yenc_encode(data),
        format!("=yend size={}", data.len()).into_bytes(),
    ]
}

pub fn par2_file_desc(name: &str, size: u64) -> Vec<u8> {
    let mut name_bytes = name.as_bytes().to_vec();
    while !name_bytes.len().is_multiple_of(4) {
        name_bytes.push(0);
    }
    let len = (64 + 56 + name_bytes.len()) as u64;
    let mut p = b"PAR2\0PKT".to_vec();
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(&[0u8; 32]);
    p.extend_from_slice(b"PAR 2.0\0FileDesc");
    p.extend_from_slice(&[7u8; 48]);
    p.extend_from_slice(&size.to_le_bytes());
    p.extend_from_slice(&name_bytes);
    p
}

pub fn post(number: u64, subject: &str, bytes: u64, body: Vec<Vec<u8>>) -> Post {
    Post { number, subject: subject.into(), message_id: format!("<msg{number}@mock>"), bytes, body }
}

pub fn handle(stream: TcpStream, state: Arc<Server>) {
    state.accepted.fetch_add(1, Ordering::SeqCst);
    let now_open = state.open.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(now_open, Ordering::SeqCst);

    let over_limit = state.max_conns > 0 && now_open > state.max_conns;
    serve(stream, &state, over_limit);

    state.open.fetch_sub(1, Ordering::SeqCst);
}

pub fn serve(stream: TcpStream, state: &Server, over_limit: bool) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    let send = |out: &mut TcpStream, s: &[u8]| {
        let _ = out.write_all(s);
        let _ = out.write_all(b"\r\n");
    };

    send(&mut out, b"200 mock news server ready");

    let mut selected: Option<String> = None;
    let mut compressed = false;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let cmd = line.trim_end().to_string();
        line.clear();
        let (verb, arg) = cmd.split_once(' ').unwrap_or((&cmd, ""));
        let posts = state.posts.lock().unwrap().clone();

        match verb.to_uppercase().as_str() {
            "AUTHINFO" if arg.starts_with("USER") => send(&mut out, b"381 more"),
            "AUTHINFO" if over_limit => {
                state.refused.fetch_add(1, Ordering::SeqCst);
                send(&mut out, b"502 Too many connections.");
                return;
            }
            "AUTHINFO" => {
                let ok = arg == format!("PASS {}", state.password);
                send(&mut out, if ok { b"281 ok".as_slice() } else { b"481 nope" })
            }
            "GROUP" if state.no_group => send(&mut out, b"501 GROUP command error"),
            "GROUP" if arg == GROUP || (state.any_group && arg.starts_with("alt.binaries.")) => {
                selected = Some(arg.to_string());
                let first = posts.iter().map(|p| p.number).min().unwrap_or(0);
                let last = posts.iter().map(|p| p.number).max().unwrap_or(0);
                send(&mut out, format!("211 {} {first} {last} {arg}", posts.len()).as_bytes());
            }
            "GROUP" => send(&mut out, b"411 no such group"),
            "LIST" => {
                send(&mut out, b"215 list follows");
                let last = posts.iter().map(|p| p.number).max().unwrap_or(0);
                send(&mut out, format!("{GROUP} {last} 1 y").as_bytes());
                send(&mut out, b"alt.binaries.empty 5 5 y");
                send(&mut out, b"comp.lang.rust 900 1 y");
                send(&mut out, b".");
            }
            "XFEATURE" if state.compress && arg.eq_ignore_ascii_case("COMPRESS GZIP") => {
                compressed = true;
                send(&mut out, b"290 feature enabled");
            }
            "XOVER" if selected.is_none() => send(&mut out, b"412 no newsgroup selected"),
            "XOVER" => {
                let now = state.xovers_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                state.xover_peak.fetch_max(now, Ordering::SeqCst);
                state.xovers.fetch_add(1, Ordering::SeqCst);
                state.groups_seen.lock().unwrap().insert(selected.clone().unwrap_or_default());
                thread::sleep(state.xover_delay);
                if state.stalls_left.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
                    state.stalled.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(state.stall);
                    state.stalled.fetch_sub(1, Ordering::SeqCst);
                }
                state.xovers_in_flight.fetch_sub(1, Ordering::SeqCst);
                let (a, b) = arg.split_once('-').unwrap();
                let (a, b): (u64, u64) = (a.parse().unwrap(), b.parse().unwrap());
                let hits: Vec<&Post> = posts.iter().filter(|p| (a..=b).contains(&p.number)).collect();
                if hits.is_empty() {
                    send(&mut out, b"423 no articles in that range");
                    continue;
                }
                let mut listing = Vec::new();
                for p in hits {
                    let row = format!(
                        "{}\t{}\tposter <p@mock>\tFri, 02 Oct 2026 10:11:12 +0000\t{}\t\t{}\t10\r\n",
                        p.number, p.subject, p.message_id, p.bytes
                    );
                    listing.extend_from_slice(row.as_bytes());
                }
                listing.extend_from_slice(b".\r\n");

                if compressed && state.compress_broken {
                    send(&mut out, b"224 overview follows [COMPRESS=GZIP]");
                    let _ = out.write_all(&[0x78, 0x9c, 0xde, 0xad, 0xbe, 0xef, 0x00, 0x00]);
                    // and hang up, like a confused server would
                    return;
                } else if compressed {
                    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                    z.write_all(&listing).unwrap();
                    send(&mut out, b"224 overview follows [COMPRESS=GZIP]");
                    let _ = out.write_all(&z.finish().unwrap());
                    state.compressed_sent.fetch_add(1, Ordering::SeqCst);
                } else {
                    send(&mut out, b"224 overview follows");
                    let _ = out.write_all(&listing);
                }
            }
            "BODY" => match posts.iter().find(|p| p.message_id == arg).filter(|_| state.bodies) {
                Some(p) => {
                    send(&mut out, format!("222 0 {arg}").as_bytes());
                    for l in &p.body {
                        // dot stuffing
                        let mut l = l.clone();
                        if l.first() == Some(&b'.') {
                            l.insert(0, b'.');
                        }
                        send(&mut out, &l);
                    }
                    send(&mut out, b".");
                }
                None => send(&mut out, b"430 no such article"),
            },
            "QUIT" => {
                send(&mut out, b"205 bye");
                return;
            }
            _ => send(&mut out, b"500 what"),
        }
    }
}

pub fn spawn_server(state: Arc<Server>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = state.clone();
            thread::spawn(move || handle(stream, state));
        }
    });
    port
}

pub fn index_until_idle(indexer: &mut Indexer) {
    for _ in 0..20 {
        indexer.index_group(GROUP).unwrap();
        if indexer.is_idle(GROUP) {
            return;
        }
    }
    panic!("indexer never went idle");
}
