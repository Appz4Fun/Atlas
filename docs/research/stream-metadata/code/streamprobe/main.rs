// streamprobe: front-bytes metadata probe for MKV (and raw AU) streams.
// Reads only the first N bytes (default 4 MiB). Combines w05a (MKV/HEVC/AVC/AV1 + SEI),
// w06a/w07c (DV RPU + HDR10+), w07a (AC-3/E-AC-3 BSI), w06b (TrueHD major sync),
// w05c (DTS core, FLAC STREAMINFO). Output: one JSON object on stdout.
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock as B;
use hdr10plus::metadata::Hdr10PlusMetadata;
use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

fn nz<T: std::fmt::Display>(x: T) -> u64 { x.to_string().parse().unwrap_or(0) }

fn unesc(b: &[u8]) -> Vec<u8> {
    let (mut o, mut z) = (Vec::with_capacity(b.len()), 0);
    for &x in b {
        if z >= 2 && x == 3 { z = 0; continue; }
        o.push(x);
        z = if x == 0 { z + 1 } else { 0 };
    }
    o
}

// Safe SEI walk (header stripped, escapes removed). Returns (payload_type, payload).
fn sei_msgs(rbsp: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let mut out = vec![];
    let mut p = 0usize;
    while p + 2 <= rbsp.len() && rbsp[p] != 0x80 {
        let mut pt = 0usize;
        loop { let Some(&b) = rbsp.get(p) else { return out }; p += 1; pt += b as usize; if b != 0xff { break; } }
        let mut ps = 0usize;
        loop { let Some(&b) = rbsp.get(p) else { return out }; p += 1; ps += b as usize; if b != 0xff { break; } }
        let end = (p + ps).min(rbsp.len());
        out.push((pt, rbsp[p.min(end)..end].to_vec()));
        p += ps;
    }
    out
}

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

fn leb128(b: &[u8]) -> Option<(u64, usize)> {
    let (mut v, mut i) = (0u64, 0usize);
    loop { let x = *b.get(i)?; v |= ((x & 0x7f) as u64) << (7 * i); i += 1; if x & 0x80 == 0 || i == 8 { return Some((v, i)); } }
}

fn encoder_from(msgs: &[(usize, Vec<u8>)]) -> Option<String> {
    for (pt, pl) in msgs {
        if *pt == 5 && pl.len() > 16 {
            let t = String::from_utf8_lossy(&pl[16..]).split('\0').next().unwrap_or("").trim().to_string();
            if t.starts_with("x264") || t.starts_with("x265") { return Some(t); }
        }
    }
    None
}

fn hevc_profile(p: u8) -> String {
    match p { 1 => "Main".into(), 2 => "Main 10".into(), 3 => "Main Still Picture".into(), 4 => "Range Extensions".into(), x => x.to_string() }
}
fn hevc_level(l: u8) -> String { format!("{}.{}", l / 30, (l % 30) / 3) }
fn avc_profile(p: u8) -> String {
    match p { 66 => "Baseline".into(), 77 => "Main".into(), 88 => "Extended".into(), 100 => "High".into(),
        110 => "High 10".into(), 122 => "High 4:2:2".into(), 244 => "High 4:4:4 Predictive".into(), x => x.to_string() }
}
fn chroma_name(c: u8) -> &'static str { match c { 0 => "4:0:0", 1 => "4:2:0", 2 => "4:2:2", 3 => "4:4:4", _ => "unknown" } }

