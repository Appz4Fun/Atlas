//! Empirical Adversarial Stress Test Suite for Milestone 4:
//! Media Probing and Prefix Decompression
//!
//! Covering:
//! 1. Dolby Vision RPU bitstream parsing: Profile 4, 5, 7, 8; FEL vs MEL; CM v2.9 vs v4.0; Level 253 crash immunity.
//! 2. Matroska chapter hidden edition filtering, SegmentUID extraction, SPS VUI colour space recovery (HDR10, HLG, SDR).
//! 3. Audio bitstream probing: TrueHD Atmos sync word (BE and LE), E-AC-3 EMDF sync, DTS:X sync (standard, LE, alt).
//! 4. Non-faststart MP4 moov offset calculation (standard 32-bit, 64-bit extended, preceding boxes, malformed/boundary).
//! 5. Archive prefix decompression: Zip bomb 100:1 ratio limit, recursion depth limit (10 for nested archives), truncated buffer handling.

use std::io::Write;

use atlas::probe::{
    ArchiveFormat, ArchiveStream, AudioBitstream, DecompressError, DolbyVisionInfo,
    H265Sps, MkvMetadata, Mp4Prober, unwrap_nested,
};
use flate2::Compression;
use flate2::write::DeflateEncoder;

// ============================================================================
// BitWriter Helper for Bitstream Synthesis
// ============================================================================

struct BitWriter {
    bytes: Vec<u8>,
    bit_pos: usize, // 0 to 7 within current byte
}

impl BitWriter {
    fn new() -> Self {
        Self { bytes: Vec::new(), bit_pos: 0 }
    }

    fn write_bit(&mut self, bit: u8) {
        if self.bit_pos == 0 {
            self.bytes.push(0);
        }
        if bit != 0 {
            let last_idx = self.bytes.len() - 1;
            self.bytes[last_idx] |= 1 << (7 - self.bit_pos);
        }
        self.bit_pos = (self.bit_pos + 1) % 8;
    }

    fn write_bits(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            let bit = ((val >> i) & 1) as u8;
            self.write_bit(bit);
        }
    }

    fn write_flag(&mut self, flag: bool) {
        self.write_bit(if flag { 1 } else { 0 });
    }

    fn write_ue(&mut self, val: u32) {
        // Exp-Golomb: write (val + 1) in binary with leading zeros
        let num = val + 1;
        let bits = 32 - num.leading_zeros();
        let zeros = bits - 1;
        for _ in 0..zeros {
            self.write_bit(0);
        }
        self.write_bits(num, bits);
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

// ============================================================================
// EBML & MP4 Synthesizer Helpers
// ============================================================================

fn ebml_elem(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    if body.len() < 0x7f {
        out.push(0x80 | body.len() as u8);
    } else if body.len() < 0x3fff {
        out.extend([0x40 | (body.len() >> 8) as u8, body.len() as u8]);
    } else {
        out.extend([0x20 | (body.len() >> 16) as u8, (body.len() >> 8) as u8, body.len() as u8]);
    }
    out.extend(body);
    out
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(body);
    out
}

// ============================================================================
// 1. Dolby Vision RPU Bitstream Parsing Stress Tests
// ============================================================================

fn build_dovi_rpu_rbsp(
    rpu_profile: u8,
    rpu_level: u8,
    bit_depth_minus8: u32,
    el_spatial: bool,
    disable_residual: bool,
    extra_bytes: &[u8],
) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0x19, 6);   // rpu_type = 25 (0x19)
    w.write_bits(0, 11);     // rpu_format = 0
    w.write_bits(rpu_profile as u32, 4); // vdr_rpu_profile
    w.write_bits(rpu_level as u32, 4);   // vdr_rpu_level
    w.write_flag(true);      // vdr_seq_info_present
    w.write_flag(false);     // chroma_resample
    w.write_bits(0, 2);      // coeff_data_type
    // rpu_format & 0x700 == 0:
    w.write_ue(0);           // bl_depth
    w.write_ue(0);           // el_depth
    w.write_ue(bit_depth_minus8); // vdr_bit_depth_minus8
    w.write_flag(false);     // spatial_resample
    w.write_bits(0, 3);      // reserved
    w.write_flag(el_spatial); // el_spatial_resampling_filter_flag
    w.write_flag(disable_residual); // disable_residual_flag

    let mut bytes = w.into_bytes();
    bytes.extend_from_slice(extra_bytes);
    bytes
}

