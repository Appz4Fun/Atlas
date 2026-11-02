//! A slice segment's header (H.265 7.3.6.1), for intra slices.

use super::nal::{self, NalHeader};
use super::params::{Pps, Sps, st_ref_pic_set};
use crate::video::bits::{BitReader, DecodeError, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SliceHeader {
    pub first_in_picture: bool,
    pub pps_id: u32,
    pub dependent: bool,
    /// the first coding tree block, in raster order
    pub address: u32,
    pub slice_type: u32,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub qp: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub deblocking_disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub loop_filter_across_slices: bool,
    /// where the slice data starts, in bytes of the RBSP
    pub data_offset: usize,
}

pub const SLICE_I: u32 = 2;

fn ceil_log2(n: u32) -> u32 {
    if n <= 1 { 0 } else { 32 - (n - 1).leading_zeros() }
}

/// Reads the header from a slice's RBSP (its NAL header included). A dependent slice segment takes what it
/// doesn't say from `previous`.
pub fn parse(
    rbsp: &[u8],
    h: NalHeader,
    sps_of: impl Fn(u32) -> Option<(Sps, Pps)>,
    previous: Option<&SliceHeader>,
) -> Result<(SliceHeader, Sps, Pps)> {
    let mut r = BitReader::new(rbsp.get(2..).ok_or(DecodeError::Truncated)?);
    let first = r.flag()?;
    if nal::is_irap(h.kind) {
        r.skip(1)?;
    }
    let pps_id = r.ue_max(63, "PPS id")?;
    let (sps, pps) = sps_of(pps_id).ok_or(DecodeError::Invalid("PPS id"))?;
    let mut s = SliceHeader { first_in_picture: first, pps_id, ..SliceHeader::default() };
    if !first {
        if pps.dependent_slices {
            s.dependent = r.flag()?;
        }
        let ctbs = sps.width_ctbs() * sps.height_ctbs();
        s.address = r.bits(ceil_log2(ctbs))?;
        if s.address >= ctbs {
            return Err(DecodeError::Invalid("slice address"));
        }
    }
    if s.dependent {
        let p = previous.ok_or(DecodeError::Invalid("dependent slice with nothing before it"))?;
        s = SliceHeader { first_in_picture: false, dependent: true, address: s.address, data_offset: 0, ..p.clone() };
    } else {
        r.skip(pps.extra_slice_header_bits as usize)?;
        s.slice_type = r.ue_max(2, "slice type")?;
        if s.slice_type != SLICE_I {
            return Err(DecodeError::Unsupported("inter slices"));
        }
        if pps.output_flag_present {
            r.skip(1)?;
        }
        if !nal::is_idr(h.kind) {
            r.skip(sps.log2_max_poc_lsb as usize)?;
            let sets = sps.rps_delta_pocs.len();
            if !r.flag()? {
                st_ref_pic_set(&mut r, sets, &sps.rps_delta_pocs, true)?;
            } else if sets > 1 {
                r.skip(ceil_log2(sets as u32) as usize)?;
            }
            if let Some(in_sps) = sps.long_term_refs {
                let from_sps = if in_sps > 0 { r.ue_max(in_sps, "long term refs")? } else { 0 };
                let pics = r.ue_max(32, "long term refs")?;
                for i in 0..from_sps + pics {
                    if i < from_sps {
                        if in_sps > 1 {
                            r.skip(ceil_log2(in_sps) as usize)?;
                        }
                    } else {
                        r.skip(sps.log2_max_poc_lsb as usize + 1)?;
                    }
                    if r.flag()? {
                        r.ue()?;
                    }
                }
            }
            if sps.temporal_mvp {
                r.skip(1)?;
            }
        }
        if sps.sao {
            s.sao_luma = r.flag()?;
            s.sao_chroma = r.flag()?;
        }
        s.qp = pps.init_qp + r.se()?;
        let min_qp = -6 * (i32::from(sps.bit_depth_luma) - 8);
        if !(min_qp..=51).contains(&s.qp) {
            return Err(DecodeError::Invalid("slice QP"));
        }
        if pps.slice_chroma_qp_offsets {
            s.cb_qp_offset = r.se()?;
            s.cr_qp_offset = r.se()?;
        }
        let override_ = pps.deblocking_override && r.flag()?;
        if override_ {
            s.deblocking_disabled = r.flag()?;
            if !s.deblocking_disabled {
                s.beta_offset = r.se()? * 2;
                s.tc_offset = r.se()? * 2;
            }
        } else {
            s.deblocking_disabled = pps.deblocking_disabled;
            s.beta_offset = pps.beta_offset;
            s.tc_offset = pps.tc_offset;
        }
        s.loop_filter_across_slices = pps.loop_filter_across_slices;
        if pps.loop_filter_across_slices && (s.sao_luma || s.sao_chroma || !s.deblocking_disabled) {
            s.loop_filter_across_slices = r.flag()?;
        }
    }
    if pps.tiles || pps.entropy_coding_sync {
        let n = r.ue_max(sps.height_ctbs() * sps.width_ctbs(), "entry points")?;
        if n > 0 {
            let len = r.ue_max(31, "entry point size")? + 1;
            for _ in 0..n {
                r.skip(len as usize)?;
            }
        }
    }
    if pps.slice_header_extension {
        let len = r.ue_max(256, "slice header extension")?;
        r.skip(len as usize * 8)?;
    }
    // byte_alignment(): a one, then zeros
    if r.bit()? != 1 {
        return Err(DecodeError::Invalid("slice header alignment"));
    }
    r.byte_align();
    s.data_offset = 2 + r.position() / 8;
    Ok((s, sps, pps))
}
