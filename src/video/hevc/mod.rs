//! HEVC (H.265): intra pictures only, Main and Main 10, 4:2:0. Enough to turn a keyframe into a still.

pub mod cabac;
pub mod decode;
pub mod filter;
pub mod intra;
pub mod nal;
pub mod params;
pub mod slice;
pub mod transform;

use std::collections::HashMap;

use self::decode::Frame;
use self::params::{Pps, Sps};
use self::slice::SliceHeader;
use crate::video::bits::{DecodeError, Result, rbsp};
use crate::video::picture::{Crop, Picture};

/// Decodes the first picture in `units` (NAL units, in order, parameter sets first), which must be intra.
pub fn decode_keyframe(units: &[&[u8]]) -> Result<Picture> {
    let (picture, error) = decode(units, false);
    match error {
        Some(e) => Err(e),
        None => picture.ok_or(DecodeError::Truncated),
    }
}

/// What was decoded before anything went wrong, and what did.
#[cfg(test)]
pub(crate) fn decode_partly(units: &[&[u8]]) -> (Option<Picture>, Option<DecodeError>) {
    decode(units, true)
}

/// The picture and any error. A picture with an error is filtered only when `partial` asks for it: no one else
/// looks at it.
fn decode(units: &[&[u8]], partial: bool) -> (Option<Picture>, Option<DecodeError>) {
    let mut frame: Option<Frame> = None;
    let mut error = read_picture(units, &mut frame).err();
    let Some(mut f) = frame else { return (None, error.or(Some(DecodeError::Truncated))) };
    // a slice that never came leaves its part of the picture grey
    if error.is_none() && f.ctb_slice.contains(&-1) {
        error = Some(DecodeError::Truncated);
    }
    if error.is_some() && !partial {
        return (None, error);
    }
    f.deblock();
    f.apply_sao();
    let crop = Crop {
        left: f.sps.crop[0] as usize,
        right: f.sps.crop[1] as usize,
        top: f.sps.crop[2] as usize,
        bottom: f.sps.crop[3] as usize,
    };
    (Some(Picture { bit_depth: f.sps.bit_depth_luma, crop, full_range: f.sps.full_range, planes: f.planes }), error)
}

fn read_picture(units: &[&[u8]], frame: &mut Option<Frame>) -> Result<()> {
    let mut sps: HashMap<u32, Sps> = HashMap::new();
    let mut pps: HashMap<u32, Pps> = HashMap::new();
    let mut previous: Option<SliceHeader> = None;
    for unit in units {
        let h = nal::header(unit)?;
        if h.layer != 0 {
            // other layers: Dolby Vision's enhancement layer, multiview
            continue;
        }
        match h.kind {
            nal::SPS => {
                let s = params::parse_sps(unit)?;
                sps.insert(s.id, s);
            }
            nal::PPS => {
                let p = params::parse_pps(unit)?;
                pps.insert(p.id, p);
            }
            kind if nal::is_slice(kind) => {
                if frame.is_none() && !nal::is_irap(kind) {
                    return Err(DecodeError::Unsupported("a picture that isn't a keyframe"));
                }
                // the next picture's first slice ends this one, read no further: it may be one we can't decode
                if frame.is_some() && unit.get(2).is_some_and(|b| b & 0x80 != 0) {
                    break;
                }
                let data = rbsp(unit);
                let lookup = |id: u32| {
                    let p = pps.get(&id)?;
                    Some((sps.get(&p.sps_id)?.clone(), p.clone()))
                };
                let (header, s, p) = slice::parse(&data, h, lookup, previous.as_ref())?;
                let f = match frame {
                    Some(f) if f.pps.id != p.id || f.sps.id != s.id => {
                        return Err(DecodeError::Unsupported("parameter sets changing within a picture"));
                    }
                    Some(f) => f,
                    None if header.first_in_picture => {
                        // Level 6.2's biggest picture
                        if u64::from(s.width) * u64::from(s.height) > crate::video::MAX_LUMA_SAMPLES {
                            return Err(DecodeError::Unsupported("picture bigger than 4K"));
                        }
                        frame.insert(Frame::new(s, p))
                    }
                    None => return Err(DecodeError::Invalid("picture starts mid way")),
                };
                f.decode_slice(&data, header.clone())?;
                previous = Some(header);
            }
            // end of sequence or stream, or an access unit delimiter after the picture
            35..=37 if frame.is_some() => break,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
