//! GF(2) polynomial algebra and CRC-32 folding, from the par-rs crate.
//!
//! Atlas does no CRC math of its own: these are `par_rs::gf2`'s functions under the names
//! Atlas's callers already use (`crc32` for every BODY postprocess reads, `crc32_combine` for
//! file keys and IFSC folding, `crc32_unzero` for padded PAR2 slices). par-rs's `crc32` runs on
//! the CPU's CRC instructions where there are some (`pclmulqdq` on x86_64, `crc32x` on AArch64)
//! and slice-by-8 elsewhere; `examples/bench_parity.rs` measured it at ~50x the bit-at-a-time
//! loop this module used to have.
//!
//! `fold_yenc_indexed_parts` takes part numbers from 0 (`part=` of a yEnc header minus one) and
//! refuses one past `total` with `FoldError::LengthMismatch`.

pub use par_rs::gf2::{
    FoldError, GF2_ORDER, GF2_POLY, crc32, crc32_combine, crc32_unzero, crc32_zero_unpad, crc32_zeros,
    fold_yenc_indexed_parts, fold_yenc_parts, multmodp, x2nmodp, xpow,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_crc(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
            // standard byte table entry
            let mut p = idx as u32;
            for _ in 0..8 {
                p = if (p & 1) != 0 { (p >> 1) ^ GF2_POLY } else { p >> 1 };
            }
            crc = (crc >> 8) ^ p;
        }
        crc ^ 0xFFFF_FFFF
    }

    #[test]
    fn combine_two_parts() {
        let a = b"Hello, world! 1234567890";
        let b = b" Testing crc32 combine with arbitrary length!";
        let full = [a.as_slice(), b.as_slice()].concat();

        let c_a = raw_crc(a);
        let c_b = raw_crc(b);
        let c_full = raw_crc(&full);

        assert_eq!(crc32_combine(c_a, c_b, b.len()), c_full);
    }

    #[test]
    fn zero_unpadding() {
        let data = b"This is tail data that gets padded by PAR2 IFSC.";
        let zeros = [0u8; 128];
        let padded = [data.as_slice(), &zeros].concat();

        let c_data = raw_crc(data);
        let c_padded = raw_crc(&padded);

        assert_eq!(crc32_unzero(c_padded, zeros.len()), c_data);
        assert_eq!(crc32_unzero(c_data, 0), c_data);
    }

    #[test]
    fn fold_multipart() {
        let parts: Vec<Vec<u8>> = (0..5).map(|i| vec![i as u8; 1000 + i * 200]).collect();
        let full: Vec<u8> = parts.concat();

        let crc_parts: Vec<(u32, usize)> = parts.iter().map(|p| (raw_crc(p), p.len())).collect();
        let folded = fold_yenc_parts(&crc_parts).unwrap();
        assert_eq!(folded, raw_crc(&full));
    }

    #[test]
    fn gigabyte_scale_unzero_instant() {
        let c = 0x1234_5678;
        // 1 GB zeros exponentiation in microseconds
        let unzeroed = crc32_unzero(c, 1_073_741_824);
        assert_ne!(unzeroed, 0);
    }

    #[test]
    fn polynomial_shifting_invariance() {
        let c = 0x9ABC_DEF0;
        let l1 = 1234usize;
        let l2 = 5678usize;
        let c_step = crc32_combine(crc32_combine(c, 0, l1), 0, l2);
        let c_direct = crc32_combine(c, 0, l1 + l2);
        assert_eq!(c_step, c_direct);
    }

    #[test]
    fn missing_intermediate_part() {
        let parts = vec![(0, 0x1111, 100), (1, 0x2222, 100), (3, 0x4444, 100)];
        assert_eq!(fold_yenc_indexed_parts(4, &parts), Err(FoldError::MissingPart(2)));
    }

    #[test]
    fn conflicting_duplicate_parts_return_corrupt_part_error() {
        let identical = vec![(0, 0x1111, 100), (0, 0x1111, 100), (1, 0x2222, 100)];
        assert!(fold_yenc_indexed_parts(2, &identical).is_ok());

        let conflicting_crc = vec![(0, 0x1111, 100), (0, 0x9999, 100), (1, 0x2222, 100)];
        assert_eq!(fold_yenc_indexed_parts(2, &conflicting_crc), Err(FoldError::CorruptPart(0)));

        let conflicting_len = vec![(0, 0x1111, 100), (1, 0x2222, 100), (1, 0x2222, 200)];
        assert_eq!(fold_yenc_indexed_parts(2, &conflicting_len), Err(FoldError::CorruptPart(1)));
    }

    #[test]
    fn a_part_number_past_the_total_is_refused() {
        // yEnc numbers parts from 1; callers pass part - 1, so a 1-based list is out by one
        let one_based = vec![(1, 0x1111, 100), (2, 0x2222, 100)];
        assert_eq!(fold_yenc_indexed_parts(2, &one_based), Err(FoldError::LengthMismatch));
    }

    #[test]
    fn crc32_matches_the_bitwise_reference_at_every_length() {
        // par-rs picks a hardware kernel (pclmulqdq / crc32x) or slice-by-8; every length up to
        // a few kernel blocks and every start offset must give the reference value
        let data: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for len in (0..600).chain([1023, 1024, 1025, 4095]) {
            for start in [0usize, 1, 3, 7] {
                let slice = &data[start..start + len.min(data.len() - start)];
                assert_eq!(crc32(slice), raw_crc(slice), "len {len} start {start}");
            }
        }
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
