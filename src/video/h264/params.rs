//! Sequence and picture parameter sets (H.264 7.3.2.1 and 7.3.2.2): what decoding an intra picture needs.

use super::tables::{DEFAULT_4X4_INTER, DEFAULT_4X4_INTRA, DEFAULT_8X8_INTER, DEFAULT_8X8_INTRA};
use crate::video::bits::{BitReader, DecodeError, Result, rbsp};

/// Scaling lists in scan order: six 4x4 (Intra Y, Cb, Cr, Inter Y, Cb, Cr), then two 8x8 (Intra Y, Inter Y).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalingLists {
    pub list4: [[u8; 16]; 6],
    pub list8: [[u8; 64]; 2],
}

impl ScalingLists {
    pub fn flat() -> ScalingLists {
        ScalingLists { list4: [[16; 16]; 6], list8: [[16; 64]; 2] }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sps {
    pub id: u32,
    pub profile: u8,
    pub chroma_format: u32,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub transform_bypass: bool,
    /// present in the SPS: when absent, a PPS falls back to the defaults rather than these
    pub scaling_present: bool,
    pub scaling: ScalingLists,
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    pub log2_max_poc_lsb: u32,
    pub delta_pic_order_always_zero: bool,
    pub width_mbs: u32,
    /// in frame macroblocks
    pub height_mbs: u32,
    pub frame_mbs_only: bool,
    pub mbaff: bool,
    /// left, right, top, bottom in luma samples
    pub crop: [u32; 4],
    pub full_range: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub cabac: bool,
    pub bottom_field_pic_order: bool,
    pub init_qp: i32,
    pub chroma_qp_offset: [i32; 2],
    pub deblocking_control: bool,
    pub constrained_intra_pred: bool,
    pub redundant_pic_cnt: bool,
    pub transform_8x8: bool,
    /// None: the SPS's lists
    pub scaling: Option<PpsScaling>,
}

/// A PPS's own scaling lists: which came in it (None: fall back), as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PpsScaling {
    pub list4: [ListRead<16>; 6],
    pub list8: [ListRead<64>; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListRead<const N: usize> {
    Absent,
    Default,
    Given([u8; N]),
}

/// One scaling_list(): the list, or the default for its place.
fn scaling_list<const N: usize>(r: &mut BitReader) -> Result<ListRead<N>> {
    if !r.flag()? {
        return Ok(ListRead::Absent);
    }
    let mut list = [0u8; N];
    let (mut last, mut next) = (8i32, 8i32);
    for (j, v) in list.iter_mut().enumerate() {
        if next != 0 {
            let delta = r.se()?;
            if !(-128..=127).contains(&delta) {
                return Err(DecodeError::Invalid("scaling list delta"));
            }
            next = (last + delta + 256) % 256;
            if j == 0 && next == 0 {
                return Ok(ListRead::Default);
            }
        }
        *v = if next == 0 { last } else { next } as u8;
        last = i32::from(*v);
    }
    Ok(ListRead::Given(list))
}

/// Lists as read, with the fall backs of Table 7-2 filled in: rule A falls back to the defaults, rule B to `base`.
fn resolve(list4: &[ListRead<16>; 6], list8: &[ListRead<64>; 2], base: Option<&ScalingLists>) -> ScalingLists {
    let mut out = ScalingLists::flat();
    for i in 0..6 {
        out.list4[i] = match list4[i] {
            ListRead::Given(l) => l,
            ListRead::Default => {
                if i < 3 {
                    DEFAULT_4X4_INTRA
                } else {
                    DEFAULT_4X4_INTER
                }
            }
            ListRead::Absent if i == 0 || i == 3 => match base {
                Some(b) => b.list4[i],
                None if i == 0 => DEFAULT_4X4_INTRA,
                None => DEFAULT_4X4_INTER,
            },
            ListRead::Absent => out.list4[i - 1],
        };
    }
    for i in 0..2 {
        out.list8[i] = match list8[i] {
            ListRead::Given(l) => l,
            ListRead::Default => {
                if i == 0 {
                    DEFAULT_8X8_INTRA
                } else {
                    DEFAULT_8X8_INTER
                }
            }
            ListRead::Absent => match base {
                Some(b) => b.list8[i],
                None if i == 0 => DEFAULT_8X8_INTRA,
                None => DEFAULT_8X8_INTER,
            },
        };
    }
    out
}

fn read_lists(r: &mut BitReader, count8: usize) -> Result<([ListRead<16>; 6], [ListRead<64>; 2])> {
    let mut list4 = [ListRead::Absent; 6];
    let mut list8 = [ListRead::Absent; 2];
    for l in &mut list4 {
        *l = scaling_list(r)?;
    }
    for l in list8.iter_mut().take(count8) {
        *l = scaling_list(r)?;
    }
    Ok((list4, list8))
}

pub fn parse_sps(nal: &[u8]) -> Result<Sps> {
    let data = rbsp(nal.get(1..).ok_or(DecodeError::Truncated)?);
    let mut r = BitReader::new(&data);
    let profile = r.bits(8)? as u8;
    r.skip(16)?; // constraint flags, level
    let id = r.ue_max(31, "SPS id")?;
    let (mut chroma_format, mut depth_luma, mut depth_chroma, mut transform_bypass) = (1, 8, 8, false);
    let mut scaling_present = false;
    let mut scaling = ScalingLists::flat();
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma_format = r.ue_max(3, "chroma format")?;
        if chroma_format == 3 && r.flag()? {
            return Err(DecodeError::Unsupported("separate colour planes"));
        }
        depth_luma = r.ue_max(6, "bit depth")? + 8;
        depth_chroma = r.ue_max(6, "bit depth")? + 8;
        transform_bypass = r.flag()?;
        if r.flag()? {
            scaling_present = true;
            if chroma_format == 3 {
                return Err(DecodeError::Unsupported("4:4:4"));
            }
            let (list4, list8) = read_lists(&mut r, 2)?;
            scaling = resolve(&list4, &list8, None);
        }
    }
    let log2_max_frame_num = r.ue_max(12, "log2_max_frame_num")? + 4;
    let poc_type = r.ue_max(2, "pic_order_cnt_type")?;
    let (mut log2_max_poc_lsb, mut delta_pic_order_always_zero) = (0, false);
    match poc_type {
        0 => log2_max_poc_lsb = r.ue_max(12, "log2_max_pic_order_cnt_lsb")? + 4,
        1 => {
            delta_pic_order_always_zero = r.flag()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue_max(255, "num_ref_frames_in_pic_order_cnt_cycle")? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.flag()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue_max(1023, "width")? + 1;
    let map_units = r.ue_max(1023, "height")? + 1;
    let frame_mbs_only = r.flag()?;
    let mbaff = !frame_mbs_only && r.flag()?;
    r.flag()?; // direct_8x8_inference_flag
    let height_mbs = map_units * if frame_mbs_only { 1 } else { 2 };
    let mut crop = [0u32; 4];
    if r.flag()? {
        let (unit_x, unit_y) = match chroma_format {
            0 => (1, 1),
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let unit_y = unit_y * if frame_mbs_only { 1 } else { 2 };
        for (i, c) in crop.iter_mut().enumerate() {
            *c = r.ue_max(8192, "crop")? * if i < 2 { unit_x } else { unit_y };
        }
        if crop[0] + crop[1] >= width_mbs * 16 || crop[2] + crop[3] >= height_mbs * 16 {
            return Err(DecodeError::Invalid("crop"));
        }
    }
    let mut full_range = false;
    if r.flag()? {
        // VUI, as far as the range
        if r.flag()? && r.bits(8)? == 255 {
            r.skip(32)?;
        }
        if r.flag()? {
            r.flag()?;
        }
        if r.flag()? {
            r.skip(3)?;
            full_range = r.flag()?;
        }
    }
    Ok(Sps {
        id,
        profile,
        chroma_format,
        bit_depth_luma: depth_luma as u8,
        bit_depth_chroma: depth_chroma as u8,
        transform_bypass,
        scaling_present,
        scaling,
        log2_max_frame_num,
        poc_type,
        log2_max_poc_lsb,
        delta_pic_order_always_zero,
        width_mbs,
        height_mbs,
        frame_mbs_only,
        mbaff,
        crop,
        full_range,
    })
}

/// Whether anything but the trailing bits is left from `r`'s position.
fn more_rbsp_data(data: &[u8], position: usize) -> bool {
    let Some(last) = data.iter().rposition(|b| *b != 0) else { return false };
    let stop = last * 8 + 7 - data[last].trailing_zeros() as usize;
    position < stop
}

/// A PPS. Its scaling lists fall back on its SPS's, so they're resolved when a slice puts the two together.
pub fn parse_pps(nal: &[u8]) -> Result<Pps> {
    let data = rbsp(nal.get(1..).ok_or(DecodeError::Truncated)?);
    let mut r = BitReader::new(&data);
    let id = r.ue_max(255, "PPS id")?;
    let sps_id = r.ue_max(31, "SPS id")?;
    let cabac = r.flag()?;
    let bottom_field_pic_order = r.flag()?;
    if r.ue()? != 0 {
        return Err(DecodeError::Unsupported("slice groups"));
    }
    r.ue()?;
    r.ue()?; // num_ref_idx_l0/l1_default_active_minus1
    r.flag()?; // weighted_pred_flag
    r.skip(2)?; // weighted_bipred_idc
    let init_qp = r.se()?;
    if !(-26..=25).contains(&init_qp) {
        return Err(DecodeError::Invalid("pic_init_qp_minus26"));
    }
    let init_qp = 26 + init_qp;
    r.se()?; // pic_init_qs_minus26
    let cb = r.se()?;
    if !(-12..=12).contains(&cb) {
        return Err(DecodeError::Invalid("chroma_qp_index_offset"));
    }
    let deblocking_control = r.flag()?;
    let constrained_intra_pred = r.flag()?;
    let redundant_pic_cnt = r.flag()?;
    let (mut transform_8x8, mut scaling, mut cr) = (false, None, cb);
    if more_rbsp_data(&data, r.position()) {
        transform_8x8 = r.flag()?;
        if r.flag()? {
            let (list4, list8) = read_lists(&mut r, if transform_8x8 { 2 } else { 0 })?;
            scaling = Some(PpsScaling { list4, list8 });
        }
        cr = r.se()?;
        if !(-12..=12).contains(&cr) {
            return Err(DecodeError::Invalid("second_chroma_qp_index_offset"));
        }
    }
    Ok(Pps {
        id,
        sps_id,
        cabac,
        bottom_field_pic_order,
        init_qp,
        chroma_qp_offset: [cb, cr],
        deblocking_control,
        constrained_intra_pred,
        redundant_pic_cnt,
        transform_8x8,
        scaling,
    })
}

/// The scaling lists a picture uses, from its SPS and PPS together.
pub fn scaling_of(sps: &Sps, pps: &Pps) -> ScalingLists {
    match &pps.scaling {
        None => sps.scaling.clone(),
        Some(p) => resolve(&p.list4, &p.list8, sps.scaling_present.then_some(&sps.scaling)),
    }
}
