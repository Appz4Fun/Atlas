//! Serializable SHA-256 checkpoint state. Compression remains in sha2.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DigestState {
    words: [u32; 8],
    bytes: u64,
    pending: Vec<u8>,
}
impl Default for DigestState {
    fn default() -> Self {
        Self {
            words: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            bytes: 0,
            pending: Vec::new(),
        }
    }
}
impl DigestState {
    fn check(&self) -> Result<()> {
        ensure!(
            self.pending.len() < 64 && self.bytes % 64 == self.pending.len() as u64 && self.bytes <= u64::MAX / 8,
            "invalid history digest continuation"
        );
        Ok(())
    }
    pub(crate) fn update(&mut self, mut bytes: &[u8]) -> Result<()> {
        self.check()?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= u64::MAX / 8)
            .context("history digest length overflow")?;
        if !self.pending.is_empty() {
            let n = bytes.len().min(64 - self.pending.len());
            self.pending.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.pending.len() == 64 {
                let block = sha2::digest::generic_array::GenericArray::clone_from_slice(&self.pending);
                sha2::compress256(&mut self.words, std::slice::from_ref(&block));
                self.pending.clear();
            }
        }
        while bytes.len() >= 64 {
            let block = sha2::digest::generic_array::GenericArray::clone_from_slice(&bytes[..64]);
            sha2::compress256(&mut self.words, std::slice::from_ref(&block));
            bytes = &bytes[64..];
        }
        self.pending.extend_from_slice(bytes);
        self.check()
    }
    pub(crate) fn finish(&self) -> Result<[u8; 32]> {
        self.check()?;
        let mut words = self.words;
        let mut tail = self.pending.clone();
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&(self.bytes * 8).to_be_bytes());
        for bytes in tail.chunks_exact(64) {
            let block = sha2::digest::generic_array::GenericArray::clone_from_slice(bytes);
            sha2::compress256(&mut words, std::slice::from_ref(&block));
        }
        let mut out = [0; 32];
        for (i, word) in words.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn restartable_digest_matches_sha256_at_all_block_boundaries() {
        let input: Vec<u8> = (0..1371).map(|i| (i % 251) as u8).collect();
        for width in [1, 7, 63, 64, 65, 511] {
            let mut state = DigestState::default();
            for bytes in input.chunks(width) {
                state.update(bytes).unwrap();
                state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
            }
            assert_eq!(state.finish().unwrap(), <[u8; 32]>::from(Sha256::digest(&input)));
        }
    }
    #[test]
    fn final_padding_lengths_and_corrupt_state_are_checked() {
        for n in [0, 1, 55, 56, 63, 64, 65, 119, 120, 127, 128, 129] {
            let bytes = vec![0xa5; n];
            let mut s = DigestState::default();
            s.update(&bytes).unwrap();
            assert_eq!(s.finish().unwrap(), <[u8; 32]>::from(Sha256::digest(&bytes)), "length {n}");
        }
        let mut state = DigestState::default();
        state.bytes = 1;
        assert!(state.finish().is_err());
        assert!(state.update(b"x").is_err());
        let mut state = DigestState::default();
        state.pending = vec![0; 64];
        assert!(state.finish().is_err());
    }
}
