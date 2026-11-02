// w06a: probe DV RPU (UNSPEC62), dvcC box, and HDR10+ SEI (payload 4) in an MKV.
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use hdr10plus::metadata::Hdr10PlusMetadata;
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use std::io::Cursor;

// Remove emulation-prevention bytes (00 00 03 -> 00 00).
fn unesc(b: &[u8]) -> Vec<u8> {
    let (mut o, mut z) = (Vec::with_capacity(b.len()), 0);
    for &x in b {
        if z >= 2 && x == 3 { z = 0; continue; }
        o.push(x);
        z = if x == 0 { z + 1 } else { 0 };
    }
    o
}

// Length-prefixed NAL split (MKV HEVC uses hvcC lengthSizeMinusOne).
fn nals(data: &[u8], lsz: usize) -> Vec<&[u8]> {
    let (mut v, mut p) = (vec![], 0usize);
    while p + lsz <= data.len() {
        let mut n = 0usize;
        for i in 0..lsz { n = (n << 8) | data[p + i] as usize; }
        p += lsz;
        if p + n > data.len() { break; }
        v.push(&data[p..p + n]);
        p += n;
    }
    v
}

// SEI message walk over an RBSP (header stripped, escapes removed).
fn sei_msgs(rbsp: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let (mut out, mut p) = (vec![], 0usize);
    while p + 2 <= rbsp.len() && rbsp[p] != 0x80 {
        let (mut pt, mut ps) = (0usize, 0usize);
        while rbsp[p] == 0xff { pt += 255; p += 1; }
        pt += rbsp[p] as usize; p += 1;
        while rbsp[p] == 0xff { ps += 255; p += 1; }
        ps += rbsp[p] as usize; p += 1;
        let end = (p + ps).min(rbsp.len());
        out.push((pt, rbsp[p..end].to_vec()));
        p += ps;
    }
    out
}

fn row(field: &str, expected: &str, got: &str) {
    let m = if expected == got { "match" } else { "MISMATCH" };
    println!("| {field} | {expected} | {got} | {m} |");
}

