//! Comprehensive Adversarial Stress Test Suite for Milestone 4:
//! Password Identification and Archive Encryption
//!
//! Stress tests:
//! 1. PBKDF2 anti-DoS iteration capping (MAX_KDF = 16) and Tokio async worker thread starvation.
//! 2. RAR5 -hp zero-download detection: tail sizes S_last in {10, 14} mod 16, nominal volume equality,
//!    and Article 1 Block Type 4 inspection eliminating false positives.
//! 3. PasswordHarvester negative phrase filtering against false positive strings
//!    ("no password", "password protected", "see inside nfo", etc.) and top-5 frequency cap.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use atlas::archive::{EncryptionMode, PasswordCheck, PasswordHarvester, Rar5Header};

// ============================================================================
// 1. PBKDF2 Anti-DoS Iteration Capping & Tokio Reactor Non-Starvation
// ============================================================================

#[test]
fn test_adversarial_kdf_capping_extremes() {
    // MAX_KDF is 16 (2^16 + 32 = 65,568 iterations).
    // Test boundary: kdf = 16 must succeed.
    let kdf16 = PasswordCheck::new(16, [1u8; 16], [0u8; 8]);
    let start = Instant::now();
    let res16 = kdf16.accepts("test_password");
    let elapsed16 = start.elapsed();
    assert!(res16.is_some(), "kdf = 16 must be accepted for evaluation");
    assert_eq!(res16, Some(false));
    println!("kdf=16 evaluation took: {:?}", elapsed16);

    // Test rejection: kdf = 17 (131,104 iterations)
    let kdf17 = PasswordCheck::new(17, [1u8; 16], [0u8; 8]);
    let start17 = Instant::now();
    assert_eq!(kdf17.accepts("test_password"), None);
    assert!(!kdf17.verify("test_password"));
    assert!(start17.elapsed() < Duration::from_millis(1), "kdf=17 must be rejected in < 1ms");

    // Test rejection: kdf = 20 (1,048,608 iterations)
    let kdf20 = PasswordCheck::new(20, [2u8; 16], [0u8; 8]);
    assert_eq!(kdf20.accepts("test_password"), None);
    assert!(!kdf20.verify("test_password"));

    // Test rejection: kdf = 24 (WinRAR theoretical max: 16,777,248 iterations)
    let kdf24 = PasswordCheck::new(24, [3u8; 16], [0u8; 8]);
    let start24 = Instant::now();
    assert_eq!(kdf24.accepts("test_password"), None);
    assert!(!kdf24.verify("test_password"));
    assert!(start24.elapsed() < Duration::from_millis(1), "kdf=24 must be rejected in < 1ms without hanging");

    // Test extreme boundary: kdf = 255 (potential integer overflow in 1 << kdf)
    let kdf255 = PasswordCheck::new(255, [4u8; 16], [0u8; 8]);
    let start255 = Instant::now();
    assert_eq!(kdf255.accepts("test_password"), None);
    assert!(!kdf255.verify("test_password"));
    assert!(start255.elapsed() < Duration::from_millis(1), "kdf=255 must be rejected without panic/overflow");
}

