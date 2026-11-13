use std::process::ExitCode;

use atlas::{app, bg_indexer, procs};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|arg| arg == flag);

    if has("--version") || has("-V") {
        println!("atlas {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if has("--help") || has("-h") {
        println!(
            "atlas {}\n\nusage: atlas [--selftest | --bg-indexer | --dump-ext | --restore-ext DIR]\n\n  (no args)     start the native Web service with checked PostgreSQL storage\n  --selftest    check the login on every Usenet server and exit\n  --bg-indexer  run the native PostgreSQL indexing loop\n  --dump-ext    dump PostgreSQL ext_* tables to ext-dumps/<time>/\n                [--allow-shrink]\n  --restore-ext DIR  restore a PostgreSQL ext_* dump [--replace]\n  --save-replay --store DIR --db DIR --pg-database NAME --pg-schema atlas_replay_NAME\n                replay copied NXV headers into a fresh PostgreSQL staging schema;\n                set ATLAS_REPLAY_PG_URL [--pg-header-source ID]\n  --header-store  read a copied header store independently of catalog storage:\n                link --store DIR --from UNIX --to UNIX | link-day --store DIR --day YYYY-MM-DD | stats --store DIR\n  --overview    read the overview archive without a provider or catalog:\n                SERVER GROUP [stats | verify | START-END | <message-id>]\n\nPostgreSQL storage configuration is required for runtime and backup commands.\nCatalog maintenance still awaiting native implementations is refused explicitly;\nsee docs/POSTGRES_FRONTDOORS.md for the remaining maintenance contracts.",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }

    // Admission precedes every dispatcher, including mixed command lines and pure tools.
    // No retired source catalog is opened and no configuration migration can run here.
    if let Some(command) = app::retired_storage_command(&args) {
        eprintln!("{command} is retired from the PostgreSQL product; no storage was opened");
        return ExitCode::from(1);
    }
    if let Some(command) = app::unavailable_native_maintenance_command(&args) {
        eprintln!("{command} requires a native PostgreSQL maintenance implementation; no storage was opened");
        return ExitCode::from(1);
    }
    if app::replay_requires_postgres(&args) {
        eprintln!("--save-replay requires --pg-database and --pg-schema; no storage was opened");
        return ExitCode::from(1);
    }
    if has("--save-replay") {
        return ExitCode::from(atlas::save_replay::run_cli(&args).clamp(0, 255) as u8);
    }
    if has("--header-store") {
        return ExitCode::from(atlas::nxv_store::tools::run_cli(&args).clamp(0, 255) as u8);
    }
    if has("--overview") {
        return ExitCode::from(atlas::overview_archive::run_cli(&args).clamp(0, 255) as u8);
    }
    if !args.is_empty() && !has("--dump-ext") && !has("--restore-ext") && !has("--selftest") && !has(procs::BG_FLAG) {
        eprintln!("unrecognized command; use --help");
        return ExitCode::from(1);
    }

    // Check the selected entry before migration, provider setup, catalog startup or backup.
    if atlas::config::checked_storage_config().is_err() {
        eprintln!(
            "PostgreSQL storage configuration is unavailable in {}; Atlas was not started",
            atlas::paths::app_dir().display()
        );
        return ExitCode::from(1);
    }
    match atlas::config::migrate_to_toml() {
        Ok(Some(path)) => eprintln!("config.json is now {}, and the old file is config.json.bak-toml", path.display()),
        Ok(None) => {}
        Err(error) => eprintln!("couldnt convert config.json to config.toml, still using it: {error}"),
    }

    let code = if has("--dump-ext") || has("--restore-ext") {
        atlas::pg::ext::run_cli(&args)
    } else if has("--selftest") {
        app::selftest()
    } else if has(procs::BG_FLAG) {
        bg_indexer::run()
    } else {
        app::main_menu()
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}
