//! AES-128, AES-192, and AES-256 (FIPS 197) for RAR4/RAR5 CBC and WinZip CTR decryption,
//! backed by `zipfast::crypto` and `zipview`.
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub struct Aes(zipfast::crypto::AesKey);

impl Aes {
    pub fn new(key: &[u8]) -> Option<Self> {
        zipfast::crypto::AesKey::new(key).map(Self)
    }

    pub fn encrypt_block(&self, s: &mut [u8; 16]) {
        *s = self.0.encrypt_block(s);
    }

    pub fn decrypt_block(&self, s: &mut [u8; 16]) {
        *s = self.0.decrypt_block(s);
    }
}

/// AES-CBC decryption over all complete 16-byte blocks in `data`.
pub fn decrypt_cbc(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    zipfast::crypto::decrypt_cbc(key, iv, data)
}

/// WinZip AES-CTR decryption (counter starts at 1 in little-endian byte order).
pub fn decrypt_winzip_ctr(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    zipview::decrypt_winzip_ctr(key, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fips_197_test_vectors() {
        let pt: [u8; 16] =
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        // AES-128
        let k128: [u8; 16] =
            [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f];
        let c128: [u8; 16] =
            [0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a];
        let aes128 = Aes::new(&k128).unwrap();
        let mut b = pt;
        aes128.encrypt_block(&mut b);
        assert_eq!(b, c128);
        aes128.decrypt_block(&mut b);
        assert_eq!(b, pt);

        // AES-256
        let k256: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
            0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
        ];
        let c256: [u8; 16] =
            [0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49, 0x60, 0x89];
        let aes256 = Aes::new(&k256).unwrap();
        let mut b = pt;
        aes256.encrypt_block(&mut b);
        assert_eq!(b, c256);
        aes256.decrypt_block(&mut b);
        assert_eq!(b, pt);
    }
}
