//! Comprehensive Empirical Adversarial Stress Test Suite for Milestone 3:
//! Discovery & Merging Engine
//!
//! Stress tests:
//! 1. GF(2) linear algebra: arbitrary chunk boundaries, large buffers (> 50 MB), empty buffers.
//! 2. PAR2 IFSC unzeroing: exact matches with raw slice bytes across block boundaries.
//! 3. Sequential file tiling: Batman Knightfall cross-group joins across 50 groups under realistic gap distributions.
//! 4. Veto-Aware DSU: Layer A multi-file anti-fragmentation and domain vetoes; Layer B cryptographic vetoes and Twin/Alt partitioning.
//! 5. Server-assigned ID decoders: Highwinds base64 LE, Giganews XOR-0x9D, Diablo, INN radix32 fuzzing and panic-safety.

use atlas::discovery::{HeaderMap, PostingTool, ServerClock, ToolType};
use atlas::gf2::{crc32, crc32_combine, crc32_unzero, fold_yenc_parts};
use atlas::merging::{
    CandidateFileRun, InterReleaseDeduplication, InterReleaseVeto,
    IntraReleaseGrouping, IntraReleaseVeto, MergeResult,
    ReleaseAssembler, ReleaseCandidate, ReleasePartition, VetoDsu,
};

// ============================================================================
// 1. GF(2) Linear Algebra Stress Tests
// ============================================================================

#[test]
fn test_gf2_combine_identity_empty_buffers() {
    let empty_crc = crc32(b"");
    assert_eq!(empty_crc, 0, "CRC32 of empty buffer must be 0");

    // combine(0, 0, 0) == 0
    assert_eq!(crc32_combine(0, 0, 0), 0);

    let sample_a = b"Atlas Usenet Indexer empirical stress test payload";
    let crc_a = crc32(sample_a);

    // Empty suffix: combine(crc_a, 0, 0) == crc_a
    assert_eq!(crc32_combine(crc_a, 0, 0), crc_a);

    // Empty prefix: combine(0, crc_a, len_a) == crc_a
    assert_eq!(crc32_combine(0, crc_a, sample_a.len()), crc_a);

    // Concatenation with empty slices
    let full_with_empty_suffix = [sample_a.as_slice(), b""].concat();
    assert_eq!(crc32(&full_with_empty_suffix), crc_a);
    assert_eq!(crc32_combine(crc_a, empty_crc, 0), crc_a);

    let full_with_empty_prefix = [b"", sample_a.as_slice()].concat();
    assert_eq!(crc32(&full_with_empty_prefix), crc_a);
    assert_eq!(crc32_combine(empty_crc, crc_a, sample_a.len()), crc_a);
}

#[test]
fn test_gf2_combine_identity_arbitrary_chunk_boundaries() {
    // Generate a deterministic buffer of prime length 65,537 bytes
    const LEN: usize = 65_537;
    let mut data = Vec::with_capacity(LEN);
    for i in 0..LEN {
        data.push((i.wrapping_mul(31) ^ (i >> 3)) as u8);
    }
    let full_crc = crc32(&data);

    // Test across boundary cuts spanning powers of 2, primes, and edge cases
    let cuts = [
        0, 1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256,
        511, 512, 1023, 1024, 2048, 4095, 4096, 8191, 8192, 16384, 32768,
        65535, 65536, 65537,
    ];

    for &cut in &cuts {
        let (left, right) = data.split_at(cut);
        let c1 = crc32(left);
        let c2 = crc32(right);
        let combined = crc32_combine(c1, c2, right.len());
        assert_eq!(
            combined, full_crc,
            "GF(2) combine failed at cut offset {cut} (left len={}, right len={})",
            left.len(),
            right.len()
        );
    }
}

#[test]
fn test_gf2_combine_multi_chunk_folding_100_parts() {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut rng_state = 0x12345678u64;

    for i in 0..100 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let part_len = ((rng_state >> 33) % 2000 + 1) as usize;
        let mut p = Vec::with_capacity(part_len);
        for j in 0..part_len {
            p.push(((i + j) ^ (rng_state as usize)) as u8);
        }
        parts.push(p);
    }

    let full_data: Vec<u8> = parts.concat();
    let expected_full_crc = crc32(&full_data);

    let yenc_parts: Vec<(u32, usize)> = parts.iter().map(|p| (crc32(p), p.len())).collect();
    let folded = fold_yenc_parts(&yenc_parts).expect("folding must succeed");

    assert_eq!(
        folded, expected_full_crc,
        "Folded 100 yEnc parts must exactly match whole-file CRC32"
    );
}

