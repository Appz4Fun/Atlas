//! Integration tests for Milestone 3: Release Merging, Sequential File Tiling & Linear Algebra
//! Covering Features 11, 12, 13, 14, 15 (TC-T1-F11 through TC-T1-F15).

use atlas::gf2::{crc32, crc32_combine, crc32_unzero, fold_yenc_parts};
use atlas::merging::{
    ArticleHead, CandidateFileRun, Deobfuscator, InterReleaseDeduplication, IntraReleaseGrouping, IntraReleaseVeto,
    JoinOptions, MergeResult, Par2Bridge, ReleaseAssembler, ReleaseCandidate, ReleasePartition, VetoDsu,
};
use md5::Digest;

// ============================================================================
// PAR2 Synthetic Packet Helpers
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

fn make_main_packet(set_id: [u8; 16], block_size: u64, file_ids: &[[u8; 16]]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&block_size.to_le_bytes());
    body.extend_from_slice(&(file_ids.len() as u32).to_le_bytes());
    for id in file_ids {
        body.extend_from_slice(id);
    }
    while body.len() % 4 != 0 {
        body.push(0);
    }
    make_par2_packet(set_id, b"PAR 2.0\0Main\0\0\0\0", &body)
}

fn make_file_desc_packet(
    set_id: [u8; 16],
    file_id: [u8; 16],
    full_md5: [u8; 16],
    md5_16k: [u8; 16],
    size: u64,
    name: &str,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&file_id);
    body.extend_from_slice(&full_md5);
    body.extend_from_slice(&md5_16k);
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(name.as_bytes());
    body.push(0); // NUL terminator
    while body.len() % 4 != 0 {
        body.push(0);
    }
    make_par2_packet(set_id, b"PAR 2.0\0FileDesc", &body)
}

