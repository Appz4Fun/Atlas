use std::env;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Last commit of the original Python history before the Rust port (`7ea4472`).
const PYTHON_LAST_COMMIT: &str = "95b5b3e383d792ee53415c9798b9522f1aac4075";

/// Tags from the original Python repository that predate the Rust port.
const LEGACY_PYTHON_TAGS: &[&str] = &["v0.1", "v0.3", "v0.4", "v5", "v5.1", "v5.1.1", "v5.1.2", "v5.2.0", "v5.2.1"];

fn run_cmd(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

fn non_empty_env(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn emit_git_rerun_hints() {
    let Some(git_dir) = run_cmd("git", &["rev-parse", "--git-dir"]) else {
        return;
    };
    let common_dir = run_cmd("git", &["rev-parse", "--git-common-dir"]).unwrap_or_else(|| git_dir.clone());

    for base in [&git_dir, &common_dir] {
        for sub in ["HEAD", "packed-refs", "refs/heads", "refs/tags"] {
            let path = Path::new(base).join(sub);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    if let Some(symref) = run_cmd("git", &["symbolic-ref", "-q", "HEAD"]) {
        for base in [&git_dir, &common_dir] {
            let ref_path = Path::new(base).join(&symref);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
        }
    }
}

fn resolve_version() -> String {
    if let Some(version) = non_empty_env("ATLAS_BUILD_VERSION") {
        return version;
    }
    if let Some(tag) = run_cmd("git", &["describe", "--tags", "--abbrev=0"])
        && !LEGACY_PYTHON_TAGS.contains(&tag.as_str())
    {
        let is_python_tag = Command::new("git")
            .args(["merge-base", "--is-ancestor", &tag, PYTHON_LAST_COMMIT])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !is_python_tag {
            return tag;
        }
    }
    "v0.0.0.1".to_string()
}

fn resolve_git_sha() -> String {
    non_empty_env("ATLAS_GIT_SHA")
        .or_else(|| run_cmd("git", &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string())
}

fn resolve_git_short_sha(full_sha: &str) -> String {
    non_empty_env("ATLAS_GIT_SHORT_SHA").or_else(|| run_cmd("git", &["rev-parse", "--short=7", "HEAD"])).unwrap_or_else(
        || {
            if full_sha != "unknown" && full_sha.len() >= 7 { full_sha[..7].to_string() } else { "unknown".to_string() }
        },
    )
}

fn resolve_rustc_version() -> String {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    if let Some(raw) = run_cmd(&rustc, &["--version"]) {
        // "rustc 1.99.0 (b940084d7 2026-09-28)" -> "1.99.0"
        if let Some(ver) = raw.strip_prefix("rustc ").and_then(|s| s.split_whitespace().next()) {
            return ver.to_string();
        }
        return raw;
    }
    "unknown".to_string()
}

/// Converts a Unix timestamp (seconds since 1970-01-01 00:00:00 UTC) into `(year, month, day, hour, minute, second)`.
fn utc_ymdhms(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;

    // Howard Hinnant's civil_from_days algorithm
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, minute, second)
}

fn main() {
    emit_git_rerun_hints();

    let version = resolve_version();
    let git_sha = resolve_git_sha();
    let git_short_sha = resolve_git_short_sha(&git_sha);
    let rustc_version = resolve_rustc_version();

    let now_secs = non_empty_env("SOURCE_DATE_EPOCH")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let (year, month, day, hour, minute, second) = utc_ymdhms(now_secs);
    let build_date = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC");
    let build_year = format!("{year:04}");

    println!("cargo:rustc-env=ATLAS_BUILD_VERSION={version}");
    println!("cargo:rustc-env=ATLAS_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=ATLAS_GIT_SHORT_SHA={git_short_sha}");
    println!("cargo:rustc-env=ATLAS_RUSTC_VERSION={rustc_version}");
    println!("cargo:rustc-env=ATLAS_BUILD_DATE={build_date}");
    println!("cargo:rustc-env=ATLAS_BUILD_YEAR={build_year}");
}