#[test]
fn test_gf2_combine_large_buffer_55mb() {
    // Large buffer test (> 50 MB): allocate 52 MB buffer
    const SIZE: usize = 52 * 1024 * 1024; // 54,525,952 bytes
    let mut buffer = vec![0u8; SIZE];

    // Populate deterministically
    for (i, b) in buffer.iter_mut().enumerate() {
        *b = ((i ^ (i >> 8) ^ (i >> 16)) & 0xFF) as u8;
    }

    let expected_crc = crc32(&buffer);

    // Split into 32 MB and 20 MB chunks
    let split_offset = 32 * 1024 * 1024;
    let (c1_bytes, c2_bytes) = buffer.split_at(split_offset);
    let c1 = crc32(c1_bytes);
    let c2 = crc32(c2_bytes);

    let combined = crc32_combine(c1, c2, c2_bytes.len());
    assert_eq!(
        combined, expected_crc,
        "GF(2) combination over 52 MB buffer split at 32 MB must match full CRC32"
    );

    // Also test split at 1 MB + 51 MB
    let (c1_1mb, c2_51mb) = buffer.split_at(1024 * 1024);
    let combined_1mb = crc32_combine(crc32(c1_1mb), crc32(c2_51mb), c2_51mb.len());
    assert_eq!(
        combined_1mb, expected_crc,
        "GF(2) combination over 52 MB buffer split at 1 MB must match full CRC32"
    );
}

// ============================================================================
// 2. PAR2 IFSC Unzeroing Stress Tests
// ============================================================================

#[test]
fn test_par2_ifsc_unzero_raw_slice_identity() {
    let test_slice_sizes = [1, 7, 16, 128, 512, 1024, 4096, 65536, 131072, 262144];
    let test_padding_lens = [0, 1, 2, 7, 15, 64, 255, 1000, 4096, 50000, 131072];

    for &slice_sz in &test_slice_sizes {
        let raw_data: Vec<u8> = (0..slice_sz).map(|i: usize| (i.wrapping_mul(17) & 0xFF) as u8).collect();
        let raw_crc = crc32(&raw_data);

        for &pad_len in &test_padding_lens {
            let mut padded = raw_data.clone();
            padded.resize(slice_sz + pad_len, 0u8);
            let padded_crc = crc32(&padded);

            let unzeroed = crc32_unzero(padded_crc, pad_len);
            assert_eq!(
                unzeroed, raw_crc,
                "PAR2 unzero failed for slice_size={slice_sz}, pad_len={pad_len}"
            );
        }
    }
}

#[test]
fn test_par2_ifsc_unzero_block_boundary_sweeps() {
    const BLOCK_SIZE: usize = 262_144; // 256 KiB standard PAR2 block
    // Sweep tails from 1 byte to BLOCK_SIZE
    let tails = [1, 2, 13, 256, 1024, 65536, 200000, BLOCK_SIZE - 1, BLOCK_SIZE];

    for &tail_len in &tails {
        let file_size = BLOCK_SIZE * 2 + tail_len;
        let mut full_file = vec![0u8; file_size];
        for (i, b) in full_file.iter_mut().enumerate() {
            *b = ((i * 37 + 11) & 0xFF) as u8;
        }

        let expected_whole_file_crc = crc32(&full_file);

        // Slice 1: full 256 KB
        let s1 = &full_file[..BLOCK_SIZE];
        let crc1 = crc32(s1);

        // Slice 2: full 256 KB
        let s2 = &full_file[BLOCK_SIZE..BLOCK_SIZE * 2];
        let crc2 = crc32(s2);

        // Slice 3: tail_len bytes, zero padded to BLOCK_SIZE
        let s3_raw = &full_file[BLOCK_SIZE * 2..];
        let mut s3_padded = s3_raw.to_vec();
        let pad_len = BLOCK_SIZE - tail_len;
        s3_padded.resize(BLOCK_SIZE, 0u8);
        let s3_padded_crc = crc32(&s3_padded);

        // Unzero slice 3
        let s3_unzeroed = crc32_unzero(s3_padded_crc, pad_len);
        assert_eq!(s3_unzeroed, crc32(s3_raw));

        // Fold slices
        let fold12 = crc32_combine(crc1, crc2, BLOCK_SIZE);
        let fold123 = crc32_combine(fold12, s3_unzeroed, tail_len);

        assert_eq!(
            fold123, expected_whole_file_crc,
            "IFSC folded whole-file CRC must match for tail_len={tail_len}, pad_len={pad_len}"
        );
    }
}

