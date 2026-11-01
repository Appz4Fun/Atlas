//! With no `ATLAS_HOME`, the real binary keeps its data in `~/.atlas`.

use std::process::Command;

#[test]
fn the_binary_defaults_to_the_users_home() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .arg("--reflag-obfuscated")
        .env_remove("ATLAS_HOME")
        .env("HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .output()
        .unwrap();
    let dir = home.path().join(".atlas");
    assert!(dir.is_dir(), "{out:?}");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains(dir.to_str().unwrap()), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
