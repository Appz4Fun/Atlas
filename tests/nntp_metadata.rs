//! Integration tests for Milestone 4: Metadata Extraction & Password Identification
//! Covering Container & Stream Metadata Features 19-26, TC-T3-04, and TC-T3-05.

use std::io::Write;

use atlas::probe::{
    ArchiveFormat, ArchiveStream, AudioBitstream, AvcDepth, DecompressError, DiscProber,
    DolbyVisionInfo, H265Sps, HevcDepth, MkvMetadata, Mp4Prober, unwrap_nested,
};
use flate2::Compression;
use flate2::write::DeflateEncoder;

// ============================================================================
// Helper Functions for EBML and MP4 Synthesizers
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
// Feature 19: Matroska Chapter Hidden Edition Filtering & SegmentUID & Tags
// ============================================================================

#[test]
fn tc_t1_f19_1_matroska_hidden_edition_filtering() {
    // Edition 0: Hidden Edition (EditionFlagHidden = 1). Should be skipped.
    let hidden_atom = {
        let display = [ebml_elem(&[0x85], b"Hidden Dummy Chapter"), ebml_elem(&[0x43, 0x7C], b"eng")].concat();
        let body = [ebml_elem(&[0x91], &0u64.to_be_bytes()), ebml_elem(&[0x80], &display)].concat();
        ebml_elem(&[0xB6], &body)
    };
    let edition_hidden = [ebml_elem(&[0x45, 0xBD], &[1]), hidden_atom].concat();

    // Edition 1: Default Edition (EditionFlagDefault = 1, EditionFlagHidden = 0). Should be selected.
    let chapter1 = {
        let display = [ebml_elem(&[0x85], b"Intro"), ebml_elem(&[0x43, 0x7C], b"eng")].concat();
        let body = [ebml_elem(&[0x91], &0u64.to_be_bytes()), ebml_elem(&[0x80], &display)].concat();
        ebml_elem(&[0xB6], &body)
    };
    let chapter2 = {
        let display = [ebml_elem(&[0x85], b"Feature"), ebml_elem(&[0x43, 0x7C], b"eng")].concat();
        let body = [ebml_elem(&[0x91], &(100_000_000_000u64).to_be_bytes()), ebml_elem(&[0x80], &display)].concat();
        ebml_elem(&[0xB6], &body)
    };
    let edition_default = [
        ebml_elem(&[0x45, 0xDB], &[1]),
        ebml_elem(&[0x45, 0xBD], &[0]),
        chapter1,
        chapter2,
    ]
    .concat();

    let chapters_elem = ebml_elem(
        &[0x10, 0x43, 0xA7, 0x70],
        &[ebml_elem(&[0x45, 0xB9], &edition_hidden), ebml_elem(&[0x45, 0xB9], &edition_default)].concat(),
    );

    let info = ebml_elem(&[0x7B, 0xA9], b"Test Title");
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

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse valid matroska");
    assert_eq!(meta.chapters.len(), 2, "hidden edition must be skipped, keeping only default edition");
    assert_eq!(meta.chapters[0].name.as_deref(), Some("Intro"));
    assert_eq!(meta.chapters[0].start_secs, 0.0);
    assert_eq!(meta.chapters[1].name.as_deref(), Some("Feature"));
    assert_eq!(meta.chapters[1].start_secs, 100.0);
}

#[test]
fn tc_t1_f19_2_matroska_segment_uid_extraction() {
    let raw_uid: [u8; 16] = [
        0x10, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01,
    ];
    let segment_uid_elem = ebml_elem(&[0x73, 0xA4], &raw_uid);
    let info = [ebml_elem(&[0x7B, 0xA9], b"UID Video"), segment_uid_elem].concat();

    let segment = [
        ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info),
        ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
    ]
    .concat();
    let mkv_bytes = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &segment),
    ]
    .concat();

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse matroska");
    assert_eq!(
        meta.segment_uid.as_deref(),
        Some("102233445566778899aabbccddeeff01"),
        "SegmentUID must be extracted as 32-character lowercase hex"
    );
}

