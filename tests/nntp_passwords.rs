//! Integration tests for Milestone 4: Metadata Extraction & Password Identification
//! Covering Password Features 16, 17, 18, and TC-T3-03.

use std::collections::HashMap;
use std::time::Instant;

use atlas::archive::{EncryptionMode, PasswordCheck, PasswordHarvester, Rar5Header, Rar5PswCheck};

// ============================================================================
// Feature 16: Two-Stage RAR5 -hp Detection (Candidate Heuristic + Article 1)
// ============================================================================

#[test]
fn tc_t1_f16_1_rar5_hp_size_modulo_14_heuristic() {
    // Multi-volume RAR5 archive with nominal volume size and tail = 14 mod 16
    let volume_sizes = vec![50_000_000, 50_000_000, 50_000_000, 24_500_014];
    let mode = Rar5Header::detect_encryption(&volume_sizes);
    assert_eq!(mode, EncryptionMode::HeaderEncryptedHp);
    assert!(mode.is_encrypted());
    assert!(mode.is_candidate());
}

#[test]
fn tc_t1_f16_2_rar5_hp_size_modulo_10_heuristic() {
    // Multi-volume RAR5 archive with long member filename (tail = 10 mod 16)
    let volume_sizes = vec![100_000_000, 100_000_000, 48_000_010];
    let mode = Rar5Header::detect_encryption(&volume_sizes);
    assert_eq!(mode, EncryptionMode::HeaderEncryptedHp);
    assert!(mode.is_encrypted());
    assert!(mode.is_candidate());
}

#[test]
fn tc_t1_f16_3_rar5_single_volume_candidate() {
    // Single volume with size = 14 mod 16 or 10 mod 16
    let single_14 = vec![15_000_014];
    assert_eq!(Rar5Header::detect_encryption(&single_14), EncryptionMode::CandidateHp);

    let single_10 = vec![15_000_010];
    assert_eq!(Rar5Header::detect_encryption(&single_10), EncryptionMode::CandidateHp);
}

#[test]
fn tc_t1_f16_4_rar5_tail_unencrypted_mod16_none() {
    // Volume tail size not congruent to 10 or 14 mod 16
    let sizes_0 = vec![50_000_000, 50_000_000, 24_500_000];
    assert_eq!(Rar5Header::detect_encryption(&sizes_0), EncryptionMode::None);

    let sizes_5 = vec![50_000_000, 50_000_000, 24_500_005];
    assert_eq!(Rar5Header::detect_encryption(&sizes_5), EncryptionMode::None);

    let single_unencrypted = vec![15_000_000];
    assert_eq!(Rar5Header::detect_encryption(&single_unencrypted), EncryptionMode::None);
}

#[test]
fn tc_t1_f16_5_rar5_indeterminate_incomplete_archive() {
    // All volumes equal nominal size (missing final tail volume)
    let incomplete = vec![50_000_000, 50_000_000, 50_000_000];
    assert_eq!(Rar5Header::detect_encryption(&incomplete), EncryptionMode::IndeterminateIncomplete);
    assert!(!Rar5Header::detect_encryption(&incomplete).is_encrypted());
}

#[test]
fn tc_t1_f16_6_article1_block4_confirmation_real_fixture() {
    // Real RAR5 archive with -hp header encryption
    let hp_bytes = include_bytes!("fixtures/rar5/rar5-hpsw.rar");
    assert!(Rar5Header::verify_article1(hp_bytes));
    assert!(Rar5Header::inspect_article1_block4(hp_bytes));
}

#[test]
fn tc_t1_f16_7_article1_block1_false_positive_elimination() {
    // Real unencrypted RAR5 archive (Block type 1, main header)
    let psw_bytes = include_bytes!("fixtures/rar5/rar5-psw.rar");
    // rar5-psw has member encryption (-p), not header encryption (-hp), so block 4 is absent
    assert!(!Rar5Header::verify_article1(psw_bytes));
    assert!(!Rar5Header::inspect_article1_block4(psw_bytes));

    // Garbage / non-RAR5 bytes
    assert!(!Rar5Header::verify_article1(b"random non-rar bytes"));
    assert!(!Rar5Header::inspect_article1_block4(b""));
}

// ============================================================================
// Feature 17: Fast Single-PBKDF2 Password Verification & Anti-DoS Hard Cap
// ============================================================================

#[test]
fn tc_t1_f17_1_fast_single_pbkdf2_verification() {
    let data = include_bytes!("fixtures/rar5/rar5-psw.rar");
    let l = atlas::archive::rar::list(data);
    let pc = l.password_check.expect("PasswordCheck record should exist in rar5-psw.rar");

    // Verify correct password "password" in fixture
    let start = Instant::now();
    let ok = pc.verify("password");
    let elapsed = start.elapsed();

    assert!(ok, "password 'password' must verify successfully");
    let max_ms = if cfg!(debug_assertions) { 1000 } else { 50 };
    assert!(
        elapsed.as_millis() < max_ms,
        "single PBKDF2 must complete rapidly (took {} ms, limit {} ms)",
        elapsed.as_millis(),
        max_ms
    );

    // Verify incorrect password fails
    assert!(!pc.verify("wrong_password"));
    assert!(!pc.verify("admin123"));
}

