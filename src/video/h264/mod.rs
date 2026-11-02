//! H.264 (AVC): intra pictures only, CABAC, 8 bit 4:2:0, progressive. Enough to turn a keyframe into a still.
//! CAVLC, interlacing, 4:2:2, 4:4:4 and high bit depths are refused as unsupported.

// block loops index several tables and buffers by the same position, as the standard writes them
#![allow(clippy::needless_range_loop)]

pub mod decode;
pub mod filter;
pub mod intra;
pub mod params;
pub mod slice;
pub mod tables;

use std::collections::HashMap;

use self::decode::Frame;
use self::params::{Pps, Sps};
use crate::video::bits::{DecodeError, Result, rbsp};
use crate::video::picture::{Crop, Picture};

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// What an avcC box holds: the length size of the frame's NAL units, and the parameter sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvcC {
    pub length_size: usize,
    pub units: Vec<Vec<u8>>,
}

pub fn parse_avcc(b: &[u8]) -> Result<AvcC> {
    if b.len() < 7 || b[0] != 1 {
        return Err(DecodeError::Invalid("avcC"));
    }
    let length_size = usize::from(b[4] & 3) + 1;
    if length_size == 3 {
        return Err(DecodeError::Invalid("avcC length size"));
    }
    let mut at = 5;
    let mut units = Vec::new();
    for kind in 0..2 {
        let count = usize::from(*b.get(at).ok_or(DecodeError::Truncated)?) & if kind == 0 { 0x1F } else { 0xFF };
        at += 1;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([
                *b.get(at).ok_or(DecodeError::Truncated)?,
                *b.get(at + 1).ok_or(DecodeError::Truncated)?,
            ]));
            units.push(b.get(at + 2..at + 2 + len).ok_or(DecodeError::Truncated)?.to_vec());
            at += 2 + len;
        }
    }
    Ok(AvcC { length_size, units })
}

/// Decodes the first picture in `units` (NAL units, in order, parameter sets first), which must be intra.
pub fn decode_keyframe(units: &[&[u8]]) -> Result<Picture> {
    let (picture, error) = decode(units, false);
    match error {
        Some(e) => Err(e),
        None => picture.ok_or(DecodeError::Truncated),
    }
}

/// The picture and any error. A picture with an error is filtered only when `partial` asks for it.
fn decode(units: &[&[u8]], partial: bool) -> (Option<Picture>, Option<DecodeError>) {
    let mut frame: Option<Frame> = None;
    let mut error = read_picture(units, &mut frame).err();
    let Some(mut f) = frame else { return (None, error.or(Some(DecodeError::Truncated))) };
    if error.is_none() && f.mb_slice.contains(&-1) {
        error = Some(DecodeError::Truncated);
    }
    if error.is_some() && !partial {
        return (None, error);
    }
    f.deblock();
    let [left, right, top, bottom] = f.sps.crop.map(|c| c as usize);
    let crop = Crop { left, right, top, bottom };
    (Some(Picture { bit_depth: 8, crop, full_range: f.sps.full_range, planes: f.planes }), error)
}

fn read_picture(units: &[&[u8]], frame: &mut Option<Frame>) -> Result<()> {
    let mut sps: HashMap<u32, Sps> = HashMap::new();
    let mut pps: HashMap<u32, Pps> = HashMap::new();
    // the first slice's frame_num and picture order count: a slice with others is the next picture
    let mut first: Option<(u32, Option<u32>, u32)> = None;
    for unit in units {
        let Some(&head) = unit.first() else { continue };
        if head & 0x80 != 0 {
            return Err(DecodeError::Invalid("NAL header"));
        }
        match head & 0x1F {
            NAL_SPS => {
                let s = params::parse_sps(unit)?;
                sps.insert(s.id, s);
            }
            NAL_PPS => {
                let p = params::parse_pps(unit)?;
                pps.insert(p.id, p);
            }
            slice::NAL_SLICE | slice::NAL_IDR => {
                let data = rbsp(&unit[1..]);
                let lookup = |id: u32| {
                    let p = pps.get(&id)?;
                    Some((sps.get(&p.sps_id)?.clone(), p.clone()))
                };
                if frame.is_some() {
                    // the next picture's first slice ends this one, read no further: it may be one we can't decode
                    match slice::parse(&data, head, lookup) {
                        Ok((h, _, _)) if Some((h.frame_num, h.idr_pic_id, h.poc_lsb)) == first => {}
                        _ => break,
                    }
                }
                let (header, s, p) = slice::parse(&data, head, lookup)?;
                let f = match frame {
                    Some(f) if f.pps.id != p.id || f.sps.id != s.id => {
                        return Err(DecodeError::Unsupported("parameter sets changing within a picture"));
                    }
                    Some(f) => f,
                    None => {
                        if u64::from(s.width_mbs) * u64::from(s.height_mbs) * 256 > crate::video::MAX_LUMA_SAMPLES {
                            return Err(DecodeError::Unsupported("picture bigger than 4K"));
                        }
                        first = Some((header.frame_num, header.idr_pic_id, header.poc_lsb));
                        frame.insert(Frame::new(s, p))
                    }
                };
                f.decode_slice(&data, header)?;
            }
            // an access unit delimiter, end of sequence or stream after the picture
            9..=11 if frame.is_some() => break,
            _ => {}
        }
    }
    Ok(())
}

/// NAL units of an Annex B stream (start code separated).
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |n| n - 3);
        while end > s && data[end - 1] == 0 {
            end -= 1;
        }
        units.push(&data[s..end]);
    }
    units
}

#[cfg(test)]
mod tests;
