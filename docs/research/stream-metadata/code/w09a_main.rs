// w09a: validate dolby_vision 3.4.0 on dovi_tool test RPUs (.bin = RPU w/ 00 00 00 01 + 0x19; .hevc/.265 = Annex B, NAL 62 walked).
// Usage: w09a_main <file>...   Prints one block per file: per-RPU parse errors, first-RPU fields, cross-RPU stability.
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock as B;
use dolby_vision::rpu::extension_metadata::{DmData, WithExtMetadataBlocks};
use std::collections::BTreeSet;

fn split_bin(d: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = d.windows(4).enumerate().filter(|(_, w)| *w == [0, 0, 0, 1]).map(|(i, _)| i + 4).collect();
    starts.push(d.len() + 4);
    starts.windows(2).map(|w| &d[w[0]..w[1] - 4]).collect()
}

// Annex B walk: returns NAL payloads (with 2-byte HEVC header) whose nal_unit_type == 62.
fn annexb_unspec62(d: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = d.windows(3).enumerate().filter(|(_, w)| *w == [0, 0, 1]).map(|(i, _)| i + 3).collect();
    starts.push(d.len() + 3);
    let mut out = vec![];
    for w in starts.windows(2) {
        let mut end = w[1] - 3;
        if end > w[0] && d[end - 1] == 0 { end -= 1; } // 4-byte start code / trailing zero
        if end <= w[0] + 2 { continue; }
        let nal = &d[w[0]..end];
        if (nal[0] >> 1) & 0x3f == 62 { out.push(nal); }
    }
    out
}

fn dm_blocks(m: Option<&DmData>) -> usize {
    m.map_or(0, |d| match d { DmData::V29(x) => x.blocks_ref().len(), DmData::V40(x) => x.blocks_ref().len() })
}

#[derive(Default, Clone, PartialEq, Debug)]
struct Sig {
    profile: u8,
    el: String,
    nlq: String,
    cm_rule: &'static str,
    container: &'static str,
    levels: String,
    l1: String,
    l2: String,
    l5: String,
    l6: String,
    l8: String,
    l9: String,
    l11: String,
    l254: String,
    src_pq: String,
    crc: String,
}

fn sig(r: &DoviRpu) -> Sig {
    let vdr = r.vdr_dm_data.as_ref();
    let mut levels = BTreeSet::new();
    let (mut l1, mut l2, mut l5, mut l6, mut l8, mut l9, mut l11, mut l254) = (vec![], BTreeSet::new(), vec![], vec![], BTreeSet::new(), vec![], vec![], vec![]);
    if let Some(v) = vdr {
        for lv in 0..=255u8 {
            for b in v.level_blocks_iter(lv) {
                levels.insert(lv);
                match b {
                    B::Level1(x) => l1.push(format!("{}/{}/{}", x.min_pq, x.max_pq, x.avg_pq)),
                    B::Level2(x) => { l2.insert(x.target_max_pq); }
                    B::Level5(x) => l5.push(format!("{}/{}/{}/{}", x.active_area_left_offset, x.active_area_right_offset, x.active_area_top_offset, x.active_area_bottom_offset)),
                    B::Level6(x) => l6.push(format!("{}/{}/{}/{}", x.max_display_mastering_luminance, x.min_display_mastering_luminance, x.max_content_light_level, x.max_frame_average_light_level)),
                    B::Level8(x) => { l8.insert(x.target_display_index); }
                    B::Level9(x) => l9.push(format!("{}", x.source_primary_index)),
                    B::Level11(x) => l11.push(format!("ct{} wp{} ref{}", x.content_type, x.whitepoint, x.reference_mode_flag)),
                    B::Level254(x) => l254.push(format!("dm{}", x.dm_mode)),
                    _ => {}
                }
            }
        }
    }
    // Task rule: any CMv4 level (3,8,9,10,11,254) present in the CMv4 container => v4.0. Container read from cmv40 blocks.
    let cm40 = dm_blocks(vdr.and_then(|v| v.cmv40_metadata.as_ref())) > 0;
    let cm29 = dm_blocks(vdr.and_then(|v| v.cmv29_metadata.as_ref())) > 0;
    let v4_levels = [3u8, 8, 9, 10, 11, 254].iter().any(|l| levels.contains(l));
    let cm_rule = if v4_levels { "v4.0" } else if cm29 { "v2.9" } else { "none" };
    let container = match (cm40, cm29) { (true, true) => "both", (true, false) => "cmv40", (false, true) => "cmv29", _ => "none" };
    let nlq = r.rpu_data_mapping.as_ref().and_then(|m| m.nlq.as_ref()).map_or("no-nlq".into(), |n| format!(
        "off{:?} vin_max{:?} dz{:?}", n.nlq_offset, n.vdr_in_max, n.linear_deadzone_slope));
    let s = |v: &Vec<String>| { let mut u = v.clone(); u.dedup(); format!("{}x [{}]", v.len(), u.join(" ")) };
    let set = |s: &BTreeSet<u16>| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    let set8 = |s: &BTreeSet<u8>| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    Sig {
        profile: r.dovi_profile,
        el: r.el_type.as_ref().map_or("None".into(), |e| format!("{e:?}")),
        nlq,
        cm_rule,
        container,
        levels: levels.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        l1: if l1.is_empty() { "-".into() } else { s(&l1) },
        l2: set(&l2.iter().map(|x| *x as u16).collect()),
        l5: if l5.is_empty() { "-".into() } else { s(&l5) },
        l6: if l6.is_empty() { "-".into() } else { s(&l6) },
        l8: set8(&l8),
        l9: if l9.is_empty() { "-".into() } else { s(&l9) },
        l11: if l11.is_empty() { "-".into() } else { s(&l11) },
        l254: if l254.is_empty() { "-".into() } else { s(&l254) },
        src_pq: vdr.map_or("-".into(), |v| format!("{}/{}", v.source_min_pq, v.source_max_pq)),
        crc: format!("{:08x}", r.rpu_data_crc32),
    }
}