#[test]
fn tc_t1_f19_3_matroska_tags_extraction() {
    let simple_tag1 = [
        ebml_elem(&[0x45, 0xA3], b"TITLE"),
        ebml_elem(&[0x44, 0x87], b"Atlas NNTP Release"),
    ]
    .concat();
    let simple_tag2 = [
        ebml_elem(&[0x45, 0xA3], b"ENCODER"),
        ebml_elem(&[0x44, 0x87], b"x265 3.5+1"),
    ]
    .concat();

    let tag_elem = [
        ebml_elem(&[0x67, 0xC8], &simple_tag1),
        ebml_elem(&[0x67, 0xC8], &simple_tag2),
    ]
    .concat();

    let tags_elem = ebml_elem(&[0x12, 0x54, 0xC3, 0x67], &ebml_elem(&[0x73, 0x73], &tag_elem));

    let info = ebml_elem(&[0x7B, 0xA9], b"Tags Video");
    let segment = [
        ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info),
        tags_elem,
        ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
    ]
    .concat();
    let mkv_bytes = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &segment),
    ]
    .concat();

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse matroska tags");
    assert_eq!(meta.tags.get("TITLE").map(|s| s.as_str()), Some("Atlas NNTP Release"));
    assert_eq!(meta.tags.get("ENCODER").map(|s| s.as_str()), Some("x265 3.5+1"));
}

#[test]
fn tc_t1_f19_4_mkv_metadata_parse_wrapper() {
    assert!(MkvMetadata::parse(b"NOT_A_MATROSKA_STREAM").is_none());
}

// ============================================================================
// Feature 20: SPS VUI Colour Space & HDR10/HLG Recovery from CodecPrivate
// ============================================================================

fn build_hevc_10bit_sps_nal(primaries: u8, transfer: u8, matrix: u8) -> Vec<u8> {
    vec![
        0x42, 0x01, // NAL header (type 33 = SPS_NUT)
        0x01,       // vps_id=0, max_sub_layers_minus1=0, temporal_id_nesting_flag=1
        0x02, 0x20, 0x40, 0x08, 0x01, 0xB0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x99, // profile_tier_level (Main 10, Level 5.1)
        0x22, 0x12, 0x24, 0xDA, 0x7A, 0xAC, 0x22, 0xB5, // SPS fields up to colour_description_present_flag=1
        primaries, transfer, matrix, 0x80,
    ]
}

#[test]
fn tc_t1_f20_1_sps_vui_hdr10_recovery() {
    // Valid ITU-T H.265 Main 10 SPS NAL unit (NAL type 33: 0x42, 0x01) containing HDR10 VUI parameters:
    // colour_primaries = 9 (BT.2020), transfer_characteristics = 16 (SMPTE ST 2084 / PQ), matrix_coeffs = 9
    let sps_nal = build_hevc_10bit_sps_nal(9, 16, 9);
    let vui = H265Sps::parse_vui(&sps_nal).expect("must parse VUI parameters");
    assert_eq!(vui.colour_primaries, 9);
    assert_eq!(vui.transfer_characteristics, 16);
    assert_eq!(vui.matrix_coeffs, 9);
    assert_eq!(vui.bit_depth, 10);
    assert_eq!(vui.hdr.as_deref(), Some("HDR10"));

    // Truncated SPS payloads that merely contain `09 10` bytes must return None
    assert!(H265Sps::parse_vui(&[0x42, 0x01, 0x09, 0x10]).is_none());
    assert!(H265Sps::parse_vui(&[0x42, 0x01, 0x01, 0x02, 9, 16, 9, 0x00]).is_none());
}

#[test]
fn tc_t1_f20_2_sps_vui_hlg_recovery() {
    // Valid ITU-T H.265 Main 10 SPS NAL unit with HLG transfer characteristics = 18 (ARIB STD-B67), colour_primaries = 9
    let sps_nal = build_hevc_10bit_sps_nal(9, 18, 9);
    let vui = H265Sps::parse_vui(&sps_nal).expect("must parse HLG VUI");
    assert_eq!(vui.colour_primaries, 9);
    assert_eq!(vui.transfer_characteristics, 18);
    assert_eq!(vui.matrix_coeffs, 9);
    assert_eq!(vui.bit_depth, 10);
    assert_eq!(vui.hdr.as_deref(), Some("HLG"));
}

