//! File-only settings mutations use the real signed-in native route, even offline.
#![cfg(unix)]
use super::*;
use std::{
    fs::OpenOptions,
    io::Write,
    net::TcpListener,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const KEY: &str = "server-settings-fixture-key";
const CHILD: &str = "ATLAS_SERVER_SETTINGS_CHILD";
const SELECTOR: &str = "web::native_server_settings_tests::native_server_connections_save_with_auth_and_csrf_without_catalog_or_provider_io";

#[test]
fn native_server_connections_save_with_auth_and_csrf_without_catalog_or_provider_io() {
    if std::env::var_os(CHILD).is_some() {
        run();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let log = OpenOptions::new().create_new(true).write(true).mode(0o600).open(home.path().join("child.log")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", SELECTOR, "--nocapture"])
        .env_clear()
        .env(CHILD, "1")
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= until {
            child.kill().unwrap();
            child.wait().unwrap();
            let keep = home.keep();
            panic!("settings child deadline; private log {}", keep.join("child.log").display());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        let keep = home.keep();
        panic!("settings child failed; private log {}", keep.join("child.log").display());
    }
    assert_eq!(std::fs::read(home.path().join("finished")).unwrap(), b"done");
}

fn run() {
    let home = crate::paths::app_dir();
    let pg = TcpListener::bind("127.0.0.1:0").unwrap();
    pg.set_nonblocking(true).unwrap();
    let nntp = TcpListener::bind("127.0.0.1:0").unwrap();
    nntp.set_nonblocking(true).unwrap();
    let port = nntp.local_addr().unwrap().port();
    let value = serde_json::json!({"storage":{"backend":"postgres","url":format!("host=127.0.0.1 port={} dbname=fixture user=fixture",pg.local_addr().unwrap().port()),"schema":"fixture","allow_plaintext":true},
        "usenet_servers":(0..3).map(|n|serde_json::json!({"host":"127.0.0.1","port":port,"username":format!("user{n}"),"password":format!("private-password-{n}"),"ssl":false,"connections":2,"priority":n*10})).collect::<Vec<_>>(),
        "media":{"tmdb":{"api_key":"private-provider-key"}},"header_store":{"enabled":true}});
    let path = home.join("config.toml");
    let mut file = OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path).unwrap();
    file.write_all(toml_edit::ser::to_string_pretty(&value).unwrap().as_bytes()).unwrap();
    drop(file);
    let storage = crate::config::checked_storage_config().unwrap();
    assert!(storage.is_postgres());
    let login =
        post_with_storage("/login", &format!("key={KEY}"), None, false, None, "http://fixture.invalid", KEY, &storage)
            .unwrap();
    assert_eq!(login.status, 303);
    let cookie =
        login.headers.iter().find(|(name, _)| name == "Set-Cookie").unwrap().1.split(';').next().unwrap().to_string();
    let token = crate::web_settings::csrf_token(KEY);
    let body = |n: usize, connections: &str, priority: usize| {
        format!(
            "do=edit&i={n}&was=127.0.0.1&host=127.0.0.1&username=user{n}&password=&port={port}&connections={connections}&priority={priority}&index=on"
        )
    };
    let before = std::fs::read(&path).unwrap();
    let unsigned = post_with_storage(
        "/ui/settings/servers",
        &body(0, "25", 1),
        None,
        false,
        Some(&token),
        "http://fixture.invalid",
        KEY,
        &storage,
    )
    .unwrap();
    assert_ne!(unsigned.status, 200);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for csrf in [None, Some("wrong-token")] {
        let response = post_with_storage(
            "/ui/settings/servers",
            &body(0, "25", 1),
            Some(&cookie),
            false,
            csrf,
            "http://fixture.invalid",
            KEY,
            &storage,
        )
        .unwrap();
        assert_eq!(response.status, 403);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let invalid = post_with_storage(
        "/ui/settings/servers",
        &body(0, "0", 1),
        Some(&cookie),
        false,
        Some(&token),
        "http://fixture.invalid",
        KEY,
        &storage,
    )
    .unwrap();
    assert_eq!(invalid.status, 200);
    assert!(invalid.body.contains("not saved"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for (n, connections) in [(0, "25"), (1, "15"), (2, "15")] {
        let response = post_with_storage(
            "/ui/settings/servers",
            &body(n, connections, n + 1),
            Some(&cookie),
            false,
            Some(&token),
            "http://fixture.invalid",
            KEY,
            &storage,
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body.contains("is saved."), "actual native server form must reach the validated file writer");
        assert!(response.body.contains("site-footer"));
        for secret in ["private-password-0", "private-password-1", "private-password-2", "private-provider-key", KEY] {
            assert!(!response.body.contains(secret));
        }
    }
    let saved = crate::config::load_config().unwrap();
    assert_eq!(
        saved.servers.iter().map(|s| (s.priority, s.connections())).collect::<Vec<_>>(),
        [(1, 25), (2, 15), (3, 15)]
    );
    for (n, server) in saved.servers.iter().enumerate() {
        assert_eq!(server.password, format!("private-password-{n}"));
    }
    assert_eq!(saved.media.tmdb.api_key, "private-provider-key");
    assert_eq!(crate::config::checked_storage_config().unwrap(), storage);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let after = std::fs::read(&path).unwrap();
    for other in ["/ui/picture", "/ui/ask", "/ui/settings/indexing", "/ui/settings/keys"] {
        let response =
            post_with_storage(other, "", Some(&cookie), true, Some(&token), "http://fixture.invalid", KEY, &storage)
                .unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(std::fs::read(&path).unwrap(), after);
    }
    std::fs::write(&path, b"[storage\nmalformed").unwrap();
    let refused = post_with_storage(
        "/ui/settings/servers",
        &body(0, "20", 1),
        Some(&cookie),
        false,
        Some(&token),
        "http://fixture.invalid",
        KEY,
        &storage,
    )
    .unwrap();
    assert_eq!(refused.status, 503);
    assert_eq!(std::fs::read(&path).unwrap(), b"[storage\nmalformed");
    assert!(matches!(pg.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    assert!(matches!(nntp.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    for name in ["atlas.db", "atlas.raw.db", "atlas.s0.db"] {
        assert!(!home.join(name).exists());
    }
    std::fs::write(home.join("finished"), b"done").unwrap();
}
