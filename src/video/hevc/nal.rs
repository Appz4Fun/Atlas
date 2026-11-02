//! HEVC NAL units: from an Annex B byte stream (`00 00 01` start codes) or from Matroska and MP4 samples
//! (each unit after its length), and the header that says what each one is.

use crate::video::bits::{DecodeError, Result};

pub const VPS: u8 = 32;
pub const SPS: u8 = 33;
pub const PPS: u8 = 34;
pub const PREFIX_SEI: u8 = 39;

/// BLA, IDR and CRA: pictures a decoder can start at.
pub fn is_irap(kind: u8) -> bool {
    (16..=23).contains(&kind)
}

pub fn is_idr(kind: u8) -> bool {
    kind == 19 || kind == 20
}

/// A coded slice of a picture, of any kind.
pub fn is_slice(kind: u8) -> bool {
    kind <= 21
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NalHeader {
    pub kind: u8,
    pub layer: u8,
    pub temporal_id: u8,
}

pub fn header(nal: &[u8]) -> Result<NalHeader> {
    let (a, b) = (*nal.first().ok_or(DecodeError::Truncated)?, *nal.get(1).ok_or(DecodeError::Truncated)?);
    if a & 0x80 != 0 {
        return Err(DecodeError::Invalid("NAL header"));
    }
    let temporal = b & 7;
    if temporal == 0 {
        return Err(DecodeError::Invalid("NAL temporal id"));
    }
    Ok(NalHeader { kind: (a >> 1) & 0x3f, layer: (a & 1) << 5 | b >> 3, temporal_id: temporal - 1 })
}

/// The units of an Annex B stream, without their start codes.
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
    let mut out = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        // the zero before a 4 byte start code, and trailing zeros, belong to no unit
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            out.push(&data[start..end]);
        }
    }
    out
}

/// The units of a sample, each after a big endian length of `length_size` bytes.
pub fn split_sized(data: &[u8], length_size: usize) -> Result<Vec<&[u8]>> {
    if !(1..=4).contains(&length_size) {
        return Err(DecodeError::Invalid("NAL length size"));
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let len_bytes = data.get(at..at + length_size).ok_or(DecodeError::Truncated)?;
        let len = len_bytes.iter().fold(0usize, |n, b| n << 8 | usize::from(*b));
        at += length_size;
        let unit = data.get(at..at.checked_add(len).ok_or(DecodeError::Truncated)?).ok_or(DecodeError::Truncated)?;
        if !unit.is_empty() {
            if out.len() == crate::video::MAX_UNITS {
                return Err(DecodeError::Invalid("too many NAL units"));
            }
            out.push(unit);
        }
        at += len;
    }
    Ok(out)
}

/// What a Matroska or MP4 `hvcC` record holds: the parameter sets, and the size of each sample's lengths.
pub struct HvcC<'a> {
    pub length_size: usize,
    pub units: Vec<&'a [u8]>,
}

pub fn parse_hvcc(data: &[u8]) -> Result<HvcC<'_>> {
    if data.len() < 23 {
        return Err(DecodeError::Truncated);
    }
    let length_size = usize::from(data[21] & 3) + 1;
    let arrays = data[22];
    let mut at = 23;
    let mut units = Vec::new();
    for _ in 0..arrays {
        let head = data.get(at..at + 3).ok_or(DecodeError::Truncated)?;
        let count = usize::from(head[1]) << 8 | usize::from(head[2]);
        at += 3;
        for _ in 0..count {
            let len = data.get(at..at + 2).ok_or(DecodeError::Truncated)?;
            let len = usize::from(len[0]) << 8 | usize::from(len[1]);
            at += 2;
            units.push(data.get(at..at + len).ok_or(DecodeError::Truncated)?);
            at += len;
        }
    }
    Ok(HvcC { length_size, units })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_on_start_codes() {
        let data = [0, 0, 0, 1, 0x40, 1, 9, 0, 0, 1, 0x42, 1, 7, 0, 0];
        let units = split_annex_b(&data);
        assert_eq!(units, [&[0x40, 1, 9][..], &[0x42, 1, 7][..]]);
        assert_eq!(header(units[0]).unwrap(), NalHeader { kind: VPS, layer: 0, temporal_id: 0 });
        assert_eq!(header(units[1]).unwrap().kind, SPS);
    }

    #[test]
    fn sized_units_split_and_bad_lengths_fail() {
        assert_eq!(split_sized(&[0, 2, 0x40, 1, 0, 1, 9], 2).unwrap(), [&[0x40, 1][..], &[9][..]]);
        assert_eq!(split_sized(&[0, 9, 1], 2), Err(DecodeError::Truncated));
    }
}
