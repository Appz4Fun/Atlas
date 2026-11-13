//! PostgreSQL product startup and provider self-test.

pub mod presentation;

use crate::api;
use crate::config::load_config;
use crate::ui;

fn native_web(storage: &crate::pg::StorageConfig) -> i32 {
    let Some(mut config) = load_config() else {
        ui::error("configuration unavailable; native Web UI was not started");
        return 1;
    };
    if !api::start_for_storage(&mut config, storage) {
        return 1;
    }
    crate::metrics::start(&config, crate::metrics::Role::Api);
    // The API already owns its worker threads. Service termination closes
    // read-only connections and anonymous spools; no terminal/shard loop runs.
    loop {
        std::thread::park();
    }
}

/// Retired SQLite storage/conversion commands. Classification reads no configuration
/// and cannot initialize a database, contact a provider, or create an asset.
pub fn retired_storage_command(args: &[String]) -> Option<&str> {
    const RETIRED: &[&str] = &[
        "--crc-links",
        "--purge",
        "--reprocess",
        "--predb-import",
        "--replay-raw-predb",
        "--reflag-obfuscated",
        "--unname",
        "--recount-articles",
        "--count-repair-candidates",
        "--split-reposts",
        "--undo-mojibake",
        "--convert",
        "--compact",
        "--reset-shards",
        "--sealed-log-move",
        "--shared-catalog-fill",
        "--shared-catalog-check",
        "--search-indexes",
        "--thaw",
        "--frozen-stats",
        "--export-keep",
        "--move-ext",
        "--restore-keep",
        "--import-main",
        "--import-history",
    ];
    args.iter().find(|arg| RETIRED.contains(&arg.as_str())).map(String::as_str)
}

/// The product replay entry point requires an explicit native staging destination.
/// The replay parser validates the flag values before opening its writer.
pub fn replay_requires_postgres(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--save-replay")
        && !(args.iter().any(|arg| arg == "--pg-database") && args.iter().any(|arg| arg == "--pg-schema"))
}

#[cfg(test)]
mod native_cli_boundary_tests {
    use super::{replay_requires_postgres, retired_storage_command};
    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).into()).collect()
    }
    #[test]
    fn retired_storage_commands_are_refused_even_with_a_native_command() {
        for command in [
            "--crc-links",
            "--purge",
            "--reprocess",
            "--predb-import",
            "--replay-raw-predb",
            "--reflag-obfuscated",
            "--unname",
            "--recount-articles",
            "--count-repair-candidates",
            "--split-reposts",
            "--undo-mojibake",
            "--convert",
            "--compact",
            "--reset-shards",
            "--sealed-log-move",
            "--shared-catalog-fill",
            "--shared-catalog-check",
            "--search-indexes",
            "--thaw",
            "--frozen-stats",
            "--export-keep",
            "--move-ext",
            "--restore-keep",
            "--import-main",
            "--import-history",
        ] {
            assert_eq!(retired_storage_command(&args(&["--dump-ext", command, "--apply"])), Some(command));
        }
    }
    #[test]
    fn replay_and_pure_readers_preserve_their_native_or_independent_destinations() {
        assert!(replay_requires_postgres(&args(&["--save-replay", "--store", "fixture", "--db", "scratch"])));
        assert!(replay_requires_postgres(&args(&["--save-replay", "--pg-schema", "atlas_replay_fixture"])));
        assert!(!replay_requires_postgres(&args(&[
            "--save-replay",
            "--pg-database",
            "fixture",
            "--pg-schema",
            "atlas_replay_fixture"
        ])));
        for command in ["--dump-ext", "--restore-ext", "--header-store", "--overview", "--selftest", "--bg-indexer"] {
            assert_eq!(retired_storage_command(&args(&[command])), None);
        }
    }
}

/// Start the native Web service. The former SQLite menu and its initializer are retired.
pub fn main_menu() -> i32 {
    match crate::config::checked_storage_config() {
        Ok(storage) => native_web(&storage),
        Err(_) => {
            ui::error("PostgreSQL storage configuration unavailable; Atlas was not started");
            1
        }
    }
}

pub fn selftest() -> i32 {
    if let Some(problem) = crate::config::file_problem() {
        println!("selftest: {problem}");
        return 1;
    }
    let Some(cfg) = load_config() else {
        println!("selftest: no config found (edit config.toml or set ATLAS_NNTP_* env)");
        return 1;
    };

    if cfg.servers.is_empty() {
        println!("selftest: no usenet servers configured");
        return 1;
    }

    // check every server, in the order they get tried
    let mut ok = 0;
    for (i, s) in cfg.servers.iter().enumerate() {
        print!(
            "selftest {}: {}:{} ssl={} user={} priority={} connections={} ... ",
            i + 1,
            s.host,
            s.port,
            s.use_ssl(),
            s.username,
            s.priority,
            s.connections()
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

        if s.username.is_empty() || s.password.is_empty() {
            println!("skipped: no username/password set");
            continue;
        }

        match crate::nntp::BlockingPool::check_server(s) {
            Ok(()) => {
                println!("connected OK");
                ok += 1;
            }
            Err(e) => println!("FAILED: {e}"),
        }
    }

    println!("selftest: {ok}/{} servers OK", cfg.servers.len());
    if ok > 0 { 0 } else { 1 }
}
