//! What is inside a release, without downloading it: inner file names and
//! whether an archive is passworded, from the first bytes of its articles.

pub mod aes;
pub mod follow;
pub mod link;
pub mod names;
pub mod nzb;
pub mod rar;
pub mod select;
pub mod sevenz;
pub mod steps;
pub mod store;
#[doc(hidden)]
pub mod testkit;
pub mod zip;

#[cfg(test)]
mod tests;

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

use crate::config::Config;
use crate::postprocess::step::{StepId, StepMask, mask};

pub const MAX_ENTRIES: usize = 4_096;
pub const MAX_NAME_BYTES: usize = 512;
pub const MAX_STORED: usize = 64;
pub const ARTICLE_CAP: usize = 4 * 1024 * 1024;
pub const PROBE_PREFIX: usize = 64 * 1024;
pub const PROBE_MIN_PARTS: i64 = 3;
pub const NAME_SOURCE_NFO: u8 = 10;
pub const NAME_SOURCE_INNER: u8 = 15;
pub const NAME_SOURCE_PAR2: u8 = 20;
/// step 8 is Part 5's naming step and 16 is Part 6's predb_match, asked directly because
/// naming may be off or skip a clean name. Unregistered bits are ignored until they exist
pub const RERUN_ON_INNER_FILES: StepMask = mask(StepId(8)) | mask(StepId(16));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rar4,
    Rar5,
    SevenZip,
    Zip,
    Par2,
    /// a posted NZB: a list of another post's files
    Nzb,
    Image,
    Media,
    Unknown,
}

/// What the decoded start of an article is: its bytes decide, never a file's name.
pub fn identify(head: &[u8]) -> Format {
    let at = |offset: usize, bytes: &[u8]| head.get(offset..offset + bytes.len()) == Some(bytes);
    if head.starts_with(b"Rar!\x1a\x07\x01\x00") {
        Format::Rar5
    } else if head.starts_with(b"Rar!\x1a\x07\x00") {
        Format::Rar4
    } else if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Format::SevenZip
    } else if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || ((head.starts_with(b"PK\x07\x08") || head.starts_with(b"PK00")) && at(4, b"PK\x03\x04"))
    {
        Format::Zip
    } else if head.starts_with(b"PAR2\0PKT") {
        Format::Par2
    } else if head.starts_with(b"\xff\xd8\xff")
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        || (head.starts_with(b"RIFF") && at(8, b"WEBP"))
        || head.starts_with(b"BM")
        || head.starts_with(b"II*\0")
        || head.starts_with(b"MM\0*")
    {
        Format::Image
    } else if head.starts_with(b"\x1a\x45\xdf\xa3")
        || at(4, b"ftyp")
        || (head.starts_with(b"RIFF") && at(8, b"AVI "))
        || head.starts_with(b"\0\0\x01\xba")
        || head.starts_with(b"fLaC")
        || head.starts_with(b"OggS")
        || head.starts_with(b"ID3")
    {
        Format::Media
    } else if nzb::is_nzb(head) {
        Format::Nzb
    } else {
        Format::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Password {
    No,
    Yes,
    Maybe,
}

impl Password {
    pub fn code(self) -> i64 {
        match self {
            Password::No => 0,
            Password::Yes => 1,
            Password::Maybe => 2,
        }
    }

    pub fn from_code(n: i64) -> Option<Password> {
        match n {
            0 => Some(Password::No),
            1 => Some(Password::Yes),
            2 => Some(Password::Maybe),
            _ => None,
        }
    }
}

/// How an archive is encrypted. Stored by code, soo the numbers stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    Rar4Aes128,
    Rar5Aes256,
    SevenZipAes256,
    ZipCrypto,
    ZipAes,
}

impl Encryption {
    pub fn code(self) -> i64 {
        match self {
            Encryption::Rar4Aes128 => 1,
            Encryption::Rar5Aes256 => 2,
            Encryption::SevenZipAes256 => 3,
            Encryption::ZipCrypto => 4,
            Encryption::ZipAes => 5,
        }
    }

    pub fn from_code(n: i64) -> Option<Encryption> {
        match n {
            1 => Some(Encryption::Rar4Aes128),
            2 => Some(Encryption::Rar5Aes256),
            3 => Some(Encryption::SevenZipAes256),
            4 => Some(Encryption::ZipCrypto),
            5 => Some(Encryption::ZipAes),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Encryption::Rar4Aes128 => "RAR4, AES-128",
            Encryption::Rar5Aes256 => "RAR5, AES-256",
            Encryption::SevenZipAes256 => "7z, AES-256",
            Encryption::ZipCrypto => "ZIP, ZipCrypto",
            Encryption::ZipAes => "ZIP, AES",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InnerFile {
    pub name: String,
    pub size: Option<u64>,
}

/// What an archive's headers keep to test a password with nothing decrypted but a few bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordTest {
    Rar5(rar::PasswordCheck),
    Zip(zip::PasswordCheck),
}

impl PasswordTest {
    /// A RAR5 check starts with its round count, which is never this high.
    const ZIP_TAG: u8 = 0xff;

    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            PasswordTest::Rar5(check) => check.to_bytes(),
            PasswordTest::Zip(check) => {
                let mut out = vec![Self::ZIP_TAG];
                out.extend(check.to_bytes());
                out
            }
        }
    }

