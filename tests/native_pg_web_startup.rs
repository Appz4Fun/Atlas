//! Normal product binary, real private PostgreSQL, no handler-only server.
//! This tests pending startup/auth; populated UI proof belongs to the canary.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
struct Schema {
    client: postgres::Client,
    name: String,
}
impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema if exists {} cascade", self.name));
    }
}
struct Wire {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Wire {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}
fn exchange(port: u16, method: &str, path: &str, cookie: Option<&str>, form: &str) -> Wire {
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    client.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(client, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n").unwrap();
    if let Some(cookie) = cookie {
        write!(client, "Cookie: {cookie}\r\n").unwrap();
    }
    if method == "POST" {
        write!(client, "Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n", form.len())
            .unwrap();
    }
    write!(client, "\r\n{form}").unwrap();
    client.flush().unwrap();
    let mut bytes = Vec::new();
    client.take(524289).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 524288, "owned startup response exceeds fixture admission");
    let split = bytes.windows(4).position(|part| part == b"\r\n\r\n").expect("literal normal-BIN HTTP response");
    let mut lines = std::str::from_utf8(&bytes[..split]).unwrap().split("\r\n");
    let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.into(), value.trim().into())
        })
        .collect();
    let wire = Wire { status, headers, body: bytes[split + 4..].to_vec() };
    assert!(wire.header("Date").is_some() && wire.header("Server").is_some());
    if method != "HEAD" {
        assert_eq!(wire.header("Content-Length").unwrap().parse::<usize>().unwrap(), wire.body.len());
    }
    wire
}
#[test]
#[ignore = "requires explicit isolated PostgreSQL and the qualified normal Atlas BIN; owned loopback only"]
fn normal_binary_serves_auth_and_static_while_unbootstrapped_pg_reads_remain_503() {
    let url = std::env::var("ATLAS_PG_TEST_URL").expect("explicit isolated PostgreSQL required");
    assert!(!url.is_empty());
    let name = format!("t_web_start_{}", std::process::id());
    let mut client = postgres::Client::connect(&url, postgres::NoTls).expect("owned PostgreSQL fixture connection");
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let identity: bool = schema
        .client
        .query_one(&format!("select exists(select 1 from {}.native_catalog_identity)", schema.name), &[])
        .unwrap()
        .get(0);
    assert!(!identity, "no forged catalog/bootstrap readiness");
    let home = tempfile::tempdir().unwrap();
    let port_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = port_reservation.local_addr().unwrap().port();
    drop(port_reservation);
    let provider = TcpListener::bind("127.0.0.1:0").unwrap();
    provider.set_nonblocking(true).unwrap();
    let config = serde_json::json!({"storage":{"backend":"postgres","url":url,"schema":schema.name,"max_connections":4,"allow_plaintext":true},
        "api_host":"127.0.0.1","api_port":port,"api_key":"native-startup-fixture-key","metrics":{"enabled":false},
        "header_store":{"enabled":true,"dir":home.path().join("headers")},"postprocess":{"enabled":false},"predb":{"enabled":false},"spots":{"enabled":false},"inferred_parts":false,
        "usenet_servers":[{"host":"127.0.0.1","port":provider.local_addr().unwrap().port(),"username":"fixture","password":"fixture","ssl":false}]});
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(home.path().join("config.json")).unwrap();
    file.write_all(&serde_json::to_vec(&config).unwrap()).unwrap();
    drop(file);
    let mut command = Command::new(env!("CARGO_BIN_EXE_atlas"));
    command
        .env_clear()
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .env("ATLAS_SAB_DIR", home.path().join("no-sab"));
    // No --bg-indexer, test server flag, private dispatch or readiness override.
    let mut child =
        OwnedChild(command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(child.0.try_wait().unwrap().is_none(), "normal BIN exited before native listener startup");
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "normal BIN never bound its configured loopback listener");
        std::thread::sleep(Duration::from_millis(20));
    }
    let login = exchange(port, "GET", "/", None, "");
    assert_eq!(login.status, 200);
    let page = std::str::from_utf8(&login.body).unwrap();
    assert!(page.contains("<footer class=\"site-footer\"") && page.contains("Patreon") && page.contains("xbmc4lyfe"));
    let css = exchange(port, "GET", "/static/atlas.css", None, "");
    assert_eq!(css.status, 200);
    assert!(css.header("Content-Type").unwrap().starts_with("text/css"));
    assert_eq!(exchange(port, "GET", "/api?t=search&q=preview", None, "").status, 401);
    assert_eq!(exchange(port, "POST", "/login", None, "key=wrong").status, 401);
    let signed = exchange(port, "POST", "/login", None, "key=native-startup-fixture-key");
    assert_eq!(signed.status, 303);
    let cookie = signed.header("Set-Cookie").unwrap().split(';').next().unwrap();
    assert!(!cookie.contains("fixture-key"));
    for path in ["/", "/ui/results", "/api?t=caps"] {
        let pending = exchange(port, "GET", path, Some(cookie), "");
        assert_eq!(pending.status, 503, "a missing real catalog identity must remain unavailable");
        assert!(!pending.body.is_empty());
    }
    let head = exchange(port, "HEAD", "/", None, "");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert!(
        matches!(provider.accept(),Err(error)if error.kind()==std::io::ErrorKind::WouldBlock),
        "Web startup/auth/pending routes must not contact a provider"
    );
    assert!(
        !home.path().join("atlas.db").exists()
            && !home.path().join("atlas.raw.db").exists()
            && !home.path().join("atlas.details.db").exists()
    );
    drop(child);
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "owned normal listener must close with its child");
    let identity: bool = schema
        .client
        .query_one(&format!("select exists(select 1 from {}.native_catalog_identity)", schema.name), &[])
        .unwrap()
        .get(0);
    assert!(!identity, "Web reads never mint the canonical catalog identity");
}
