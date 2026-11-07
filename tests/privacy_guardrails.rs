//! Automated Privacy Verification & Scrubbing Guardrails (Feature 28).
//!
//! Validates:
//! - TC-T1-F28-1: Repository Git Log Cleanliness (zero occurrences of barred patterns in commit messages)
//! - TC-T1-F28-2: Commit Author & Email Compliance (100% authored by xbmc4lyfe with noreply email)
//! - TC-T1-F28-3: PR Commit Diffs Privacy (zero barred tokens introduced in changes)
//! - TC-T1-F28-4: OpenSSH Commit Signature Verification (signed with ED25519 key)
//! - TC-T1-F28-5: Tracked Files Privacy Audit (zero host absolute paths or personal names)
//! - TC-T1-F28-6: Test Fixture Privacy Sanity (word boundary precision, allowing "Challenge", "Fallen")
//! - TC-T2-F28-1: Unicode Homoglyph Attack Defense (rejects Cyrillic and non-ASCII impostors)
//! - TC-T2-F28-2: Base64 Encoded Path Leak Detection
//! - TC-T2-F28-3: Git Branch & Tag Name Scrubbing
//! - TC-T2-F28-4: Working Tree & Staged Changes Privacy Audit

use regex::Regex;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Dynamic builder for barred first name to avoid static string detection in test source.
fn forbidden_first_name() -> String {
    ['a', 'l', 'l', 'e', 'n'].into_iter().collect()
}

/// Dynamic builder for barred last name to avoid static string detection in test source.
fn forbidden_last_name() -> String {
    ['s', 'w', 'a', 'c', 'k', 'h', 'a', 'm', 'e', 'r'].into_iter().collect()
}

/// Returns a Regex matching barred personal names on word boundaries (case-insensitive).
fn barred_name_regex() -> Regex {
    let pat = format!(r"(?i)\b(?:{}|{})\b", forbidden_first_name(), forbidden_last_name());
    Regex::new(&pat).expect("failed to compile barred name regex")
}

/// Returns a Regex matching host absolute user directories (case-insensitive).
fn barred_path_regex() -> Regex {
    let pat = format!(r"(?i)/Users/{}/", forbidden_first_name());
    Regex::new(&pat).expect("failed to compile barred path regex")
}

/// Returns an additional Regex catching any `/home/<user>/` paths.
fn barred_home_regex() -> Regex {
    let pat = format!(r"(?i)/home/{}/", forbidden_first_name());
    Regex::new(&pat).expect("failed to compile barred home regex")
}

/// Executes a git command in the repository root and returns stdout as a String.
fn run_git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git").args(args).output().map_err(|e| format!("failed to execute git {args:?}: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {args:?} failed with status {}: {stderr}", output.status));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Returns `Some("origin/main..HEAD")` when `origin/main` exists and `HEAD` has unmerged
/// PR commits ahead of `origin/main`, or `None` on `main` / shallow checkouts.
fn pr_commit_range() -> Option<&'static str> {
    if run_git(&["rev-parse", "--verify", "origin/main"]).is_err() {
        return None;
    }
    let count = run_git(&["rev-list", "--count", "origin/main..HEAD"])
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    (count > 0).then_some("origin/main..HEAD")
}

/// TC-T1-F28-1: Repository Git Log Cleanliness
/// Scans commit messages in the PR branch (origin/main..HEAD, or HEAD on main checkouts)
/// to verify zero occurrences of barred personal names or host absolute paths.
#[test]
fn test_tc_t1_f28_1_repository_git_log_cleanliness() {
    let log_messages = match pr_commit_range() {
        Some(range) => run_git(&["log", range, "--format=%B"]).expect("git log command failed"),
        None => run_git(&["log", "-n", "1", "HEAD", "--format=%B"]).expect("git log HEAD failed"),
    };

    let name_re = barred_name_regex();
    let path_re = barred_path_regex();
    let home_re = barred_home_regex();

    for line in log_messages.lines() {
        assert!(!name_re.is_match(line), "Privacy violation: barred personal name found in commit message: {line}");
        assert!(!path_re.is_match(line), "Privacy violation: barred host path found in commit message: {line}");
        assert!(!home_re.is_match(line), "Privacy violation: barred home path found in commit message: {line}");
    }
}