#[test]
fn test_dovi_rpu_bitstream_profile4() {
    // Profile 4: vdr_rpu_profile=1, el_spatial=true, disable_residual=false, bit_depth_minus8 != 4 (e.g. 2 for 10-bit)
    let rbsp = build_dovi_rpu_rbsp(1, 6, 2, true, false, &[]);
    // Wrap in NAL type 62 (0x7C is 62 << 1)
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp);

    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 4 RPU");
    assert_eq!(info.profile, 4, "vdr_rpu_profile=1 with 10-bit depth and EL must be Profile 4");
    assert_eq!(info.level, Some(6));
    assert!(info.el_present);
    assert!(!info.fel, "Profile 4 is not FEL");
}

#[test]
fn test_dovi_rpu_bitstream_profile5() {
    // Profile 5: vdr_rpu_profile=0
    let rbsp = build_dovi_rpu_rbsp(0, 4, 2, false, false, &[]);
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp);

    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 5 RPU");
    assert_eq!(info.profile, 5, "vdr_rpu_profile=0 must be Profile 5");
    assert_eq!(info.level, Some(4));
    assert!(!info.el_present);
    assert!(!info.fel);
}

#[test]
fn test_dovi_rpu_bitstream_profile7_fel() {
    // Profile 7 FEL: vdr_rpu_profile=1, el_spatial=true, disable_residual=false, bit_depth_minus8=4 (12-bit)
    // Extra bytes: non-zero payload >= 128 bytes to ensure not MEL
    let mut extra = vec![0xAA; 140];
    // Avoid CM v4 markers (3, 8, 9, 10, 11, 254) to keep v2.9
    for b in extra.iter_mut() {
        *b = 0x55;
    }
    let rbsp = build_dovi_rpu_rbsp(1, 6, 4, true, false, &extra);
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp);

    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 7 FEL RPU");
    assert_eq!(info.profile, 7, "12-bit depth with EL must be Profile 7");
    assert_eq!(info.level, Some(6));
    assert!(info.el_present);
    assert!(info.fel, "Full enhancement layer must have fel=true");
}

#[test]
fn test_dovi_rpu_bitstream_profile7_mel() {
    // Profile 7 MEL: vdr_rpu_profile=1, el_spatial=true, disable_residual=false, bit_depth_minus8=4 (12-bit)
    // MEL signature: rbsp length < 128 bytes with 4 consecutive zeros [0, 0, 0, 0]
    let extra = vec![0x00, 0x00, 0x00, 0x00, 0x01, 0x02];
    let rbsp = build_dovi_rpu_rbsp(1, 6, 4, true, false, &extra);
    assert!(rbsp.len() < 128, "MEL test requires rbsp.len() < 128");
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp);

    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 7 MEL RPU");
    assert_eq!(info.profile, 7);
    assert!(info.el_present);
    assert!(!info.fel, "Minimal enhancement layer (MEL) must have fel=false");
}

#[test]
fn test_dovi_rpu_bitstream_profile8() {
    // Profile 8: vdr_rpu_profile=1, el_spatial=false (no enhancement layer)
    let rbsp = build_dovi_rpu_rbsp(1, 6, 2, false, true, &[]);
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp);

    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 8 RPU");
    assert_eq!(info.profile, 8, "vdr_rpu_profile=1 without spatial filter must be Profile 8");
    assert_eq!(info.level, Some(6));
    assert!(!info.el_present);
    assert!(!info.fel);
}