#[test]
fn test_adversarial_tokio_thread_starvation_prevention() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build current_thread runtime");

    rt.block_on(async {
        let tick_counter = Arc::new(AtomicUsize::new(0));
        let tick_counter_clone = tick_counter.clone();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Spawn a high-frequency ticker task on the async reactor (1ms ticks)
        let ticker_handle = tokio::spawn(async move {
            while running_clone.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
                tick_counter_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        // Yield to let the ticker start
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let initial_ticks = tick_counter.load(Ordering::SeqCst);
        assert!(initial_ticks > 0, "Ticker must have ticked initially");

        // Now run verify_async with kdf = 16 (substantial CPU work, ~30-50ms)
        let pc = PasswordCheck::new(16, [42u8; 16], [99u8; 8]);
        let start = Instant::now();
        let res = pc.verify_async("test_candidate_password").await;
        let elapsed = start.elapsed();
        assert!(!res);

        let final_ticks = tick_counter.load(Ordering::SeqCst);
        let ticks_during_verify = final_ticks - initial_ticks;

        // Stop the ticker
        running.store(false, Ordering::SeqCst);
        let _ = ticker_handle.await;

        println!(
            "verify_async took {:?}, ticks during verify: {}",
            elapsed, ticks_during_verify
        );

        // If verify_async starved the async thread, ticks_during_verify would be 0 or 1.
        // Because it offloaded to spawn_blocking, the current_thread runtime continued ticking.
        assert!(
            ticks_during_verify >= 5,
            "Tokio async thread was starved! Only {} ticks occurred during {:?} PBKDF2 calculation",
            ticks_during_verify,
            elapsed
        );
    });
}

// ============================================================================
// 2. RAR5 -hp Zero-Download Detection & Article 1 Verification
// ============================================================================

#[test]
fn test_adversarial_rar5_hp_modulo_heuristics() {
    let nominal = 50_000_000i64;

    // Tail congruent to 14 mod 16 (e.g. 25_000_014 = 1562500 * 16 + 14)
    assert_eq!(
        Rar5Header::detect_encryption(&[nominal, nominal, nominal, 25_000_014]),
        EncryptionMode::HeaderEncryptedHp
    );

    // Tail congruent to 10 mod 16 (e.g. 25_000_010 = 1562500 * 16 + 10)
    assert_eq!(
        Rar5Header::detect_encryption(&[nominal, nominal, nominal, 25_000_010]),
        EncryptionMode::HeaderEncryptedHp
    );

    // All other modulo 16 remainders {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 15}
    // must return EncryptionMode::None
    for r in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 15] {
        let tail = 25_000_000 + r;
        assert_eq!(
            Rar5Header::detect_encryption(&[nominal, nominal, nominal, tail]),
            EncryptionMode::None,
            "Remainder {} mod 16 must NOT be flagged as HeaderEncryptedHp",
            r
        );
    }

    // Incomplete archive: tail volume equals nominal volume
    assert_eq!(
        Rar5Header::detect_encryption(&[nominal, nominal, nominal]),
        EncryptionMode::IndeterminateIncomplete
    );

    // Single volume archive: tail is the only volume
    assert_eq!(
        Rar5Header::detect_encryption(&[25_000_014]),
        EncryptionMode::CandidateHp
    );
    assert_eq!(
        Rar5Header::detect_encryption(&[25_000_010]),
        EncryptionMode::CandidateHp
    );
    assert_eq!(
        Rar5Header::detect_encryption(&[25_000_005]),
        EncryptionMode::None
    );

    // Edge cases: empty, zero, negative
    assert_eq!(Rar5Header::detect_encryption(&[]), EncryptionMode::None);
    assert_eq!(Rar5Header::detect_encryption(&[0, -5, 0]), EncryptionMode::None);
}

#[test]
fn test_adversarial_rar5_nominal_volume_mismatch() {
    // When non-tail volumes differ in size (nominal equality fails),
    // e.g., vol1 = 50MB, vol2 = 40MB, vol3 = 24_500_014.
    // The volume set is irregular/indeterminate, NOT a clean split archive.
    let irregular_sizes = vec![50_000_000, 40_000_000, 24_500_014];
    let mode = Rar5Header::detect_encryption(&irregular_sizes);
    // Observe what current implementation returns:
    println!("Irregular non-tail volumes returned: {:?}", mode);
    // Document whether nominal_match was respected.
    // According to Plan §6.1 Step 2: "Check nominal split equality: V_1 == V_2 == ... == V_{N-1} == V".
    // If nominal split equality fails, it should NOT be flagged as high-confidence HeaderEncryptedHp.
    assert_ne!(
        mode,
        EncryptionMode::HeaderEncryptedHp,
        "Irregular volume sizes with mismatched nominal volumes must NOT be classified as HeaderEncryptedHp"
    );
}

