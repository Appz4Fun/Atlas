//! Neutral costly-cache records and checked PostgreSQL entrypoints.
//! No database files are discovered, opened, reset or harvested here. Native
//! cache calls require their original attempt scope and preserve driver errors.

mod native_costly;
pub(crate) mod nfo_key;
pub use native_costly::*;

use crate::probe::opensubtitles::Feature;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_PREDB_LOG_ROWS: i64 = 10_000;
pub const MAX_PREDB_LOG_BYTES: i64 = 512 << 20;
pub const MAX_HTTP_CACHE_ROWS: i64 = 100_000;
pub const MAX_HTTP_CACHE_BYTES: i64 = 512 << 20;
pub const MAX_OPENSUBTITLES_ROWS: i64 = 200_000;
pub const MAX_RAW_PROBES_ROWS: i64 = 250_000;
pub const MAX_RAW_PROBES_BYTES: i64 = 512 << 20;
pub const MAX_RAW_STILLS_ROWS: i64 = 300_000;
pub const MAX_RAW_AUDIO_PRINTS_ROWS: i64 = 300_000;
pub const MAX_RAW_ARCHIVES_ROWS: i64 = 250_000;
pub const MAX_RAW_ARCHIVE_NAME_BYTES: i64 = 256 << 20;
pub const LIVE_HTTP_CACHE_TTL_SECS: i64 = 12 * 3_600;

pub fn canonical_cache_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or("");
            !key.eq_ignore_ascii_case("api_key") && !key.eq_ignore_ascii_case("apikey")
        })
        .collect();
    if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }
}

pub fn is_empty_media_json(json: &Value) -> bool {
    if json.as_array().is_some_and(Vec::is_empty) {
        return true;
    }
    let Some(obj) = json.as_object() else {
        return false;
    };
    if obj.get("Response").and_then(Value::as_str).is_some_and(|r| r.eq_ignore_ascii_case("false")) {
        return true;
    }
    for key in ["results", "data", "movie_results", "tv_results", "tv_episode_results"] {
        if let Some(arr) = obj.get(key).and_then(Value::as_array)
            && arr.is_empty()
            && obj.len() <= 4
        {
            return true;
        }
    }
    false
}

pub fn is_immutable_predb_page(source_key: &str, _page: u32) -> bool {
    matches!(source_key, "nzedb_archive" | "dats_pciso_1998")
}

pub(crate) fn hash_file_fields_into(
    hasher: &mut Sha256,
    filename: &str,
    first_mid: &str,
    expected: Option<i64>,
    seen: &[u8],
) {
    hasher.update((filename.len() as u64).to_le_bytes());
    hasher.update(filename.as_bytes());
    hasher.update((first_mid.len() as u64).to_le_bytes());
    hasher.update(first_mid.as_bytes());
    hasher.update(expected.unwrap_or(-1).to_le_bytes());
    hasher.update((seen.len() as u64).to_le_bytes());
    hasher.update(seen);
}

fn nested_nzb_pw_key(name: &str) -> String {
    format!("{name}\0nzb")
}

pub fn get_opensubtitles(moviehash: &str) -> anyhow::Result<Option<Option<Feature>>> {
    use crate::pg::cache_actor::{NativeCacheCommand, NativeCacheReply, execute_if_native};
    match execute_if_native(NativeCacheCommand::LookupOpenSubtitles { hash: moviehash.to_owned() })? {
        Some(NativeCacheReply::Lookup(found)) => Ok(found),
        Some(_) => anyhow::bail!("invalid cache lookup reply"),
        None => anyhow::bail!("natural cache requires the native owner scope"),
    }
}