// ---------- DV ----------
fn dv_block(rpu_nal: &[u8]) -> Result<Value, String> {
    let rpu = DoviRpu::parse_unspec62_nalu(rpu_nal).map_err(|e| format!("{e:?}"))?;
    let vdr = rpu.vdr_dm_data.as_ref();
    let blk = |lvl: u8| vdr.map(|v| v.level_blocks_iter(lvl).collect::<Vec<_>>()).unwrap_or_default();
    let has = |lv: u8| vdr.map_or(false, |v| v.level_blocks_iter(lv).next().is_some());
    let cm = if [3u8, 8, 9, 10, 11, 254].iter().any(|&l| has(l)) { json!("4.0") } else if has(1) || has(2) || has(5) || has(6) { json!("2.9") } else { Value::Null };
    let el = match &rpu.el_type { None => "none".to_string(), Some(e) => format!("{e:?}").to_lowercase() };
    let mut l6 = Value::Null; let mut l5 = Value::Null; let mut l1 = Value::Null;
    let mut l2: Vec<u16> = vec![]; let mut l8: Vec<u8> = vec![];
    let mut l9 = Value::Null; let mut l11 = Value::Null; let mut l254 = Value::Null;
    for b in blk(1) { if let B::Level1(x) = b { l1 = json!({"min_pq": x.min_pq, "max_pq": x.max_pq, "avg_pq": x.avg_pq}); break; } }
    for b in blk(2) { if let B::Level2(x) = b { l2.push(x.target_max_pq); } }
    for b in blk(5) { if let B::Level5(x) = b { l5 = json!({"left": x.active_area_left_offset, "right": x.active_area_right_offset,
        "top": x.active_area_top_offset, "bottom": x.active_area_bottom_offset}); break; } }
    for b in blk(6) { if let B::Level6(x) = b { l6 = json!({"max_display_mastering_nits": x.max_display_mastering_luminance,
        "min_display_mastering_nits_raw": x.min_display_mastering_luminance, "max_cll": x.max_content_light_level,
        "max_fall": x.max_frame_average_light_level}); break; } }
    for b in blk(8) { if let B::Level8(x) = b { l8.push(x.target_display_index); } }
    for b in blk(9) { if let B::Level9(x) = b { l9 = json!(x.source_primary_index); break; } }
    for b in blk(11) { if let B::Level11(x) = b { l11 = json!({"content_type": x.content_type, "reference_mode": x.reference_mode_flag}); break; } }
    for b in blk(254) { if let B::Level254(x) = b { l254 = json!(x.dm_mode); break; } }
    Ok(json!({
        "profile": rpu.dovi_profile.to_string(),
        "el": el,
        "cm_version": cm,
        "source_min_pq": vdr.map(|v| v.source_min_pq), "source_max_pq": vdr.map(|v| v.source_max_pq),
        "l1": l1, "l2_target_max_pq": l2, "l5": l5, "l6": l6, "l8_target_display_index": l8, "l9": l9, "l11": l11, "l254_dm_mode": l254,
    }))
}

// ---------- HDR10+ ----------
fn hdr10plus_block(frame_nals: &[&[u8]]) -> Value {
    for n in frame_nals.iter().filter(|n| n.len() > 2 && (n[0] >> 1) & 0x3f == 39) {
        for (pt, pl) in sei_msgs(&unesc(&n[2..])) {
            if pt != 4 || pl.len() < 6 || pl[0] != 0xB5 || pl[1..3] != [0x00, 0x3C] || pl[3..5] != [0x00, 0x01] { continue; }
            if let Ok(m) = Hdr10PlusMetadata::parse(&pl) {
                return json!({"profile": m.profile, "application_version": m.application_version,
                    "num_windows": m.num_windows,
                    "targeted_system_display_maximum_luminance": m.targeted_system_display_maximum_luminance,
                    "frame0_average_maxrgb": m.average_maxrgb, "frame0_maxscl": m.maxscl});
            }
        }
    }
    Value::Null
}

// ---------- AC-3 / E-AC-3 (A/52 5.4.1, E.1.3) ----------
struct Bits<'a> { d: &'a [u8], pos: usize }
impl<'a> Bits<'a> {
    fn get(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        v
    }
}
const AC3_KBPS: [u32; 19] = [32,40,48,56,64,80,96,112,128,160,192,224,256,320,384,448,512,576,640];
const AC3_WORDS: [[u32; 38]; 3] = [
    [64,64,80,80,96,96,112,112,128,128,160,160,192,192,224,224,256,256,320,320,384,384,448,448,512,512,640,640,768,768,896,896,1024,1024,1152,1152,1280,1280],
    [69,70,87,88,104,105,121,122,139,140,174,175,208,209,243,244,278,279,348,349,417,418,487,488,557,558,696,697,835,836,975,976,1114,1115,1253,1254,1393,1394],
    [96,96,120,120,144,144,168,168,192,192,240,240,288,288,336,336,384,384,480,480,576,576,672,672,768,768,960,960,1152,1152,1344,1344,1536,1536,1728,1728,1920,1920],
];
const AC3_SR: [u32; 3] = [48000, 44100, 32000];
const ACMOD_CH: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
// E-AC-3 chanmap bits (Table E.2.5): bit, channels
const CHANMAP_W: [(u32, u32); 11] = [(15, 2), (14, 2), (13, 1), (12, 1), (11, 2), (10, 2), (9, 2), (8, 1), (7, 2), (6, 1), (5, 1)];

fn dial_db(d: u64) -> i64 { if d == 0 { -31 } else { -(d as i64) } }

