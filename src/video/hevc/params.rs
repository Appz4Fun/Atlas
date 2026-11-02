//! Parameter sets (H.265 7.3.2): what a picture's slices need to be read.

use crate::video::bits::{BitReader, DecodeError, Result, rbsp};

/// Table 7-6: the default 8x8 lists, intra then inter, in up-right diagonal order.
pub const DEFAULT_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20, 21, 20, 19, 21, 24,
    22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31, 29, 36, 41, 44, 41, 36, 47, 54, 54, 47,
    65, 70, 65, 88, 88, 115,
];
pub const DEFAULT_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20, 20, 20, 20, 20, 24,
    24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28, 28, 33, 33, 33, 33, 33, 41, 41, 41, 41,
    54, 54, 54, 71, 71, 91,
];

/// Scaling lists per size (4x4, 8x8, 16x16, 32x32) and matrix (intra Y Cb Cr, inter Y Cb Cr), with the DC of the
/// two biggest sizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalingList {
    pub lists: [[[u8; 64]; 6]; 4],
    pub dc: [[u8; 6]; 4],
}

impl Default for ScalingList {
    fn default() -> ScalingList {
        let mut lists = [[[16u8; 64]; 6]; 4];
        for per_size in lists.iter_mut().skip(1) {
            for (matrix, list) in per_size.iter_mut().enumerate() {
                *list = if matrix < 3 { DEFAULT_INTRA } else { DEFAULT_INTER };
            }
        }
        ScalingList { lists, dc: [[16; 6]; 4] }
    }
}

fn scaling_list_data(r: &mut BitReader<'_>) -> Result<ScalingList> {
    let mut sl = ScalingList::default();
    for size in 0..4 {
        let step = if size == 3 { 3 } else { 1 };
        let mut matrix = 0;
        while matrix < 6 {
            let coefs = 64.min(1 << (4 + (size << 1)));
            if !r.flag()? {
                let delta = r.ue_max(matrix as u32 / step as u32, "scaling list reference")? as usize * step;
                if delta != 0 {
                    let from = matrix - delta;
                    sl.lists[size][matrix] = sl.lists[size][from];
                    sl.dc[size][matrix] = sl.dc[size][from];
                } else {
                    let default = ScalingList::default();
                    sl.lists[size][matrix] = default.lists[size][matrix];
                    sl.dc[size][matrix] = 16;
                }
            } else {
                let mut next: i32 = 8;
                if size > 1 {
                    let dc = r.se()?;
                    if !(-7..=247).contains(&dc) {
                        return Err(DecodeError::Invalid("scaling list DC"));
                    }
                    next = dc + 8;
                    sl.dc[size][matrix] = next as u8;
                }
                for i in 0..coefs {
                    let delta = r.se()?;
                    if !(-128..=127).contains(&delta) {
                        return Err(DecodeError::Invalid("scaling list delta"));
                    }
                    next = (next + delta + 256).rem_euclid(256);
                    if next == 0 {
                        return Err(DecodeError::Invalid("scaling list value"));
                    }
                    sl.lists[size][matrix][i] = next as u8;
                }
                if size <= 1 {
                    sl.dc[size][matrix] = sl.lists[size][matrix][0];
                }
            }
            matrix += step;
        }
    }
    // 32x32 chroma lists, for 4:4:4 only, copy the 16x16 ones
    for matrix in [1, 2, 4, 5] {
        sl.lists[3][matrix] = sl.lists[2][matrix];
        sl.dc[3][matrix] = sl.dc[2][matrix];
    }
    Ok(sl)
}

fn profile_tier_level(r: &mut BitReader<'_>, max_sub_layers_minus1: u32) -> Result<u8> {
    r.skip(2 + 1)?;
    let profile = r.bits(5)? as u8;
    r.skip(32 + 4 + 43 + 1)?;
    r.skip(8)?; // general_level_idc
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..max_sub_layers_minus1 as usize {
        profile_present[i] = r.flag()?;
        level_present[i] = r.flag()?;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.skip(2)?;
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if profile_present[i] {
            r.skip(88)?;
        }
        if level_present[i] {
            r.skip(8)?;
        }
    }
    Ok(profile)
}