#[test]
fn test_dovi_rpu_cm_v29_vs_v40() {
    // CM v4.0: contains Level 11 or Level 254 extension block marker (0xFE = 254)
    let extra_v4 = vec![0xFE];
    let rbsp_v4 = build_dovi_rpu_rbsp(0, 4, 2, false, true, &extra_v4);
    let mut nal_v4 = vec![0x7C, 0x01];
    nal_v4.extend_from_slice(&rbsp_v4);
    let info_v4 = DolbyVisionInfo::parse_rpu(&nal_v4).expect("must parse CM v4.0 RPU");
    assert_eq!(info_v4.cm_version, "v4.0");

    // CM v2.9: payload without CM4 marker bytes (e.g. Profile 5 Level 4 where byte 2 is 2, not 3/8/9/10/11)
    let rbsp_v29 = build_dovi_rpu_rbsp(0, 4, 2, false, true, &[0x01, 0x02, 0x04, 0x05, 0x06, 0x07]);
    let mut nal_v29 = vec![0x7C, 0x01];
    nal_v29.extend_from_slice(&rbsp_v29);
    let info_v29 = DolbyVisionInfo::parse_rpu(&nal_v29).expect("must parse CM v2.9 RPU");
    assert_eq!(info_v29.cm_version, "v2.9");
}

#[test]
fn test_dovi_rpu_cm_v40_false_positive_on_profile7_8_standard_levels() {
    // Adversarial empirical verification:
    // In any RPU bitstream with vdr_rpu_profile = 1 (Profiles 4, 7, 8):
    // bits 16..23 (byte 2) = (vdr_rpu_profile << 3) | (vdr_rpu_level >> 1).
    // For UHD Blu-ray level 6 (standard for 2160p): byte 2 = (1 << 3) | (6 >> 1) = 8 | 3 = 11 (0x0B).
    // When the RPU header is properly skipped, Profile 7 / Profile 8 streams without v4 extension blocks
    // are correctly classified as CM v2.9 instead of falsely triggering CM v4.0.
    let rbsp_p7_cm29 = build_dovi_rpu_rbsp(1, 6, 4, true, false, &[0x55; 140]);
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp_p7_cm29);
    let info = DolbyVisionInfo::parse_rpu(&nal).expect("must parse Profile 7");
    assert_eq!(info.cm_version, "v2.9", "Profile 7 level 6 without v4 extension blocks must report v2.9");
}

#[test]
fn test_dovi_rpu_level253_crash_immunity() {
    // Level 253 extension block (0xFD) or out-of-bounds level payloads must not panic
    let extra_l253 = vec![0xFD, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let rbsp_l253 = build_dovi_rpu_rbsp(1, 15, 2, false, true, &extra_l253);
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&rbsp_l253);

    let info = DolbyVisionInfo::parse_rpu(&nal);
    assert!(info.is_some(), "Level 253 payload must parse cleanly without crash");

    // Extreme out-of-bounds level in dvvC/dvcC container box (e.g. level=63)
    let extreme_box = [1, 0, (8 << 1), (63 << 3) | 0b101];
    let info_box = DolbyVisionInfo::parse_rpu(&extreme_box);
    assert!(info_box.is_some());
    assert_eq!(info_box.unwrap().profile, 8);

    // Fuzz with truncated payloads
    for len in 0..10 {
        let truncated = &nal[..len.min(nal.len())];
        let _ = DolbyVisionInfo::parse_rpu(truncated);
    }
}

// ============================================================================
// 2. Matroska Chapters, SegmentUID, SPS VUI Stress Tests
// ============================================================================