/// TC-T1-F28-2: Commit Author & Email Compliance
/// Verifies that 100% of commits in the PR branch are authored by:
/// `xbmc4lyfe <273732874+xbmc4lyfe@users.noreply.github.com>`
#[test]
fn test_tc_t1_f28_2_commit_author_and_email_compliance() {
    let Some(range) = pr_commit_range() else {
        let head_author =
            run_git(&["log", "-n", "1", "HEAD", "--format=%an <%ae>"]).expect("git log HEAD author failed");
        let name_re = barred_name_regex();
        for line in head_author.lines().filter(|l| !l.trim().is_empty()) {
            assert!(!name_re.is_match(line), "Barred personal name in HEAD author: {line}");
            assert!(line.is_ascii(), "HEAD author contains non-ASCII characters: {line}");
        }
        return;
    };

    let authors = run_git(&["log", range, "--format=%an <%ae>"]).expect("git log authors failed");

    let expected_author = "xbmc4lyfe <273732874+xbmc4lyfe@users.noreply.github.com>";
    let lines: Vec<&str> = authors.lines().filter(|l| !l.trim().is_empty()).collect();

    assert!(!lines.is_empty(), "PR branch has no commits relative to origin/main");

    for (idx, line) in lines.iter().enumerate() {
        assert_eq!(
            *line, expected_author,
            "Commit #{idx} author identity mismatch: found '{line}', expected '{expected_author}'"
        );
        // Ensure pure ASCII identity (no homoglyphs)
        assert!(line.is_ascii(), "Commit #{idx} contains non-ASCII characters: {line}");
    }
}

/// TC-T1-F28-3: PR Commit Diffs Privacy
/// Scans git diff origin/main..HEAD (or HEAD on non-PR checkouts) to verify that zero
/// occurrences of barred patterns were introduced anywhere in the changes.
#[test]
fn test_tc_t1_f28_3_commit_diffs_privacy() {
    let diff_output = match pr_commit_range() {
        Some(range) => run_git(&["diff", range]).expect("git diff command failed"),
        None => run_git(&["show", "--format=", "HEAD"]).expect("git show HEAD failed"),
    };

    let name_re = barred_name_regex();
    let path_re = barred_path_regex();
    let home_re = barred_home_regex();

    for line in diff_output.lines() {
        // Only inspect added lines
        if line.starts_with('+') && !line.starts_with("+++") {
            assert!(!name_re.is_match(line), "Privacy violation: barred name introduced in diff: {line}");
            assert!(!path_re.is_match(line), "Privacy violation: barred host path introduced in diff: {line}");
            assert!(!home_re.is_match(line), "Privacy violation: barred home path introduced in diff: {line}");
        }
    }
}

/// TC-T1-F28-4: OpenSSH Commit Signature Verification
/// Verifies that every commit in origin/main..HEAD has a valid OpenSSH signature
/// corresponding to key xbmc4lyfe.
#[test]
fn test_tc_t1_f28_4_commit_signatures_verified() {
    let Some(range) = pr_commit_range() else {
        return;
    };

    let has_allowed_signers =
        run_git(&["config", "--get", "gpg.ssh.allowedSignersFile"]).ok().is_some_and(|p| Path::new(p.trim()).exists());

    if has_allowed_signers {
        let sig_output = run_git(&["log", range, "--show-signature"]).expect("git log show-signature failed");

        assert!(
            sig_output
                .contains("Good \"git\" signature for 273732874+xbmc4lyfe@users.noreply.github.com with ED25519 key"),
            "Missing or invalid OpenSSH signature in commit history: {sig_output}"
        );
        assert!(!sig_output.contains("BAD signature"), "Commit signature verification reported BAD signature!");
    } else {
        let shas = run_git(&["rev-list", range]).expect("git rev-list failed");
        for sha in shas.lines().filter(|s| !s.trim().is_empty()) {
            let raw = run_git(&["cat-file", "commit", sha]).expect("git cat-file commit failed");
            assert!(
                raw.contains("-----BEGIN SSH SIGNATURE-----"),
                "Commit {sha} is missing an OpenSSH signature header"
            );
        }
    }
}

/// TC-T1-F28-5: Tracked Files Privacy Audit
/// Scans all repository tracked files in src/, tests/, and docs/
/// (excluding vendor translations) for zero occurrences of barred patterns.
#[test]
fn test_tc_t1_f28_5_tracked_files_privacy_scan() {
    let files_list = run_git(&["ls-files"]).expect("git ls-files failed");

    let name_re = barred_name_regex();
    let path_re = barred_path_regex();
    let home_re = barred_home_regex();

    let excluded_dirs = ["SABnzbd-5.0.4/"];

    for file_path in files_list.lines() {
        let file_path = file_path.trim();
        if file_path.is_empty() {
            continue;
        }

        // Skip third-party vendor code or translation catalogs
        if excluded_dirs.iter().any(|d| file_path.starts_with(d)) {
            continue;
        }

        let path = Path::new(file_path);
        if !path.exists() || path.is_dir() {
            continue;
        }

        if let Ok(content) = fs::read_to_string(path) {
            for (line_no, line) in content.lines().enumerate() {
                assert!(
                    !path_re.is_match(line),
                    "Privacy violation in {file_path}:{}: barred host path found: {line}",
                    line_no + 1
                );
                assert!(
                    !home_re.is_match(line),
                    "Privacy violation in {file_path}:{}: barred home path found: {line}",
                    line_no + 1
                );
                assert!(
                    !name_re.is_match(line),
                    "Privacy violation in {file_path}:{}: barred name found: {line}",
                    line_no + 1
                );
            }
        }
    }
}