/// One short-term reference picture set: only how many pictures it names matters to an intra decoder, to read
/// the sets after it.
pub(crate) fn st_ref_pic_set(r: &mut BitReader<'_>, idx: usize, delta_pocs: &[u32], in_slice: bool) -> Result<u32> {
    let predicted = idx != 0 && r.flag()?;
    if predicted {
        let delta_idx = if in_slice { r.ue_max(idx as u32 - 1, "RPS reference")? + 1 } else { 1 };
        let reference = idx.checked_sub(delta_idx as usize).ok_or(DecodeError::Invalid("RPS reference"))?;
        r.skip(1)?; // delta_rps_sign
        r.ue_max(1 << 15, "RPS delta")?;
        let mut count = 0;
        for _ in 0..=*delta_pocs.get(reference).ok_or(DecodeError::Invalid("RPS reference"))? {
            let used = r.flag()?;
            let use_delta = if used { true } else { r.flag()? };
            count += u32::from(used || use_delta);
        }
        Ok(count)
    } else {
        let negative = r.ue_max(16, "RPS size")?;
        let positive = r.ue_max(16, "RPS size")?;
        for _ in 0..negative + positive {
            r.ue_max(1 << 15, "RPS delta")?;
            r.skip(1)?;
        }
        Ok(negative + positive)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sps {
    pub id: u32,
    pub profile: u8,
    pub chroma_format_idc: u32,
    pub width: u32,
    pub height: u32,
    /// the conformance window, in luma samples
    pub crop: [u32; 4],
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_max_poc_lsb: u32,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub max_transform_depth_intra: u32,
    pub scaling_list: Option<ScalingList>,
    pub amp: bool,
    pub sao: bool,
    pub pcm: Option<Pcm>,
    /// how many pictures each short-term set names
    pub rps_delta_pocs: Vec<u32>,
    pub long_term_refs: Option<u32>,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    /// colour primaries, transfer and matrix, as the VUI says (H.273 codes)
    pub colour: Option<(u8, u8, u8)>,
    pub full_range: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pcm {
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_min: u32,
    pub log2_max: u32,
    pub loop_filter_disabled: bool,
}

impl Sps {
    pub fn ctb_size(&self) -> u32 {
        1 << self.log2_ctb
    }
    pub fn width_ctbs(&self) -> u32 {
        self.width.div_ceil(self.ctb_size())
    }
    pub fn height_ctbs(&self) -> u32 {
        self.height.div_ceil(self.ctb_size())
    }
}

pub fn parse_sps(nal: &[u8]) -> Result<Sps> {
    let data = rbsp(nal.get(2..).ok_or(DecodeError::Truncated)?);
    let mut r = BitReader::new(&data);
    let mut s = Sps::default();
    r.skip(4)?;
    let max_sub_layers_minus1 = r.bits(3)?;
    if max_sub_layers_minus1 > 6 {
        return Err(DecodeError::Invalid("sub layers"));
    }
    r.skip(1)?;
    s.profile = profile_tier_level(&mut r, max_sub_layers_minus1)?;
    s.id = r.ue_max(15, "SPS id")?;
    s.chroma_format_idc = r.ue_max(3, "chroma format")?;
    if s.chroma_format_idc != 1 {
        return Err(DecodeError::Unsupported("chroma other than 4:2:0"));
    }
    s.width = r.ue_max(16_888, "width")?;
    s.height = r.ue_max(16_888, "height")?;
    if s.width == 0 || s.height == 0 {
        return Err(DecodeError::Invalid("picture size"));
    }
    if r.flag()? {
        for side in &mut s.crop {
            // in chroma samples for 4:2:0: two luma samples each
            *side = r.ue_max(8_192, "conformance window")? * 2;
        }
    }
    s.bit_depth_luma = r.ue_max(8, "bit depth")? as u8 + 8;
    s.bit_depth_chroma = r.ue_max(8, "bit depth")? as u8 + 8;
    if s.bit_depth_luma > 10 || s.bit_depth_chroma > 10 {
        return Err(DecodeError::Unsupported("bit depth over 10"));
    }
    s.log2_max_poc_lsb = r.ue_max(12, "POC size")? + 4;
    let ordering_all = r.flag()?;
    let first = if ordering_all { 0 } else { max_sub_layers_minus1 };
    for _ in first..=max_sub_layers_minus1 {
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    s.log2_min_cb = r.ue_max(3, "coding block size")? + 3;
    s.log2_ctb = s.log2_min_cb + r.ue_max(3, "coding tree size")?;
    if !(4..=6).contains(&s.log2_ctb) {
        return Err(DecodeError::Invalid("coding tree size"));
    }
    s.log2_min_tb = r.ue_max(3, "transform size")? + 2;
    s.log2_max_tb = s.log2_min_tb + r.ue_max(3, "transform size")?;
    if s.log2_max_tb > 5 || s.log2_min_tb >= s.log2_min_cb || s.log2_max_tb > s.log2_ctb {
        return Err(DecodeError::Invalid("transform size"));
    }
    r.ue_max(4, "transform depth")?; // inter
    s.max_transform_depth_intra = r.ue_max(4, "transform depth")?;
    if r.flag()? {
        s.scaling_list = Some(if r.flag()? { scaling_list_data(&mut r)? } else { ScalingList::default() });
    }
    s.amp = r.flag()?;
    s.sao = r.flag()?;
    if r.flag()? {
        let bit_depth_luma = r.bits(4)? as u8 + 1;
        let bit_depth_chroma = r.bits(4)? as u8 + 1;
        let log2_min = r.ue_max(2, "PCM size")? + 3;
        let log2_max = log2_min + r.ue_max(2, "PCM size")?;
        s.pcm = Some(Pcm { bit_depth_luma, bit_depth_chroma, log2_min, log2_max, loop_filter_disabled: r.flag()? });
    }
    let sets = r.ue_max(64, "RPS count")? as usize;
    for i in 0..sets {
        let count = st_ref_pic_set(&mut r, i, &s.rps_delta_pocs, false)?;
        s.rps_delta_pocs.push(count);
    }
    if r.flag()? {
        let n = r.ue_max(32, "long term refs")?;
        for _ in 0..n {
            r.skip(s.log2_max_poc_lsb as usize + 1)?;
        }
        s.long_term_refs = Some(n);
    }
    s.temporal_mvp = r.flag()?;
    s.strong_intra_smoothing = r.flag()?;
    if r.flag()? {
        vui_colour(&mut r, &mut s)?;
    }
    Ok(s)
}

/// The start of the VUI, up to the colour description: the rest says nothing about how a still looks.
fn vui_colour(r: &mut BitReader<'_>, s: &mut Sps) -> Result<()> {
    if r.flag()? && r.bits(8)? == 255 {
        r.skip(32)?;
    }
    if r.flag()? {
        r.skip(1)?;
    }
    if r.flag()? {
        r.skip(3)?;
        s.full_range = r.flag()?;
        if r.flag()? {
            s.colour = Some((r.bits(8)? as u8, r.bits(8)? as u8, r.bits(8)? as u8));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub dependent_slices: bool,
    pub output_flag_present: bool,
    pub extra_slice_header_bits: u32,
    pub sign_data_hiding: bool,
    pub cabac_init_present: bool,
    pub init_qp: i32,
    pub constrained_intra_pred: bool,
    pub transform_skip: bool,
    /// None: no QP changes inside a slice. Some: the depth they're signalled at
    pub cu_qp_delta_depth: Option<u32>,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub slice_chroma_qp_offsets: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub transquant_bypass: bool,
    pub tiles: bool,
    pub entropy_coding_sync: bool,
    pub loop_filter_across_slices: bool,
    pub deblocking_override: bool,
    pub deblocking_disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub scaling_list: Option<ScalingList>,
    pub lists_modification: bool,
    pub slice_header_extension: bool,
}

pub fn parse_pps(nal: &[u8]) -> Result<Pps> {
    let data = rbsp(nal.get(2..).ok_or(DecodeError::Truncated)?);
    let mut r = BitReader::new(&data);
    let mut p = Pps { id: r.ue_max(63, "PPS id")?, sps_id: r.ue_max(15, "SPS id")?, ..Pps::default() };
    p.dependent_slices = r.flag()?;
    p.output_flag_present = r.flag()?;
    p.extra_slice_header_bits = r.bits(3)?;
    p.sign_data_hiding = r.flag()?;
    p.cabac_init_present = r.flag()?;
    r.ue_max(14, "reference count")?;
    r.ue_max(14, "reference count")?;
    p.init_qp = 26 + r.se()?;
    p.constrained_intra_pred = r.flag()?;
    p.transform_skip = r.flag()?;
    if r.flag()? {
        p.cu_qp_delta_depth = Some(r.ue_max(3, "QP delta depth")?);
    }
    p.cb_qp_offset = r.se()?;
    p.cr_qp_offset = r.se()?;
    if !(-12..=12).contains(&p.cb_qp_offset) || !(-12..=12).contains(&p.cr_qp_offset) {
        return Err(DecodeError::Invalid("chroma QP offset"));
    }
    p.slice_chroma_qp_offsets = r.flag()?;
    p.weighted_pred = r.flag()?;
    p.weighted_bipred = r.flag()?;
    p.transquant_bypass = r.flag()?;
    p.tiles = r.flag()?;
    p.entropy_coding_sync = r.flag()?;
    if p.tiles {
        return Err(DecodeError::Unsupported("tiles"));
    }
    p.loop_filter_across_slices = r.flag()?;
    if r.flag()? {
        p.deblocking_override = r.flag()?;
        p.deblocking_disabled = r.flag()?;
        if !p.deblocking_disabled {
            p.beta_offset = r.se()? * 2;
            p.tc_offset = r.se()? * 2;
        }
    }
    if r.flag()? {
        p.scaling_list = Some(scaling_list_data(&mut r)?);
    }
    p.lists_modification = r.flag()?;
    r.ue()?; // log2_parallel_merge_level
    p.slice_header_extension = r.flag()?;
    if r.flag()? && r.flag()? {
        return Err(DecodeError::Unsupported("range extensions"));
    }
    Ok(p)
}
