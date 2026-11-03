// w07c: parse dovi_tool RPU.bin (00 00 00 01 + 0x19 RPU, no NAL header) with dolby_vision 3.4.0.
// Usage: w07c_probe <RPU.bin> [--stab]
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::{DmData, WithExtMetadataBlocks};
use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock as B;
use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlockInfo;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
struct Sig {
    profile: u8,
    el: String,
    cm: &'static str,
    cm40_n: usize,
    cm29_n: usize,
    levels: String,
    l1: String,
    l2_targets: String,
    l5: String,
    l6: String,
    l8_targets: String,
    l9: String,
    l11: String,
    l254: String,
    src_pq: String,
    lv29: String,
    lv40: String,
}

fn split_rpus(d: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<usize> = d.windows(4).enumerate().filter(|(_, w)| *w == [0, 0, 0, 1]).map(|(i, _)| i + 4).collect();
    starts.push(d.len() + 4);
    starts.windows(2).map(|w| &d[w[0]..w[1] - 4]).collect()
}

fn setlv(m: Option<&DmData>) -> String {
    let v: Vec<u8> = match m {
        None => vec![],
        Some(DmData::V29(x)) => x.blocks_ref().iter().map(|b| b.level()).collect(),
        Some(DmData::V40(x)) => x.blocks_ref().iter().map(|b| b.level()).collect(),
    };
    v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

fn sig(r: &DoviRpu) -> Sig {
    let vdr = r.vdr_dm_data.as_ref();
    let nb = |m: Option<&DmData>| m.map_or(0, |d| match d { DmData::V29(x) => x.blocks_ref().len(), DmData::V40(x) => x.blocks_ref().len() });
    let cm40_n = nb(vdr.and_then(|v| v.cmv40_metadata.as_ref()));
    let cm29_n = nb(vdr.and_then(|v| v.cmv29_metadata.as_ref()));
    let cm = match (cm40_n > 0, cm29_n > 0) {
        (true, false) => "CMv4.0",
        (false, true) => "CMv2.9",
        (true, true) => "BOTH",
        (false, false) => "none",
    };
    let mut levels = BTreeSet::new();
    let mut l2 = BTreeSet::new();
    let mut l8 = BTreeSet::new();
    let (mut l1, mut l5, mut l6, mut l9, mut l11, mut l254) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    if let Some(v) = vdr {
        for lv in 0..=255u8 {
            for b in v.level_blocks_iter(lv) {
                levels.insert(lv);
                match b {
                    B::Level1(x) => { l1.push(format!("{}/{}/{}", x.min_pq, x.max_pq, x.avg_pq)); }
                    B::Level2(x) => { l2.insert(x.target_max_pq); }
                    B::Level5(x) => { l5.push(format!("{}/{}/{}/{}", x.active_area_left_offset, x.active_area_right_offset, x.active_area_top_offset, x.active_area_bottom_offset)); }
                    B::Level6(x) => { l6.push(format!("{}/{}/{}/{}", x.max_display_mastering_luminance, x.min_display_mastering_luminance, x.max_content_light_level, x.max_frame_average_light_level)); }
                    B::Level8(x) => { l8.insert(x.target_display_index); }
                    B::Level9(x) => { l9.push(format!("{}", x.source_primary_index)); }
                    B::Level11(x) => { l11.push(format!("ct{} wp{} ref{}", x.content_type, x.whitepoint, x.reference_mode_flag)); }
                    B::Level254(x) => { l254.push(format!("dm{}", x.dm_mode)); }
                    _ => {}
                }
            }
        }
    }
    let set = |s: &BTreeSet<u16>| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    let set8 = |s: &BTreeSet<u8>| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    let uniq = |v: &Vec<String>| { let mut s: Vec<String> = v.clone(); s.dedup(); format!("{}x [{}]", v.len(), s.join(" ")) };
    Sig {
        profile: r.dovi_profile,
        el: r.el_type.as_ref().map_or("None".into(), |e| format!("{e:?}")),
        cm,
        cm40_n,
        cm29_n,
        levels: levels.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        l1: uniq(&l1),
        l2_targets: set(&l2.iter().map(|x| *x as u16).collect()),
        l5: uniq(&l5),
        l6: uniq(&l6),
        l8_targets: set8(&l8),
        l9: uniq(&l9),
        l11: uniq(&l11),
        l254: uniq(&l254),
        src_pq: vdr.map_or("-".into(), |v| format!("{}/{}", v.source_min_pq, v.source_max_pq)),
        lv29: setlv(vdr.and_then(|v| v.cmv29_metadata.as_ref())),
        lv40: setlv(vdr.and_then(|v| v.cmv40_metadata.as_ref())),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1])?;
    let stab = a.iter().any(|x| x == "--stab");
    let rpus: Vec<DoviRpu> = split_rpus(&data).iter().map(|c| DoviRpu::parse_unspec62_nalu(c)).collect::<Result<_, _>>()?;
    println!("file={} rpus={}", a[1], rpus.len());
    let s0 = sig(&rpus[0]);
    println!("| field | RPU 1 (first) |\n|---|---|");
    println!("| dovi_profile | {} |", s0.profile);
    println!("| el_type | {} |", s0.el);
    println!("| CM version (by non-empty block set) | {} (cmv40 blocks={}, cmv29 blocks={}) |", s0.cm, s0.cm40_n, s0.cm29_n);
    println!("| levels present (union) | {} |", s0.levels);
    println!("| levels in cmv29 set | {} |", s0.lv29);
    println!("| levels in cmv40 set | {} |", s0.lv40);
    println!("| L1 min/max/avg | {} |", s0.l1);
    println!("| L2 target_max_pq | {} |", s0.l2_targets);
    println!("| L5 L/R/T/B | {} |", s0.l5);
    println!("| L6 maxmast/minmast/maxcll/maxfall | {} |", s0.l6);
    println!("| L8 target_display_index | {} |", s0.l8_targets);
    println!("| L9 | {} |", s0.l9);
    println!("| L11 | {} |", s0.l11);
    println!("| L254 | {} |", s0.l254);
    println!("| source_min/max_pq | {} |", s0.src_pq);
    if stab {
        // Track per-field: index of last RPU that changed it (1-based stable-from).
        let sigs: Vec<Sig> = rpus.iter().map(sig).collect();
        let mut stable = |name: &str, f: &dyn Fn(&Sig) -> String| {
            let vals: Vec<String> = sigs.iter().map(f).collect();
            let distinct: BTreeSet<&String> = vals.iter().collect();
            let last_change = (1..vals.len()).filter(|&i| vals[i] != vals[i - 1]).last().map_or(1, |i| i + 1);
            println!("| {name} | {} | {} | {} |", last_change, distinct.len(), vals.iter().take(1).cloned().collect::<String>());
        };
        println!("\n| field | stable from RPU # (1-based) | distinct values | RPU1 value |\n|---|---|---|---|");
        stable("dovi_profile", &|s| s.profile.to_string());
        stable("el_type", &|s| s.el.clone());
        stable("CM version", &|s| s.cm.to_string());
        stable("levels present", &|s| s.levels.clone());
        stable("cmv40 level set", &|s| s.lv40.clone());
        stable("cmv29 level set", &|s| s.lv29.clone());
        stable("L2 target set", &|s| s.l2_targets.clone());
        stable("L8 target set", &|s| s.l8_targets.clone());
        stable("L5 (per-shot values)", &|s| s.l5.clone());
        stable("L6", &|s| s.l6.clone());
        stable("L1 (per-shot values)", &|s| s.l1.clone());
        stable("L9", &|s| s.l9.clone());
        stable("L11", &|s| s.l11.clone());
        stable("source_min/max_pq", &|s| s.src_pq.clone());
        let nshot = (1..sigs.len()).filter(|&i| sigs[i].l1 != sigs[i - 1].l1).count();
        println!("L1 changes between consecutive RPUs: {nshot}");
    }
    Ok(())
}
