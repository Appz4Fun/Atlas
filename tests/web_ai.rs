//! Ask AI over a real socket, against a stand-in for Gemini: the key is saved, never shown, and a question becomes
//! the search page's own fields.
//!
//! One test: it points ATLAS_HOME and the Gemini url at temp ones before any thread is spawned.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use atlas::config::Config;
use atlas::parser::Release;
use atlas::{api, db, store};
use common::stage::release_of;
use serde_json::json;

const KEY: &str = "web-ai-key";
const GEMINI_KEY: &str = "AIzaGood-0123456789wxyz";

struct Reply {
    head: String,
    body: String,
}

impl Reply {
    fn status(&self) -> u16 {
        self.head.split(' ').nth(1).unwrap().parse().unwrap()
    }

    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with(&prefix))
            .map(|l| l[prefix.len()..].trim().to_string())
    }
}

fn send(port: u16, request: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    Reply { head: head.into(), body: body.into() }
}

fn fetch(port: u16, path: &str, cookie: &str) -> Reply {
    send(port, &format!("GET {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"))
}

fn post(port: u16, path: &str, body: &str, cookie: &str, htmx: bool) -> Reply {
    let hx = if htmx { "HX-Request: true\r\n" } else { "" };
    send(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: atlas.test\r\nCookie: {cookie}\r\n{hx}Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

/// Answers like Gemini: a model lookup, and a generateContent that reads every question as 2160p "findword".
fn fake_gemini(seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let length = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().to_string()))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request = format!("{head}\r\n{}", String::from_utf8_lossy(&body));
            seen.lock().unwrap().push(request);
            let good = head.lines().any(|l| l.eq_ignore_ascii_case(&format!("x-goog-api-key: {GEMINI_KEY}")));
            let (status, reply) = if !good {
                (400, json!({"error": {"code": 400, "message": "API key not valid. Please pass a valid API key."}}))
            } else if head.starts_with("POST ") {
                let plan = json!({
                    "note": "4K copies of Findword",
                    "q": "Findword",
                    "cat": "all",
                    "filters": ["2160p", "nonsense"],
                    "team": "",
                    "sort": "relevance",
                    "dir": "desc",
                });
                (200, json!({"candidates": [{"content": {"parts": [{"text": plan.to_string()}]}}]}))
            } else {
                (200, json!({"name": "models/gemini-flash-latest"}))
            };
            let reply = reply.to_string();
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    port
}