// Walk all AC-3/E-AC-3 syncframes in one MKV block.
fn ac3_block(f: &[u8]) -> Option<Value> {
    let (mut p, mut codec, mut sr, mut ch, mut bytes, mut dial, mut bsid, mut bsmod) = (0usize, "", 0u32, 0u32, 0usize, None::<i64>, 0u64, 0u64);
    let mut dep_w = 0u32; let mut eac3 = false; let mut ac3_kbps = 0u32; let mut sec = 0.0f64;
    let mut indep_samples = 0u32;
    while p + 8 <= f.len() && f[p] == 0x0B && f[p + 1] == 0x77 {
        let mut b = Bits { d: &f[p + 2..], pos: 0 };
        let bsid_here = (f[p + 5] >> 3) as u64;
        if bsid_here <= 8 {
            // AC-3
            b.get(16); // crc1
            let fscod = b.get(2) as usize; let frmsizecod = b.get(6) as usize; let bid = b.get(5); let bm = b.get(3);
            let acmod = b.get(3) as usize;
            if acmod & 1 != 0 && acmod != 1 { b.get(2); }
            if acmod & 4 != 0 { b.get(2); }
            if acmod == 2 { b.get(2); }
            let lfe = b.get(1); let dn = b.get(5);
            let fs = fscod.min(2);
            let fb = AC3_WORDS[fs][frmsizecod.min(37)] as usize * 2;
            if fb == 0 { break; }
            if dial.is_none() { dial = Some(dial_db(dn)); codec = "ac3"; sr = AC3_SR[fs]; ch = ACMOD_CH[acmod & 7] + lfe as u32; ac3_kbps = AC3_KBPS[(frmsizecod / 2).min(18)]; bsid = bid; bsmod = bm; }
            bytes += fb; p += fb;
        } else {
            // E-AC-3 substream
            eac3 = true;
            let strmtyp = b.get(2); let _sid = b.get(3); let frmsiz = b.get(11) as usize;
            let fscod = b.get(2);
            let (fsr, numblks) = if fscod == 3 { let f2 = b.get(2); ([24000u32, 22050, 16000].get(f2 as usize).copied().unwrap_or(0), 6u32) }
                else { let nb = b.get(2); ([48000u32, 44100, 32000][fscod as usize & 3], [1u32, 2, 3, 6][nb as usize]) };
            let acmod = b.get(3) as usize; let lfe = b.get(1); let _bid = b.get(5); let dn = b.get(5);
            let fb = (frmsiz + 1) * 2;
            if fb == 0 { break; }
            if strmtyp == 0 && dial.is_none() {
                dial = Some(dial_db(dn)); codec = "eac3"; sr = fsr; ch = ACMOD_CH[acmod & 7] + lfe as u32; bsid = bid_or(bsid_here); bsmod = 0;
                indep_samples = numblks * 256;
            } else if strmtyp == 1 {
                // dependent substream: chanmap, if present, adds channels
                if b.get(1) == 1 { let m = b.get(16); for (bit, w) in CHANMAP_W.iter() { if m >> bit & 1 == 1 && *bit != 5 && *bit != 6 { dep_w += w; } } }
            }
            if indep_samples == 0 && strmtyp == 0 { indep_samples = numblks * 256; }
            sec += numblks as f64 * 256.0 / fsr.max(1) as f64;
            bytes += fb; p += fb;
        }
    }
    let dial = dial?;
    let ch_total = if eac3 { ch + dep_w } else { ch };
    let bitrate_bps = if eac3 {
        if indep_samples == 0 { 0 } else { (bytes as u64 * 8 * sr as u64) / (indep_samples as u64 * 0 + (sec * sr as f64).round().max(1.0) as u64) }
    } else { ac3_kbps as u64 * 1000 };
    let _ = (bsid, bsmod);
    Some(json!({"codec": codec, "channels": ch_total, "sample_rate": sr, "bitrate": bitrate_bps,
        "dialnorm": dial, "atmos": if eac3 { Value::Null } else { json!(false) }, "flac_md5": Value::Null}))
}
fn bid_or(x: u64) -> u64 { x }

// ---------- TrueHD / MLP (w06b) ----------
const THD_CH: [(u32, &str); 13] = [(2,"L R"),(1,"C"),(1,"LFE"),(2,"Ls Rs"),(2,"Ltop Rtop"),(2,"Lc Rc"),(2,"Lrs Rrs"),(1,"Cs"),(1,"Ts"),(2,"Lsd Rsd"),(2,"Lw Rw"),(1,"Cvh"),(1,"LFE2")];
fn chanmap_channels(m: u32) -> u32 { (0..13).map(|i| THD_CH[i].0 * ((m >> i) & 1)).sum() }
fn chanmap_names(m: u32) -> String { (0..13).filter(|i| (m >> i) & 1 == 1).map(|i| THD_CH[i].1).collect::<Vec<_>>().join(" ") }