fn make_ifsc_packet(set_id: [u8; 16], file_id: [u8; 16], slices: &[([u8; 16], u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&file_id);
    for (md5, crc) in slices {
        body.extend_from_slice(md5);
        body.extend_from_slice(&crc.to_le_bytes());
    }
    while body.len() % 4 != 0 {
        body.push(0);
    }
    make_par2_packet(set_id, b"PAR 2.0\0IFSC\0\0\0\0", &body)
}

// ============================================================================
// Feature 11: Sequential File Tiling & Join
// ============================================================================

#[test]
fn tc_t1_f11_1_multi_file_end_to_start_chain_join() {
    let run1 = CandidateFileRun {
        filename: "file1.rar".into(),
        group: "alt.binaries.movies".into(),
        poster: "anon@news".into(),
        size: 50_000_000,
        yenc_total: Some(10),
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: Some("rar5".into()),
        member_id: None,
        byte_offset: 0,
        t_start: 90.0,
        t_end: 100.0,
        file_crc: Some(0x11111111),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let run2 = CandidateFileRun {
        filename: "file2.rar".into(),
        group: "alt.binaries.test".into(), // different newsgroup!
        poster: "anon@news".into(),
        size: 50_000_000,
        yenc_total: Some(10),
        yenc_part_size: Some(750_000),
        file_index: Some(2),
        rar_volume_idx: Some(2),
        archive_format: Some("rar5".into()),
        member_id: None,
        byte_offset: 0,
        t_start: 100.15, // Gap: +0.15s (within <= 2.0s)
        t_end: 110.0,
        file_crc: Some(0x22222222),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let joined = ReleaseAssembler::join_runs(&[run1, run2], None);
    assert_eq!(joined.len(), 1, "File 1 and File 2 must join across groups into 1 release");
    assert_eq!(joined[0].runs.len(), 2);
}

#[test]
fn tc_t1_f11_2_50_file_batman_knightfall_reassembly() {
    // 50 files posted sequentially across 50 separate simulated newsgroups,
    // with randomized poster names, obfuscated subject lines, and median gap 0.12s.
    let mut runs = Vec::new();
    let mut current_t = 1000.0;

    for i in 1..=50 {
        let t_start = current_t;
        let t_end = t_start + 1.5;
        current_t = t_end + 0.12; // gap = +0.12s

        runs.push(CandidateFileRun {
            filename: format!("obf_{:04x}.tmp", i * 17),
            group: format!("alt.binaries.group_{}", i),
            poster: format!("poster_{:08x}@usenet", i * 31),
            size: 50_000_000,
            yenc_total: Some(10),
            yenc_part_size: Some(750_000),
            file_index: Some(i),
            rar_volume_idx: Some(i as u32),
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
    }

    let joined = ReleaseAssembler::join_runs(&runs, None);
    assert_eq!(joined.len(), 1, "Batman Knightfall: all 50 sprayed files must reassemble into 1 unified release");
    assert_eq!(joined[0].runs.len(), 50);
    assert_eq!(joined[0].total_size, 50 * 50_000_000);
}

#[test]
fn tc_t1_f11_3_window_boundary_acceptance() {
    let run1 = CandidateFileRun {
        filename: "a.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 10_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 10.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let run2 = CandidateFileRun {
        filename: "b.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 10_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(2),
        rar_volume_idx: Some(2),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 10.50, // Gap: exactly +0.50s
        t_end: 20.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let joined = ReleaseAssembler::join_runs(&[run1, run2], None);
    assert_eq!(joined.len(), 1, "Gap at +0.50s must be accepted");
}

#[test]
fn tc_t1_f11_4_window_boundary_rejection() {
    let run1 = CandidateFileRun {
        filename: "a.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 10_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 10.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let run2 = CandidateFileRun {
        filename: "b.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 10_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(2),
        rar_volume_idx: Some(2),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 12.05, // Gap: +2.05s (exceeds +2.00s upper limit)
        t_end: 22.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let joined = ReleaseAssembler::join_runs(&[run1, run2], None);
    assert_eq!(joined.len(), 2, "Gap at +2.05s must be rejected into 2 distinct releases");
}

#[test]
fn tc_t1_f11_5_width_to_rate_dynamic_window_scaling() {
    // Large file transfer with w / r = 1.0 s
    // Lower bound: -1.2 * (1.0) - 0.5 = -1.7 s
    let options = JoinOptions {
        avg_width: 1_000_000.0,
        posting_rate: 1_000_000.0,
        max_gap_seconds: 2.0,
        lower_scale: -1.2,
        lower_offset: -0.5,
    };

    let run1 = CandidateFileRun {
        filename: "a.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 50.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    // Candidate file starting within -1.6s of run1's completion (t_start = 48.4s)
    let run2 = CandidateFileRun {
        filename: "b.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(2),
        rar_volume_idx: Some(2),
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 48.4, // Delta t = 48.4 - 50.0 = -1.6s (>= -1.7s lower bound)
        t_end: 98.4,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let joined = ReleaseAssembler::join_runs(&[run1, run2], Some(&options));
    assert_eq!(joined.len(), 1, "Pipelined candidate at -1.6s must be joined under dynamic window");
}

// ============================================================================
// Feature 12: GF(2) CRC32 Linear Algebra
// ============================================================================

#[test]
fn tc_t1_f12_1_standard_two_part_crc32_combination() {
    let mut chunk_a = vec![0u8; 500_000];
    let mut chunk_b = vec![0u8; 200_000];
    for (i, b) in chunk_a.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    for (i, b) in chunk_b.iter_mut().enumerate() {
        *b = ((i + 13) % 241) as u8;
    }

    let crc_a = crc32(&chunk_a);
    let crc_b = crc32(&chunk_b);

    let combined = crc32_combine(crc_a, crc_b, chunk_b.len());

    let mut full = chunk_a;
    full.extend_from_slice(&chunk_b);
    let expected = crc32(&full);

    assert_eq!(combined, expected, "crc32_combine must exactly match byte-level concatenation");
}

#[test]
fn tc_t1_f12_2_multi_part_yenc_pcrc32_folding() {
    let data: Vec<u8> = (0..500_000).map(|i| (i * 37 % 256) as u8).collect();
    let part_size = 50_000;
    let parts: Vec<(u32, usize)> = data.chunks(part_size).map(|chunk| (crc32(chunk), chunk.len())).collect();

    let folded = fold_yenc_parts(&parts).expect("folding must succeed");
    let expected_whole = crc32(&data);
    assert_eq!(folded, expected_whole, "Folded pcrc32 parts must match whole-file CRC32");
}

#[test]
fn tc_t1_f12_3_par2_ifsc_last_slice_zero_unpadding() {
    let raw_tail: Vec<u8> = (0..200_000).map(|i| (i * 19 % 256) as u8).collect();
    let expected_unpadded_crc = crc32(&raw_tail);

    let mut padded_slice = raw_tail;
    padded_slice.resize(250_000, 0); // zero padded with 50_000 zeros
    let padded_crc = crc32(&padded_slice);

    let unpadded = crc32_unzero(padded_crc, 50_000);
    assert_eq!(unpadded, expected_unpadded_crc, "crc32_unzero must reverse zero-padding");
}

#[test]
fn tc_t1_f12_4_galois_field_polynomial_shifting_invariance() {
    let initial_crc = 0x12345678u32;
    let l1 = 123_456usize;
    let l2 = 654_321usize;

    // Shift in two steps: C * x^(8*l1) * x^(8*l2)
    let step1 = crc32_combine(initial_crc, 0, l1);
    let two_step = crc32_combine(step1, 0, l2);

    // Shift in one combined step: C * x^(8*(l1 + l2))
    let one_step = crc32_combine(initial_crc, 0, l1 + l2);

    assert_eq!(two_step, one_step, "Polynomial shift over GF(2) must be associative");
}

#[test]
fn tc_t1_f12_5_whole_file_cross_verification() {
    let data: Vec<u8> = (0..1_000_000).map(|i| (i * 101 % 256) as u8).collect();
    let whole_crc = crc32(&data);

    // Stream 1: yEnc articles of 716,800 B
    let yenc_slice = 716_800;
    let yenc_parts: Vec<(u32, usize)> = data.chunks(yenc_slice).map(|c| (crc32(c), c.len())).collect();
    let yenc_folded = fold_yenc_parts(&yenc_parts).unwrap();

    // Stream 2: PAR2 slices of 393,216 B
    let par2_slice = 393_216;
    let mut par2_slices = Vec::new();
    for chunk in data.chunks(par2_slice) {
        let mut padded = chunk.to_vec();
        let pad_len = par2_slice.saturating_sub(chunk.len());
        padded.resize(par2_slice, 0);
        par2_slices.push((crc32(&padded), chunk.len(), pad_len));
    }

    // Fold PAR2 IFSC slices with last slice unpadded
    let mut ifsc_folded = 0u32;
    for (i, &(padded_crc, actual_len, pad_len)) in par2_slices.iter().enumerate() {
        let slice_crc =
            if i + 1 == par2_slices.len() && pad_len > 0 { crc32_unzero(padded_crc, pad_len) } else { padded_crc };
        ifsc_folded = crc32_combine(ifsc_folded, slice_crc, actual_len);
    }

    assert_eq!(yenc_folded, whole_crc);
    assert_eq!(ifsc_folded, whole_crc);
    assert_eq!(yenc_folded, ifsc_folded, "yEnc folded CRC must equal PAR2 IFSC folded CRC with 0 bit divergence");
}

#[test]
fn tc_t1_f12_6_par2_ifsc_packet_parsing_and_folding() {
    let set_id = [0x99; 16];
    let file_id = [0x88; 16];
    let data = vec![0xABu8; 450_000]; // 450 KB file
    let block_size = 250_000u64;

    // Slice 0: full 250,000 B
    let s0 = &data[..250_000];
    let md5_0: [u8; 16] = md5::Md5::digest(s0).into();
    let crc_0 = crc32(s0);

    // Slice 1: 200,000 B data + 50,000 B zero-padding = 250,000 B
    let mut s1_padded = data[250_000..].to_vec();
    s1_padded.resize(250_000, 0);
    let md5_1: [u8; 16] = md5::Md5::digest(&s1_padded).into();
    let crc_1 = crc32(&s1_padded);

    let desc = make_file_desc_packet(set_id, file_id, [0; 16], [0; 16], 450_000, "sample.bin");
    let main = make_main_packet(set_id, block_size, &[file_id]);
    let ifsc = make_ifsc_packet(set_id, file_id, &[(md5_0, crc_0), (md5_1, crc_1)]);

    let mut par2 = desc;
    par2.extend_from_slice(&main);
    par2.extend_from_slice(&ifsc);

    let inv = atlas::par2::inventory(&par2);
    assert_eq!(inv.files.len(), 1);
    let whole_crc = crc32(&data);
    assert_eq!(
        inv.files[0].crc32,
        Some(whole_crc),
        "PAR2 inventory must fold IFSC slices and unpad the final slice to produce whole-file CRC32"
    );
}

// ============================================================================
// Feature 13: PAR2 Bridge & 16 KB MD5
// ============================================================================

#[test]
fn tc_t1_f13_1_sizechain_hub_and_spoke_resolution() {
    let set_id = [0x42; 16];
    let mut par2_data = Vec::new();

    let mut file_ids = Vec::new();
    let mut runs = Vec::new();

    for i in 1..=20 {
        let file_id = [i as u8; 16];
        file_ids.push(file_id);
        let size = 10_000_000 + (i as u64) * 123_456; // 20 distinct sizes!
        let full_md5 = [i as u8; 16];
        let md5_16k = [i as u8 + 100; 16];
        let canonical_name = format!("Video.Release.part{:02}.rar", i);

        let desc = make_file_desc_packet(set_id, file_id, full_md5, md5_16k, size, &canonical_name);
        par2_data.extend_from_slice(&desc);

        runs.push(CandidateFileRun {
            filename: format!("random_hex_{:08x}.rar", i * 997),
            group: "alt.binaries.test".into(),
            poster: "anon@usenet".into(),
            size,
            yenc_total: Some(10),
            yenc_part_size: Some(750_000),
            file_index: Some(i as i64),
            rar_volume_idx: None,
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: true,
        });
    }

    let main = make_main_packet(set_id, 250_000, &file_ids);
    par2_data.extend_from_slice(&main);

    let res = Par2Bridge::resolve(&par2_data, &mut runs, &[]);
    assert_eq!(res.resolved_count, 20);
    assert!(res.success);

    for (i, run) in runs.iter().enumerate() {
        let expected_name = format!("Video.Release.part{:02}.rar", i + 1);
        assert_eq!(run.filename, expected_name);
        assert!(!run.is_obfuscated);
    }
}

#[test]
fn tc_t1_f13_2_16kb_md5_tie_breaker_on_equal_sizes() {
    let set_id = [0x55; 16];
    let mut par2_data = Vec::new();
    let mut file_ids = Vec::new();
    let mut runs = Vec::new();
    let mut heads = Vec::new();

    const EQUAL_SIZE: u64 = 50_000_000;

    for i in 1..=10 {
        let file_id = [i as u8; 16];
        file_ids.push(file_id);
        let full_md5 = [i as u8; 16];

        // Unique first 16 KiB payload per volume
        let head_payload = vec![i as u8; 20_000];
        let md5_16k: [u8; 16] = md5::Md5::digest(&head_payload[..16384]).into();
        let canonical_name = format!("Release.part{:02}.rar", i);

        let desc = make_file_desc_packet(set_id, file_id, full_md5, md5_16k, EQUAL_SIZE, &canonical_name);
        par2_data.extend_from_slice(&desc);

        runs.push(CandidateFileRun {
            filename: format!("obf_{:04}.bin", i),
            group: "alt.binaries.test".into(),
            poster: "anon@usenet".into(),
            size: EQUAL_SIZE,
            yenc_total: Some(50),
            yenc_part_size: Some(750_000),
            file_index: Some(i as i64),
            rar_volume_idx: None,
            archive_format: Some("rar5".into()),
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: true,
        });

        heads.push(ArticleHead { file_index: (i - 1) as usize, payload: head_payload });
    }

    let main = make_main_packet(set_id, 250_000, &file_ids);
    par2_data.extend_from_slice(&main);

    let res = Par2Bridge::resolve(&par2_data, &mut runs, &heads);
    assert_eq!(res.resolved_count, 10);
    assert!(res.success);

    for (i, run) in runs.iter().enumerate() {
        assert_eq!(run.filename, format!("Release.part{:02}.rar", i + 1));
        assert!(!run.is_obfuscated);
    }
}

#[test]
fn tc_t1_f13_3_single_par2_body_fetch_efficiency() {
    let set_id = [0x77; 16];
    let mut par2_data = Vec::new();
    let mut file_ids = Vec::new();
    let mut runs = Vec::new();

    // 100-volume obfuscated upload
    for i in 1..=100 {
        let file_id = [(i % 256) as u8; 16];
        file_ids.push(file_id);
        let size = 10_000_000 + (i as u64) * 10_000;
        let desc =
            make_file_desc_packet(set_id, file_id, [i as u8; 16], [i as u8; 16], size, &format!("Vol.{:03}.rar", i));
        par2_data.extend_from_slice(&desc);

        runs.push(CandidateFileRun {
            filename: format!("hash_{}.rar", i),
            group: "g".into(),
            poster: "p".into(),
            size,
            yenc_total: None,
            yenc_part_size: None,
            file_index: Some(i as i64),
            rar_volume_idx: None,
            archive_format: None,
            member_id: None,
            byte_offset: 0,
            t_start: 0.0,
            t_end: 1.0,
            file_crc: None,
            md5_16k: None,
            full_md5: None,
            unpacked_crc32: None,
            creation_salt: None,
            is_obfuscated: true,
        });
    }

    let main = make_main_packet(set_id, 500_000, &file_ids);
    par2_data.extend_from_slice(&main);

    // All 100 volumes resolved from 1 single PAR2 data slice
    let res = Par2Bridge::resolve(&par2_data, &mut runs, &[]);
    assert_eq!(res.resolved_count, 100);
}

#[test]
fn tc_t1_f13_4_filedesc_md5_full_verification() {
    let set_id = [1; 16];
    let file_id = [2; 16];
    let full_md5 = [0xEE; 16];
    let desc = make_file_desc_packet(set_id, file_id, full_md5, [0; 16], 12345, "test.mkv");
    let main = make_main_packet(set_id, 1000, &[file_id]);

    let mut par2 = desc;
    par2.extend_from_slice(&main);

    let mut runs = vec![CandidateFileRun {
        filename: "obf.bin".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 12345,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 1.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: true,
    }];

    Par2Bridge::resolve(&par2, &mut runs, &[]);
    assert_eq!(runs[0].full_md5, Some(full_md5));
}

#[test]
fn tc_t1_f13_5_missing_par2_fallback() {
    let mut runs = vec![CandidateFileRun {
        filename: "corrupt.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 1.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: true,
    }];

    // Corrupted / empty par2 data must not panic and return 0 resolved
    let res = Par2Bridge::resolve(&[0u8; 32], &mut runs, &[]);
    assert_eq!(res.resolved_count, 0);
    assert!(!res.success);
}

// ============================================================================
// Feature 14: Real-Name Deobfuscation
// ============================================================================

#[test]
fn tc_t1_f14_1_postie_filedesc_canonical_name_extraction() {
    let set_id = [3; 16];
    let file_id = [4; 16];
    let real_name = "Ubuntu.24.04.iso";
    let desc = make_file_desc_packet(set_id, file_id, [0; 16], [0; 16], 1_000_000, real_name);
    let names = Deobfuscator::resolve_names(&desc);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].canonical_name, "Ubuntu.24.04.iso");
    assert!(names[0].is_clean);
}

#[test]
fn tc_t1_f14_2_newsup_rename_par2_packet_parsing() {
    let set_id = [5; 16];
    let file_id = [6; 16];
    let scene_name = "Debian.12.Bookworm.img";
    let desc = make_file_desc_packet(set_id, file_id, [0; 16], [0; 16], 2_000_000, scene_name);
    let names = Deobfuscator::resolve_names(&desc);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].canonical_name, scene_name);
}

#[test]
fn tc_t1_f14_3_relative_path_sanitization() {
    let dirty_unix = "../../subdir/release.mkv";
    let dirty_win = r"..\..\windows\subdir\movie.mp4";

    assert_eq!(Deobfuscator::sanitize_name(dirty_unix), "release.mkv");
    assert_eq!(Deobfuscator::sanitize_name(dirty_win), "movie.mp4");
}

#[test]
fn tc_t1_f14_4_looks_random_detection() {
    assert!(Deobfuscator::looks_random("a1b2c3d4e5f60718293a4b5c6d7e8f90.rar"));
    assert!(Deobfuscator::looks_random("9876543210abcdef.bin"));
    assert!(!Deobfuscator::looks_random("Linux.Kernel.v6.10.tar.gz"));
    assert!(!Deobfuscator::looks_random("Movie.Title.2024.1080p.mkv"));
}

#[test]
fn tc_t1_f14_5_obfuscated_flag_clearing() {
    let set_id = [7; 16];
    let file_id = [8; 16];
    let desc = make_file_desc_packet(set_id, file_id, [1; 16], [2; 16], 5000, "Clean.Movie.mkv");
    let main = make_main_packet(set_id, 1000, &[file_id]);

    let mut par2 = desc;
    par2.extend_from_slice(&main);

    let mut runs = vec![CandidateFileRun {
        filename: "hash12345.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 5000,
        yenc_total: None,
        yenc_part_size: None,
        file_index: Some(1),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 1.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: true,
    }];

    assert!(runs[0].is_obfuscated);
    Par2Bridge::resolve(&par2, &mut runs, &[]);
    assert!(!runs[0].is_obfuscated);
    assert_eq!(runs[0].filename, "Clean.Movie.mkv");
}

// ============================================================================
// Feature 15: Veto-Aware DSU & Graph
// ============================================================================

#[test]
fn tc_t1_f15_1_valid_release_equivalence_class_merge() {
    let run1 = CandidateFileRun {
        filename: "part01.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: Some(50),
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: Some(1),
        archive_format: Some("rar5".into()),
        member_id: None,
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

    let run2 = CandidateFileRun {
        filename: "part02.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: Some(50),
        yenc_part_size: Some(750_000),
        file_index: Some(2),
        rar_volume_idx: Some(2),
        archive_format: Some("rar5".into()),
        member_id: None,
        byte_offset: 0,
        t_start: 10.1,
        t_end: 20.0,
        file_crc: Some(0x2),
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let mut dsu = VetoDsu::new(vec![run1, run2]);
    let res = dsu.union(0, 1, IntraReleaseGrouping::can_group);
    assert_eq!(res, MergeResult::Merged);
    assert_eq!(dsu.find(0), dsu.find(1));
}

#[test]
fn tc_t1_f15_2_domain_veto_on_mismatched_part_size() {
    let run1 = CandidateFileRun {
        filename: "chunk1.bin".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: None,
        yenc_part_size: Some(768_000),
        file_index: None,
        rar_volume_idx: None,
        archive_format: None,
        member_id: Some("file_alpha".into()),
        byte_offset: 0,
        t_start: 0.0,
        t_end: 1.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let run2 = CandidateFileRun {
        filename: "chunk2.bin".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 50_000_000,
        yenc_total: None,
        yenc_part_size: Some(716_800), // Mismatched nominal part size for same member!
        file_index: None,
        rar_volume_idx: None,
        archive_format: None,
        member_id: Some("file_alpha".into()),
        byte_offset: 50_000_000,
        t_start: 1.1,
        t_end: 2.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let mut dsu = VetoDsu::new(vec![run1, run2]);
    let res = dsu.union(0, 1, IntraReleaseGrouping::can_group);
    assert_eq!(res, MergeResult::Vetoed(IntraReleaseVeto::PartSizeMismatch));
    assert_ne!(dsu.find(0), dsu.find(1));
}

#[test]
fn tc_t1_f15_3_domain_veto_on_yenc_total_discrepancy() {
    let run1 = CandidateFileRun {
        filename: "file.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 20_000_000,
        yenc_total: Some(20),
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 0.0,
        t_end: 1.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let run2 = CandidateFileRun {
        filename: "file.rar".into(),
        group: "g".into(),
        poster: "p".into(),
        size: 20_000_000,
        yenc_total: Some(25), // Discrepancy for same file index 1!
        yenc_part_size: Some(750_000),
        file_index: Some(1),
        rar_volume_idx: None,
        archive_format: None,
        member_id: None,
        byte_offset: 0,
        t_start: 1.1,
        t_end: 2.0,
        file_crc: None,
        md5_16k: None,
        full_md5: None,
        unpacked_crc32: None,
        creation_salt: None,
        is_obfuscated: false,
    };

    let mut dsu = VetoDsu::new(vec![run1, run2]);
    let res = dsu.union(0, 1, IntraReleaseGrouping::can_group);
    assert_eq!(res, MergeResult::Vetoed(IntraReleaseVeto::DiscrepantYencTotalForSameFile));
}

#[test]
fn tc_t1_f15_4_twin_release_graph_partitioning() {
    let rel1 = ReleaseCandidate {
        name: "Movie.1080p".into(),
        full_md5: Some([0xAA; 16]),
        size: 2_000_000_000,
        unpacked_crc32: Some(0xDEADBEEF),
        yenc_total: Some(2000),
        creation_salt: Some(vec![1, 2, 3]),
        segment_uid: Some([1; 16]),
        lcm_matched_intervals: 0,
        yenc_part_size: Some(750_000),
    };

    let rel2 = ReleaseCandidate {
        name: "Movie.1080p.DuplicatePost".into(),
        full_md5: Some([0xAA; 16]),
        size: 2_000_000_000,
        unpacked_crc32: Some(0xDEADBEEF),
        yenc_total: Some(2000),
        creation_salt: Some(vec![1, 2, 3]),
        segment_uid: Some([1; 16]),
        lcm_matched_intervals: 0,
        yenc_part_size: Some(750_000),
    };

    assert_eq!(InterReleaseDeduplication::partition_releases(&rel1, &rel2), ReleasePartition::Twin);
}

#[test]
fn tc_t1_f15_5_alt_release_graph_partitioning() {
    let rel1 = ReleaseCandidate {
        name: "Movie.1080p.Original".into(),
        full_md5: Some([0xAA; 16]),
        size: 2_000_000_000,
        unpacked_crc32: Some(0xCAFEBABE),
        yenc_total: Some(2000),
        creation_salt: Some(vec![1]),
        segment_uid: Some([5; 16]),
        lcm_matched_intervals: 0,
        yenc_part_size: Some(750_000),
    };

    let rel2 = ReleaseCandidate {
        name: "Movie.1080p.AlternativeEncode".into(),
        full_md5: Some([0xBB; 16]), // Differing container hash!
        size: 1_900_000_000,
        unpacked_crc32: Some(0xCAFEBABE), // Identical inner media CRC!
        yenc_total: Some(1900),
        creation_salt: Some(vec![2]),
        segment_uid: Some([5; 16]), // Identical MKV SegmentUID!
        lcm_matched_intervals: 0,
        yenc_part_size: Some(750_000),
    };

    assert_eq!(InterReleaseDeduplication::partition_releases(&rel1, &rel2), ReleasePartition::Alt);
}