// ============================================================================
// 3. Sequential File Tiling Stress Tests (Batman Knightfall)
// ============================================================================

#[test]
fn test_batman_knightfall_50_groups_realistic_gap_distribution() {
    // 50 files posted across 50 separate newsgroups
    const NUM_FILES: usize = 50;
    let mut runs = Vec::with_capacity(NUM_FILES);

    let mut current_start = 1000.0;
    let file_duration = 10.0; // 10 seconds upload per file

    // Realistic gaps within [-0.68, +2.0]s
    let realistic_gaps: [f64; 49] = [
        -0.50, 0.12, 0.05, 0.80, -0.20, 0.15, 0.30, 1.20, -0.67, 0.10,
        0.02, 0.45, 1.90, -0.10, 0.08, 0.12, 0.22, 0.95, -0.35, 0.18,
        0.04, 0.60, 1.80, -0.05, 0.14, 0.11, 0.33, 1.10, -0.40, 0.16,
        0.06, 0.70, 1.99, -0.15, 0.09, 0.13, 0.28, 1.05, -0.30, 0.17,
        0.03, 0.55, 1.85, -0.25, 0.11, 0.15, 0.40, 2.00, -0.12,
    ];

    #[allow(clippy::needless_range_loop)]
    for i in 0..NUM_FILES {
        let t_start = current_start;
        let t_end = t_start + file_duration;

        runs.push(CandidateFileRun {
            filename: format!("knightfall.part{:02}.rar", i + 1),
            group: format!("alt.binaries.group{:02}", i),
            poster: format!("uploader_{:02}@usenet", i),
            size: 50_000_000,
            yenc_total: Some(NUM_FILES as i64),
            yenc_part_size: Some(750_000),
            file_index: Some((i + 1) as i64),
            rar_volume_idx: Some((i + 1) as u32),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start,
            t_end,
            file_crc: Some(0x10000000 + i as u32),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: true,
        });

        if i < 49 {
            let gap = realistic_gaps[i];
            current_start = t_end + gap;
        }
    }

    // Pass runs intentionally out of order to verify chronological sorting
    let mut shuffled = runs.clone();
    shuffled.reverse();

    let merged = ReleaseAssembler::join_runs(&shuffled, None);

    assert_eq!(
        merged.len(),
        1,
        "Batman Knightfall 50-file cross-group upload must join into exactly 1 release"
    );
    assert_eq!(merged[0].runs.len(), 50);
    assert_eq!(merged[0].total_size, 50 * 50_000_000);
}