#[test]
fn ask_ai_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let gemini = fake_gemini(seen.clone());
    // SAFETY: only test in this binary; the fake's thread reads no environment
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_GEMINI_URL", format!("http://127.0.0.1:{gemini}"));
        std::env::remove_var("GEMINI_API_KEY");
    }
    db::create_db().unwrap();
    let main = home.path().join("atlas.db");
    for (name, group) in [
        ("Tv.Findword.S01E01-GRP", "alt.binaries.g1"),
        ("Film.Findword.2024.2160p.WEB-DL.DV.H265-GRP", "alt.binaries.g3"),
    ] {
        store::save(&main, &[Release { group: group.into(), ..release_of(name, "x.bin", 2, 2) }]).unwrap();
    }

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut config =
        Config { api_port: Some(port), api_host: "127.0.0.1".into(), api_key: Some(KEY.into()), ..Config::default() };
    assert!(api::start(&mut config));

    // nothing without signing in
    assert_eq!(fetch(port, "/ui/settings", "").status(), 303);
    assert_eq!(post(port, "/ui/settings", &format!("key={GEMINI_KEY}"), "", false).status(), 303);
    assert_eq!(post(port, "/ui/ask", "ask=findword", "", true).status(), 401);
    assert!(!home.path().join("gemini.json").exists());

    let login = post(port, "/login", &format!("key={KEY}"), "", false);
    let cookie = login.header("Set-Cookie").unwrap().split(';').next().unwrap().to_string();

    // no key: the search page says where to add one
    let page = fetch(port, "/", &cookie);
    assert!(page.body.contains("href=\"/ui/settings\"") && !page.body.contains("id=\"ask\""), "{}", page.body);
    let settings = fetch(port, "/ui/settings", &cookie);
    assert_eq!(settings.status(), 200);
    assert!(settings.body.contains("No key yet"), "{}", settings.body);

    // a key Gemini doesn't take is saved, and said so
    let bad = post(port, "/ui/settings", "key=AIzaWrong-0123456789&model=&do=save", &cookie, false);
    assert!(bad.body.contains("Gemini did not take it") && bad.body.contains("API key not valid"), "{}", bad.body);
    assert!(post(port, "/ui/settings", "key=two+words&do=save", &cookie, false).body.contains("not a Gemini key"));
    assert!(post(port, "/ui/settings", "model=..%2Fx&do=save", &cookie, false).body.contains("not a model name"));

    let good = post(port, "/ui/settings", &format!("key={GEMINI_KEY}&model=&do=save"), &cookie, false);
    assert!(good.body.contains("Gemini took the key"), "{}", good.body);
    assert!(!good.body.contains(GEMINI_KEY), "the key is never shown whole");
    assert!(good.body.contains("••••••••wxyz"), "{}", good.body);
    let saved = std::fs::read_to_string(home.path().join("gemini.json")).unwrap();
    assert!(saved.contains(GEMINI_KEY));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join("gemini.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let lookup = seen.lock().unwrap().last().cloned().unwrap();
    assert!(lookup.starts_with("GET /v1beta/models/gemini-flash-latest "), "{lookup}");
    // saving again without a key keeps it
    let again = post(port, "/ui/settings", "key=&model=gemini-3.8-flash&do=save", &cookie, false);
    assert!(again.body.contains("knows gemini-3.8-flash") && !again.body.contains(GEMINI_KEY), "{}", again.body);

    // the search page has the box now, and never the key
    let page = fetch(port, "/", &cookie);
    assert!(page.body.contains("id=\"ask\"") && !page.body.contains(GEMINI_KEY), "{}", page.body);
    // a newer plain search supersedes a question still waiting on Gemini, and the other way round
    assert!(
        page.body.contains("hx-post=\"/ui/ask\" hx-target=\"#results\" hx-sync=\"#search:replace\""),
        "{}",
        page.body
    );

    // a question becomes a search
    let asked = post(port, "/ui/ask", "ask=4k+findword+please", &cookie, true);
    assert_eq!(asked.status(), 200);
    assert!(asked.body.contains("Film.Findword") && !asked.body.contains("Tv.Findword"), "{}", asked.body);
    assert!(asked.body.contains("4K copies of Findword") && asked.body.contains("id=\"ask-note\""), "{}", asked.body);
    assert!(asked.body.contains("data-q=\"Findword\""), "the search box follows: {}", asked.body);
    assert!(!asked.body.contains("id=\"q\""), "the box keeps its listeners, soo it isn't swapped: {}", asked.body);
    assert_eq!(asked.header("HX-Push-Url").as_deref(), Some("/?q=Findword&filters=2160p"));
    assert!(!asked.body.contains(GEMINI_KEY));
    let request = seen.lock().unwrap().last().cloned().unwrap();
    assert!(request.starts_with("POST /v1beta/models/gemini-3.8-flash:generateContent "), "{request}");
    assert!(request.contains("4k findword please") && request.contains("responseSchema"), "{request}");

    // without htmx, the search's own page
    let plain = post(port, "/ui/ask", "ask=4k+findword", &cookie, false);
    assert_eq!(plain.status(), 303);
    assert_eq!(plain.header("Location").as_deref(), Some("/?q=Findword&filters=2160p"));

    // too long to be a question: turned away whole, never cut short
    let long = format!("ask={}", "%E4%B8%AD".repeat(1000));
    assert_eq!(post(port, "/ui/ask", &long, &cookie, true).status(), 413);
    // 500 characters of CJK fits
    let cjk = format!("ask={}", "%E4%B8%AD".repeat(500));
    assert_eq!(post(port, "/ui/ask", &cjk, &cookie, true).status(), 200);

    // nothing asked: said where the note goes, the results stay
    let empty = post(port, "/ui/ask", "ask=+", &cookie, true);
    assert_eq!(empty.header("HX-Retarget").as_deref(), Some("#ask-note"));

    // a plain search empties the note
    let typed = fetch(port, "/ui/results?q=Tv", &cookie);
    assert!(
        typed.body.contains("<div id=\"ask-note\" aria-live=\"polite\" hx-swap-oob=\"true\"></div>"),
        "{}",
        typed.body
    );

    // removing the key turns it off
    let removed = post(port, "/ui/settings", "do=forget", &cookie, false);
    assert!(removed.body.contains("the key is removed") && removed.body.contains("No key yet"), "{}", removed.body);
    let failed = post(port, "/ui/ask", "ask=findword", &cookie, true);
    assert_eq!(failed.header("HX-Retarget").as_deref(), Some("#ask-note"));
    assert!(failed.body.contains("no Gemini key"), "{}", failed.body);
}
