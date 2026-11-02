//! Slice headers (H.264 7.3.3), for I slices.

use super::params::{Pps, Sps};
use crate::video::bits::{BitReader, DecodeError, Result};

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceHeader {
    pub first_mb: u32,
    pub pps_id: u32,
    pub frame_num: u32,
    pub idr_pic_id: Option<u32>,
    pub poc_lsb: u32,
    pub qp: i32,
    /// 0: on, 1: off, 2: on, but not across slice edges
    pub deblocking: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    /// where the slice data starts, in bytes, after the cabac_alignment_one_bits
    pub data_start: usize,
}

/// The PPS id of a slice, the third thing in its header.
pub fn pps_id(data: &[u8]) -> Result<u32> {
    let mut r = BitReader::new(data);
    r.ue()?;
    r.ue()?;
    r.ue_max(255, "PPS id")
}

/// A slice header from `data` (the RBSP after the NAL header byte). `nal` is the NAL header byte.
pub fn parse(data: &[u8], nal: u8, lookup: impl Fn(u32) -> Option<(Sps, Pps)>) -> Result<(SliceHeader, Sps, Pps)> {
    let kind = nal & 0x1F;
    let ref_idc = nal >> 5 & 3;
    let mut r = BitReader::new(data);
    let first_mb = r.ue()?;
    let slice_type = r.ue_max(9, "slice type")?;
    if slice_type % 5 != 2 {
        return Err(DecodeError::Unsupported("a slice that isn't intra"));
    }
    let pps_id = r.ue_max(255, "PPS id")?;
    let (sps, pps) = lookup(pps_id).ok_or(DecodeError::Invalid("slice without its parameter sets"))?;
    if !pps.cabac {
        return Err(DecodeError::Unsupported("CAVLC"));
    }
    if sps.chroma_format != 1 || sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 {
        return Err(DecodeError::Unsupported("anything but 8 bit 4:2:0"));
    }
    if sps.transform_bypass {
        return Err(DecodeError::Unsupported("lossless"));
    }
    if first_mb >= sps.width_mbs * sps.height_mbs {
        return Err(DecodeError::Invalid("first_mb_in_slice"));
    }
    let frame_num = r.bits(sps.log2_max_frame_num)?;
    if !sps.frame_mbs_only && r.flag()? {
        return Err(DecodeError::Unsupported("field pictures"));
    }
    if sps.mbaff {
        return Err(DecodeError::Unsupported("MBAFF"));
    }
    let idr_pic_id = if kind == NAL_IDR { Some(r.ue()?) } else { None };
    let mut poc_lsb = 0;
    if sps.poc_type == 0 {
        poc_lsb = r.bits(sps.log2_max_poc_lsb)?;
        if pps.bottom_field_pic_order {
            r.se()?;
        }
    }
    if sps.poc_type == 1 && !sps.delta_pic_order_always_zero {
        r.se()?;
        if pps.bottom_field_pic_order {
            r.se()?;
        }
    }
    if pps.redundant_pic_cnt && r.ue()? != 0 {
        return Err(DecodeError::Unsupported("redundant pictures"));
    }
    if ref_idc != 0 {
        // dec_ref_pic_marking
        if kind == NAL_IDR {
            r.skip(2)?;
        } else if r.flag()? {
            loop {
                let op = r.ue_max(6, "memory management op")?;
                match op {
                    0 => break,
                    1 | 2 | 4 | 6 => {
                        r.ue()?;
                    }
                    3 => {
                        r.ue()?;
                        r.ue()?;
                    }
                    _ => {}
                }
            }
        }
    }
    let qp = i64::from(pps.init_qp) + i64::from(r.se()?);
    if !(0..=51).contains(&qp) {
        return Err(DecodeError::Invalid("slice QP"));
    }
    let (mut deblocking, mut alpha_offset, mut beta_offset) = (0, 0, 0);
    if pps.deblocking_control {
        deblocking = r.ue_max(2, "disable_deblocking_filter_idc")?;
        if deblocking != 1 {
            let (alpha, beta) = (r.se()?, r.se()?);
            if !(-6..=6).contains(&alpha) || !(-6..=6).contains(&beta) {
                return Err(DecodeError::Invalid("deblocking offsets"));
            }
            (alpha_offset, beta_offset) = (alpha * 2, beta * 2);
        }
    }
    let data_start = r.position().div_ceil(8);
    let header = SliceHeader {
        first_mb,
        pps_id,
        frame_num,
        idr_pic_id,
        poc_lsb,
        qp: qp as i32,
        deblocking,
        alpha_offset,
        beta_offset,
        data_start,
    };
    Ok((header, sps, pps))
}