fn truehd_block(f: &[u8]) -> Option<Value> {
    let s = f.windows(4).position(|w| w == [0xF8, 0x72, 0x6F, 0xBA])?;
    let b = f.get(s..)?;
    if b.len() < 28 { return None; }
    let mut r = Bits { d: b, pos: 32 };
    let ratebits = r.get(4) as u32; r.get(4);
    r.get(2); r.get(2);
    let arr6 = r.get(5) as u32; r.get(2);
    let arr8 = r.get(13) as u32;
    r.pos += 48;
    let is_vbr = r.get(1);
    let peak_field = r.get(15);
    let sr: u32 = if ratebits & 8 != 0 { 44100 << (ratebits & 7) } else { 48000 << (ratebits & 7) };
    let num_sub = r.get(4); r.get(2); let _ext = r.get(2); let sub_info = r.get(8);
    let samples = 40u64 << (ratebits & 7);
    let ch = if arr8 != 0 { chanmap_channels(arr8) } else { chanmap_channels(arr6) };
    let bitrate = f.len() as u64 * 8 * sr as u64 / samples;
    let atmos = num_sub == 4 && (sub_info >> 7) == 1;
    Some(json!({"codec": "truehd", "channels": ch, "channel_layout": if arr8 != 0 { chanmap_names(arr8) } else { chanmap_names(arr6) },
        "sample_rate": sr, "bitrate": bitrate, "peak_bitrate": ((peak_field as u64) * (sr as u64) + 8) >> 4, "is_vbr": is_vbr,
        "num_substreams": num_sub, "dialnorm": Value::Null, "atmos": atmos, "flac_md5": Value::Null}))
}

// ---------- DTS core (w05c) ----------
const DTS_SFREQ: [u32; 16] = [0,8000,16000,32000,0,0,11025,22050,44100,0,0,12000,24000,48000,0,0];
const DTS_RATE: [u32; 25] = [32000,56000,64000,96000,112000,128000,192000,224000,256000,320000,384000,448000,512000,576000,640000,768000,896000,1024000,1152000,1280000,1344000,1408000,1411200,1472000,1536000];
const DTS_CH: [u32; 16] = [1,2,2,2,2,3,3,4,4,5,6,6,6,7,8,8];
fn dts_block(f: &[u8]) -> Option<Value> {
    let s = f.windows(4).position(|w| w == [0x7F, 0xFE, 0x80, 0x01])?;
    let g = f.get(s + 4..s + 16)?;
    let mut b = Bits { d: g, pos: 0 };
    b.get(1); b.get(5); b.get(1); b.get(7); b.get(14);
    let amode = b.get(6) as usize; let sfc = b.get(4) as usize; let rc = b.get(5) as usize;
    b.get(1 + 1 + 1 + 1 + 1 + 3 + 1 + 1);
    let lff = b.get(2);
    let ch = DTS_CH.get(amode).copied().unwrap_or(0) + if lff != 0 { 1 } else { 0 };
    Some(json!({"codec": "dts", "channels": ch, "sample_rate": DTS_SFREQ[sfc & 15], "bitrate": DTS_RATE.get(rc).copied().unwrap_or(0),
        "dialnorm": Value::Null, "atmos": false, "flac_md5": Value::Null}))
}

// ---------- FLAC STREAMINFO (w05c) ----------
fn flac_info(cp: &[u8]) -> Option<Value> {
    if cp.len() < 42 || &cp[0..4] != b"fLaC" { return None; }
    let s = &cp[8..42];
    let mut b = Bits { d: s, pos: 0 };
    b.get(16); b.get(16); b.get(24); b.get(24);
    let sr = b.get(20); let ch = b.get(3) + 1; let bps = b.get(5) + 1;
    let md5: String = s[18..34].iter().map(|x| format!("{:02x}", x)).collect();
    Some(json!({"codec": "flac", "channels": ch, "sample_rate": sr, "bit_depth": bps, "bitrate": Value::Null,
        "dialnorm": Value::Null, "atmos": false, "flac_md5": md5}))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut path = None; let mut front: u64 = 4 << 20;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--front-bytes" {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            let (num, mul) = if let Some(x) = v.strip_suffix("MiB") { (x, 1u64 << 20) } else if let Some(x) = v.strip_suffix("KiB") { (x, 1 << 10) }
                else if let Some(x) = v.strip_suffix('M') { (x, 1 << 20) } else if let Some(x) = v.strip_suffix('K') { (x, 1 << 10) } else { (v.as_str(), 1) };
            front = num.parse::<u64>().expect("bad --front-bytes") * mul;
            i += 2;
        } else { path = Some(args[i].clone()); i += 1; }
    }
    let path = path.expect("usage: streamprobe <file> [--front-bytes N]");
    let mut head = Vec::new();
    std::fs::File::open(&path).and_then(|f| f.take(front).read_to_end(&mut head)).expect("read");
    let real_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(head.len() as u64);
    let out = probe(&head, front, real_len).unwrap_or_else(|e| json!({"error": e}));
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