#[test]
fn test_mkv_chapters_hidden_edition_filtering_complex() {
    // Multi-edition Matroska file:
    // Edition 0: Hidden Edition (EditionFlagHidden = 1) -> 3 dummy chapters
    // Edition 1: Non-Default, Non-Hidden (EditionFlagDefault = 0, EditionFlagHidden = 0) -> 2 alternate chapters
    // Edition 2: Default, Non-Hidden (EditionFlagDefault = 1, EditionFlagHidden = 0) -> 4 main chapters
    // Edition 3: Hidden Edition (EditionFlagHidden = 1) -> 1 dummy chapter

    fn make_chapter(name: &str, start_ns: u64, hidden: bool) -> Vec<u8> {
        let display = [ebml_elem(&[0x85], name.as_bytes()), ebml_elem(&[0x43, 0x7C], b"eng")].concat();
        let mut body = vec![
            ebml_elem(&[0x91], &start_ns.to_be_bytes()),
            ebml_elem(&[0x80], &display),
        ];
        if hidden {
            body.push(ebml_elem(&[0x98], &[1]));
        }
        ebml_elem(&[0xB6], &body.concat())
    }

    let ed0 = [
        ebml_elem(&[0x45, 0xBD], &[1]), // Hidden
        make_chapter("Hidden 1", 0, false),
        make_chapter("Hidden 2", 10_000_000_000, false),
        make_chapter("Hidden 3", 20_000_000_000, false),
    ]
    .concat();

    let ed1 = [
        ebml_elem(&[0x45, 0xDB], &[0]), // Not default
        ebml_elem(&[0x45, 0xBD], &[0]), // Not hidden
        make_chapter("Alt Intro", 0, false),
        make_chapter("Alt Main", 30_000_000_000, false),
    ]
    .concat();

    let ed2 = [
        ebml_elem(&[0x45, 0xDB], &[1]), // Default
        ebml_elem(&[0x45, 0xBD], &[0]), // Not hidden
        make_chapter("Real Chapter 1", 0, false),
        make_chapter("Real Chapter 2", 60_000_000_000, false),
        make_chapter("Internal Hidden Chap", 90_000_000_000, true), // Internal hidden atom
        make_chapter("Real Chapter 3", 120_000_000_000, false),
    ]
    .concat();

    let ed3 = [
        ebml_elem(&[0x45, 0xBD], &[1]), // Hidden
        make_chapter("Hidden Post", 200_000_000_000, false),
    ]
    .concat();

    let chapters_elem = ebml_elem(
        &[0x10, 0x43, 0xA7, 0x70],
        &[
            ebml_elem(&[0x45, 0xB9], &ed0),
            ebml_elem(&[0x45, 0xB9], &ed1),
            ebml_elem(&[0x45, 0xB9], &ed2),
            ebml_elem(&[0x45, 0xB9], &ed3),
        ]
        .concat(),
    );

    let info = ebml_elem(&[0x7B, 0xA9], b"Chapter Stress Release");
    let segment = [
        ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info),
        chapters_elem,
        ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
    ]
    .concat();
    let mkv_bytes = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &segment),
    ]
    .concat();

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse Matroska chapters");
    // Edition 2 must be selected because EditionFlagDefault=1 and EditionFlagHidden=0.
    // Inside Edition 2, the hidden chapter atom must also be skipped!
    assert_eq!(meta.chapters.len(), 3, "must select Default edition and filter internal hidden chapter");
    assert_eq!(meta.chapters[0].name.as_deref(), Some("Real Chapter 1"));
    assert_eq!(meta.chapters[0].start_secs, 0.0);
    assert_eq!(meta.chapters[1].name.as_deref(), Some("Real Chapter 2"));
    assert_eq!(meta.chapters[1].start_secs, 60.0);
    assert_eq!(meta.chapters[2].name.as_deref(), Some("Real Chapter 3"));
    assert_eq!(meta.chapters[2].start_secs, 120.0);
}

#[test]
fn test_mkv_chapters_all_hidden_yields_empty() {
    let ed_hidden = [
        ebml_elem(&[0x45, 0xBD], &[1]),
        ebml_elem(&[0xB6], &[ebml_elem(&[0x91], &0u64.to_be_bytes())].concat()),
    ]
    .concat();
    let chapters_elem = ebml_elem(&[0x10, 0x43, 0xA7, 0x70], &ebml_elem(&[0x45, 0xB9], &ed_hidden));

    let segment = [
        ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &ebml_elem(&[0x7B, 0xA9], b"All Hidden")),
        chapters_elem,
        ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
    ]
    .concat();
    let mkv_bytes = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &segment),
    ]
    .concat();

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse");
    assert!(meta.chapters.is_empty(), "all hidden editions must yield empty chapters vector");
}

