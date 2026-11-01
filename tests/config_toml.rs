//! A config.json converts itself when the real binary starts.

use std::process::Command;

fn run(home: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--reflag-obfuscated")
        .env("ATLAS_HOME", home)
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_config_json_becomes_config_toml_once() {
    let home = tempfile::tempdir().unwrap();
    let (json, toml, kept) =
        (home.path().join("config.json"), home.path().join("config.toml"), home.path().join("config.json.bak-toml"));
    let old = serde_json::json!({
        "api_port": 9091,
        "groups": ["alt.binaries.a"],
        "note": null,
        "usenet_servers": [{"host": "news.x", "password": "p", "port": 563, "ssl": true}],
        "media": {"tvmaze": {"enabled": false}},
    });
    std::fs::write(&json, old.to_string()).unwrap();
    let said = run(home.path());
    assert!(said.contains("config.json is now"), "{said}");
    assert!(!json.exists());
    assert!(kept.exists());
    let text = std::fs::read_to_string(&toml).unwrap();
    assert!(text.contains("api_port = 9091") && text.contains("[[usenet_servers]]"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&toml).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // a config.toml wins, and nothing converts again
    std::fs::write(&json, r#"{"api_port": 1}"#).unwrap();
    assert!(!run(home.path()).contains("config.json is now"));
    assert!(std::fs::read_to_string(&toml).unwrap().contains("api_port = 9091"));
}

#[test]
fn a_broken_config_json_stays_for_the_error_to_name() {
    let home = tempfile::tempdir().unwrap();
    let json = home.path().join("config.json");
    std::fs::write(&json, "{ not json").unwrap();
    run(home.path());
    assert!(json.exists());
    assert!(!home.path().join("config.toml").exists());
}
