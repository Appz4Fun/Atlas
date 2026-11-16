//! Round trips one helper save part makes to PostgreSQL, counted by a TCP relay between the session and the server:
//! each time the client speaks again after the server answered is one round trip it waited on.
use crate::parser::{Article, Release};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Relay {
    port: u16,
    turns: Arc<AtomicUsize>,
    server_spoke: Arc<AtomicBool>,
}

impl Relay {
    fn start(upstream: String) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let turns = Arc::new(AtomicUsize::new(0));
        let server_spoke = Arc::new(AtomicBool::new(true));
        let (t, s) = (turns.clone(), server_spoke.clone());
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { return };
                let server = TcpStream::connect(&upstream).unwrap();
                let _ = (client.set_nodelay(true), server.set_nodelay(true));
                let (t, s1, s2) = (t.clone(), s.clone(), s.clone());
                let (mut c_read, mut s_write) = (client.try_clone().unwrap(), server.try_clone().unwrap());
                let (mut s_read, mut c_write) = (server, client);
                std::thread::spawn(move || {
                    let mut buf = vec![0; 1 << 16];
                    while let Ok(n @ 1..) = c_read.read(&mut buf) {
                        if s1.swap(false, Ordering::SeqCst) {
                            t.fetch_add(1, Ordering::SeqCst);
                        }
                        if s_write.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = s_write.shutdown(std::net::Shutdown::Both);
                });
                std::thread::spawn(move || {
                    let mut buf = vec![0; 1 << 16];
                    while let Ok(n @ 1..) = s_read.read(&mut buf) {
                        s2.store(true, Ordering::SeqCst);
                        if c_write.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = c_write.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        Relay { port, turns, server_spoke }
    }

    fn reset(&self) {
        self.server_spoke.store(true, Ordering::SeqCst);
        self.turns.store(0, Ordering::SeqCst);
    }
}

fn release(name: &str, message_id: &str) -> Release {
    Release {
        name: name.into(),
        group: "alt.binaries.roundtrips".into(),
        articles: vec![Article {
            filename: Some(format!("{name}.rar")),
            message_id: message_id.into(),
            subject: format!("{name}.rar yEnc (1/2)"),
            part: Some(1),
            total_parts: Some(2),
            bytes: 17,
            ..Article::default()
        }],
        ..Release::default()
    }
}

/// The relay's address in place of the server's in a key=value connection string.
fn through(url: &str, port: u16) -> (String, String) {
    let value = |key: &str| url.split_whitespace().find_map(|kv| kv.strip_prefix(key)).map(str::to_owned);
    let upstream = format!("{}:{}", value("host=").unwrap_or("localhost".into()), value("port=").unwrap_or("5432".into()));
    let rest: Vec<&str> = url.split_whitespace().filter(|kv| !kv.starts_with("host=") && !kv.starts_with("port=")).collect();
    (upstream, format!("{} host=127.0.0.1 port={port}", rest.join(" ")))
}

#[test]
#[ignore = "requires isolated PostgreSQL"]
fn a_helper_save_part_waits_on_few_round_trips() {
    let schema = super::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime = super::stage_queue_tests::runtime(&schema);
    let epoch = *runtime.owner.epoch().expect("owner epoch").id();
    // the production writer keeps its articles in a local store
    let dir = tempfile::tempdir().unwrap();
    let registered = !super::testing::articles_are_local(&mut *schema.pool.get().unwrap());
    if registered {
        let store = crate::article_local::Store::open_writer(dir.path(), &schema.name).unwrap();
        crate::article_local::register(&schema.name, store).unwrap();
    }
    let (upstream, _) = through(&schema.url, 0);
    let relay = Relay::start(upstream);
    let (_, url) = through(&schema.url, relay.port);
    let storage = super::StorageConfig {
        backend: super::Backend::Postgres,
        url,
        schema: schema.name.clone(),
        max_connections: 1,
        allow_plaintext: true,
    };
    let helpers = super::ingest::Helpers::new(Arc::new(super::Pool::new(&storage).unwrap()), &epoch);
    // the session is open and its stage tables made: what is left is what every save pays
    helpers.save(&[release("Warm.Up", "<warm@example.test>")], 1).unwrap();
    let mut counts = Vec::new();
    for (n, name) in ["Fresh.Release", "Warm.Up"].into_iter().enumerate() {
        relay.reset();
        helpers.save(&[release(name, &format!("<measured{n}@example.test>"))], 2).unwrap();
        counts.push(relay.turns.load(Ordering::SeqCst));
    }
    if registered {
        crate::article_local::unregister(&schema.name);
    }
    eprintln!("round trips per helper save part (new release, existing release): {counts:?}");
    let saved: i64 = runtime.owner.with(|c| Ok(c.query_one("select count(*) from releases", &[])?.get(0))).unwrap();
    assert_eq!(saved, 2);
    // 48 before the statements were combined and sent typed (one round trip each instead of prepare + execute)
    assert!(counts.iter().all(|&n| n <= 26), "{counts:?}");
}