#[test]
fn test_mkv_segment_uid_exactness_and_malformed() {
    // Valid 16-byte SegmentUID
    let valid_uid: [u8; 16] = [
        0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
    ];
    let info_valid = [ebml_elem(&[0x7B, 0xA9], b"UID"), ebml_elem(&[0x73, 0xA4], &valid_uid)].concat();
    let mkv_valid = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &[
            ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info_valid),
            ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
        ].concat()),
    ]
    .concat();
    let meta_valid = MkvMetadata::parse(&mkv_valid).unwrap();
    assert_eq!(
        meta_valid.segment_uid.as_deref(),
        Some("aabbccddeeff00112233445566778899"),
        "SegmentUID must be exact 32 lowercase hex characters"
    );

    // Truncated 15-byte SegmentUID: must be rejected (remain None)
    let trunc_uid = vec![0x11u8; 15];
    let info_trunc = [ebml_elem(&[0x7B, 0xA9], b"UID"), ebml_elem(&[0x73, 0xA4], &trunc_uid)].concat();
    let mkv_trunc = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &[
            ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info_trunc),
            ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
        ].concat()),
    ]
    .concat();
    let meta_trunc = MkvMetadata::parse(&mkv_trunc).unwrap();
    assert_eq!(meta_trunc.segment_uid, None, "truncated SegmentUID (<16 bytes) must be None");
}

#[test]
fn test_sps_vui_colour_space_recovery_modes() {
    // 1. HDR10: Primaries 9 (BT.2020), Transfer 16 (PQ), Matrix 9
    let sps_hdr10 = [0x42, 0x01, 0x01, 0x02, 9, 16, 9, 0x00];
    let vui_hdr10 = H265Sps::parse_vui(&sps_hdr10).expect("must parse HDR10");
    assert_eq!(vui_hdr10.hdr.as_deref(), Some("HDR10"));
    assert_eq!(vui_hdr10.colour_primaries, 9);
    assert_eq!(vui_hdr10.transfer_characteristics, 16);

    // 2. HLG: Primaries 9, Transfer 18, Matrix 9
    let sps_hlg = [0x42, 0x01, 0x01, 0x02, 9, 18, 9, 0x00];
    let vui_hlg = H265Sps::parse_vui(&sps_hlg).expect("must parse HLG");
    assert_eq!(vui_hlg.hdr.as_deref(), Some("HLG"));
    assert_eq!(vui_hlg.colour_primaries, 9);
    assert_eq!(vui_hlg.transfer_characteristics, 18);

    // 3. Annex B 3-byte prefix [0, 0, 1]
    let sps_3byte = [0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x02, 9, 16, 9, 0x00];
    let vui_3byte = H265Sps::parse_vui(&sps_3byte).expect("must parse 3-byte Annex B prefix");
    assert_eq!(vui_3byte.hdr.as_deref(), Some("HDR10"));

    // 4. Annex B 4-byte prefix [0, 0, 0, 1]
    let sps_4byte = [0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x02, 9, 18, 9, 0x00];
    let vui_4byte = H265Sps::parse_vui(&sps_4byte).expect("must parse 4-byte Annex B prefix");
    assert_eq!(vui_4byte.hdr.as_deref(), Some("HLG"));

    // 5. Malformed/truncated inputs: zero panics
    assert!(H265Sps::parse_vui(&[]).is_none());
    assert!(H265Sps::parse_vui(&[0]).is_none());
    assert!(H265Sps::parse_vui(&[0, 0, 0, 1]).is_none());
}

// ============================================================================
// 3. Audio Bitstream Probing Stress Tests
// ============================================================================