struct PadReader { d: Vec<u8>, pos: u64, len: u64 }
impl std::io::Read for PadReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.len.saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() { *b = self.d.get(self.pos as usize + i).copied().unwrap_or(0); }
        self.pos += n as u64; Ok(n)
    }
}
impl std::io::Seek for PadReader {
    fn seek(&mut self, f: std::io::SeekFrom) -> std::io::Result<u64> {
        self.pos = match f { std::io::SeekFrom::Start(x) => x, std::io::SeekFrom::Current(x) => (self.pos as i64 + x) as u64, std::io::SeekFrom::End(x) => (self.len as i64 + x) as u64 };
        Ok(self.pos)
    }
}

fn probe(head: &[u8], front: u64, real_len: u64) -> Result<Value, String> {
    // Reads past the front window return zeros, so SeekHead targets beyond it (Cues/Tags at EOF) do not abort open().
    let mut mkv = MatroskaFile::open(PadReader { d: head.to_vec(), pos: 0, len: real_len }).map_err(|e| format!("mkv open: {e:?}"))?;
    let scale = mkv.info().timestamp_scale().get() as f64;
    let duration_s = mkv.info().duration().map(|d| d * scale / 1e9);
    let mut container = json!({"format": "matroska", "duration_s": duration_s, "muxing_app": mkv.info().muxing_app(),
        "writing_app": mkv.info().writing_app(), "front_bytes": front, "bytes_read": head.len()});
    let chapters: Vec<Value> = mkv.chapters().map(|eds| eds.iter().flat_map(|e| e.chapter_atoms().iter().map(|a| {
        json!({"start_ns": a.time_start(), "title": a.displays().first().map(|d| d.string().to_string())})
    })).collect()).unwrap_or_default();
    let ntags = mkv.tags().map_or(0, |t| t.len());
    let subs: Vec<Value> = mkv.tracks().iter().filter(|t| t.track_type() == TrackType::Subtitle)
        .map(|t| json!({"track": t.track_number().get(), "codec": t.codec_id(), "language": t.language(), "name": t.name()})).collect();
    container["chapters"] = json!(chapters.len());
    container["chapter_list"] = json!(chapters);
    container["tags_count"] = json!(ntags);
    container["subtitles"] = json!(subs);

    // Track descriptions (owned copies, so next_frame can borrow mkv mutably).
    struct VT { no: u64, codec: String, cp: Vec<u8>, w: u64, h: u64, dd_ns: Option<u64>, lang: Option<String> }
    struct AT { no: u64, codec: String, cp: Vec<u8>, sr: u64, ch: u64, name: Option<String>, lang: Option<String> }
    let vt = mkv.tracks().iter().find(|t| t.track_type() == TrackType::Video).ok_or("no video track")?;
    let v = VT { no: vt.track_number().get(), codec: vt.codec_id().to_string(), cp: vt.codec_private().map(|c| c.to_vec()).unwrap_or_default(),
        w: nz(vt.video().ok_or("no video element")?.pixel_width()), h: nz(vt.video().unwrap().pixel_height()),
        dd_ns: vt.default_duration().map(|d| nz(d)), lang: vt.language().map(|s| s.to_string()) };
    let ats: Vec<AT> = mkv.tracks().iter().filter(|t| t.track_type() == TrackType::Audio).map(|t| {
        let a = t.audio().unwrap();
        AT { no: t.track_number().get(), codec: t.codec_id().to_string(), cp: t.codec_private().map(|c| c.to_vec()).unwrap_or_default(),
            sr: a.sampling_frequency() as u64, ch: nz(a.channels()), name: t.name().map(|s| s.to_string()), lang: t.language().map(|s| s.to_string()) }
    }).collect();
    let mkv_colour = mkv.tracks().iter().find(|t| t.track_type() == TrackType::Video).and_then(|t| t.video()).and_then(|x| x.colour()).map(|c| {
        json!({"primaries": format!("{:?}", c.primaries()), "max_cll": c.max_cll(), "max_fall": c.max_fall(),
               "mastering_max_nits": c.mastering_metadata().and_then(|m| m.luminance_max()), "mastering_min_nits": c.mastering_metadata().and_then(|m| m.luminance_min())})
    });
    let dd = v.dd_ns;

    // Read frames until: first video keyframe found and one frame per audio track collected, or head exhausted.
    let mut vkey: Option<Vec<u8>> = None;
    let mut afirst: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut fr = Frame::from(Vec::new());
    loop {
        match mkv.next_frame(&mut fr) { Ok(true) => {}, _ => break }
        if fr.track == v.no && vkey.is_none() && fr.is_keyframe != Some(false) { vkey = Some(fr.data.clone()); }
        else if ats.iter().any(|a| a.no == fr.track) && !afirst.contains_key(&fr.track) { afirst.insert(fr.track, fr.data.clone()); }
        if vkey.is_some() && afirst.len() == ats.len() { break; }
    }
    let key = vkey.ok_or("no video keyframe within front bytes")?;

    // ---- video ----
    let mut video = json!({"track": v.no, "codec_id": v.codec, "width": v.w, "height": v.h, "language": v.lang,
        "fps": dd.map(|ns| (1e9 / ns as f64 * 1000.0).round() / 1000.0), "fps_source": if dd.is_some() { "mkv_default_duration" } else { "none" },
        "mkv_colour": mkv_colour});
    let mut hdr_formats: Vec<&str> = vec![];
    let mut encoder: Option<String> = None;
    let mut dv = Value::Null; let mut hdr10p = Value::Null;
    let mut sei_all: Vec<(usize, Vec<u8>)> = vec![];
    let mut mastering = Value::Null; let mut max_cll = Value::Null; let mut max_fall = Value::Null;
    let (mut transfer, mut primaries): (Option<u8>, Option<u8>) = (None, None);
    let mut matrix: Option<u8> = None;

    if v.codec == "V_MPEGH/ISO/HEVC" {
        let cfg = scuffle_h265::HEVCDecoderConfigurationRecord::demux(&v.cp[..]).map_err(|e| format!("hvcC: {e:?}"))?;
        let mut sps_info = Value::Null;
        for arr in &cfg.arrays {
            let nm = format!("{:?}", arr.nal_unit_type);
            for n in &arr.nalus {
                if nm.contains("Sps") {
                    let s = scuffle_h265::SpsNALUnit::parse(&n[..]).map_err(|e| format!("sps: {e:?}"))?.rbsp;
                    let vs = &s.vui_parameters.as_ref().map(|x| x.video_signal_type.clone());
                    let (full, pr, tr, mx) = match vs { Some(t) => (Some(t.video_full_range_flag), Some(t.colour_primaries), Some(t.transfer_characteristics), Some(t.matrix_coeffs)), None => (None, None, None, None) };
                    primaries = pr; transfer = tr; matrix = mx;
                    let timing = s.vui_parameters.as_ref().and_then(|x| x.vui_timing_info.as_ref()).map(|t| json!({"num_units_in_tick": nz(t.num_units_in_tick), "time_scale": nz(t.time_scale)}));
                    sps_info = json!({"sps_w": s.cropped_width(), "sps_h": s.cropped_height(), "bit_depth_y": s.bit_depth_y(), "bit_depth_c": s.bit_depth_c(),
                        "chroma_idc": s.chroma_format_idc, "full_range": full, "primaries": pr, "transfer": tr, "matrix": mx, "vui_timing": timing});
                }
                if nm.contains("Sei") { sei_all.extend(sei_msgs(&unesc(&n[2..]))); }
            }
        }
        let lsz = cfg.length_size_minus_one as usize + 1;
        let fnals = nals(&key, lsz);
        for n in &fnals {
            if n.len() < 3 { continue; }
            let t = (n[0] >> 1) & 0x3f;
            if t == 39 || t == 40 { sei_all.extend(sei_msgs(&unesc(&n[2..]))); }
        }
        if let Some(r) = fnals.iter().find(|n| n.len() > 2 && (n[0] >> 1) & 0x3f == 62) { dv = dv_block(r).map(|x| x).unwrap_or_else(|e| json!({"error": e})); }
        hdr10p = hdr10plus_block(&fnals);
        for (pt, pl) in &sei_all {
            if *pt == 137 && pl.len() >= 24 {
                let be16 = |i: usize| u16::from_be_bytes([pl[i], pl[i + 1]]) as f64 / 50000.0;
                let be32 = |i: usize| u32::from_be_bytes([pl[i], pl[i + 1], pl[i + 2], pl[i + 3]]);
                mastering = json!({"green": [be16(0), be16(2)], "blue": [be16(4), be16(6)], "red": [be16(8), be16(10)], "white": [be16(12), be16(14)],
                    "max_nits": be32(16) as f64 / 10000.0, "min_nits": be32(20) as f64 / 10000.0, "source": "sei137"});
            }
            if *pt == 144 && pl.len() >= 4 { max_cll = json!(u16::from_be_bytes([pl[0], pl[1]])); max_fall = json!(u16::from_be_bytes([pl[2], pl[3]])); }
        }
        encoder = encoder_from(&sei_all);
        let (prof, lvl) = (cfg.general_profile_idc, cfg.general_level_idc);
        video["codec"] = json!("hevc");
        video["profile"] = json!(hevc_profile(prof));
        video["level"] = json!(hevc_level(lvl));
        video["bit_depth"] = json!(cfg.bit_depth_luma_minus8 as u32 + 8);
        video["chroma"] = json!(chroma_name(cfg.chroma_format_idc));
        video["hvcc_avg_frame_rate"] = json!(cfg.avg_frame_rate as f64 / 256.0);
        video["sps"] = sps_info;
    } else if v.codec == "V_MPEG4/ISO/AVC" {
        let lsz = (v.cp[4] & 3) as usize + 1;
        let sl = u16::from_be_bytes([v.cp[6], v.cp[7]]) as usize;
        let rb = unesc(&v.cp[9..8 + sl]);
        let s = h264_reader::nal::sps::SeqParameterSet::from_bits(h264_reader::rbsp::BitReader::new(&rb[..])).map_err(|e| format!("sps: {e:?}"))?;
        let (w, h) = s.pixel_dimensions().map_err(|e| format!("{e:?}"))?;
        let cf: u8 = match format!("{:?}", s.chroma_info.chroma_format).as_str() { x if x.contains("444") => 3, x if x.contains("422") => 2, x if x.contains("420") => 1, x if x.contains("onochrome") || x.contains("400") => 0, _ => 9 };
        video["codec"] = json!("h264");
        video["profile"] = json!(avc_profile(s.profile().profile_idc()));
        video["level"] = json!(format!("{}.{}", s.level_idc / 10, s.level_idc % 10));
        video["width"] = json!(w); video["height"] = json!(h);
        video["bit_depth"] = json!(s.chroma_info.bit_depth_luma_minus8 as u32 + 8);
        video["chroma"] = json!(chroma_name(cf));
        if let Some(f) = s.fps() { video["fps"] = json!((f * 1000.0).round() / 1000.0); video["fps_source"] = json!("sps_vui_or_mkv"); }
        if let Some(vui) = &s.vui_parameters {
            if let Some(t) = &vui.video_signal_type { video["full_range"] = json!(t.video_full_range_flag);
                if let Some(c) = &t.colour_description { primaries = Some(c.colour_primaries); transfer = Some(c.transfer_characteristics); matrix = Some(c.matrix_coefficients); } }
        }
        for n in nals(&key, lsz) { if n.len() > 1 && n[0] & 0x1f == 6 { sei_all.extend(sei_msgs(&unesc(&n[1..]))); } }
        encoder = encoder_from(&sei_all);
        for (pt, pl) in &sei_all { if *pt == 137 && pl.len() >= 24 { let be16 = |i: usize| u16::from_be_bytes([pl[i], pl[i + 1]]) as f64 / 50000.0;
            let be32 = |i: usize| u32::from_be_bytes([pl[i], pl[i + 1], pl[i + 2], pl[i + 3]]);
            mastering = json!({"green": [be16(0), be16(2)], "blue": [be16(4), be16(6)], "red": [be16(8), be16(10)], "white": [be16(12), be16(14)],
                "max_nits": be32(16) as f64 / 10000.0, "min_nits": be32(20) as f64 / 10000.0, "source": "sei137"}); } }
        for (pt, pl) in &sei_all { if *pt == 144 && pl.len() >= 4 { max_cll = json!(u16::from_be_bytes([pl[0], pl[1]])); max_fall = json!(u16::from_be_bytes([pl[2], pl[3]])); } }
    } else if v.codec == "V_AV1" {
        let rec = scuffle_av1::AV1CodecConfigurationRecord::demux(&mut Cursor::new(bytes::Bytes::copy_from_slice(&v.cp)))
            .map_err(|e| format!("av1C: {e:?}"))?;
        video["codec"] = json!("av1");
        video["profile"] = json!(match rec.seq_profile { 0 => "Main".to_string(), 1 => "High".into(), 2 => "Professional".into(), x => x.to_string() });
        let lv = rec.seq_level_idx_0;
        video["level"] = json!(format!("{}.{}", 2 + lv / 4, lv % 4));
        video["chroma"] = json!(match (rec.chroma_subsampling_x, rec.chroma_subsampling_y) { (true, true) => "4:2:0", (true, false) => "4:2:2", (false, false) => "4:4:4", _ => "unknown" });
        // Sequence header inside the first TU.
        let d = &key; let mut p = 0usize;
        while p < d.len() {
            let t = (d[p] >> 3) & 0xf; let ext = (d[p] >> 2) & 1; let hs = (d[p] >> 1) & 1;
            let mut hl = 1 + ext as usize;
            let (sz, nn) = if hs == 1 { match leb128(&d[(p + hl).min(d.len())..]) { Some(x) => x, None => break } } else { ((d.len() - p - hl) as u64, 0) };
            hl += nn;
            let end = (p + hl + sz as usize).min(d.len());
            if t == 1 {
                let mut c = &d[p..end];
                if let Ok(h) = scuffle_av1::ObuHeader::parse(&mut c) {
                    let sz2 = h.size.map(|s| s as usize).unwrap_or(c.len()).min(c.len());
                    if let Ok(s) = scuffle_av1::seq::SequenceHeaderObu::parse(h, &mut &c[..sz2]) {
                        let cc = &s.color_config;
                        video["width"] = json!(s.max_frame_width); video["height"] = json!(s.max_frame_height);
                        video["bit_depth"] = json!(cc.bit_depth); video["full_range"] = json!(cc.full_color_range);
                        primaries = Some(cc.color_primaries); transfer = Some(cc.transfer_characteristics);
                        matrix = Some(cc.matrix_coefficients);
                        video["av1_seq_profile"] = json!(s.seq_profile);
                    }
                }
                break;
            }
            p = end;
        }
    } else {
        return Err(format!("unsupported video codec {}", v.codec));
    }

    // Shared: colour + HDR format labels.
    video["primaries"] = json!(primaries);
    video["transfer"] = json!(transfer);
    video["matrix"] = json!(matrix);
    if transfer == Some(16) { hdr_formats.push("HDR10"); }
    if transfer == Some(18) { hdr_formats.push("HLG"); }
    if hdr10p != Value::Null { hdr_formats.push("HDR10+"); }
    if dv != Value::Null { hdr_formats.push("Dolby Vision"); }
    if hdr_formats.is_empty() { hdr_formats.push("SDR"); }
    // dvcC box (BlockAdditionMapping) for container-level DV profile.
    let dvcc = head.windows(4).position(|w| w == b"dvcC").and_then(|i| head.get(i + 4..i + 9)).map(|b| {
        let v16 = u16::from_be_bytes([b[2], b[3]]);
        json!({"profile": v16 >> 9, "level": (v16 >> 3) & 0x3f, "rpu": (v16 >> 2) & 1, "el": (v16 >> 1) & 1, "bl": v16 & 1, "bl_compat": b[4] >> 4})
    });
    let mut dv_out = dv.clone();
    if let (Some(d), Some(c)) = (dv_out.as_object_mut(), dvcc) { d.insert("dvcc".into(), c); }
    if dv != Value::Null && dv_out.get("dvcc").is_none() { dv_out["dvcc"] = Value::Null; }
    video["hdr_formats"] = json!(hdr_formats);
    video["mastering"] = mastering;
    video["max_cll"] = max_cll;
    video["max_fall"] = max_fall;
    video["encoder"] = json!(encoder);
    video["dv"] = if dv == Value::Null { Value::Null } else { dv_out };
    video["hdr10plus"] = hdr10p;

    // ---- audio ----
    let mut audio = vec![];
    for a in &ats {
        let mut info = match afirst.get(&a.no) {
            None => json!({"codec": a.codec, "channels": a.ch, "sample_rate": a.sr, "bitrate": Value::Null, "dialnorm": Value::Null, "atmos": Value::Null, "flac_md5": Value::Null}),
            Some(f) => {
                let parsed = match a.codec.as_str() {
                    "A_AC3" | "A_EAC3" => ac3_block(f),
                    "A_TRUEHD" | "A_MLP" => truehd_block(f),
                    "A_DTS" => dts_block(f),
                    "A_FLAC" => flac_info(&a.cp),
                    _ => None,
                };
                parsed.unwrap_or_else(|| json!({"codec": a.codec, "channels": a.ch, "sample_rate": a.sr, "bitrate": Value::Null,
                    "dialnorm": Value::Null, "atmos": Value::Null, "flac_md5": Value::Null}))
            }
        };
        info["track"] = json!(a.no);
        info["codec_id"] = json!(a.codec);
        info["language"] = json!(a.lang);
        info["name"] = json!(a.name);
        audio.push(info);
    }
    Ok(json!({"container": container, "video": video, "audio": audio}))
}
