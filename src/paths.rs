use std::path::{Path, PathBuf};
use std::sync::Once;
use std::{env, fs};

/// Where atlas keeps config.toml, atlas.db, logs and status files.
/// `ATLAS_HOME` wins, otherwise see `default_home`.
pub fn app_dir() -> PathBuf {
    if let Some(home) = env::var_os("ATLAS_HOME").filter(|v| !v.is_empty()) {
        // absolute soo the background indexer (started with cwd = app dir) resolves the same place
        let home = PathBuf::from(home);
        return std::path::absolute(&home).unwrap_or(home);
    }

    let exe = exe_dir();
    let user_home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from);
    let dir = default_home(exe.as_deref(), user_home.as_deref());
    if exe.as_deref() != Some(dir.as_path()) {
        static MADE: Once = Once::new();
        MADE.call_once(|| make_private_dir(&dir));
    }
    dir
}

/// `~/.atlas`, soo moving or cleaning a checkout can't touch the data. Beside the binary instead when an older
/// install keeps its data there, for a test binary (cargo's `deps`, which must never see real data), or with no home.
fn default_home(exe: Option<&Path>, user_home: Option<&Path>) -> PathBuf {
    if let Some(exe) = exe {
        let is_test = exe.file_name().is_some_and(|name| name == "deps");
        if is_test || ["config.toml", "config.json", "atlas.db"].iter().any(|f| exe.join(f).exists()) {
            return exe.to_path_buf();
        }
    }
    match (user_home, exe) {
        (Some(home), _) => home.join(".atlas"),
        (None, Some(exe)) => exe.to_path_buf(),
        (None, None) => PathBuf::from("."),
    }
}

/// config.json holds passwords. A failure shows up where the first file can't be written.
fn make_private_dir(dir: &Path) {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    let _ = builder.create(dir);
}

pub fn exe_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// `config.toml`. `config.json` only where that's all there is, until the next start moves it.
pub fn config_file() -> PathBuf {
    let dir = app_dir();
    let (toml, json) = (dir.join("config.toml"), dir.join("config.json"));
    if !toml.exists() && json.exists() { json } else { toml }
}

pub fn database() -> PathBuf {
    app_dir().join("atlas.db")
}

pub fn status_file() -> PathBuf {
    app_dir().join("status.json")
}

pub fn stats_file() -> PathBuf {
    app_dir().join("stats.json")
}

pub fn pid_file() -> PathBuf {
    app_dir().join("bg_indexer.pid")
}

pub fn indexer_log() -> PathBuf {
    app_dir().join("bg_index.log")
}

pub fn sab_log() -> PathBuf {
    app_dir().join("sabnzbd.log")
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// the PreDB titles, next to the shards
pub fn predb_database() -> PathBuf {
    crate::predb::db::path_for(&database())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_under_the_users_home() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, home) = (dir.path().join("bin"), dir.path().join("me"));
        fs::create_dir_all(&exe).unwrap();
        assert_eq!(default_home(Some(&exe), Some(&home)), home.join(".atlas"));
        assert_eq!(default_home(None, Some(&home)), home.join(".atlas"));
    }

    #[test]
    fn an_older_install_keeps_its_data_beside_the_binary() {
        for file in ["config.toml", "config.json", "atlas.db"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(file), b"").unwrap();
            assert_eq!(default_home(Some(dir.path()), Some(Path::new("/me"))), dir.path(), "{file}");
        }
    }

    #[test]
    fn a_test_binary_never_defaults_to_real_data() {
        let exe = Path::new("/repo/target/debug/deps");
        assert_eq!(default_home(Some(exe), Some(Path::new("/me"))), exe);
    }

    #[test]
    fn with_no_home_it_stays_beside_the_binary() {
        let exe = Path::new("/somewhere/bin");
        assert_eq!(default_home(Some(exe), None), exe);
        assert_eq!(default_home(None, None), Path::new("."));
    }
}