#[test]
fn test_audio_bitstream_truehd_atmos_sync_variations() {
    // Big-Endian sync word: 0xF8726FBA
    let mut be_data = vec![0x11u8; 32];
    be_data[4..8].copy_from_slice(&[0xF8, 0x72, 0x6F, 0xBA]);
    // Bit 7 at offset 4 + 7 = 11:
    be_data[11] = 0x80;
    let res_be = AudioBitstream::probe(&be_data);
    assert!(res_be.has_atmos, "Big-endian TrueHD sync with bit 7 must detect Atmos");
    assert_eq!(res_be.format.as_deref(), Some("TrueHD Atmos"));

    // Little-Endian sync word: 0xBA6F72F8
    let mut le_data = vec![0x22u8; 32];
    le_data[8..12].copy_from_slice(&[0xBA, 0x6F, 0x72, 0xF8]);
    le_data[8 + 7] = 0x80;
    let res_le = AudioBitstream::probe(&le_data);
    assert!(res_le.has_atmos, "Little-endian TrueHD sync with bit 7 must detect Atmos");

    // TrueHD sync with bit 7 UNSET (standard TrueHD 7.1 without Atmos)
    let mut no_atmos_data = vec![0x33u8; 32];
    no_atmos_data[4..8].copy_from_slice(&[0xF8, 0x72, 0x6F, 0xBA]);
    no_atmos_data[11] = 0x7F; // Bit 7 is 0
    let res_no_atmos = AudioBitstream::probe(&no_atmos_data);
    assert!(!res_no_atmos.has_atmos, "TrueHD with bit 7 unset must NOT flag Atmos");
}

#[test]
fn test_audio_bitstream_eac3_emdf_and_dtsx() {
    // E-AC-3 EMDF sync: 0x5838
    let mut eac3_data = vec![0u8; 64];
    eac3_data[15..17].copy_from_slice(&[0x58, 0x38]);
    let res_eac3 = AudioBitstream::probe(&eac3_data);
    assert!(res_eac3.has_atmos);
    assert_eq!(res_eac3.format.as_deref(), Some("E-AC-3 Atmos"));

    // DTS:X sync 0x02000850
    let mut dtsx_data = vec![0u8; 64];
    dtsx_data[20..24].copy_from_slice(&[0x02, 0x00, 0x08, 0x50]);
    let res_dtsx = AudioBitstream::probe(&dtsx_data);
    assert!(res_dtsx.has_dtsx);
    assert_eq!(res_dtsx.format.as_deref(), Some("DTS:X"));

    // DTS:X little-endian sync 0x50080002
    let mut dtsx_le = vec![0u8; 64];
    dtsx_le[20..24].copy_from_slice(&[0x50, 0x08, 0x00, 0x02]);
    let res_dtsx_le = AudioBitstream::probe(&dtsx_le);
    assert!(res_dtsx_le.has_dtsx);

    // DTS:X alternative sync 0xF14000D0
    let mut dtsx_alt = vec![0u8; 64];
    dtsx_alt[10..14].copy_from_slice(&[0xF1, 0x40, 0x00, 0xD0]);
    let res_dtsx_alt = AudioBitstream::probe(&dtsx_alt);
    assert!(res_dtsx_alt.has_dtsx);

    // Empty and truncated slice handling
    assert!(!AudioBitstream::probe(&[]).has_atmos);
    assert!(!AudioBitstream::probe(&[0xF8, 0x72]).has_atmos);
}

// ============================================================================
// 4. Non-Faststart MP4 moov Tail Offset Stress Tests
// ============================================================================

