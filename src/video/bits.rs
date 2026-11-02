//! Reading a NAL unit's payload bit by bit.

/// Why a stream couldn't be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// it ended before what it said was there
    Truncated,
    /// a value out of what the standard allows
    Invalid(&'static str),
    /// valid, but not something this decoder does
    Unsupported(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "the stream ends early"),
            DecodeError::Invalid(what) => write!(f, "invalid {what}"),
            DecodeError::Unsupported(what) => write!(f, "unsupported: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub type Result<T> = std::result::Result<T, DecodeError>;

/// A NAL unit's bytes with the emulation prevention bytes (the 3 in `00 00 03`) taken out.
pub fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Big endian bits, as the parameter sets and slice headers are written.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0 }
    }

    /// bits read so far
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub fn bit(&mut self) -> Result<u32> {
        let byte = *self.data.get(self.pos / 8).ok_or(DecodeError::Truncated)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u32::from(bit))
    }

    pub fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? == 1)
    }

    /// `n` bits, at most 32
    pub fn bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        let mut v = 0u32;
        for _ in 0..n {
            v = v << 1 | self.bit()?;
        }
        Ok(v)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        if self.bits_left() < n {
            return Err(DecodeError::Truncated);
        }
        self.pos += n;
        Ok(())
    }

    /// ue(v): exp-Golomb, unsigned
    pub fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(DecodeError::Invalid("exp-Golomb code"));
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let rest = self.bits(zeros)?;
        ((1u64 << zeros) - 1 + u64::from(rest)).try_into().map_err(|_| DecodeError::Invalid("exp-Golomb code"))
    }

    /// se(v): exp-Golomb, signed
    pub fn se(&mut self) -> Result<i32> {
        let k = i64::from(self.ue()?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).map_err(|_| DecodeError::Invalid("exp-Golomb code"))
    }

    /// ue(v) that must be at most `max`
    pub fn ue_max(&mut self, max: u32, what: &'static str) -> Result<u32> {
        let v = self.ue()?;
        if v > max {
            return Err(DecodeError::Invalid(what));
        }
        Ok(v)
    }

    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    pub fn is_byte_aligned(&self) -> bool {
        self.pos.is_multiple_of(8)
    }

    /// what's after the bits read, from the next whole byte
    pub fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos.div_ceil(8)..).unwrap_or(&[])
    }

    /// whether more than the trailing stop bit is left
    pub fn more_rbsp_data(&self) -> bool {
        let Some(last) = self.data.iter().rposition(|b| *b != 0) else { return false };
        let stop = last * 8 + 7 - self.data[last].trailing_zeros() as usize;
        self.pos < stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulation_prevention_bytes_come_out() {
        assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 0, 3]), [0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(rbsp(&[0, 3, 0, 0, 3]), [0, 3, 0, 0]);
    }

    #[test]
    fn exp_golomb_reads() {
        // 1 | 010 | 011 | 00100 | 00101 -> 0, 1, 2, 3, 4
        let data = [0b1010_0110, 0b0100_0010, 0b1000_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(
            [r.ue().unwrap(), r.ue().unwrap(), r.ue().unwrap(), r.ue().unwrap(), r.ue().unwrap()],
            [0, 1, 2, 3, 4]
        );
        let mut r = BitReader::new(&data);
        assert_eq!([r.se().unwrap(), r.se().unwrap(), r.se().unwrap(), r.se().unwrap()], [0, 1, -1, 2]);
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 0]).ue(), Err(DecodeError::Invalid("exp-Golomb code")));
        assert_eq!(BitReader::new(&[0]).ue(), Err(DecodeError::Truncated));
    }

    #[test]
    fn the_stop_bit_ends_the_data() {
        let mut r = BitReader::new(&[0b1010_0000, 0]);
        assert!(r.more_rbsp_data());
        r.bits(2).unwrap();
        assert!(!r.more_rbsp_data(), "what's left is the stop bit");
    }
}