#[test]
fn test_adversarial_article1_block4_inspection() {
    let mark5 = b"Rar!\x1a\x07\x01\x00";

    fn build_rar5_block(block_type: u64, inner_payload: &[u8]) -> Vec<u8> {
        let mut inner = Vec::new();
        let mut val = block_type;
        while val >= 0x80 {
            inner.push((val & 0x7f) as u8 | 0x80);
            val >>= 7;
        }
        inner.push(val as u8);
        inner.push(0x00);
        inner.extend_from_slice(inner_payload);

        let size = inner.len();
        let mut size_bytes = Vec::new();
        let mut s_val = size;
        while s_val >= 0x80 {
            size_bytes.push((s_val & 0x7f) as u8 | 0x80);
            s_val >>= 7;
        }
        size_bytes.push(s_val as u8);

        let mut body = Vec::new();
        body.extend_from_slice(&size_bytes);
        body.extend_from_slice(&inner);

        let crc = atlas::archive::crc32(&body);
        let mut block = Vec::new();
        block.extend_from_slice(&crc.to_le_bytes());
        block.extend_from_slice(&body);
        block
    }

    // 1. Block Type 4 (Encryption Header)
    let block4 = build_rar5_block(4, &[0x00, 0x01, 0x0f]);
    let mut article1_hp = mark5.to_vec();
    article1_hp.extend_from_slice(&block4);
    assert!(
        Rar5Header::verify_article1(&article1_hp),
        "Block Type 4 must confirm -hp encryption"
    );

    // 2. Block Type 1 (Main Archive Header - unencrypted false positive)
    let block1 = build_rar5_block(1, &[0x00, 0x00]);
    let mut article1_unencrypted = mark5.to_vec();
    article1_unencrypted.extend_from_slice(&block1);
    assert!(
        !Rar5Header::verify_article1(&article1_unencrypted),
        "Block Type 1 must eliminate false positive and return false"
    );

    // 3. Block Type 2 (File Header)
    let block2 = build_rar5_block(2, &[0x00, 0x00]);
    let mut article1_file = mark5.to_vec();
    article1_file.extend_from_slice(&block2);
    assert!(
        !Rar5Header::verify_article1(&article1_file),
        "Block Type 2 must return false"
    );

    // 4. Corrupted CRC in header block
    let mut corrupted_crc = article1_hp.clone();
    if corrupted_crc.len() > 10 {
        corrupted_crc[8] ^= 0xff;
    }
    assert!(
        !Rar5Header::verify_article1(&corrupted_crc),
        "Corrupted CRC must be rejected"
    );

    // 5. Truncated inputs
    assert!(!Rar5Header::verify_article1(b""));
    assert!(!Rar5Header::verify_article1(b"Rar!\x1a\x07\x01\x00"));
    assert!(!Rar5Header::verify_article1(&article1_hp[..12]));
}

// ============================================================================
// 3. PasswordHarvester Negative Phrase Filtering & Top-5 Cap
// ============================================================================

#[test]
fn test_adversarial_password_harvester_false_positives() {
    let test_cases = vec![
        "no password",
        "NO PASSWORD",
        "No Password Needed",
        "none",
        "NONE",
        "null",
        "nil",
        "n/a",
        "N/A",
        "false",
        "FALSE",
        "password protected",
        "PASSWORD PROTECTED",
        "see inside nfo",
        "see nfo",
        "check inside nfo",
        "in comments",
    ];

    let mut failing_phrases = Vec::new();
    for phrase in test_cases {
        let nfo = format!("Release Info\nPassword: {}\n", phrase);
        let extracted = PasswordHarvester::extract_candidates("", Some(&nfo), &HashMap::new());
        if !extracted.is_empty() {
            failing_phrases.push((phrase, extracted));
        }
    }

    assert!(
        failing_phrases.is_empty(),
        "The following false positive phrases were NOT filtered out by PasswordHarvester: {:?}",
        failing_phrases
    );
}

#[test]
fn test_adversarial_password_harvester_frequency_cap() {
    // 6 distinct candidate passwords
    // pw_f appears 10 times across NFO and NZB
    // pw_a, pw_b, pw_c, pw_d, pw_e appear only once
    let subject = "{{pw_a}} {{pw_b}} {{pw_c}} {{pw_d}} {{pw_e}}";
    let nfo = "pwd: pw_f\npwd: pw_f\npwd: pw_f\npwd: pw_f\npwd: pw_f\n";
    let mut meta = HashMap::new();
    meta.insert("password".to_string(), "pw_f".to_string());
    meta.insert("X-DNZB-Password".to_string(), "pw_f".to_string());
    meta.insert("meta:password".to_string(), "pw_f".to_string());

    let extracted = PasswordHarvester::extract_candidates(subject, Some(nfo), &meta);
    assert_eq!(extracted.len(), 5, "Must be capped at 5 candidates");

    // The most frequent candidate pw_f MUST be present in top 5!
    assert!(
        extracted.contains(&"pw_f".to_string()),
        "Highest frequency candidate 'pw_f' was dropped due to arrival order: {:?}",
        extracted
    );
}