#[test]
fn tc_t1_f17_2_async_spawn_blocking_pbkdf2_latency() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let data = include_bytes!("fixtures/rar5/rar5-psw.rar");
        let l = atlas::archive::rar::list(data);
        let pc = l.password_check.expect("PasswordCheck record must exist");

        let ok = pc.verify_async("password").await;
        assert!(ok);

        let fail = pc.verify_async("wrongpassword").await;
        assert!(!fail);
    });
}

#[test]
fn tc_t1_f17_3_anti_dos_hard_cap_kdf_count_rejection() {
    // Adversarial archive specifying kdf_count = 17 (2^17 + 32 = 131,104 iterations > 65,536 limit)
    let dos_check: Rar5PswCheck = PasswordCheck::new(17, [7u8; 16], [1u8; 8]);
    assert_eq!(
        dos_check.accepts("candidate_password"),
        None,
        "kdf > 16 must be rejected without performing derivation"
    );
    assert!(!dos_check.verify("candidate_password"), "verify must return false for rejected DoS archive");

    // KDF count 24 (2^24 rounds = ~16.7M iterations) must also be instantly rejected
    let extreme_dos = PasswordCheck::new(24, [0u8; 16], [0u8; 8]);
    assert_eq!(extreme_dos.accepts("any_password"), None);
    assert!(!extreme_dos.verify("any_password"));
}

// ============================================================================
// Feature 18: Password Harvesting Channels
// ============================================================================

#[test]
fn tc_t1_f18_1_harvest_subject_tags() {
    let subject1 = "[01/50] - \"My.Video.Release.2024\" {{my_secret_pw_1}} - 4.5 GB yEnc";
    let extracted = PasswordHarvester::extract_candidates(subject1, None, &HashMap::new());
    assert_eq!(extracted, vec!["my_secret_pw_1"]);

    let subject2 = "Release Title [pw: second_pass] pass=third_pass";
    let extracted2 = PasswordHarvester::extract_candidates(subject2, None, &HashMap::new());
    assert!(extracted2.contains(&"second_pass".to_string()));
    assert!(extracted2.contains(&"third_pass".to_string()));
}

#[test]
fn tc_t1_f18_2_harvest_nfo_document() {
    let nfo =
        "Release Notes\n----------------\nPassword: secret_nfo_password\nPwd = backup_pwd\nPasswort: german_pwd\n";
    let extracted = PasswordHarvester::extract_candidates("", Some(nfo), &HashMap::new());
    assert!(extracted.contains(&"secret_nfo_password".to_string()));
    assert!(extracted.contains(&"backup_pwd".to_string()));
    assert!(extracted.contains(&"german_pwd".to_string()));
}

#[test]
fn tc_t1_f18_3_harvest_nzb_metadata() {
    let mut meta = HashMap::new();
    meta.insert("password".to_string(), "nzb_direct_pwd".to_string());
    meta.insert("X-DNZB-Password".to_string(), "dnzb_pwd".to_string());
    meta.insert("meta:password".to_string(), "meta_pwd".to_string());

    let extracted = PasswordHarvester::extract_candidates("", None, &meta);
    assert!(extracted.contains(&"nzb_direct_pwd".to_string()));
    assert!(extracted.contains(&"dnzb_pwd".to_string()));
    assert!(extracted.contains(&"meta_pwd".to_string()));
}

#[test]
fn tc_t1_f18_4_negative_phrase_filtering() {
    let subject = "Movie [pw: none] {{no password}} pass=null";
    let nfo = "Password: N/A\npwd: false\nPasswort: none\n";
    let mut meta = HashMap::new();
    meta.insert("password".to_string(), "No Password Needed".to_string());

    let extracted = PasswordHarvester::extract_candidates(subject, Some(nfo), &meta);
    assert!(
        extracted.is_empty(),
        "Negative phrases like 'none', 'no password', 'null', 'n/a' must be filtered out: {:?}",
        extracted
    );
}

#[test]
fn tc_t1_f18_5_deduplication_and_cap_top_5() {
    let subject = "{{pw1}} {{pw2}} {{pw3}} [pw: pw1] pass: pw4";
    let nfo = "password: pw5\npwd: pw6\npass: pw7\n";
    let mut meta = HashMap::new();
    meta.insert("password".to_string(), "pw2".to_string());

    let extracted = PasswordHarvester::extract_candidates(subject, Some(nfo), &meta);
    assert_eq!(extracted.len(), 5, "candidates must be capped at 5");
    // Verify uniqueness
    let mut set = std::collections::HashSet::new();
    for c in &extracted {
        assert!(set.insert(c.clone()), "candidate '{c}' was duplicated");
    }
}

// ============================================================================
// TC-T3-03: Password Identification & Attack Resistance
// ============================================================================

#[test]
fn tc_t3_03_integrated_password_attack_resistance() {
    // Simulate adversarial archive with high KDF exponent 20
    let dos = PasswordCheck::new(20, [1u8; 16], [2u8; 8]);
    let start = Instant::now();
    assert!(!dos.verify("candidate"));
    assert!(start.elapsed().as_millis() < 5, "DoS rejection must be instant (< 5 ms)");

    // Heuristic with mixed empty/negative inputs
    assert_eq!(Rar5Header::detect_encryption(&[]), EncryptionMode::None);
    assert_eq!(Rar5Header::detect_encryption(&[0]), EncryptionMode::None);
}