#[test]
fn test_batman_knightfall_gap_boundary_rejection() {
    let run1 = CandidateFileRun {
        filename: "run1.rar".into(),
        group: "group.a".into(),
        poster: "anon".into(),
        size: 50_000_000,
        yenc_total: Some(2),
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: Some("rar5".into()),
        member_id: None,
        byte_offset: 0,
        t_start: 100.0,
        t_end: 110.0,
        file_crc: Some(0x1),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    // Upper boundary test: +2.001s gap -> must reject join
    let mut run_too_late = run1.clone();
    run_too_late.file_index = Some(2);
    run_too_late.rar_volume_idx = Some(2);
    run_too_late.t_start = 112.001;
    run_too_late.t_end = 122.001;

    let merged_late = ReleaseAssembler::join_runs(&[run1.clone(), run_too_late], None);
    assert_eq!(
        merged_late.len(),
        2,
        "Gap of +2.001s exceeds window and must produce 2 distinct releases"
    );

    // Lower boundary test: default lower bound is -0.68s. Gap of -0.681s -> must reject join
    let mut run_too_early = run1.clone();
    run_too_early.file_index = Some(2);
    run_too_early.rar_volume_idx = Some(2);
    run_too_early.t_start = 109.319; // 109.319 - 110.0 = -0.681s
    run_too_early.t_end = 119.319;

    let merged_early = ReleaseAssembler::join_runs(&[run1, run_too_early], None);
    assert_eq!(
        merged_early.len(),
        2,
        "Gap of -0.681s falls below window and must produce 2 distinct releases"
    );
}

// ============================================================================
// 4. Veto-Aware DSU Architecture Stress Tests
// ============================================================================

#[test]
fn test_intra_release_dsu_multi_file_anti_fragmentation() {
    // 10 sibling files in a legitimate release:
    // Files 1..8: 50 parts, 750 KB part size, distinct CRCs
    // File 9 (tail): 13 parts, 750 KB part size, distinct CRC
    // File 10 (.nfo): 1 part, 15 KB, distinct CRC
    let mut files = Vec::new();
    for i in 1..=8 {
        files.push(CandidateFileRun {
            filename: format!("movie.part{:02}.rar", i),
            group: "alt.binaries.movies".into(),
            poster: "release_bot".into(),
            size: 50_000_000,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(i),
            rar_volume_idx: Some(i as u32),
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: i as f64 * 10.0,
            t_end: i as f64 * 10.0 + 9.0,
            file_crc: Some(0xAAAA0000 + i as u32),
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: false,
        });
    }

    // Tail volume
    files.push(CandidateFileRun {
        filename: "movie.part09.rar".into(),
        group: "alt.binaries.movies".into(),
        poster: "release_bot".into(),
        size: 9_500_000,
        yenc_total: Some(13), // Differing part total!
        yenc_part_size: Some(750_000),
        file_index: Some(9),
        rar_volume_idx: Some(9),
        archive_format: Some("rar5".into()),
        member_id: None,
        byte_offset: 0,
        t_start: 90.0,
        t_end: 92.0,
        file_crc: Some(0xBBBB0009), // Distinct CRC!
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    });

    // NFO file
    files.push(CandidateFileRun {
        filename: "movie.nfo".into(),
        group: "alt.binaries.movies".into(),
        poster: "release_bot".into(),
        size: 15_000,
        yenc_total: Some(1), // 1 part!
        yenc_part_size: Some(15_000),
        file_index: Some(10),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 92.1,
        t_end: 92.2,
        file_crc: Some(0xCCCC0010),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    });

    // Pairwise check: NO false vetoes between sibling files!
    for i in 0..files.len() {
        for j in i + 1..files.len() {
            let res = IntraReleaseGrouping::can_group(&files[i], &files[j]);
            assert!(
                res.is_ok(),
                "Layer A false veto between {} and {}: {:?}",
                files[i].filename,
                files[j].filename,
                res.err()
            );
        }
    }

    // DSU union must merge all 10 into a single component
    let mut dsu = VetoDsu::new(files);
    for i in 1..10 {
        let res = dsu.union(0, i, IntraReleaseGrouping::can_group);
        assert_eq!(res, MergeResult::Merged);
    }

    let root0 = dsu.find(0);
    for i in 1..10 {
        assert_eq!(dsu.find(i), root0, "File {i} must belong to release root");
    }
}

#[test]
fn test_intra_release_dsu_domain_vetoes() {
    let base = CandidateFileRun {
        filename: "file1.rar".into(),
        group: "alt.binaries".into(),
        poster: "anon".into(),
        size: 50_000_000,
        yenc_total: Some(50),
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: Some("rar5".into()),
        member_id: Some("member_1".into()),
        byte_offset: 0,
        t_start: 0.0,
        t_end: 10.0,
        file_crc: Some(0x1),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    // Veto 1: Duplicate volume index
    let mut dup_vol = base.clone();
    dup_vol.member_id = None;
    dup_vol.filename = "file1_duplicate.rar".into();
    assert_eq!(
        IntraReleaseGrouping::can_group(&base, &dup_vol),
        Err(IntraReleaseVeto::DuplicateVolumeIndex)
    );

    // Veto 2: Overlapping byte offsets for same member ID
    let mut overlap = base.clone();
    overlap.rar_volume_idx = Some(2); // different volume
    overlap.byte_offset = 25_000_000; // overlaps [0, 50M]
    assert_eq!(
        IntraReleaseGrouping::can_group(&base, &overlap),
        Err(IntraReleaseVeto::OverlappingByteOffsets)
    );

    // Contiguous byte offsets for same member ID: OK
    let mut non_overlap = base.clone();
    non_overlap.rar_volume_idx = Some(2);
    non_overlap.byte_offset = 50_000_000; // contiguous, no overlap
    assert!(IntraReleaseGrouping::can_group(&base, &non_overlap).is_ok());

    // Veto 3: Incompatible container format
    let mut bad_container = base.clone();
    bad_container.rar_volume_idx = Some(2);
    bad_container.member_id = None;
    bad_container.archive_format = Some("7z".into());
    assert_eq!(
        IntraReleaseGrouping::can_group(&base, &bad_container),
        Err(IntraReleaseVeto::IncompatibleContainer)
    );

    // Veto 4: Part size mismatch for same member ID
    let mut part_size_mismatch = base.clone();
    part_size_mismatch.rar_volume_idx = Some(2);
    part_size_mismatch.byte_offset = 50_000_000;
    part_size_mismatch.yenc_part_size = Some(716_800);
    assert_eq!(
        IntraReleaseGrouping::can_group(&base, &part_size_mismatch),
        Err(IntraReleaseVeto::PartSizeMismatch)
    );

    // Veto 5: Discrepant yEnc total for the SAME file index
    let mut discrepant_total = base.clone();
    discrepant_total.rar_volume_idx = None;
    discrepant_total.member_id = None;
    discrepant_total.yenc_total = Some(49);
    assert_eq!(
        IntraReleaseGrouping::can_group(&base, &discrepant_total),
        Err(IntraReleaseVeto::DiscrepantYencTotalForSameFile)
    );
}

#[test]
fn test_inter_release_dsu_cryptographic_vetoes_and_partitioning() {
    let rel_alpha = ReleaseCandidate {
        name: "Movie.1080p.Release".into(),
        full_md5: Some([0xAA; 16]),
        size: 1_000_000_000,
        unpacked_crc32: Some(0x12345678),
        yenc_total: Some(1000),
        creation_salt: Some(vec![1, 2, 3, 4]),
        segment_uid: Some([0x01; 16]),
        lcm_matched_intervals: 0,
        yenc_part_size: Some(750_000),
    };

    // Conflicting full MD5
    let mut rel_diff_md5 = rel_alpha.clone();
    rel_diff_md5.full_md5 = Some([0xBB; 16]);
    assert_eq!(
        InterReleaseDeduplication::can_deduplicate_as_twin(&rel_alpha, &rel_diff_md5),
        Err(InterReleaseVeto::ConflictingFullMd5)
    );

    // Conflicting unpacked CRC32
    let mut rel_diff_crc = rel_alpha.clone();
    rel_diff_crc.unpacked_crc32 = Some(0x87654321);
    assert_eq!(
        InterReleaseDeduplication::can_deduplicate_as_twin(&rel_alpha, &rel_diff_crc),
        Err(InterReleaseVeto::DifferingUnpackedCrc32)
    );

    // Conflicting yEnc total without LCM match
    let mut rel_diff_yenc = rel_alpha.clone();
    rel_diff_yenc.yenc_total = Some(950);
    rel_diff_yenc.yenc_part_size = Some(750_000); // same part size but different total
    assert_eq!(
        InterReleaseDeduplication::can_deduplicate_as_twin(&rel_alpha, &rel_diff_yenc),
        Err(InterReleaseVeto::ConflictingYencPartsNonLcm)
    );

    // Conflicting creation salt
    let mut rel_diff_salt = rel_alpha.clone();
    rel_diff_salt.creation_salt = Some(vec![9, 9, 9, 9]);
    assert_eq!(
        InterReleaseDeduplication::can_deduplicate_as_twin(&rel_alpha, &rel_diff_salt),
        Err(InterReleaseVeto::ConflictingCreationSalt)
    );

    // Partitioning: Twin
    let rel_twin = rel_alpha.clone();
    assert_eq!(
        InterReleaseDeduplication::partition_releases(&rel_alpha, &rel_twin),
        ReleasePartition::Twin
    );

    // Partitioning: Alt (differing container MD5, but identical unpacked CRC32)
    let mut rel_alt_crc = rel_alpha.clone();
    rel_alt_crc.full_md5 = Some([0xCC; 16]);
    rel_alt_crc.creation_salt = Some(vec![5, 6, 7, 8]);
    assert_eq!(
        InterReleaseDeduplication::partition_releases(&rel_alpha, &rel_alt_crc),
        ReleasePartition::Alt
    );

    // Partitioning: Alt (differing container MD5, but identical SegmentUID)
    let mut rel_alt_segment = rel_alpha.clone();
    rel_alt_segment.full_md5 = Some([0xDD; 16]);
    rel_alt_segment.unpacked_crc32 = Some(0x99999999);
    rel_alt_segment.creation_salt = Some(vec![7, 7, 7, 7]);
    assert_eq!(
        InterReleaseDeduplication::partition_releases(&rel_alpha, &rel_alt_segment),
        ReleasePartition::Alt
    );

    // Partitioning: Distinct (everything differs)
    let mut rel_distinct = rel_alpha.clone();
    rel_distinct.full_md5 = Some([0xEE; 16]);
    rel_distinct.unpacked_crc32 = Some(0x55555555);
    rel_distinct.segment_uid = Some([0x99; 16]);
    rel_distinct.creation_salt = Some(vec![8, 8, 8, 8]);
    assert_eq!(
        InterReleaseDeduplication::partition_releases(&rel_alpha, &rel_distinct),
        ReleasePartition::Distinct
    );
}

// ============================================================================
// 5. Server-Assigned ID Decoders Fuzzing & Panic Safety
// ============================================================================

#[test]
fn test_server_clock_fuzz_highwinds() {
    let adversarial_mids = [
        "",
        "@",
        "<@>",
        "<@fx01.iad>",
        "<.123$456@fx01.iad>",
        "<!!!.123$456@fx01.iad>",
        "<QUJDRA.abc$def@fx01.iad>",
        "<ZZZZZZZZZZZZ.123$456@fx01.iad>",
        "<++++.123$456@fx01.iad>",
        "<////.123$456@fx01.iad>",
        "<____.123$456@fx01.iad>",
        "<%%%%.123$456@fx01.iad>",
        "<0.0$0.0@fx99.site>",
        "<QUJDRA@fx01.iad>",
        "<$@fx01.iad>",
        "<..$$..@fx01.iad>",
    ];

    for mid in adversarial_mids {
        // Must never panic!
        let _ = ServerClock::decode(mid);
        let local = mid.trim().trim_start_matches('<').trim_end_matches('>').split_once('@').map(|(l, _)| l).unwrap_or("");
        let _ = ServerClock::decode_highwinds(local);
    }
}

#[test]
fn test_server_clock_fuzz_giganews() {
    let adversarial_mids = [
        "",
        "@giganews.com",
        "<@giganews.com>",
        "<short@giganews.com>",
        "<dGVzdHRlc3R0ZXN0dGVzdHRlc3R0ZXN0dGVz@giganews.com>", // 36 chars
        "<AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@giganews.com>",       // 31 chars
        "<AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@giganews.com>",      // 32 valid chars
        "<!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!@giganews.com>",      // 32 invalid chars
        "<================================@giganews.com>",
        "<                                @giganews.com>",
    ];

    for mid in adversarial_mids {
        // Must never panic!
        let _ = ServerClock::decode(mid);
        let local = mid.trim().trim_start_matches('<').trim_end_matches('>').split_once('@').map(|(l, _)| l).unwrap_or("");
        let _ = ServerClock::decode_giganews(local);
    }
}

#[test]
fn test_server_clock_fuzz_diablo() {
    let adversarial_mids = [
        "",
        "<@cluster>",
        "<$$$@cluster>",
        "<nothex$1$2$3@cluster>",
        "<66f01a2b$@cluster>",
        "<66f01a2b$$$@cluster>",
        "<ffffffff$1$2$3@cluster>", // Timestamp too large
        "<00000000$1$2$3@cluster>", // Timestamp too small (< 900M)
        "<1234567890abcdef$1$2$3@cluster>",
    ];

    for mid in adversarial_mids {
        let _ = ServerClock::decode(mid);
        let local = mid.trim().trim_start_matches('<').trim_end_matches('>').split_once('@').map(|(l, _)| l).unwrap_or("");
        let _ = ServerClock::decode_diablo(local);
    }
}

#[test]
fn test_server_clock_fuzz_inn() {
    let adversarial_mids = [
        "",
        "<@host>",
        "<$$$@host>",
        "<notradix$1$1@host>",
        "<2vbo80$$$@host>",
        "<ZZZZZZZZ$1$1@host>", // out of range
        "<0$0$0@host>",        // epoch base too small
    ];

    for mid in adversarial_mids {
        let _ = ServerClock::decode(mid);
        let local = mid.trim().trim_start_matches('<').trim_end_matches('>').split_once('@').map(|(l, _)| l).unwrap_or("");
        let _ = ServerClock::decode_inn(local);
    }
}

#[test]
fn test_posting_tool_fuzz_adversarial_grammars() {
    let long_str = "a".repeat(5000);
    let adversarial_inputs = [
        "",
        "@",
        "<>",
        "<@>",
        "not_an_email",
        "missing_brackets@domain.com",
        "<missing_domain@>",
        "<@missing_local>",
        "<multiple@@domains>",
        &format!("<{}@domain>", long_str),
        "<invalid_utf8_\u{FFFD}@domain>",
        "<nyuu_fake-notadigit@random.org>",
        "<e4b7c1a2-8d9e-1a6f-b2c3-d4e5f6a7b8c9@random.org>", // UUID v1, not v4, not ngpost domain
        "<gps-nanos-notanumber@domain>",
    ];

    let headers = HeaderMap::new();
    for mid in adversarial_inputs {
        let info = PostingTool::classify(mid, &headers);
        assert_ne!(
            info.tool,
            ToolType::Nyuu,
            "Invalid Nyuu syntax should not falsely classify for {mid}"
        );
    }
}

// ============================================================================
// 6. End-to-End PAR2 IFSC Inventory Unzeroing Integration
// ============================================================================

fn make_par2_packet(set_id: [u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::new();
    pkt.extend_from_slice(b"PAR2\0PKT");
    let len = (64 + body.len()) as u64;
    pkt.extend_from_slice(&len.to_le_bytes());
    pkt.extend_from_slice(&[0u8; 16]); // packet MD5
    pkt.extend_from_slice(&set_id);
    pkt.extend_from_slice(kind);
    pkt.extend_from_slice(body);
    pkt
}

#[test]
fn test_par2_inventory_ifsc_unzero_end_to_end() {
    let set_id = [0x55; 16];
    let file_id = [0x66; 16];
    const BLOCK_SIZE: u64 = 262_144; // 256 KiB
    const TAIL_LEN: usize = 50_000;
    let file_size: u64 = BLOCK_SIZE + TAIL_LEN as u64;

    // Create synthetic raw file data
    let mut raw_file = vec![0u8; file_size as usize];
    for (i, b) in raw_file.iter_mut().enumerate() {
        *b = ((i * 41 + 7) & 0xFF) as u8;
    }
    let expected_crc32 = crc32(&raw_file);

    // Slice 1: full 256 KB
    let s1 = &raw_file[..BLOCK_SIZE as usize];
    let s1_crc = crc32(s1);

    // Slice 2: 50,000 bytes raw, zero padded to 262,144 bytes
    let s2_raw = &raw_file[BLOCK_SIZE as usize..];
    let mut s2_padded = s2_raw.to_vec();
    s2_padded.resize(BLOCK_SIZE as usize, 0u8);
    let s2_padded_crc = crc32(&s2_padded);

    // Build synthetic PAR2 binary
    let mut par2_bytes = Vec::new();

    // 1. Main packet
    let mut main_body = Vec::new();
    main_body.extend_from_slice(&BLOCK_SIZE.to_le_bytes());
    main_body.extend_from_slice(&1u32.to_le_bytes()); // 1 file
    main_body.extend_from_slice(&file_id);
    while main_body.len() % 4 != 0 {
        main_body.push(0);
    }
    par2_bytes.extend(make_par2_packet(set_id, b"PAR 2.0\0Main\0\0\0\0", &main_body));

    // 2. FileDesc packet
    let mut desc_body = Vec::new();
    desc_body.extend_from_slice(&file_id);
    desc_body.extend_from_slice(&[0x11; 16]); // md5_full
    desc_body.extend_from_slice(&[0x22; 16]); // md5_16k
    desc_body.extend_from_slice(&file_size.to_le_bytes());
    desc_body.extend_from_slice(b"release_movie.mkv\0");
    while desc_body.len() % 4 != 0 {
        desc_body.push(0);
    }
    par2_bytes.extend(make_par2_packet(set_id, b"PAR 2.0\0FileDesc", &desc_body));

    // 3. IFSC packet with slice 1 and zero-padded slice 2
    let mut ifsc_body = Vec::new();
    ifsc_body.extend_from_slice(&file_id);
    // Slice 1
    ifsc_body.extend_from_slice(&[0x33; 16]);
    ifsc_body.extend_from_slice(&s1_crc.to_le_bytes());
    // Slice 2 (zero padded CRC in IFSC packet)
    ifsc_body.extend_from_slice(&[0x44; 16]);
    ifsc_body.extend_from_slice(&s2_padded_crc.to_le_bytes());
    while ifsc_body.len() % 4 != 0 {
        ifsc_body.push(0);
    }
    par2_bytes.extend(make_par2_packet(set_id, b"PAR 2.0\0IFSC\0\0\0\0", &ifsc_body));

    // Parse PAR2 inventory
    let inv = atlas::par2::inventory(&par2_bytes);
    assert_eq!(inv.files.len(), 1, "Must find exactly 1 file in PAR2");
    let file = &inv.files[0];
    assert_eq!(file.name, "release_movie.mkv");
    assert_eq!(file.size, file_size);

    // Verify unzeroed folded CRC32
    assert!(file.crc32.is_some(), "file.crc32 must be populated from IFSC slices");
    assert_eq!(
        file.crc32.unwrap(),
        expected_crc32,
        "PAR2 inventory folded CRC32 must match raw file CRC32 after zero-unpadding"
    );
}

// ============================================================================
// 7. Server Message-ID Exact Decoding Known Vectors
// ============================================================================

#[test]
fn test_server_clock_exact_decoding_known_vectors() {
    use base64::Engine;

    // 1. Highwinds: verify epoch offset and custom base64 LE decoding
    let mut val = 10_000_000u64;
    let hw_alphabet = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_%";
    let mut le_chars = String::new();
    while val > 0 {
        let sym = (val % 64) as usize;
        le_chars.push(hw_alphabet.chars().nth(sym).unwrap());
        val /= 64;
    }
    let hw_mid = format!("<{}.123$456.789@fx01.iad>", le_chars);
    let hw_dt = ServerClock::decode(&hw_mid).expect("Highwinds LE base64 must decode");
    assert_eq!(hw_dt.timestamp(), 876_645_500 + 10_000_000);

    let hw_doc_dt = ServerClock::decode("<EaPmB.546176$JP7.228032@fx09.am4>")
        .expect("Documented Highwinds vector must decode");
    assert_eq!(hw_doc_dt.timestamp(), 1_503_379_236);

    // 2. Giganews: verify XOR-0x9D unpacked base64
    let target_ts = 1729000000u32;
    let mut giga_raw = [0u8; 24];
    giga_raw[0..4].copy_from_slice(&12345u32.to_le_bytes()); // PID
    giga_raw[4..8].copy_from_slice(&1u32.to_le_bytes());     // seq
    giga_raw[8..12].copy_from_slice(&target_ts.to_le_bytes()); // timestamp w2
    giga_raw[12..16].copy_from_slice(&99u32.to_le_bytes());
    giga_raw[16..20].copy_from_slice(&42u32.to_le_bytes()); // box ID
    giga_raw[20..24].copy_from_slice(&0u32.to_le_bytes());

    let mut giga_masked = [0u8; 24];
    for (i, &b) in giga_raw.iter().enumerate() {
        giga_masked[i] = b ^ 0x9D;
    }
    let giga_b64 = base64::engine::general_purpose::STANDARD.encode(giga_masked);
    let giga_mid = format!("<{}@giganews.com>", giga_b64);
    let giga_dt = ServerClock::decode(&giga_mid).expect("Giganews must decode");
    assert_eq!(giga_dt.timestamp(), target_ts as i64);

    // 3. Diablo: verify hex timestamp
    let diablo_ts = 1727011371i64; // 0x66f01a2b
    let diablo_mid = format!("<{:x}$0001$1234$a1b2c3d4@usenetserver.com>", diablo_ts);
    let diablo_dt = ServerClock::decode(&diablo_mid).expect("Diablo must decode");
    assert_eq!(diablo_dt.timestamp(), diablo_ts);

    // 4. INN: verify radix32 epoch offset
    let inn_offset = 200_000_000u64;
    // 200,000,000 in radix32:
    let mut inn_val = inn_offset;
    let r32_alphabet = "0123456789abcdefghijklmnopqrstuv";
    let mut r32_str = String::new();
    while inn_val > 0 {
        let sym = (inn_val % 32) as usize;
        r32_str.insert(0, r32_alphabet.chars().nth(sym).unwrap());
        inn_val /= 32;
    }
    let inn_mid = format!("<{}$123$1@news.inn.org>", r32_str);
    let inn_dt = ServerClock::decode(&inn_mid).expect("INN must decode");
    assert_eq!(inn_dt.timestamp(), 673_416_000 + inn_offset as i64);
}