fn main() {
    for path in std::env::args().skip(1) {
        let data = match std::fs::read(&path) { Ok(d) => d, Err(e) => { println!("## {path}: read error {e}"); continue; } };
        let is_hevc = path.ends_with(".hevc") || path.ends_with(".265") || path.ends_with(".h265");
        let chunks: Vec<&[u8]> = if is_hevc { annexb_unspec62(&data) } else { split_bin(&data) };
        let mut sigs = vec![];
        let mut errs = vec![];
        for (i, c) in chunks.iter().enumerate() {
            match DoviRpu::parse_unspec62_nalu(c) {
                Ok(r) => sigs.push(sig(&r)),
                Err(e) => errs.push(format!("#{}: {}", i + 1, format!("{e}").lines().next().unwrap_or("").chars().take(70).collect::<String>())),
            }
        }
        let crate_count = if is_hevc { "n/a".into() } else {
            match dolby_vision::rpu::utils::parse_rpu_file(&path) { Ok(v) => format!("{}", v.len()), Err(e) => format!("err: {}", format!("{e}").chars().take(60).collect::<String>()) }
        };
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!("## {name} | nal_units={} parsed={} errors={} | parse_rpu_file count={crate_count}", chunks.len(), sigs.len(), errs.len());
        for e in &errs { println!("  ERR {e}"); }
        let Some(s0) = sigs.first() else { continue };
        println!("  first: profile={} el={} nlq=[{}] cm_rule={} container={} levels={} crc={} src_pq={}", s0.profile, s0.el, s0.nlq, s0.cm_rule, s0.container, s0.levels, s0.crc, s0.src_pq);
        println!("  L1={} L2={} L5={} L6={}", s0.l1, s0.l2, s0.l5, s0.l6);
        println!("  L8={} L9={} L11={} L254={}", s0.l8, s0.l9, s0.l11, s0.l254);
        // cross-RPU: which fields vary
        let uniq = |f: &dyn Fn(&Sig) -> String| sigs.iter().map(f).collect::<BTreeSet<String>>().len();
        println!("  distinct over {} RPUs: profile={} el={} cm={} levels={} L1={} L5={} L6={} L8={} L11={} L254={}",
            sigs.len(), uniq(&|s| s.profile.to_string()), uniq(&|s| s.el.clone()), uniq(&|s| s.cm_rule.to_string()),
            uniq(&|s| s.levels.clone()), uniq(&|s| s.l1.clone()), uniq(&|s| s.l5.clone()), uniq(&|s| s.l6.clone()),
            uniq(&|s| s.l8.clone()), uniq(&|s| s.l11.clone()), uniq(&|s| s.l254.clone()));
    }
}
