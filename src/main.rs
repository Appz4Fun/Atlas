use std::process::ExitCode;

use atlas::{app, bg_indexer, procs};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--version") || has("-V") {
        println!("atlas {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if has("--help") || has("-h") {
        println!(
            "atlas {}\n\nusage: atlas [--selftest | --bg-indexer]\n\n  (no args)     interactive menu\n  --selftest    check the login on every usenet server and exit\n  --bg-indexer  run the indexing loop headless (the menu starts this for you)",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }

    let code = if has("--selftest") {
        app::selftest()
    } else if has(procs::BG_FLAG) {
        bg_indexer::run()
    } else {
        app::main_menu()
    };

    ExitCode::from(code.clamp(0, 255) as u8)
}