    pub fn from_bytes(b: &[u8]) -> Option<PasswordTest> {
        if b.first() == Some(&Self::ZIP_TAG) {
            zip::PasswordCheck::from_bytes(b.get(1..)?).map(PasswordTest::Zip)
        } else {
            rar::PasswordCheck::from_bytes(b).map(PasswordTest::Rar5)
        }
    }

    /// Whether this is the password. None: too many rounds to find out here.
    pub fn accepts(&self, password: &str) -> Option<bool> {
        match self {
            PasswordTest::Rar5(check) => check.accepts(password),
            PasswordTest::Zip(check) => Some(check.accepts(password)),
        }
    }

    /// Whether a yes settles it. ZipCrypto's check is one byte, soo one wrong password in 256 passes.
    pub fn certain(&self) -> bool {
        !matches!(self, PasswordTest::Zip(zip::PasswordCheck::Crypto { .. }))
    }

    /// Fast verification of candidate password.
    pub fn verify(&self, candidate: &str) -> bool {
        match self {
            PasswordTest::Rar5(check) => check.verify(candidate),
            PasswordTest::Zip(check) => check.accepts(candidate),
        }
    }

    /// Asynchronous offload of verification to spawn_blocking.
    pub async fn verify_async(&self, candidate: &str) -> bool {
        match self {
            PasswordTest::Rar5(check) => check.verify_async(candidate).await,
            PasswordTest::Zip(check) => {
                let check = *check;
                let cand = candidate.to_string();
                tokio::task::spawn_blocking(move || check.accepts(&cand)).await.unwrap_or(false)
            }
        }
    }
}

/// What a parser read. Total functions: bad bytes give a listing that says so.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    /// at most MAX_ENTRIES, directories left out, names cleaned
    pub files: Vec<InnerFile>,
    /// encryption was proven
    pub encrypted: bool,
    /// and the names cannot be read
    pub headers_encrypted: bool,
    /// what kind, once `encrypted`
    pub encryption: Option<Encryption>,
    /// RAR5: what a password can be tried against, with nothing downloaded
    pub password_check: Option<PasswordTest>,
    /// the end of the list was seen
    pub complete: bool,
    /// a header failed its CRC or its structure
    pub damaged: bool,
    /// 7z: the header lies before the bytes given
    pub need_more_tail: bool,
}

static ARCHIVE_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(rar|zip|7z|ace|cab|arj|lzh|r\d{2})$").unwrap());

impl Listing {
    pub fn password(&self) -> Password {
        if self.encrypted {
            Password::Yes
        } else if self.files.is_empty() {
            if self.complete && !self.damaged { Password::No } else { Password::Maybe }
        } else if self.files.iter().all(|f| ARCHIVE_FILE.is_match(&f.name)) {
            Password::Maybe
        } else {
            Password::No
        }
    }

    /// A file for the list, unless it's full or its name is no use.
    pub(crate) fn push(&mut self, raw: &str, size: Option<u64>) -> bool {
        if self.files.len() >= MAX_ENTRIES {
            return false;
        }
        if let Some(name) = names::clean_name(raw) {
            self.files.push(InnerFile { name, size });
        }
        true
    }
}

pub fn crc32(b: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(b);
    crc.sum()
}

static INLINE: AtomicBool = AtomicBool::new(true);

/// Whether a save looks up real names itself, as it did before the stage.
pub fn inline_lookups() -> bool {
    INLINE.load(Ordering::Relaxed)
}

pub fn stage_owns_names(cfg: &Config) -> bool {
    cfg.postprocess.enabled && cfg.postprocess.body_fetches_per_minute > 0 && cfg.archive.enabled
}

pub fn sync_inline_lookups(cfg: &Config) {
    INLINE.store(!stage_owns_names(cfg), Ordering::Relaxed);
}

#[doc(hidden)]
pub fn set_inline_lookups(on: bool) {
    INLINE.store(on, Ordering::Relaxed);
}

pub use rar::{EncryptionMode, PasswordCheck, PasswordCheck as Rar5PswCheck, Rar5Header};

pub struct PasswordHarvester;

