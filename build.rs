use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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

fn is_hex_sha(value: &str) -> bool {
    value.len() >= 7 && value.len() <= 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn find_git_dirs() -> Option<(PathBuf, PathBuf)> {
    if let Some(git_dir) = run_cmd("git", &["rev-parse", "--git-dir"]) {
        let common_dir = run_cmd("git", &["rev-parse", "--git-common-dir"]).unwrap_or_else(|| git_dir.clone());
        return Some((PathBuf::from(git_dir), PathBuf::from(common_dir)));
    }

    let dotgit = Path::new(".git");
    let git_dir = if dotgit.is_dir() {
        dotgit.to_path_buf()
    } else if dotgit.is_file() {
        let raw = fs::read_to_string(dotgit).ok()?;
        let rel = raw.trim().strip_prefix("gitdir:")?.trim();
        PathBuf::from(rel)
    } else {
        return None;
    };

    let common_dir = fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|rel| git_dir.join(rel.trim()))
        .unwrap_or_else(|| git_dir.clone());

    Some((git_dir, common_dir))
}

fn read_git_ref(git_dir: &Path, common_dir: &Path, ref_name: &str) -> Option<String> {
    for base in [git_dir, common_dir] {
        let ref_path = base.join(ref_name);
        if let Ok(contents) = fs::read_to_string(ref_path) {
            let trimmed = contents.trim();
            if let Some(target) = trimmed.strip_prefix("ref:") {
                return read_git_ref(git_dir, common_dir, target.trim());
            }
            if is_hex_sha(trimmed) {
                return Some(trimmed.to_string());
            }
        }
    }

    for base in [common_dir, git_dir] {
        let packed_path = base.join("packed-refs");
        if let Ok(packed) = fs::read_to_string(packed_path) {
            for line in packed.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                    continue;
                }
                let mut parts = line.split_whitespace();
                if let (Some(sha), Some(name)) = (parts.next(), parts.next())
                    && name == ref_name
                    && is_hex_sha(sha)
                {
                    return Some(sha.to_string());
                }
            }
        }
    }

    None
}

fn resolve_git_sha_from_dotgit() -> Option<String> {
    let (git_dir, common_dir) = find_git_dirs()?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let trimmed = head.trim();
    if let Some(symref) = trimmed.strip_prefix("ref:") {
        read_git_ref(&git_dir, &common_dir, symref.trim())
    } else if is_hex_sha(trimmed) {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn collect_loose_tags(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let tag_name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if path.is_dir() {
            collect_loose_tags(&path, &tag_name, out);
        } else if let Ok(contents) = fs::read_to_string(&path) {
            let sha = contents.trim();
            if is_hex_sha(sha) {
                out.insert(tag_name, sha.to_string());
            }
        }
    }
}

fn version_sort_key(tag: &str) -> (Vec<u64>, String) {
    let nums = tag
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect::<Vec<_>>();
    (nums, tag.to_string())
}

fn resolve_version_from_dotgit(head_sha: &str) -> Option<String> {
    let (git_dir, common_dir) = find_git_dirs()?;
    let mut tags = BTreeMap::<String, String>::new();

    for base in [&common_dir, &git_dir] {
        let packed_path = base.join("packed-refs");
        if let Ok(packed) = fs::read_to_string(packed_path) {
            let mut last_tag: Option<String> = None;
            for line in packed.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(peeled) = line.strip_prefix('^') {
                    let peeled = peeled.trim();
                    if let Some(tag) = &last_tag
                        && is_hex_sha(peeled)
                    {
                        tags.insert(tag.clone(), peeled.to_string());
                    }
                    continue;
                }
                last_tag = None;
                let mut parts = line.split_whitespace();
                if let (Some(sha), Some(ref_name)) = (parts.next(), parts.next())
                    && let Some(tag) = ref_name.strip_prefix("refs/tags/")
                    && is_hex_sha(sha)
                {
                    tags.insert(tag.to_string(), sha.to_string());
                    last_tag = Some(tag.to_string());
                }
            }
        }
        collect_loose_tags(&base.join("refs/tags"), "", &mut tags);
    }

    tags.retain(|tag, sha| !LEGACY_PYTHON_TAGS.contains(&tag.as_str()) && sha != PYTHON_LAST_COMMIT);
    if tags.is_empty() {
        return None;
    }
    if let Some((tag, _)) = tags.iter().filter(|(_, sha)| *sha == head_sha).max_by_key(|(tag, _)| version_sort_key(tag))
    {
        return Some(tag.clone());
    }
    tags.keys().max_by_key(|tag| version_sort_key(tag)).cloned()
}

fn emit_git_rerun_hints() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some((git_dir, common_dir)) = find_git_dirs() else {
        return;
    };

    for base in [&git_dir, &common_dir] {
        for sub in ["HEAD", "packed-refs", "refs/heads", "refs/tags"] {
            let path = base.join(sub);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    let symref = run_cmd("git", &["symbolic-ref", "-q", "HEAD"]).or_else(|| {
        fs::read_to_string(git_dir.join("HEAD"))
            .ok()
            .and_then(|s| s.trim().strip_prefix("ref:").map(|r| r.trim().to_string()))
    });
    if let Some(symref) = symref {
        for base in [&git_dir, &common_dir] {
            let ref_path = base.join(&symref);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
        }
    }
}

fn resolve_version(head_sha: &str) -> String {
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
    if let Some(tag) = resolve_version_from_dotgit(head_sha) {
        return tag;
    }
    "v0.0.0.1".to_string()
}

fn resolve_git_sha() -> String {
    non_empty_env("ATLAS_GIT_SHA")
        .or_else(|| run_cmd("git", &["rev-parse", "HEAD"]))
        .or_else(resolve_git_sha_from_dotgit)
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

    let git_sha = resolve_git_sha();
    let git_short_sha = resolve_git_short_sha(&git_sha);
    let version = resolve_version(&git_sha);
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