#[test]
fn tc_t1_f20_3_sps_vui_annex_b_prefix_support() {
    // Same SPS with 4-byte Annex B prefix [0, 0, 0, 1]
    let mut sps_annex_b = vec![0x00, 0x00, 0x00, 0x01];
    sps_annex_b.extend_from_slice(&build_hevc_10bit_sps_nal(9, 16, 9));
    let vui = H265Sps::parse_vui(&sps_annex_b).expect("must strip Annex B prefix and parse VUI");
    assert_eq!(vui.hdr.as_deref(), Some("HDR10"));
}

// ============================================================================
// Feature 21: 10-bit Depth Extraction from hvcC and avcC
// ============================================================================

#[test]
fn tc_t1_f21_1_hevc_10bit_depth_from_hvcc() {
    // hvcC record of at least 23 bytes. Byte 17 holds bitDepthLumaMinus8 (bottom 3 bits)
    let mut hvcc = vec![0u8; 24];
    hvcc[17] = 2; // 2 + 8 = 10 bits
    hvcc[18] = 2; // chroma 10 bits
    let depth = HevcDepth::from_hvcc(&hvcc).expect("must extract HEVC depth");
    assert_eq!(depth, 10);
}

#[test]
fn tc_t1_f21_2_hevc_8bit_depth_from_hvcc() {
    let mut hvcc = vec![0u8; 24];
    hvcc[17] = 0; // 0 + 8 = 8 bits
    let depth = HevcDepth::from_hvcc(&hvcc).expect("must extract HEVC depth");
    assert_eq!(depth, 8);
}

#[test]
fn tc_t1_f21_3_avc_10bit_depth_from_avcc_high10() {
    // Standard ISO/IEC 14496-15 High 10 avcC record with 1 SPS (8 bytes, so byte 6 is 0x00),
    // 1 PPS, and the post-PPS High profile configuration extension.
    let sps = [0x67, 110, 0x00, 0x28, 0xA6, 0xCE, 0x9E, 0x40];
    let pps = [0x68, 0xEE, 0x3C, 0x80];
    let mut avcc = vec![1, 110, 0x00, 0x28, 0xFF, 0xE1, 0x00, sps.len() as u8];
    avcc.extend_from_slice(&sps);
    avcc.extend_from_slice(&[0x01, 0x00, pps.len() as u8]);
    avcc.extend_from_slice(&pps);
    avcc.extend_from_slice(&[0xFD, 0xFA, 0xFA, 0x00]);
    assert_eq!(avcc[6], 0, "byte 6 is the high byte of the first SPS length");
    let depth = AvcDepth::from_avcc(&avcc).expect("must extract AVC High10 depth from SPS");
    assert_eq!(depth, 10);

    // High 10 avcC record without inline SPS, relying on the post-PPS configuration extension
    let avcc_ext_only = [1, 110, 0x00, 0x28, 0xFF, 0xE0, 0x00, 0xFD, 0xFA, 0xFA, 0x00];
    assert_eq!(AvcDepth::from_avcc(&avcc_ext_only), Some(10));
}

#[test]
fn tc_t1_f21_4_avc_8bit_depth_from_avcc_high() {
    // avcC record with profile_idc = 100 (High Profile, 8 bits default)
    let mut avcc = vec![0u8; 10];
    avcc[1] = 100;
    let depth = AvcDepth::from_avcc(&avcc).expect("must extract AVC High depth");
    assert_eq!(depth, 8);
}

// ============================================================================
// Feature 22: Dolby Vision NAL 62 RPU Bitstream Parser
// ============================================================================