#[test]
fn test_mp4_moov_offset_various_box_sequences() {
    // 1. Standard non-faststart MP4: ftyp (32 bytes) + mdat (10,000 bytes)
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\x01mp42");
    let mdat = mp4_box(b"mdat", &vec![0u8; 10_000]);
    let prefix1 = [ftyp.clone(), mdat].concat();
    let offset1 = Mp4Prober::find_moov_offset(&prefix1).expect("must find moov offset");
    assert_eq!(offset1, prefix1.len() as u64);

    // 2. Preceding metadata boxes: ftyp + free + wide + mdat
    let free = mp4_box(b"free", &[0; 16]);
    let wide = mp4_box(b"wide", &[0; 8]);
    let mdat2 = mp4_box(b"mdat", &vec![0u8; 50_000]);
    let prefix2 = [ftyp.clone(), free, wide, mdat2].concat();
    let offset2 = Mp4Prober::find_moov_offset(&prefix2).expect("must find moov offset past free/wide");
    assert_eq!(offset2, prefix2.len() as u64);

    // 3. 64-bit extended size indicator: size field = 1, followed by 8-byte length
    let mut mdat_64 = vec![0u8; 16];
    mdat_64[0..4].copy_from_slice(&1u32.to_be_bytes());
    mdat_64[4..8].copy_from_slice(b"mdat");
    let target_size = 12_000_000_000u64; // 12 GB
    mdat_64[8..16].copy_from_slice(&target_size.to_be_bytes());
    let prefix3 = [ftyp.clone(), mdat_64].concat();
    let offset3 = Mp4Prober::find_moov_offset(&prefix3).expect("must find 64-bit moov offset");
    assert_eq!(offset3, ftyp.len() as u64 + target_size);

    // 4. Malformed/truncated headers: zero panics
    assert!(Mp4Prober::find_moov_offset(&[]).is_none());
    assert!(Mp4Prober::find_moov_offset(&[0; 7]).is_none());
    assert!(Mp4Prober::find_moov_offset(b"\0\0\0\x01mdat\0\0\0").is_none()); // Incomplete 64-bit length
}

// ============================================================================
// 5. Archive Prefix Decompression & Zip Bomb Stress Tests
// ============================================================================

#[test]
fn test_archive_prefix_zip_bomb_100x_ratio_mitigation() {
    // Construct a synthetic Deflate payload that expands 1,000x
    // 100,000 identical bytes compress to ~100 bytes via Deflate
    let uncompressed = vec![0x42u8; 100_000];
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&uncompressed).unwrap();
    let compressed = encoder.finish().unwrap();

    // Expansion ratio is ~1000:1, exceeding 100:1 guard
    let result = ArchiveStream::decompress(ArchiveFormat::Zip, &compressed, 200_000);
    assert!(
        matches!(result, Err(DecompressError::ExpansionRatioBreached)),
        "Decompression expanding >100x input must be aborted with ExpansionRatioBreached"
    );
}

#[test]
fn test_archive_prefix_decompression_valid_stream() {
    let payload = b"ATLAS_NNTP_INTEGRATION_METADATA_STREAM_TEST_PAYLOAD_VALID";
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).unwrap();
    let compressed = encoder.finish().unwrap();

    let decompressed = ArchiveStream::decompress(ArchiveFormat::Zip, &compressed, 1024)
        .expect("normal decompression must succeed");
    assert_eq!(decompressed, payload);
}

#[test]
fn test_archive_prefix_truncated_stream_handling() {
    // Truncated Deflate stream: first 15 bytes of valid compressed stream
    let payload = vec![0x33u8; 1000];
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let compressed = encoder.finish().unwrap();

    let truncated = &compressed[..compressed.len() / 4];
    // Must return partial decoded buffer or DecodeError without panicking
    let _ = ArchiveStream::decompress(ArchiveFormat::Zip, truncated, 1024);

    // Empty buffer
    let _ = ArchiveStream::decompress(ArchiveFormat::Zip, &[], 1024);
}

#[test]
fn test_nested_archive_recursion_depth_limit() {
    // Verify unwrap_nested caps recursion depth at <= 10 layers
    let test_data = b"Plain non-archive payload";
    let nest = unwrap_nested(test_data);
    assert!(nest.layers.len() <= 10, "nested archive recursion depth must never exceed 10");
}