impl PasswordHarvester {
    pub fn extract_candidates(
        subject: &str,
        nfo: Option<&str>,
        nzb_meta: &std::collections::HashMap<String, String>,
    ) -> Vec<String> {
        let mut candidates = Vec::new();

        Self::harvest_subject(subject, &mut candidates);

        if let Some(nfo_text) = nfo {
            Self::harvest_nfo(nfo_text, &mut candidates);
        }

        for (k, v) in nzb_meta {
            let k_lower = k.to_lowercase();
            if k_lower == "password"
                || k_lower == "meta:password"
                || k_lower == "x-dnzb-password"
                || k_lower.contains("password")
                || k_lower == "pwd"
            {
                Self::push_candidate(v, &mut candidates);
            }
        }

        let mut counts: std::collections::HashMap<&str, (usize, usize)> =
            std::collections::HashMap::new();
        let mut arrival_order = 0;
        for c in &candidates {
            let trimmed = c.trim();
            if trimmed.is_empty() || Self::is_negative_phrase(trimmed) {
                continue;
            }
            let entry = counts.entry(trimmed).or_insert_with(|| {
                let idx = arrival_order;
                arrival_order += 1;
                (0, idx)
            });
            entry.0 += 1;
        }

        let mut ranked: Vec<(&str, usize, usize)> = counts
            .into_iter()
            .map(|(pw, (count, arrival))| (pw, count, arrival))
            .collect();

        // Sort primarily by frequency descending, tie-breaking by arrival order ascending
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));

        ranked
            .into_iter()
            .take(5)
            .map(|(pw, _, _)| pw.to_string())
            .collect()
    }

    pub fn extract(
        subject: &str,
        nfo: Option<&str>,
        nzb_meta: &std::collections::HashMap<String, String>,
    ) -> Vec<String> {
        Self::extract_candidates(subject, nfo, nzb_meta)
    }

    fn harvest_subject(subject: &str, out: &mut Vec<String>) {
        static RE_BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\{([^}]+)\}\}").unwrap());
        static RE_BRACKETS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\[pw:\s*([^\]]+)\]").unwrap());
        static RE_PASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bpass(?:word)?[:=]\s*(\S+)").unwrap());

        for cap in RE_BRACES.captures_iter(subject) {
            if let Some(m) = cap.get(1) {
                let s = m.as_str().trim();
                if !matches!(s.to_ascii_lowercase().as_str(), "password" | "pass" | "pw" | "pwd") {
                    out.push(s.to_string());
                }
            }
        }
        for cap in RE_BRACKETS.captures_iter(subject) {
            if let Some(m) = cap.get(1) {
                out.push(m.as_str().to_string());
            }
        }
        for cap in RE_PASS.captures_iter(subject) {
            let whole = cap.get(0).map_or("", |m| m.as_str());
            if matches!(whole.to_ascii_lowercase().as_str(), "pass=1" | "pass=2" | "pass=3") {
                continue;
            }
            if let Some(m) = cap.get(1) {
                out.push(m.as_str().to_string());
            }
        }
    }

    fn harvest_nfo(nfo: &str, out: &mut Vec<String>) {
        static RE_NFO: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)\b(?:password|passwd|pwd|pass|passwort|pw|mot\s+de\s+passe|mdp|wachtwoord)\b[\s._-]*(?:is\s*)?[:=]\s*([^\r\n]+)").unwrap()
        });

        for cap in RE_NFO.captures_iter(nfo) {
            let whole = cap.get(0).map_or("", |m| m.as_str()).to_ascii_lowercase();
            if whole.starts_with("pass=1") || whole.starts_with("pass=2") || whole.starts_with("pass=3") {
                continue;
            }
            if let Some(m) = cap.get(1) {
                let s = m.as_str().trim();
                out.push(s.to_string());
            }
        }
    }

    fn push_candidate(val: &str, out: &mut Vec<String>) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }

    pub(crate) fn is_negative_phrase(s: &str) -> bool {
        let lower = s.to_lowercase();
        let l = lower.trim();
        l == "none"
            || l == "no"
            || l == "null"
            || l == "nil"
            || l == "n/a"
            || l == "na"
            || l == "false"
            || l == "keins"
            || l == "keine"
            || l == "kein"
            || l == "aucun"
            || l == "geen"
            || l == "datum"
            || l == "available"
            || l == "needed"
            || l == "http://"
            || l == "https://"
            || l.starts_with("none")
            || l.starts_with("no pass")
            || l.contains("no password")
            || l.contains("password protected")
            || l.contains("see inside")
            || l.contains("see nfo")
            || l.contains("check inside")
            || l.contains("check nfo")
            || l.contains("inside nfo")
            || l.contains("in comments")
            || l.contains("check comments")
            || l.contains("see comments")
            || l.chars().any(|c| ('\u{2500}'..='\u{259f}').contains(&c))
    }
}

