// w05a: probe video elementary-stream metadata from the first MiB of an MKV.
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use std::io::{Cursor, Read};

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

fn uuid(b: &[u8]) -> String {
    let h: Vec<String> = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

fn be16(b: &[u8]) -> u32 { u16::from_be_bytes([b[0], b[1]]) as u32 }
fn be32(b: &[u8]) -> u32 { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }

// Walk SEI RBSP (header already stripped, escapes removed).
fn sei(rbsp: &[u8], tag: &str) {
    let mut p = 0usize;
    while p + 2 <= rbsp.len() && rbsp[p] != 0x80 {
        let (mut pt, mut ps) = (0usize, 0usize);
        while rbsp[p] == 0xff { pt += 255; p += 1; }
        pt += rbsp[p] as usize; p += 1;
        while rbsp[p] == 0xff { ps += 255; p += 1; }
        ps += rbsp[p] as usize; p += 1;
        let pl = &rbsp[p..(p + ps).min(rbsp.len())];
        match pt {
            5 if pl.len() >= 16 => {
                let txt: String = pl[16..].iter().take(90).map(|&c| if (32..127).contains(&c) { c as char } else { '.' }).collect();
                println!("{tag} SEI5 user_data_unregistered uuid={} len={} text=\"{}\"", uuid(&pl[..16]), pl.len(), txt);
            }
            137 if pl.len() >= 24 => println!(
                "{tag} SEI137 mastering: G=({},{}) B=({},{}) R=({},{}) WP=({},{}) max_lum={}cd/m2 min_lum={:.4}cd/m2 (raw {} {})",
                be16(&pl[0..]), be16(&pl[2..]), be16(&pl[4..]), be16(&pl[6..]), be16(&pl[8..]), be16(&pl[10..]),
                be16(&pl[12..]), be16(&pl[14..]), be32(&pl[16..]) as f64 / 10000.0, be32(&pl[20..]) as f64 / 10000.0,
                be32(&pl[16..]), be32(&pl[20..])),
            144 if pl.len() >= 4 => println!("{tag} SEI144 CLL: max_content={} max_frame_average={}", be16(pl), be16(&pl[2..])),
            _ => println!("{tag} SEI payload_type={pt} size={ps}"),
        }
        p += ps;
    }
}

// Length-prefixed NAL walker (MKV/MP4 style).
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

fn leb128(b: &[u8]) -> (u64, usize) {
    let (mut v, mut i) = (0u64, 0usize);
    loop { v |= ((b[i] & 0x7f) as u64) << (7 * i); i += 1; if b[i - 1] & 0x80 == 0 || i == 8 { return (v, i); } }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: w05a_main <file.mkv>");
    // Only the first MiB of the file is read: header, tracks, first cluster.
    let mut head = Vec::new();
    std::fs::File::open(&path)?.take(1 << 20).read_to_end(&mut head)?;
    let mut mkv = MatroskaFile::open(Cursor::new(head.clone()))?;
    println!("container: doc_type={} timestamp_scale={} duration_ms={:?} muxing_app={} writing_app={}",
        "matroska", mkv.info().timestamp_scale(), mkv.info().duration(), mkv.info().muxing_app(), mkv.info().writing_app());
    println!("chapters={} tags={}", mkv.chapters().map_or(0, |c| c.len()), mkv.tags().map_or(0, |t| t.len()));
    let vt = mkv.tracks().iter().find(|t| t.track_type() == TrackType::Video).ok_or("no video track")?;
    let (vno, codec) = (vt.track_number().get(), vt.codec_id().to_string());
    let cp = vt.codec_private().map(|c| c.to_vec()).unwrap_or_default();
    let v = vt.video().ok_or("no video element")?;
    println!("video: track={vno} codec_id={codec} codec_private_len={} pixel={}x{} default_duration_ns={:?}",
        cp.len(), v.pixel_width(), v.pixel_height(), vt.default_duration());
    println!("colour: {:?}", v.colour());
    for t in mkv.tracks().iter().filter(|t| t.track_type() == TrackType::Audio) {
        let a = t.audio().unwrap();
        println!("audio: track={} codec_id={} sr={} ch={} bit_depth={:?} lang={:?} name={:?}",
            t.track_number(), t.codec_id(), a.sampling_frequency(), a.channels(), a.bit_depth(), t.language(), t.name());
    }
    // First keyframe on the video track.
    let mut fr = Frame::from(Vec::new());
    loop {
        if !mkv.next_frame(&mut fr)? { return Err("no keyframe".into()); }
        if fr.track == vno && fr.is_keyframe != Some(false) { break; }
    }
    let probe = &fr.data[..32.min(fr.data.len())];
    let off = head.windows(probe.len()).position(|w| w == probe);
    println!("first_keyframe: size={} timestamp={} file_offset_of_payload={:?} (ffprobe pkt_pos is block start)", fr.data.len(), fr.timestamp, off);

    match codec.as_str() {
        "V_MPEGH/ISO/HEVC" => {
            let cfg = scuffle_h265::HEVCDecoderConfigurationRecord::demux(&cp[..])?;
            println!("hvcC: profile_idc={} tier={} level_idc={} chroma={} bit_depth_luma={} lsz={} avg_frame_rate={} cfr={:?}",
                cfg.general_profile_idc, cfg.general_tier_flag, cfg.general_level_idc, cfg.chroma_format_idc,
                cfg.bit_depth_luma_minus8 + 8, cfg.length_size_minus_one + 1, cfg.avg_frame_rate, cfg.constant_frame_rate);
            for arr in &cfg.arrays {
                println!("hvcC array {:?} n={}", arr.nal_unit_type, arr.nalus.len());
                let nm = format!("{:?}", arr.nal_unit_type);
                for n in &arr.nalus {
                    if nm.contains("Sps") {
                        let s = scuffle_h265::SpsNALUnit::parse(&n[..])?.rbsp;
                        println!("SPS: {}x{} cropped={}x{} chroma_idc={} bd_y={} bd_c={}",
                            s.pic_width_in_luma_samples, s.pic_height_in_luma_samples, s.cropped_width(), s.cropped_height(),
                            s.chroma_format_idc, s.bit_depth_y(), s.bit_depth_c());
                        if let Some(vui) = &s.vui_parameters {
                            println!("SPS VUI video_signal: {:?}", vui.video_signal_type);
                            println!("SPS VUI timing: {:?}", vui.vui_timing_info);
                        }
                    }
                }
                if nm.contains("Sei") { for x in &arr.nalus { sei(&unesc(&x[2..]), "hvcC"); } }
            }
            let frame_nals = nals(&fr.data, cfg.length_size_minus_one as usize + 1);
            let mut kinds = std::collections::BTreeMap::new();
            for n in &frame_nals {
                let t = (n[0] >> 1) & 0x3f;
                *kinds.entry(t).or_insert(0) += 1;
                if t == 39 || t == 40 { sei(&unesc(&n[2..]), &format!("frame0-nal{t}")); }
            }
            println!("frame0 HEVC NAL types (type:count): {:?}", kinds);
        }
        "V_MPEG4/ISO/AVC" => {
            let lsz = (cp[4] & 3) as usize + 1;
            let nsps = cp[5] & 0x1f;
            println!("avcC: profile={} compat={:#x} level={} lsz={} nsps={}", cp[1], cp[2], cp[3], lsz, nsps);
            let sl = be16(&cp[6..]) as usize;
            let sps_nal = &cp[8..8 + sl];
            let rb = unesc(&sps_nal[1..]);
            let s = h264_reader::nal::sps::SeqParameterSet::from_bits(h264_reader::rbsp::BitReader::new(&rb[..])).map_err(|e| format!("sps: {e:?}"))?;
            println!("SPS: profile_idc={:?} level_idc={} level={:?} dims={:?} chroma={:?}", s.profile_idc, s.level_idc, s.level(), s.pixel_dimensions().map_err(|e| format!("{e:?}"))?, s.chroma_info);
            println!("SPS VUI: {:?}", s.vui_parameters.as_ref().map(|v| format!("{:?}", v)).unwrap_or_default().chars().take(400).collect::<String>());
            let mut kinds = std::collections::BTreeMap::new();
            for n in nals(&fr.data, lsz) {
                let t = n[0] & 0x1f;
                *kinds.entry(t).or_insert(0) += 1;
                if t == 6 { sei(&unesc(&n[1..]), "frame0-nal6"); }
            }
            println!("frame0 AVC NAL types (type:count): {:?}", kinds);
        }
        "V_AV1" => {
            let mut obus: Vec<u8> = cp.get(4..).unwrap_or(&[]).to_vec();
            // Walk low-overhead OBUs in the first TU; keep sequence header (type 1).
            let d = &fr.data;
            let (mut p, mut types) = (0usize, vec![]);
            while p < d.len() {
                let t = (d[p] >> 3) & 0xf; let ext = (d[p] >> 2) & 1; let hs = (d[p] >> 1) & 1;
                let mut hl = 1 + ext as usize;
                let (sz, n) = if hs == 1 { leb128(&d[p + hl..]) } else { ((d.len() - p - hl) as u64, 0) };
                hl += n;
                types.push(t);
                if t == 1 { obus.extend_from_slice(&d[p..p + hl + sz as usize]); }
                p += hl + sz as usize;
            }
            println!("frame0 AV1 OBU types: {:?}", types);
            let rec = scuffle_av1::AV1CodecConfigurationRecord::demux(&mut Cursor::new(bytes::Bytes::copy_from_slice(&cp)))?;
            println!("av1C: profile={} level_idx0={} tier0={} high_bitdepth={} mono={} sub_x={} sub_y={} config_obu_len={}",
                rec.seq_profile, rec.seq_level_idx_0, rec.seq_tier_0, rec.high_bitdepth, rec.monochrome,
                rec.chroma_subsampling_x, rec.chroma_subsampling_y, rec.config_obu.len());
            let mut q = 0usize;
            while q < obus.len() {
                let mut c = &obus[q..];
                let h = scuffle_av1::ObuHeader::parse(&mut c)?;
                let sz = h.size.map(|s| s as usize).unwrap_or(c.len());
                if matches!(h.obu_type, scuffle_av1::ObuType::SequenceHeader) {
                    let s = scuffle_av1::seq::SequenceHeaderObu::parse(h, &mut &c[..sz])?;
                    let cc = &s.color_config;
                    println!("AV1 seq: profile={} max_w={} max_h={} bit_depth={} mono={} primaries={} transfer={} matrix={} full_range={} subsampling=({},{}) timing={:?}",
                        s.seq_profile, s.max_frame_width, s.max_frame_height, cc.bit_depth, cc.mono_chrome, cc.color_primaries,
                        cc.transfer_characteristics, cc.matrix_coefficients, cc.full_color_range, cc.subsampling_x, cc.subsampling_y, s.timing_info);
                    break;
                }
                q += (obus.len() - q) - c.len() + sz;
            }
        }
        other => println!("unsupported codec {other}"),
    }
    Ok(())
}
