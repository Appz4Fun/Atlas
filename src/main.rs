use std::process::ExitCode;

use atlas::{app, bg_indexer, compact, convert, db, paths, postprocess, procs, store};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--version") || has("-V") {
        println!("atlas {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if has("--help") || has("-h") {
        println!(
            "atlas {}\n\nusage: atlas [--selftest | --bg-indexer | --convert]\n\n  (no args)     interactive menu\n  --selftest    check the login on every usenet server and exit\n  --bg-indexer  run the indexing loop headless (the menu starts this for you)\n  --convert     move a database from before the shards into them, then exit\n                (the indexer does this on its own when it starts)\n  --compact     rewrite the shards smaller (stop indexing first), then exit\n  --purge       delete what no step can help, a dry run unless --apply. Alone it takes\n                fragments only: [--fragments] [--blocked] [--too-small [--below BYTES]]\n                [--image-only] [--rejected] [--dead] [--sample N] [--apply]\n  --reprocess   look at stored releases again, a dry run unless --apply:\n                [--step NAME]... [--group NAME]... [--failed] [--apply]\n  --reflag-obfuscated  flag old rows the current check calls random, a dry run unless\n                --apply: [--sample N] [--apply]\n  --unname      clear names taken from subjects and posted files, a dry run unless\n                --apply: [--sample N] [--apply]\n  --recount-reposts  take reposted articles (a part posted again under another message-id) out of\n                old rows' size and parts, a dry run unless --apply: [--sample N] [--apply]\n  --undo-mojibake  rows named from a subject a server encoded twice (CultFilmsâ„¢): into the row of the\n                same articles under the right name, or renamed, a dry run unless --apply: [--sample N] [--apply]\n  --sealed-log-move  move sealed articles to the sealed log at full speed (stop indexing first)\n  --predb-import  read PreDB titles from a local file, a dry run unless --apply:\n                FILE [--apply]\n  --replay-raw-predb  re-parse all archived raw PreDB feed payloads in atlas.raw.db:\n                [--apply]\n  --reset-shards  harvest shard probes/hashes to atlas.raw.db and wipe only shards/headers:\n                [--apply]\n  --search-indexes  index poster, size, posted date and name, for searching and sorting
                on them (stop indexing first),\n                a dry run unless --apply: [--apply]\n  --thaw        put frozen releases back into their shards: RELEASE_ID...\n  --frozen-stats  count frozen releases per shard and reason\n  --crc-links   count releases the file and first-article CRC keys link, reading only:\n                [--sample N]",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }

    match atlas::config::migrate_to_toml() {
        Ok(Some(path)) => eprintln!("config.json is now {}, and the old file is config.json.bak-toml", path.display()),
        Ok(None) => {}
        Err(e) => eprintln!("couldnt convert config.json to config.toml, still using it: {e}"),
    }

    atlas::sealed_store::configure(&atlas::config::sealed_log_config().addr);

    let code = if has("--convert") {
        run_convert()
    } else if has("--compact") {
        run_compact()
    } else if has("--purge") {
        postprocess::purge::main(&args)
    } else if has("--reprocess") {
        postprocess::reprocess::main(&args)
    } else if has("--predb-import") {
        atlas::predb::file_import::run(&args)
    } else if has("--replay-raw-predb") {
        atlas::raw_store::run_replay_predb(&args)
    } else if has("--reset-shards") {
        atlas::raw_store::run_reset_shards(&args)
    } else if has("--reflag-obfuscated") {
        atlas::naming::reflag::run(&args)
    } else if has("--unname") {
        atlas::naming::unname::run(&args)
    } else if has("--recount-reposts") {
        atlas::naming::recount::run(&args)
    } else if has("--undo-mojibake") {
        atlas::naming::mojibake::run(&args)
    } else if has("--sealed-log-move") {
        atlas::sealed_store::run_move()
    } else if has("--shared-catalog-fill") {
        atlas::shared_catalog::run_fill()
    } else if has("--shared-catalog-check") {
        atlas::shared_catalog::run_check(has("--prune"))
    } else if has("--search-indexes") {
        atlas::field_search::run(&args)
    } else if has("--thaw") {
        atlas::frozen::run_thaw(&args)
    } else if has("--frozen-stats") {
        atlas::frozen::run_stats()
    } else if has("--crc-links") {
        atlas::keys::run_report(&args)
    } else if has("--selftest") {
        app::selftest()
    } else if has(procs::BG_FLAG) {
        bg_indexer::run()
    } else {
        app::main_menu()
    };

    ExitCode::from(code.clamp(0, 255) as u8)
}

/// `--convert`: the one time move into the shards, in the foreground.
fn run_convert() -> i32 {
    let main = paths::database();
    // a swap cut short is finished (or undone) by `run_alone`, not skipped
    if !convert::to_do(&main) {
        println!("{} is already converted (or doesnt exist)", main.display());
        return 0;
    }
    if procs::indexer_alive() {
        println!("stop indexing first");
        return 1;
    }
    // alone: not alongside the indexer, a compaction or another conversion
    match convert::run_alone(&main, &|msg| println!("{msg}")) {
        Ok(Some(_)) => 0,
        Ok(None) => {
            println!("{} is already converted (or doesnt exist)", main.display());
            0
        }
        Err(e) => {
            println!("couldnt convert, nothing was changed: {e:#}");
            1
        }
    }
}

/// `--compact`: rewrite the shards smaller, in the foreground.
fn run_compact() -> i32 {
    if procs::indexer_alive() {
        println!("stop indexing first");
        return 1;
    }
    // nothing sets it: only the indexer stops a compaction early
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match compact::run(&paths::database(), &|msg| println!("{msg}"), &stop) {
        Ok(_) => {
            // noted like the indexer's own compaction does, soo the auto one waits its interval
            // never made here: an empty main database would pass for the real one
            if let Ok(conn) = db::open_shard(&paths::database()) {
                let _ = store::set_meta(&conn, "last_compact", chrono::Utc::now().timestamp());
            }
            0
        }
        Err(e) => {
            println!("couldnt compact: {e:#}");
            1
        }
    }
}