#[test]
fn tc_t1_f22_1_dolby_vision_dvcc_dvvc_profile7_fel() {
    // dvvC/dvcC atom payload: version=1, profile=7 (byte 2 >> 1 = 7), level=6, el_present=1.
    // The configuration record preserves el_present=true while leaving fel=false until RPU/NLQ is inspected.
    let dv_box = [1, 0, (7 << 1), (6 << 3) | 0b011];
    let dv_info = DolbyVisionInfo::from_dvcc_dvvc(&dv_box).expect("must parse dvvC record");
    assert_eq!(dv_info.profile, 7);
    assert_eq!(dv_info.level, Some(6));
    assert!(dv_info.el_present);
    assert!(!dv_info.fel, "dvvC configuration record alone must not assume FEL before RPU inspection");
}

#[test]
fn tc_t1_f22_2_dolby_vision_dvcc_dvvc_profile8_cm29() {
    // dvvC atom payload: version=1, profile=8, level=6, el_present=0
    let dv_box = [1, 0, (8 << 1), (6 << 3) | 0b101];
    let dv_info = DolbyVisionInfo::from_dvcc_dvvc(&dv_box).expect("must parse dvvC record");
    assert_eq!(dv_info.profile, 8);
    assert_eq!(dv_info.level, Some(6));
    assert!(!dv_info.fel);
    assert_eq!(dv_info.cm_version, "v2.9");
}

#[test]
fn tc_t1_f22_3_dolby_vision_level253_crash_immunity() {
    // Adversarial or truncated NAL 62 / RPU payload must not panic
    let corrupt_payload = vec![0x7C, 0x01, 0xFF, 0xFD, 0xAA];
    let _ = DolbyVisionInfo::parse_rpu(&corrupt_payload);

    let empty = &[];
    assert!(DolbyVisionInfo::parse_rpu(empty).is_none());

    // Ordinary length-prefixed HEVC type-19 (IDR_W_RADL) sample of 2,560 bytes (`00 00 0A 00`)
    // must not be misinterpreted as a Dolby Vision Profile 5 configuration record.
    let mut idr_sample = vec![0x00, 0x00, 0x0A, 0x00, 19 << 1, 0x01];
    idr_sample.resize(4 + 2_560, 0x55);
    assert!(DolbyVisionInfo::parse_rpu(&idr_sample).is_none());
    assert!(DolbyVisionInfo::parse_rpu_with_length_size(&idr_sample, 4).is_none());
}

// ============================================================================
// Feature 23: Audio Bitstream Prober
// ============================================================================

#[test]
fn tc_t1_f23_1_audio_bitstream_truehd_atmos() {
    let mut data = vec![0u8; 64];
    // TrueHD major sync word: 0xF8726FBA
    data[10..14].copy_from_slice(&[0xF8, 0x72, 0x6F, 0xBA]);
    // Atmos 4th substream flag in substream_info at offset pos + 17
    data[10 + 17] = 0x80;

    let probe = AudioBitstream::probe(&data);
    assert!(probe.has_atmos);
    assert_eq!(probe.format.as_deref(), Some("TrueHD Atmos"));
    assert!(AudioBitstream::probe_truehd_atmos(&data));
}

#[test]
fn tc_t1_f23_2_audio_bitstream_eac3_atmos() {
    let mut data = vec![0u8; 64];
    // EMDF sync word 0x5838 + container_len=4 + version=0, key_id=0, payload_id=14 (JOC) + 12b joc_header
    data[20..28].copy_from_slice(&[0x58, 0x38, 0x00, 0x04, 0x03, 0x80, 0x00, 0x00]);

    let probe = AudioBitstream::probe(&data);
    assert!(probe.has_atmos);
    assert_eq!(probe.format.as_deref(), Some("E-AC-3 Atmos"));
}

#[test]
fn tc_t1_f23_3_audio_bitstream_dtsx() {
    let mut data = vec![0u8; 64];
    // DTS:X XLL sync word: 0x02000850
    data[16..20].copy_from_slice(&[0x02, 0x00, 0x08, 0x50]);

    let probe = AudioBitstream::probe(&data);
    assert!(probe.has_dtsx);
    assert_eq!(probe.format.as_deref(), Some("DTS:X"));
    assert!(AudioBitstream::probe_dtsx(&data));
}

// ============================================================================
// Feature 24: Non-faststart MP4 moov Tail Atom Fetch
// ============================================================================