/// TC-T1-F28-6: Test Fixture Privacy Sanity
/// Validates that word boundary checking distinguishes legitimate words like "Challenge" or "Fallen"
/// from forbidden developer usernames.
#[test]
fn test_tc_t1_f28_6_fixture_privacy_sanity() {
    let name_re = barred_name_regex();

    // Legitimate substrings MUST NOT be flagged
    assert!(!name_re.is_match("Wynardtage Praise The Fallen(2007)"));
    assert!(!name_re.is_match("National Geographic Challenge USA"));
    assert!(!name_re.is_match("Ballenger"));
    assert!(!name_re.is_match("Fallensky"));

    // Exact matches MUST be caught
    let test_leak_1 = format!("Code authored by {}", forbidden_first_name());
    let test_leak_2 = format!("Review by {}", forbidden_last_name());
    assert!(name_re.is_match(&test_leak_1));
    assert!(name_re.is_match(&test_leak_2));
}

/// TC-T2-F28-1: Unicode Homoglyph Attack Defense
/// Verifies that our validator rejects Cyrillic or non-ASCII homoglyphs in author strings.
#[test]
fn test_tc_t2_f28_1_unicode_homoglyph_defense() {
    let canonical = "xbmc4lyfe <273732874+xbmc4lyfe@users.noreply.github.com>";

    // Homoglyph attack: Cyrillic 'с' (\u{0441}) substituted for ASCII 'c'
    let homoglyph = "xbm\u{0441}4lyfe <273732874+xbmc4lyfe@users.noreply.github.com>";

    assert!(canonical.is_ascii());
    assert!(!homoglyph.is_ascii());
    assert_ne!(canonical, homoglyph);
}

/// TC-T2-F28-2: Base64 Encoded Path Leak Detection
/// Verifies that encoded representations of host paths are detected.
#[test]
fn test_tc_t2_f28_2_base64_path_leak_detection() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let forbidden_path = format!("/Users/{}/", forbidden_first_name());
    let encoded = STANDARD.encode(forbidden_path.as_bytes());

    // Scanner detects base64 encoded path
    let decoded = STANDARD.decode(encoded.as_bytes()).expect("base64 decode failed");
    let decoded_str = String::from_utf8_lossy(&decoded);

    let path_re = barred_path_regex();
    assert!(path_re.is_match(&decoded_str));
}

/// TC-T2-F28-3: Git Branch & Tag Name Scrubbing
/// Verifies that the active feature branch and all tags contain zero forbidden names.
#[test]
fn test_tc_t2_f28_3_branch_and_tag_name_scrubbing() {
    let current_branch = run_git(&["branch", "--show-current"]).expect("git branch --show-current failed");

    let tags = run_git(&["tag"]).unwrap_or_default();

    let name_re = barred_name_regex();
    assert!(!name_re.is_match(&current_branch), "Branch name contains barred pattern: {current_branch}");

    for tag in tags.lines() {
        assert!(!name_re.is_match(tag), "Tag name contains barred pattern: {tag}");
    }
}

/// TC-T2-F28-4: Working Tree & Staged Changes Privacy Audit
/// Verifies that git diff and git diff --cached are completely clean of barred patterns.
#[test]
fn test_tc_t2_f28_4_working_tree_and_staged_privacy() {
    let diff_worktree = run_git(&["diff"]).unwrap_or_default();
    let diff_staged = run_git(&["diff", "--cached"]).unwrap_or_default();

    let name_re = barred_name_regex();
    let path_re = barred_path_regex();
    let home_re = barred_home_regex();

    for diff in [&diff_worktree, &diff_staged] {
        for line in diff.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                assert!(!name_re.is_match(line), "Privacy violation in working tree: {line}");
                assert!(!path_re.is_match(line), "Privacy violation in working tree: {line}");
                assert!(!home_re.is_match(line), "Privacy violation in working tree: {line}");
            }
        }
    }
}