fn dv(rpu_nal: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let rpu = DoviRpu::parse_unspec62_nalu(rpu_nal)?;
    let vdr = rpu.vdr_dm_data.as_ref();
    let cm40 = vdr.and_then(|v| v.cmv40_metadata.as_ref());
    let cm29 = vdr.and_then(|v| v.cmv29_metadata.as_ref());
    println!("\n| field | expected (generator JSON) | Rust-parsed | match |\n|---|---|---|---|");
    row("dovi_profile", "8", &rpu.dovi_profile.to_string());
    row("el_type", "None", &format!("{:?}", rpu.el_type));
    row("vdr_rpu_profile / get_dovi_profile", "8", &rpu.header.get_dovi_profile().to_string());
    row("CM v4.0 block set present", "true", &cm40.is_some().to_string());
    row("CM v2.9 block set present", "false", &cm29.is_some().to_string());
    row("source_min_pq / source_max_pq", "62/3079", &vdr.map_or("-".into(), |v| format!("{}/{}", v.source_min_pq, v.source_max_pq)));
    let blk = |lvl: u8| vdr.map(|v| v.level_blocks_iter(lvl).collect::<Vec<_>>()).unwrap_or_default();
    println!("(counts) L1={} L2={} L5={} L6={} L8={} L9={} L11={} L254={} L255={}",
        blk(1).len(), blk(2).len(), blk(5).len(), blk(6).len(), blk(8).len(), blk(9).len(), blk(11).len(), blk(254).len(), blk(255).len());
    for b in blk(1) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level1(x) = b {
        row("L1 min_pq/max_pq/avg_pq", "62/3079/2048", &format!("{}/{}/{}", x.min_pq, x.max_pq, x.avg_pq)); } }
    for b in blk(2) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level2(x) = b {
        row("L2 target_max_pq", "2081", &x.target_max_pq.to_string()); } }
    for b in blk(5) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level5(x) = b {
        row("L5 active area L/R/T/B", "0/0/0/0", &format!("{}/{}/{}/{}", x.active_area_left_offset, x.active_area_right_offset, x.active_area_top_offset, x.active_area_bottom_offset)); } }
    for b in blk(6) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level6(x) = b {
        row("L6 mastering max/min (nits / 1e-4 nits raw)", "1000/50", &format!("{}/{}", x.max_display_mastering_luminance, x.min_display_mastering_luminance));
        row("L6 MaxCLL/MaxFALL (nits)", "1000/400", &format!("{}/{}", x.max_content_light_level, x.max_frame_average_light_level)); } }
    for b in blk(8) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level8(x) = b {
        row("L8 target_display_index", "1", &x.target_display_index.to_string()); } }
    for b in blk(9) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level9(x) = b {
        row("L9 source_primary_index", "0", &x.source_primary_index.to_string()); } }
    for b in blk(11) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level11(x) = b {
        row("L11 content_type", "1", &x.content_type.to_string());
        row("L11 reference_mode_flag", "true", &x.reference_mode_flag.to_string()); } }
    for b in blk(254) { if let dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock::Level254(x) = b {
        row("L254 dm_mode", "0", &x.dm_mode.to_string()); } }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: w06a_main <file.mkv>");
    let data = std::fs::read(&path)?;
    let mut mkv = MatroskaFile::open(Cursor::new(data.clone()))?;
    let vt = mkv.tracks().iter().find(|t| t.track_type() == TrackType::Video).ok_or("no video track")?;
    let (vno, codec) = (vt.track_number().get(), vt.codec_id().to_string());
    let cp = vt.codec_private().map(|c| c.to_vec()).unwrap_or_default();
    let lsz = (cp[21] & 3) as usize + 1; // hvcC lengthSizeMinusOne
    println!("container: codec={codec} hvcC_len_size={lsz}");

    // dvcC / dvvC BlockAdditionMapping: search raw bytes for the box tag.
    match data.windows(4).position(|w| w == b"dvcC") {
        Some(i) => {
            let b = &data[i + 4..i + 4 + 5];
            let v = u16::from_be_bytes([b[2], b[3]]);
            println!("dvcC: major={} minor={} profile={} level={} rpu={} el={} bl={} compat={} md_comp={}",
                b[0], b[1], v >> 9, (v >> 3) & 0x3f, (v >> 2) & 1, (v >> 1) & 1, v & 1, b[4] >> 4, (b[4] >> 2) & 3);
        }
        None => println!("dvcC: absent"),
    }

    // First keyframe on the video track.
    let mut fr = Frame::from(Vec::new());
    loop {
        if !mkv.next_frame(&mut fr)? { return Err("no keyframe".into()); }
        if fr.track == vno && fr.is_keyframe != Some(false) { break; }
    }
    let frame_nals = nals(&fr.data, lsz);
    println!("first keyframe: {} bytes, {} NALs", fr.data.len(), frame_nals.len());

    // ---- Dolby Vision: NAL 62 (UNSPEC62) ----
    match frame_nals.iter().find(|n| (n[0] >> 1) & 0x3f == 62) { None => println!("\nno UNSPEC62 (not DV)"), Some(r) => dv(r)? }

    // ---- HDR10+: prefix SEI (39), payload type 4, T.35 B5 003C 0001, app id 4 ----
    let mut hdr10p_found = false;
    for n in frame_nals.iter().filter(|n| (n[0] >> 1) & 0x3f == 39) {
        for (pt, pl) in sei_msgs(&unesc(&n[2..])) {
            if pt != 4 || pl.len() < 6 || pl[0] != 0xB5 || pl[1..3] != [0x00, 0x3C] || pl[3..5] != [0x00, 0x01] { continue; }
            hdr10p_found = true;
            println!("\nHDR10+ T.35 payload: {} bytes", pl.len());
            let m = Hdr10PlusMetadata::parse(&pl)?;
            println!("| field | expected (hdr10plus JSON) | Rust-parsed | match |\n|---|---|---|---|");
            row("profile", "B", &m.profile);
            row("itu_t_t35 country/provider/oriented", "181/60/1", &format!("{}/{}/{}", m.itu_t_t35_country_code, m.itu_t_t35_terminal_provider_code, m.itu_t_t35_terminal_provider_oriented_code));
            row("application_identifier/version", "4/1", &format!("{}/{}", m.application_identifier, m.application_version));
            row("num_windows", "1", &m.num_windows.to_string());
            row("targeted_system_display_maximum_luminance", "400", &m.targeted_system_display_maximum_luminance.to_string());
            row("maxscl", "[800, 750, 700]", &format!("{:?}", m.maxscl));
            row("average_maxrgb", "300", &m.average_maxrgb.to_string());
            row("num_distribution_maxrgb_percentiles", "9", &m.num_distribution_maxrgb_percentiles.to_string());
            row("distribution_maxrgb (first pct/value, last pct/value)", "1/10 .. 99/780",
                &m.distribution_maxrgb.first().zip(m.distribution_maxrgb.last()).map_or("-".into(), |(a, b)| format!("{}/{} .. {}/{}", a.percentage, a.percentile, b.percentage, b.percentile)));
            row("bezier knee_x/knee_y/anchors", "100/200/3",
                &m.bezier_curve.as_ref().map_or("-".into(), |b| format!("{}/{}/{}", b.knee_point_x, b.knee_point_y, b.num_bezier_curve_anchors)));
            row("tone_mapping_flag", "true", &m.tone_mapping_flag.to_string());
            break;
        }
    }
    if !hdr10p_found { println!("\nHDR10+: no T.35 payload 4 in first keyframe prefix SEI"); }
    Ok(())
}