#[test]
fn tc_t1_f24_1_mp4_non_faststart_moov_offset() {
    let ftyp = mp4_box(b"ftyp", b"isom");
    let mdat_body = vec![0u8; 100_000];
    let mdat = mp4_box(b"mdat", &mdat_body);

    let prefix = [ftyp, mdat].concat();
    let moov_offset = Mp4Prober::find_moov_offset(&prefix).expect("must calculate moov offset");
    assert_eq!(moov_offset, prefix.len() as u64);
}

#[test]
fn tc_t1_f24_2_mp4_extended_size_mdat_offset() {
    let ftyp = mp4_box(b"ftyp", b"isom");
    let mut mdat_head = vec![0u8; 16];
    // 64-bit size indicator: size field = 1
    mdat_head[0..4].copy_from_slice(&1u32.to_be_bytes());
    mdat_head[4..8].copy_from_slice(b"mdat");
    // Extended size = 500,000,000 bytes
    mdat_head[8..16].copy_from_slice(&500_000_000u64.to_be_bytes());

    let prefix = [ftyp.clone(), mdat_head].concat();
    let moov_offset = Mp4Prober::find_moov_offset(&prefix).expect("must calculate extended moov offset");
    assert_eq!(moov_offset, ftyp.len() as u64 + 500_000_000);

    // Malformed extended-size box with u64::MAX must return None without arithmetic overflow
    let mut overflow_free = vec![0u8; 16];
    overflow_free[0..4].copy_from_slice(&1u32.to_be_bytes());
    overflow_free[4..8].copy_from_slice(b"free");
    overflow_free[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(Mp4Prober::find_moov_offset(&overflow_free).is_none());
    assert!(Mp4Prober::probe_tail_moov(&overflow_free).is_none());
}

// ============================================================================
// Feature 25: Optical Disc Descriptors
// ============================================================================

#[test]
fn tc_t1_f25_1_disc_prober_iso9660_pvd() {
    let mut disc = vec![0u8; 0x9000];
    // Sector 16 PVD at offset 0x8000: byte 0 = 0x01, bytes 1..6 = CD001
    disc[0x8000] = 0x01;
    disc[0x8001..0x8006].copy_from_slice(b"CD001");
    // Volume ID at offset 0x8000 + 40..72
    let vol_id = b"TEST_ISO_VOLUME                 ";
    disc[0x8028..0x8028 + 32].copy_from_slice(vol_id);

    let info = DiscProber::probe(&disc).expect("must identify ISO9660 PVD");
    assert_eq!(info.disc_type, "ISO9660");
    assert_eq!(info.volume_id.as_deref(), Some("TEST_ISO_VOLUME"));
}

#[test]
fn tc_t1_f25_2_disc_prober_udf_avdp() {
    let mut disc = vec![0u8; 0x81000];
    // Sector 256 AVDP at offset 0x80000: tag identifier 0x0002
    disc[0x80000] = 0x02;
    disc[0x80001] = 0x00;

    let info = DiscProber::probe(&disc).expect("must identify UDF AVDP");
    assert_eq!(info.disc_type, "UDF");
}

#[test]
fn tc_t1_f25_3_disc_prober_dvd_disc_id_md5() {
    let video_ts = vec![0x42u8; 20_000];
    let vts = vec![0x99u8; 20_000];

    let disc_id_hex = DiscProber::dvd_disc_id_hex(&video_ts, &vts);
    assert_eq!(disc_id_hex.len(), 32, "DVD Disc ID must be 32-character hex");
    assert_eq!(
        disc_id_hex, "732ca863c2a0e85d17bdf1de2a09813e",
        "DVDDiscID must hash the complete VIDEO_TS.IFO and VTS_01_0.IFO files without 16 KiB truncation"
    );

    let mut video_ts_tail_modified = video_ts.clone();
    video_ts_tail_modified[19_999] ^= 0xFF;
    assert_ne!(
        DiscProber::dvd_disc_id_hex(&video_ts_tail_modified, &vts),
        disc_id_hex,
        "bytes past 16 KiB in VIDEO_TS.IFO must contribute to DVDDiscID"
    );

    let mut vts_tail_modified = vts.clone();
    vts_tail_modified[19_999] ^= 0xFF;
    assert_ne!(
        DiscProber::dvd_disc_id_hex(&video_ts, &vts_tail_modified),
        disc_id_hex,
        "bytes past 16 KiB in VTS_01_0.IFO must contribute to DVDDiscID"
    );

    let marker_disc = [vec![0u8; 100], b"VIDEO_TS.IFO".to_vec(), vec![0u8; 100]].concat();
    let info = DiscProber::probe(&marker_disc).expect("must identify DVD marker");
    assert!(info.is_dvd);
    assert_eq!(info.disc_type, "DVD");
}

#[test]
fn tc_t1_f25_4_disc_prober_bluray_marker() {
    let marker_disc = [vec![0u8; 50], b"index.bdmv".to_vec(), vec![0u8; 50]].concat();
    let info = DiscProber::probe(&marker_disc).expect("must identify Blu-ray marker");
    assert!(info.is_bluray);
    assert_eq!(info.disc_type, "Blu-ray");
}

// ============================================================================
// Feature 26: Streaming Prefix Decompression & Nested Archives
// ============================================================================

#[test]
fn tc_t1_f26_1_streaming_prefix_decompression_zip_flate() {
    let original = b"Atlas NNTP Integration Milestone 4 Metadata Prefix Test Data";
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(original).unwrap();
    let compressed = encoder.finish().unwrap();

    let decompressed = ArchiveStream::decompress(ArchiveFormat::Zip, &compressed, 1024)
        .expect("must decompress pure Rust Deflate stream");
    assert_eq!(decompressed, original);
}

#[test]
fn tc_t1_f26_2_zip_bomb_expansion_ratio_protection() {
    // 50,000 zero bytes compressed via Deflate compresses down to ~60 bytes
    // Expansion ratio is ~800:1, exceeding 100:1 cap
    let original = vec![0u8; 50_000];
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&original).unwrap();
    let compressed = encoder.finish().unwrap();

    let res = ArchiveStream::decompress(ArchiveFormat::Zip, &compressed, 100_000);
    assert!(
        matches!(res, Err(DecompressError::ExpansionRatioBreached)),
        "100:1 expansion ratio breach must abort decompression"
    );
}

