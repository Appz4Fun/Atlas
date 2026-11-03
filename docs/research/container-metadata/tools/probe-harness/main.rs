//! Runs Atlas's media probe on file prefixes and prints the result as JSON.
//! Usage: probe-harness FILE...   (prints one JSON object per file and prefix)
//! Prefixes: 700 KB (one article), 4 MB (six articles), and the full file.

#[path = "media.rs"]
#[allow(dead_code)]
mod media;

use std::fs;

const PREFIXES: [(&str, usize); 3] = [("700k", 700_000), ("4m", 4_000_000), ("full", usize::MAX)];

fn main() {
    for path in std::env::args().skip(1) {
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        for (label, size) in PREFIXES {
            let cut = size.min(data.len());
            let prefix = &data[..cut];
            // catch_unwind so a panic on one prefix is reported, not fatal
            let result = std::panic::catch_unwind(|| media::parse(prefix));
            let out = match result {
                Ok(Some(info)) => serde_json::json!({
                    "file": path,
                    "prefix": label,
                    "prefix_bytes": cut,
                    "header_complete": info.header_complete,
                    "info": info,
                }),
                Ok(None) => serde_json::json!({"file": path, "prefix": label, "prefix_bytes": cut, "info": null}),
                Err(_) => serde_json::json!({"file": path, "prefix": label, "prefix_bytes": cut, "PANIC": true}),
            };
            println!("{}", serde_json::to_string(&out).unwrap());
        }
    }
}