pub fn put_opensubtitles(moviehash: &str, feature: Option<&Feature>) -> anyhow::Result<()> {
    use crate::pg::cache_actor::{NativeCacheCommand, NativeCacheReply, execute_if_native};
    match execute_if_native(NativeCacheCommand::PutOpenSubtitles {
        hash: moviehash.to_owned(),
        feature: feature.cloned(),
        checked_at: chrono::Utc::now().timestamp(),
    })? {
        Some(NativeCacheReply::Stored) => Ok(()),
        Some(_) => anyhow::bail!("invalid cache store reply"),
        None => anyhow::bail!("natural cache requires the native owner scope"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedProbe {
    pub crc32: Option<i64>,
    pub agent: Option<String>,
    pub info: String,
    pub moviehash: Option<String>,
    pub nfo_password: Option<String>,
    pub cover: Option<(String, String)>,
}

pub struct ProbeRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
    pub crc32: Option<i64>,
    pub agent: Option<&'a str>,
    pub info: &'a str,
    pub moviehash: Option<&'a str>,
    pub parts: i64,
    pub complete: bool,
    pub config_stamp: u64,
    pub nfo_password: Option<&'a str>,
    pub cover: Option<(&'a str, &'a str)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedStill {
    pub position: u8,
    pub at_ms: Option<i64>,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub phash: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPrint {
    pub position: u8,
    pub at_ms: i64,
    pub items: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedArchiveFile {
    pub seq: i64,
    pub name: String,
    pub size: Option<u64>,
    pub source: i64,
    pub crc32: Option<u32>,
    pub mtime: Option<i64>,
    pub host_os: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedArchive {
    pub password: Option<i64>,
    pub encryption: Option<i64>,
    pub names_hidden: bool,
    pub password_check: Option<Vec<u8>>,
    pub listing_complete: bool,
    pub listing_damaged: bool,
    pub content_fp: Option<Vec<u8>>,
    pub files: Vec<SavedArchiveFile>,
    /// the packer and the archive's keys as they were kept; the salted ones under `key_salt` (`keys::salt_id`)
    pub keys: crate::archive::store::ArchiveKeys,
    pub key_salt: Option<i64>,
    /// the `archive::store::FACTS` the list was read with: 0 for one read before files kept their CRCs
    pub facts: i64,
}

pub struct ArchiveRecord<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
    pub source: i64,
    pub files: &'a [crate::archive::InnerFile],
    pub password: Option<i64>,
    pub encryption: Option<i64>,
    pub names_hidden: bool,
    pub password_check: Option<&'a [u8]>,
    pub parts: i64,
    pub complete: bool,
    pub listing_complete: bool,
    pub listing_damaged: bool,
    pub content_fp: Option<&'a [u8]>,
    pub max_names_bytes: i64,
    /// the packer and the archive's keys; the salted ones made under `key_salt` (`keys::salt_id`)
    pub keys: crate::archive::store::ArchiveKeys,
    pub key_salt: Option<i64>,
    /// `archive::store::FACTS` for a list read now
    pub facts: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPar2 {
    pub covers_all: bool,
    pub posted: Option<Vec<u8>>,
    pub sizes: Option<(i64, i64)>,
    pub content_fp: Option<Vec<u8>>,
    pub inventory: Option<crate::par2::Inventory>,
}

pub struct Par2Record<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    pub content_id: &'a [u8],
    pub covers_all: bool,
    pub posted: Option<&'a [u8]>,
    pub sizes: Option<(i64, i64)>,
    pub parts: i64,
    pub complete: bool,
    pub content_fp: Option<&'a [u8]>,
    pub inventory: Option<&'a crate::par2::Inventory>,
}

impl SavedArchiveFile {
    pub fn into_inner(self) -> crate::archive::InnerFile {
        crate::archive::InnerFile {
            name: self.name,
            size: self.size,
            crc32: self.crc32,
            mtime: self.mtime,
            host_os: self.host_os,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_url_strips_api_keys_only() {
        assert_eq!(
            canonical_cache_url("https://api.themoviedb.org/3/search/movie?api_key=SECRET123&query=Matrix&year=1999"),
            "https://api.themoviedb.org/3/search/movie?query=Matrix&year=1999"
        );
        assert_eq!(
            canonical_cache_url("https://www.omdbapi.com/?apikey=MYKEY&i=tt0133093"),
            "https://www.omdbapi.com/?i=tt0133093"
        );
        assert_eq!(
            canonical_cache_url("https://api.themoviedb.org/3/movie/603?api_key=SECRET"),
            "https://api.themoviedb.org/3/movie/603"
        );
    }
    #[test]
    fn media_empty_result_policy_keeps_real_and_metadata_rich_responses() {
        for text in ["[]", r#"{"results":[]}"#, r#"{"Response":"False"}"#, r#"{"movie_results":[],"page":1}"#] {
            assert!(is_empty_media_json(&serde_json::from_str(text).unwrap()), "{text}");
        }
        for text in [r#"{"results":[{"id":42}]}"#, r#"{"results":[],"a":1,"b":2,"c":3,"d":4}"#, "null", "42"] {
            assert!(!is_empty_media_json(&serde_json::from_str(text).unwrap()), "{text}");
        }
    }
    #[test]
    fn content_hash_retains_original_nul_filename_and_exact_mid_bytes() {
        let mut hash = Sha256::new();
        hash_file_fields_into(&mut hash, "a\0.bin", "<one@example.test>", Some(2), &[6]);
        let encoded =
            b"\x06\0\0\0\0\0\0\0a\0.bin\x12\0\0\0\0\0\0\0<one@example.test>\x02\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x06";
        assert_eq!(hash.finalize().as_slice(), Sha256::digest(encoded).as_slice());
        let mut changed = Sha256::new();
        hash_file_fields_into(&mut changed, "a�.bin", "<one@example.test>", Some(2), &[6]);
        assert_ne!(changed.finalize().as_slice(), Sha256::digest(encoded).as_slice());
    }
}

#[cfg(test)]
pub(crate) mod native_pg_opensubtitles_cache_tests;