#[test]
fn tc_t1_f26_3_nested_archive_recursion_depth_cap() {
    // Non-archive or single-layer data should produce finite layers <= 10
    let nest = unwrap_nested(b"plain non-archive content");
    assert!(nest.layers.len() <= 10);
}

// ============================================================================
// TC-T3-04: Metadata Extraction & HDR/Audio Recovery Integration Test
// ============================================================================

#[test]
fn tc_t3_04_metadata_extraction_hdr_audio_recovery_integration() {
    // Composite test:
    // 1. Matroska container with SegmentUID and Tags
    // 2. Video track with HEVC SPS (HDR10) and dvvC (Dolby Vision Profile 7 FEL)
    // 3. Audio track with E-AC-3 Atmos
    // 4. Chapters with hidden edition filtering

    // SegmentUID
    let raw_uid: [u8; 16] = [0x55; 16];
    let segment_uid_elem = ebml_elem(&[0x73, 0xA4], &raw_uid);
    let info = [
        ebml_elem(&[0x7B, 0xA9], b"Master Feature (2026)"),
        segment_uid_elem,
    ]
    .concat();

    // Video Track
    let colour = [
        ebml_elem(&[0x55, 0xB2], &[10]), // 10-bit
        ebml_elem(&[0x55, 0xBA], &[16]), // transfer = SMPTE ST 2084
        ebml_elem(&[0x55, 0xBB], &[9]),  // primaries = BT.2020
    ]
    .concat();
    let video_fields = [
        ebml_elem(&[0xB0], &[0x0f, 0x00]), // 3840
        ebml_elem(&[0xBA], &[0x08, 0x70]), // 2160
        ebml_elem(&[0x55, 0xB0], &colour),
    ]
    .concat();
    let dv_box = [
        ebml_elem(&[0x41, 0xE7], b"dvvC"),
        ebml_elem(&[0x41, 0xED], &[1, 0, 7 << 1, 6 << 3 | 0b011]),
    ]
    .concat();
    let video_entry = [
        ebml_elem(&[0x83], &[1]), // Video track number 1
        ebml_elem(&[0x86], b"V_MPEGH/ISO/HEVC"),
        ebml_elem(&[0xE0], &video_fields),
        ebml_elem(&[0x41, 0xE4], &dv_box),
    ]
    .concat();

    // Audio Track
    let audio_fields = [ebml_elem(&[0x9F], &[6]), ebml_elem(&[0xB5], &48000f32.to_be_bytes())].concat();
    let audio_entry = [
        ebml_elem(&[0x83], &[2]), // Audio track number 2
        ebml_elem(&[0x86], b"A_EAC3"),
        ebml_elem(&[0x53, 0x6E], b"Dolby Digital Plus with Dolby Atmos"),
        ebml_elem(&[0xE1], &audio_fields),
    ]
    .concat();

    let tracks = ebml_elem(
        &[0x16, 0x54, 0xAE, 0x6B],
        &[ebml_elem(&[0xAE], &video_entry), ebml_elem(&[0xAE], &audio_entry)].concat(),
    );

    // Tags
    let tag = ebml_elem(
        &[0x67, 0xC8],
        &[ebml_elem(&[0x45, 0xA3], b"TITLE"), ebml_elem(&[0x44, 0x87], b"Master Feature (2026)")].concat(),
    );
    let tags_elem = ebml_elem(&[0x12, 0x54, 0xC3, 0x67], &ebml_elem(&[0x73, 0x73], &tag));

    // Chapters
    let chap = {
        let display = ebml_elem(&[0x80], &ebml_elem(&[0x85], b"Chapter 1"));
        let body = [ebml_elem(&[0x91], &0u64.to_be_bytes()), display].concat();
        ebml_elem(&[0xB6], &body)
    };
    let edition = [ebml_elem(&[0x45, 0xDB], &[1]), chap].concat();
    let chapters = ebml_elem(&[0x10, 0x43, 0xA7, 0x70], &ebml_elem(&[0x45, 0xB9], &edition));

    let segment = [
        ebml_elem(&[0x15, 0x49, 0xA9, 0x66], &info),
        tracks,
        tags_elem,
        chapters,
        ebml_elem(&[0x1F, 0x43, 0xB6, 0x75], &[0; 16]),
    ]
    .concat();

    let mkv_bytes = [
        ebml_elem(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_elem(&[0x42, 0x82], b"matroska")),
        ebml_elem(&[0x18, 0x53, 0x80, 0x67], &segment),
    ]
    .concat();

    let meta = MkvMetadata::parse(&mkv_bytes).expect("must parse composite media");

    // 1. Container & info
    assert_eq!(meta.container, "matroska");
    assert_eq!(meta.segment_uid.as_deref(), Some("55555555555555555555555555555555"));
    assert_eq!(meta.tags.get("TITLE").map(|s| s.as_str()), Some("Master Feature (2026)"));

    // 2. Video track & HDR/Dolby Vision
    assert_eq!(meta.video.len(), 1);
    let v = &meta.video[0];
    assert_eq!(v.codec, "V_MPEGH/ISO/HEVC");
    assert_eq!(v.width, Some(3840));
    assert_eq!(v.height, Some(2160));
    assert_eq!(v.bit_depth, Some(10));
    assert_eq!(v.hdr.as_deref(), Some("HDR10"));
    assert_eq!(v.dv_profile, Some(7));
    assert_eq!(v.dv_enhancement_layer, Some(true));

    // 3. Audio track & Atmos
    assert_eq!(meta.audio.len(), 1);
    let a = &meta.audio[0];
    assert_eq!(a.codec, "A_EAC3");
    assert!(a.atmos);

    // 4. Chapters
    assert_eq!(meta.chapters.len(), 1);
    assert_eq!(meta.chapters[0].name.as_deref(), Some("Chapter 1"));
}
