//! Keys worth indexing: the few keys that tie a release to others, kept in one small table soo the release page
//! can say "in 12 other releases" and list them.
//!
//! Each key is the first 8 bytes of a SHA-256, stored as a big-endian i64, soo a row is four integers and an index
//! entry is one. The page prints the same digest as hex: 16 digits for the layout and Lines keys, the first 12 for
//! the salted account and host keys. Two keys the twin and probe work already index are counted from their own
//! tables, not copied here: the PAR2 set id (`par2_files.set_id`) and the OpenSubtitles hash
//! (`release_probe.moviehash`).
//!
//! Two more come from the CRCs posters and servers leave in the article itself. The file key is the main file's
//! size and whole-file CRC32, as its last part's yEnc trailer gives it: the same bytes whatever the file is called,
//! the article size it was cut at or the tool that posted it, and matched against the files a PAR2 set describes
//! (`par2_files.size`, `par2_files.crc32`) as well as other releases' file keys. The first-article key is the CRC32 a
//! Highwinds-family or Usenet.Farm server stamps on every article (`X-Received-Body-CRC`, over the body as posted
//! with its trailing dot line), with the part's number and decoded size: the same article posted again byte for
//! byte, under any Message-ID, poster or subject.
//!
//! Four come from an archive's own headers (`archive::facts`), kept by the archive step: a file inside an archive by
//! its unpacked size and CRC32 (`archive_inner_files`), the same in a RAR, a 7z or a ZIP and against a file posted
//! bare or described by a PAR2 set; the random values one archive carries and no other (`archive_release.twin_key`:
//! RAR5 `-hp` salt and check, a RAR4 or RAR5 file's salt, 7z's AES IV or start header); and two salted ones, the Unix
//! owner a packer recorded and the packer's clock zone with how it packed (`owner_key`, `zone_key`).
//!
//! These are candidates for a reader, never a merge: no key here groups, links or completes anything on its own.
//! The raw posting account and host, an archive's owner and zone are never stored, only their salted digests, and the
//! archive keys are never printed: the page shows how many releases share one, not its value.

use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params_from_iter};
use sha2::{Digest, Sha256};

use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
use crate::probe::ProbeInfo;
use crate::store;

pub const RELEASE_KEYS: ShardTable = ShardTable {
    name: "release_keys",
    create: "create table if not exists release_keys (
        release_id INTEGER PRIMARY KEY,
        account    INTEGER,
        host       INTEGER,
        layout     INTEGER,
        lines      INTEGER,
        file_crc   INTEGER,
        body_crc   INTEGER
    )",
    indexes: &[
        (
            "release_keys_account",
            "create index if not exists release_keys_account on release_keys(account) where account is not null",
        ),
        (
            "release_keys_host",
            "create index if not exists release_keys_host on release_keys(host) where host is not null",
        ),
        (
            "release_keys_layout",
            "create index if not exists release_keys_layout on release_keys(layout) where layout is not null",
        ),
        (
            "release_keys_lines",
            "create index if not exists release_keys_lines on release_keys(lines) where lines is not null",
        ),
        (
            "release_keys_file_crc",
            "create index if not exists release_keys_file_crc on release_keys(file_crc) where file_crc is not null",
        ),
        (
            "release_keys_body_crc",
            "create index if not exists release_keys_body_crc on release_keys(body_crc) where body_crc is not null",
        ),
        (
            "release_keys_video",
            "create index if not exists release_keys_video on release_keys(video) where video is not null",
        ),
        (
            "release_keys_audio",
            "create index if not exists release_keys_audio on release_keys(audio) where audio is not null",
        ),
    ],
    release_key: Some("release_id"),
    // `video`: the first keyframe's key, from the probe's `keyframe_key`; `audio`: the first audio track's
    // `stream_key`
    added: &[("file_crc", "INTEGER"), ("body_crc", "INTEGER"), ("video", "INTEGER"), ("audio", "INTEGER")],
};

const UPSERT: &str =
    "insert or replace into release_keys (release_id, account, host, layout, lines, file_crc, body_crc, video, audio)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
const DELETE: &str = "delete from release_keys where release_id = ?1";

pub const KEYS_STEP: StepId = StepId(40);

/// The most other releases a count goes to, and the most the list page shows.
pub const COUNT_CAP: i64 = 1_000;
pub const LIST_CAP: i64 = 100;
/// PAR2 sets a release is looked up by, at most: nearly every release has one.
const SETS_CAP: usize = 4;
/// Files a Lines key is taken over, at most: the page reads as many (`web_meta::FILES_CAP`).
const FILES_CAP: i64 = 5_000;
/// Files (size, CRC32) a release is looked up by, at most: its probed main file and what its PAR2 sets describe.
const CRC_FILES_CAP: usize = 8;
/// Files smaller than this aren't looked up by their CRC: an empty or tiny file (a 0-byte marker, a stock .sfv) is
/// the same in thousands of unrelated posts.
pub const CRC_MIN_SIZE: i64 = 1 << 20;

/// What another release can share with this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Account,
    Host,
    Layout,
    Lines,
    Par2,
    MovieHash,
    FileCrc,
    BodyCrc,
    Video,
    Audio,
    InnerFile,
    ArchiveTwin,
    Owner,
    Zone,
    /// the same sound by audio fingerprint (`probe::audio_print`), kept only for releases nothing else ties
    AudioPrint,
}

impl Kind {
    pub const ALL: [Kind; 15] = [
        Kind::Account,
        Kind::Host,
        Kind::Layout,
        Kind::Lines,
        Kind::Par2,
        Kind::MovieHash,
        Kind::FileCrc,
        Kind::BodyCrc,
        Kind::Video,
        Kind::Audio,
        Kind::InnerFile,
        Kind::ArchiveTwin,
        Kind::Owner,
        Kind::Zone,
        Kind::AudioPrint,
    ];

    /// The word in `/ui/same?kind=`.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Account => "account",
            Kind::Host => "host",
            Kind::Layout => "layout",
            Kind::Lines => "lines",
            Kind::Par2 => "par2",
            Kind::MovieHash => "moviehash",
            Kind::FileCrc => "file",
            Kind::BodyCrc => "article",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::InnerFile => "inner",
            Kind::ArchiveTwin => "archive",
            Kind::Owner => "owner",
            Kind::Zone => "zone",
            Kind::AudioPrint => "audioprint",
        }
    }

    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.word() == word)
    }

    /// What the releases on the list have in common, for its heading.
    pub fn heading(self) -> &'static str {
        match self {
            Kind::Account => "Same posting account",
            Kind::Host => "Same posting host",
            Kind::Layout => "Same track layout and length",
            Kind::Lines => "Same Lines per file",
            Kind::Par2 => "Same PAR2 set",
            Kind::MovieHash => "Same OpenSubtitles hash",
            Kind::FileCrc => "Same file, by size and CRC32",
            Kind::BodyCrc => "Same first article, by the server's body CRC",
            Kind::Video => "Same video stream",
            Kind::Audio => "Same audio stream",
            Kind::InnerFile => "Same file inside an archive, by unpacked size and CRC32",
            Kind::ArchiveTwin => "Same archive, by its own random values",
            Kind::Owner => "Same Unix owner in the archive",
            Kind::Zone => "Same packer settings and clock zone",
            Kind::AudioPrint => "Same sound, by audio fingerprint",
        }
    }

    /// One sentence under the heading: what sharing the key means, and what it doesn't.
    pub fn about(self) -> &'static str {
        match self {
            Kind::Account => {
                "Posted from the same account at the injecting server, going by a salted hash of it: the same uploader, as a rule, whatever the names say."
            }
            Kind::Host => {
                "Posted from the same host, going by a salted hash of it. A shared posting service puts many uploaders behind one host."
            }
            Kind::Layout => {
                "The same video, audio and subtitle codecs in the same order, the same chapter count and the same length to the second: often the same encode, renamed or posted again. Not proof."
            }
            Kind::Lines => {
                "Every file posted with the same number of lines: the same post made again, as a rule. Kept for releases indexed since Atlas began timing articles."
            }
            Kind::Par2 => "The same PAR2 recovery set: the same files, whoever posted them.",
            Kind::MovieHash => {
                "The same OpenSubtitles hash over the main file's first and last 64 KB and its size: the same file, as a rule."
            }
            Kind::FileCrc => {
                "A file of the same size and whole-file CRC32, from the yEnc trailer of a probed post or from a PAR2 set's file list: the same bytes, as a rule, whatever the file is called or how it was cut into articles. A 32-bit CRC can match by chance, soo check the size and names."
            }
            Kind::BodyCrc => {
                "The main file's first article as the server received it: the same CRC32 over the posted body (yEnc name and cut included), the same part number and size. The same post made again, under any poster, subject or Message-ID. Kept for releases probed since Atlas began keeping the header."
            }
            Kind::Video => {
                "The same first keyframe, to the bit: the same encode or the same disc's video, remuxed, renamed or in another container, whoever posted it and in whatever group. Another encode of the same film has another."
            }
            Kind::Audio => {
                "The same first audio track, to the bit: FLAC's MD5 of its samples, or the first eight different AC-3 or E-AC-3 frames. The same audio remuxed, whatever the video, container or group: a hybrid or another encode can keep the source's audio. A studio's logo music, encoded once and reused, can match across films, soo check the names."
            }
            Kind::InnerFile => {
                "A file inside one of its archives with the same unpacked size and CRC32 as a file in another release's archive, a file posted bare or one a PAR2 set describes: the same file, as a rule, whether it was put in a RAR, a 7z or a ZIP. Only files a header gives the whole CRC for count: a RAR file split over volumes doesn't. A 32-bit CRC can match by chance, soo check the size and names."
            }
            Kind::ArchiveTwin => {
                "The same random values an archive is made with: a RAR5 password's salt and check, a RAR file's salt, a 7z's AES IV or its start header. The same archive, byte for byte, posted again, whatever it and its volumes are called. A new archive of the same files has new ones. The values are kept as a key only and never shown."
            }
            Kind::Owner => {
                "The same Unix user and group a RAR or ZIP recorded for its files, going by a salted hash of them: the same packing account, as a rule. Shared names like root are common, soo many uploaders can share one. The names are never stored or shown."
            }
            Kind::Zone => {
                "Packed with the same settings on a clock the same distance from UTC, going by a salted hash of both: from a ZIP's local and UTC times of one moment, or an archive's local time set against the post's date, which is a guess. Many uploaders share a zone and a tool, soo it's a weak hint. The zone is never stored or shown."
            }
            Kind::AudioPrint => {
                "The same sound a quarter, half and three quarters of the way in, as Chromaprint hears it: often the same film or episode in another encode, codec or container. Taken only for releases no other key ties to another, from AAC, FLAC, MP3, MP2, Vorbis and PCM tracks in Matroska. A theme tune or a recap can match across episodes, soo check the names."
            }
        }
    }

    /// The `release_keys` column, for the kinds kept there.
    fn column(self) -> Option<&'static str> {
        match self {
            Kind::Account => Some("account"),
            Kind::Host => Some("host"),
            Kind::Layout => Some("layout"),
            Kind::Lines => Some("lines"),
            Kind::BodyCrc => Some("body_crc"),
            Kind::Video => Some("video"),
            Kind::Audio => Some("audio"),
            Kind::Par2 | Kind::MovieHash | Kind::FileCrc | Kind::InnerFile => None,
            Kind::ArchiveTwin | Kind::Owner | Kind::Zone | Kind::AudioPrint => None,
        }
    }

    /// The `archive_release` column, for the archive's own keys.
    fn archive_column(self) -> Option<&'static str> {
        match self {
            Kind::ArchiveTwin => Some("twin_key"),
            Kind::Owner => Some("owner_key"),
            Kind::Zone => Some("zone_key"),
            _ => None,
        }
    }
}

/// The keys `release_keys` keeps for a release. All None: no row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub account: Option<i64>,
    pub host: Option<i64>,
    pub layout: Option<i64>,
    pub lines: Option<i64>,
    pub file_crc: Option<i64>,
    pub body_crc: Option<i64>,
    pub video: Option<i64>,
    pub audio: Option<i64>,
}

impl Keys {
    pub fn is_empty(&self) -> bool {
        *self == Keys::default()
    }

    fn get(&self, kind: Kind) -> Option<i64> {
        match kind {
            Kind::Account => self.account,
            Kind::Host => self.host,
            Kind::Layout => self.layout,
            Kind::Lines => self.lines,
            Kind::BodyCrc => self.body_crc,
            Kind::Video => self.video,
            Kind::Audio => self.audio,
            _ => None,
        }
    }
}

/// A file as its whole-file CRC32 knows it: its size in bytes and the CRC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileCrc {
    pub size: i64,
    pub crc32: u32,
}

impl FileCrc {
    /// One worth looking up: big enough not to match every post that carries the same small file.
    pub fn new(size: i64, crc32: u32) -> Option<FileCrc> {
        (size >= CRC_MIN_SIZE).then_some(FileCrc { size, crc32 })
    }

    /// The `release_keys.file_crc` it is kept under.
    pub fn key(self) -> i64 {
        as_key(digest8(format!("file:{}:{:08x}", self.size, self.crc32).as_bytes()))
    }
}

/// What a release can be looked up by: its `release_keys`, its PAR2 set ids, its OpenSubtitles hash and the files it
/// knows the whole-file CRC32 of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    pub keys: Keys,
    pub par2_sets: Vec<Vec<u8>>,
    pub moviehash: Option<String>,
    pub files: Vec<FileCrc>,
    /// the files inside its archives a header gives the whole CRC32 of (`archive_inner_files`)
    pub inner_files: Vec<FileCrc>,
    /// its archive's own keys (`archive_release`)
    pub archive: crate::archive::store::ArchiveKeys,
    /// it has an audio print (`probe::audio_print`), matched by its own lookup
    pub audio_print: bool,
}

impl Lookup {
    pub fn has(&self, kind: Kind) -> bool {
        match kind {
            Kind::Par2 => !self.par2_sets.is_empty(),
            Kind::MovieHash => self.moviehash.is_some(),
            Kind::FileCrc => !self.files.is_empty(),
            Kind::InnerFile => !self.inner_files.is_empty(),
            Kind::ArchiveTwin | Kind::Owner | Kind::Zone => self.archive_key(kind).is_some(),
            Kind::AudioPrint => self.audio_print,
            _ => self.keys.get(kind).is_some(),
        }
    }

    fn archive_key(&self, kind: Kind) -> Option<i64> {
        match kind {
            Kind::ArchiveTwin => self.archive.twin_key,
            Kind::Owner => self.archive.owner_key,
            Kind::Zone => self.archive.zone_key,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- the digests

/// SHA-256, first 8 bytes.
pub fn digest8(bytes: &[u8]) -> [u8; 8] {
    let d = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}

/// SHA-256 over the salt and the value, first 8 bytes: lines two postings up without naming the account.
pub fn salted8(salt: i64, value: &str) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(salt.to_le_bytes());
    h.update(value.as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&h.finalize()[..8]);
    out
}

/// A digest as the integer `release_keys` stores.
pub fn as_key(digest: [u8; 8]) -> i64 {
    i64::from_be_bytes(digest)
}

/// A stored key as the page prints it: the digest's 16 hex digits.
pub fn hex16(key: i64) -> String {
    format!("{:016x}", key as u64)
}

/// A salted key as the page prints it: the digest's first 12 hex digits.
pub fn hex12(key: i64) -> String {
    hex16(key)[..12].to_string()
}

/// The `key=value;` parts of an Injection-Info header that name who posted. The values stay in this module and
/// `web_meta`: the pages print their salted keys only.
#[derive(Default)]
pub(crate) struct InjectionParts {
    pub account: Option<String>,
    pub host: Option<String>,
    pub logging_data: bool,
    pub complaints: bool,
}

pub(crate) fn injection_parts(info: &str) -> InjectionParts {
    let mut out = InjectionParts::default();
    for part in info.split(';') {
        let Some((key, value)) = part.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "posting-account" => out.account = Some(value.to_string()),
            "posting-host" => out.host = Some(value.to_string()),
            "logging-data" => out.logging_data = true,
            "mail-complaints-to" => out.complaints = true,
            _ => {}
        }
    }
    out
}

/// The posting account and host from a probe's headers: Injection-Info's `posting-account` and `posting-host`,
/// the host from `NNTP-Posting-Host` when Injection-Info names none.
pub(crate) fn account_and_host(probe: &ProbeInfo) -> (Option<String>, Option<String>) {
    let parts = probe.headers.get("Injection-Info").map(|info| injection_parts(info)).unwrap_or_default();
    let host = parts.host.or_else(|| probe.headers.get("NNTP-Posting-Host").map(|h| h.trim().to_string()));
    (parts.account, host.filter(|h| !h.is_empty()))
}

/// The first video track's keyframe key, as the integer `release_keys` stores: its 16 hex digits read back.
pub(crate) fn video_key(media: &crate::probe::media::MediaInfo) -> Option<i64> {
    let hex = media.video.first()?.keyframe_key.as_deref()?;
    (hex.len() == 16).then(|| u64::from_str_radix(hex, 16).ok()).flatten().map(|k| k as i64)
}

/// The first audio track's stream key that the probe found, as the probe keeps it.
pub(crate) fn audio_stream_key(media: &crate::probe::media::MediaInfo) -> Option<&str> {
    media.audio.first()?.stream_key.as_deref()
}

/// The same as the integer `release_keys` stores.
pub(crate) fn audio_key(media: &crate::probe::media::MediaInfo) -> Option<i64> {
    let hex = audio_stream_key(media)?;
    (hex.len() == 16).then(|| u64::from_str_radix(hex, 16).ok()).flatten().map(|k| k as i64)
}

/// What the layout key is taken over: the codecs in track order, the chapter count and the duration to the
/// second. None for a probe that found no tracks and no duration.
pub(crate) fn layout_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    if media.video.is_empty() && media.audio.is_empty() && media.subtitles.is_empty() && media.duration_secs.is_none() {
        return None;
    }
    let codecs = |list: Vec<&str>| list.join(",");
    Some(format!(
        "v:{}|a:{}|s:{}|c:{}|d:{}",
        codecs(media.video.iter().map(|t| t.codec.as_str()).collect()),
        codecs(media.audio.iter().map(|t| t.codec.as_str()).collect()),
        codecs(media.subtitles.iter().map(|t| t.codec.as_str()).collect()),
        media.chapters.len(),
        media.duration_secs.map_or(0, |d| d.round() as i64)
    ))
}

/// What the Lines key is taken over: each file's Lines sum in file name order, joined by commas. None unless the
/// rows cover every article of the release (`parts`) and every file has lines: a sum over part of a file, or a file
/// with no row, would key a different post. `files` is `(lines, articles)` a file.
pub(crate) fn lines_text(files: &[(Option<i64>, i64)], parts: Option<i64>) -> Option<String> {
    let timed: i64 = files.iter().map(|(_, articles)| articles).sum();
    if files.is_empty() || parts != Some(timed) {
        return None;
    }
    let lines: Vec<String> =
        files.iter().map(|(lines, _)| lines.filter(|l| *l > 0).map(|l| l.to_string())).collect::<Option<_>>()?;
    Some(lines.join(","))
}

/// The probed main file's size and whole-file CRC32, from its yEnc trailer: None unless both are known and the file
/// is big enough to look up.
pub fn probe_file(probe: &ProbeInfo) -> Option<FileCrc> {
    let size = probe.yenc.as_ref()?.file_size?;
    FileCrc::new(i64::try_from(size).ok()?, probe.file_crc32?)
}

/// The server's body CRC of an article, from its `X-Received-Body-CRC` header: decimal, as Highwinds-family servers
/// and Usenet.Farm write it, `0x` hex taken too.
pub fn received_body_crc(value: &str) -> Option<u32> {
    let v = value.trim();
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => v.parse::<u32>().ok(),
    }
}

/// What the first-article key is taken over: the server's body CRC of the probed main file's first article, its
/// part number and its decoded size. None without the header or the part's yEnc lines.
pub(crate) fn body_text(probe: &ProbeInfo) -> Option<String> {
    let crc = probe
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(crate::probe::BODY_CRC_HEADER))
        .and_then(|(_, v)| received_body_crc(v))?;
    let y = probe.yenc.as_ref()?;
    let decoded = match (y.begin, y.end) {
        (Some(begin), Some(end)) if end >= begin => end - begin + 1,
        _ => y.yend_size.or(y.file_size)?,
    };
    Some(format!("body:{crc:08x}:{}:{decoded}", y.part.unwrap_or(1)))
}

/// The keys over what a release has: its probe and its `file_timing` rows.
pub fn keys_of(
    probe: Option<&ProbeInfo>,
    timing: &[(Option<i64>, i64)],
    parts: Option<i64>,
    salt: Option<i64>,
) -> Keys {
    let mut keys = Keys::default();
    if let Some(p) = probe {
        if let Some(salt) = salt {
            let (account, host) = account_and_host(p);
            keys.account = account.map(|a| as_key(salted8(salt, &a)));
            keys.host = host.map(|h| as_key(salted8(salt, &h)));
        }
        keys.layout = p.media.as_ref().and_then(layout_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.file_crc = probe_file(p).map(FileCrc::key);
        keys.body_crc = body_text(p).map(|t| as_key(digest8(t.as_bytes())));
        keys.video = p.media.as_ref().and_then(video_key);
        keys.audio = p.media.as_ref().and_then(audio_key);
    }
    keys.lines = lines_text(timing, parts).map(|t| as_key(digest8(t.as_bytes())));
    keys
}

// ---------------------------------------------------------------- reading

/// The install's salt for the account and host keys: made once, by whoever asks first, and kept in shard 0's
/// `meta`. `conn` has the shards attached. None when it can't be read or made, and then no salted key is made.
pub fn salt(conn: &Connection) -> Option<i64> {
    salt_in(conn, "s0.meta")
}

/// `salt` over `meta`, the shard 0 table as `conn` names it: `s0.meta` with the shards attached, `meta` on shard 0
/// opened alone.
fn salt_in(conn: &Connection, meta: &str) -> Option<i64> {
    let read = || {
        conn.query_row(&format!("select value from {meta} where key = 'key_salt'"), [], |r| r.get::<_, i64>(0))
            .optional()
            .ok()
            .flatten()
    };
    if let Some(found) = read() {
        return Some(found);
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    let fresh = i64::from_le_bytes(bytes);
    // two at once: `or ignore` keeps the first, and both read it back
    let _ = conn.execute(&format!("insert or ignore into {meta} (key, value) values ('key_salt', ?1)"), [fresh]);
    read()
}

/// The install's salt as a step on any shard reads it: from shard 0 beside `conn`'s shard (`shard_zero`), made there
/// when it isn't yet, and remembered by shard 0's path. None for a connection with no shard file behind it.
pub fn step_salt(conn: &Connection) -> Option<i64> {
    static SALTS: std::sync::Mutex<Vec<(std::path::PathBuf, i64)>> = std::sync::Mutex::new(Vec::new());
    let zero = conn.path().and_then(shard_zero)?;
    if let Some((_, s)) = SALTS.lock().ok()?.iter().find(|(p, _)| *p == zero) {
        return Some(*s);
    }
    let shard = crate::db::open_shard(&zero).ok()?;
    let found = salt_in(&shard, "meta")?;
    SALTS.lock().ok()?.push((zero, found));
    Some(found)
}

/// Which salt a salted key was made with, as a key of its own: soo a key kept outside the shards (the raw store) is
/// only used again under the same salt.
pub fn salt_id(salt: i64) -> i64 {
    as_key(digest8(format!("salt:{salt}").as_bytes()))
}

/// The probe a release has, decoded. None when it has none or it doesn't decode.
fn probe_of(shard: &Connection, release_id: i64) -> rusqlite::Result<Option<ProbeInfo>> {
    let info: Option<String> = shard
        .prepare_cached("select info from release_probe where release_id = ?1")?
        .query_row([release_id], |r| r.get(0))
        .optional()?;
    Ok(info.and_then(|i| serde_json::from_str(&i).ok()))
}

/// A release's `file_timing` rows as `(lines, articles)`, in file name order.
fn timing_of(shard: &Connection, release_id: i64) -> rusqlite::Result<Vec<(Option<i64>, i64)>> {
    shard
        .prepare_cached(
            "select t.lines, t.articles from file_timing t join files f on f.id = t.file_id
             where f.release_id = ?1 order by f.filename limit ?2",
        )?
        .query_map(rusqlite::params![release_id, FILES_CAP], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// The keys a release has now, read from its own shard (`shard`, on its own or as `main`).
pub fn read_keys(shard: &Connection, release_id: i64, parts: Option<i64>, salt: Option<i64>) -> rusqlite::Result<Keys> {
    let probe = probe_of(shard, release_id)?;
    let timing = timing_of(shard, release_id)?;
    Ok(keys_of(probe.as_ref(), &timing, parts, salt))
}

/// What a release can be looked up by, read from its own shard: the keys it has now, and the PAR2 sets and the
/// OpenSubtitles hash.
pub fn read_lookup(
    shard: &Connection,
    release_id: i64,
    parts: Option<i64>,
    salt: Option<i64>,
) -> rusqlite::Result<Lookup> {
    let probe = probe_of(shard, release_id)?;
    let timing = timing_of(shard, release_id)?;
    let keys = keys_of(probe.as_ref(), &timing, parts, salt);
    let par2_sets = par2_sets(shard, release_id)?;
    let files = crc_files(shard, release_id, probe.as_ref())?;
    let moviehash = shard
        .prepare_cached("select moviehash from release_probe where release_id = ?1")
        .ok()
        .and_then(|mut q| q.query_row([release_id], |r| r.get::<_, Option<String>>(0)).optional().ok().flatten())
        .flatten();
    let inner_files = inner_crc_files(shard, release_id);
    let archive = archive_keys(shard, release_id);
    let audio_print = crate::probe::audio_print::has_print(shard, release_id);
    Ok(Lookup { keys, par2_sets, moviehash, files, inner_files, archive, audio_print })
}

/// Sources of `archive_inner_files` rows read from an archive's own headers: RAR, 7z, ZIP.
const ARCHIVE_SOURCES: &str = "(2, 3, 4)";

/// The files inside a release's archives whose whole CRC32 a header gives, the biggest first, at most
/// `CRC_FILES_CAP`, each once. Empty on a shard without the columns yet.
pub fn inner_crc_files(shard: &Connection, release_id: i64) -> Vec<FileCrc> {
    let sql = format!(
        "select distinct size, crc32 from archive_inner_files
         where release_id = ?1 and source in {ARCHIVE_SOURCES} and crc32 is not null and size >= ?2
         order by size desc limit ?3"
    );
    let Ok(mut q) = shard.prepare_cached(&sql) else { return Vec::new() };
    let Ok(rows) = q.query_map(rusqlite::params![release_id, CRC_MIN_SIZE, CRC_FILES_CAP as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    }) else {
        return Vec::new();
    };
    rows.flatten().filter_map(|(size, crc)| u32::try_from(crc).ok().and_then(|c| FileCrc::new(size, c))).collect()
}

/// A release's archive keys as `archive_release` keeps them. Nothing on a shard without the columns yet.
pub fn archive_keys(shard: &Connection, release_id: i64) -> crate::archive::store::ArchiveKeys {
    shard
        .prepare_cached("select packer, twin_key, owner_key, zone_key from archive_release where release_id = ?1")
        .ok()
        .and_then(|mut q| {
            q.query_row([release_id], |r| {
                Ok(crate::archive::store::ArchiveKeys {
                    packer: r.get(0)?,
                    twin_key: r.get(1)?,
                    owner_key: r.get(2)?,
                    zone_key: r.get(3)?,
                })
            })
            .optional()
            .ok()
            .flatten()
        })
        .unwrap_or_default()
}

/// The files a release knows the size and whole-file CRC32 of: the probed main file first, then the biggest ones
/// its PAR2 sets describe, at most `CRC_FILES_CAP`, each once. None on a shard without `par2_files` yet.
pub fn crc_files(shard: &Connection, release_id: i64, probe: Option<&ProbeInfo>) -> rusqlite::Result<Vec<FileCrc>> {
    let mut out: Vec<FileCrc> = probe.and_then(probe_file).into_iter().collect();
    if let Ok(mut q) = shard.prepare_cached(
        "select distinct size, crc32 from par2_files
         where release_id = ?1 and bound = 1 and crc32 is not null and size >= ?2
         order by size desc limit ?3",
    ) {
        let rows = q.query_map(rusqlite::params![release_id, CRC_MIN_SIZE, CRC_FILES_CAP as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (size, crc) = row?;
            if let Some(f) = u32::try_from(crc).ok().and_then(|c| FileCrc::new(size, c))
                && !out.contains(&f)
            {
                out.push(f);
            }
        }
    }
    out.truncate(CRC_FILES_CAP);
    Ok(out)
}

/// A file a release knows the whole-file CRC32 of, under a name: the one it was posted as, for the probed file, or the
/// one its PAR2 set gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedCrc {
    pub file: FileCrc,
    pub name: String,
    /// the whole-file MD5 a set of this release gives the file, when one does: a donor's set that says another MD5
    /// for the same size and CRC is another file, a CRC32 collision or a set that lies
    pub md5: Option<Vec<u8>>,
}

/// The most releases one file asks for donors from.
const DONORS_CAP: i64 = 8;

/// `crc_files` with names, for a release in any shard of `conn` (the main database with the shards attached).
pub fn named_crc_files(conn: &Connection, release_id: i64) -> Vec<NamedCrc> {
    let s = store::shard_of_id(release_id);
    let mut out: Vec<NamedCrc> = Vec::new();
    if let Some(probe) = probe_in(conn, release_id)
        && let (Some(file), Some(name)) = (probe_file(&probe), probe.file.clone())
    {
        out.push(NamedCrc { file, name, md5: None });
    }
    let sql = format!(
        "select size, crc32, name, md5 from s{s}.par2_files
         where release_id = ?1 and bound = 1 and crc32 is not null and size >= ?2
         order by size desc, name limit ?3"
    );
    if let Ok(mut q) = conn.prepare_cached(&sql)
        && let Ok(rows) = q.query_map(rusqlite::params![release_id, CRC_MIN_SIZE, CRC_FILES_CAP as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, Vec<u8>>(3)?))
        })
    {
        for (size, crc, name, md5) in rows.flatten() {
            let Some(file) = u32::try_from(crc).ok().and_then(|c| FileCrc::new(size, c)) else { continue };
            // the probed file, when the set describes it too, takes the set's MD5
            for probed in out.iter_mut().filter(|n| n.file == file && n.md5.is_none()) {
                probed.md5 = Some(md5.clone());
            }
            if !out.iter().any(|n| n.file == file && n.name == name) {
                out.push(NamedCrc { file, name, md5: Some(md5) });
            }
        }
    }
    out.truncate(CRC_FILES_CAP);
    out
}

/// Whether a set of `id` gives `file` (by size and CRC) another whole-file MD5 than `md5`.
fn md5_differs(conn: &Connection, id: i64, file: FileCrc, md5: Option<&[u8]>) -> bool {
    let Some(md5) = md5 else { return false };
    let s = store::shard_of_id(id);
    let sql = format!(
        "select 1 from s{s}.par2_files where release_id = ?1 and bound = 1 and size = ?2 and crc32 = ?3 and md5 != ?4"
    );
    conn.prepare_cached(&sql)
        .and_then(|mut q| q.exists(rusqlite::params![id, file.size, i64::from(file.crc32), md5]))
        .unwrap_or(false)
}

/// A release's probe, read through `conn` with the shards attached.
fn probe_in(conn: &Connection, release_id: i64) -> Option<ProbeInfo> {
    let s = store::shard_of_id(release_id);
    let sql = format!("select info from s{s}.release_probe where release_id = ?1");
    let info: String = conn.prepare_cached(&sql).ok()?.query_row([release_id], |r| r.get(0)).optional().ok()??;
    serde_json::from_str(&info).ok()
}

/// The other releases that hold `file`, each with the name it has there: a probed file of the same size and CRC
/// (under its posted name), or a file a bound PAR2 set lists with them (under the set's name). Shown releases only,
/// newest first, at most `DONORS_CAP`.
pub fn crc_donors(conn: &Connection, own: i64, file: FileCrc, md5: Option<&[u8]>) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for s in 0..store::SHARDS {
        let probed = format!(
            "select k.release_id from s{s}.release_keys k join s{s}.releases r on r.id = k.release_id
             where k.file_crc = ?1 and k.release_id != ?2 and not atlas_blocked(r.group_name)
             order by k.release_id desc limit ?3"
        );
        if let Ok(mut q) = conn.prepare_cached(&probed)
            && let Ok(rows) = q.query_map(rusqlite::params![file.key(), own, DONORS_CAP], |r| r.get::<_, i64>(0))
        {
            for id in rows.flatten().collect::<Vec<_>>() {
                // the key is a hash: the donor's own probe must say the same size and CRC, and name the file
                if let Some(probe) = probe_in(conn, id)
                    && probe_file(&probe) == Some(file)
                    && !md5_differs(conn, id, file, md5)
                    && let Some(name) = probe.file
                {
                    out.push((id, name));
                }
            }
        }
        let described = format!(
            "select distinct p.release_id, p.name from s{s}.par2_files p join s{s}.releases r on r.id = p.release_id
             where p.bound = 1 and p.size = ?1 and p.crc32 = ?2 and p.release_id != ?3
               and (?5 is null or p.md5 = ?5) and not atlas_blocked(r.group_name)
             order by p.release_id desc limit ?4"
        );
        if let Ok(mut q) = conn.prepare_cached(&described)
            && let Ok(rows) = q
                .query_map(rusqlite::params![file.size, i64::from(file.crc32), own, DONORS_CAP, md5], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
        {
            out.extend(rows.flatten());
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.dedup();
    out.truncate(DONORS_CAP as usize);
    out
}

/// The PAR2 set ids a release's files are described by, at most `SETS_CAP`; none on a shard without the table yet.
pub fn par2_sets(shard: &Connection, release_id: i64) -> rusqlite::Result<Vec<Vec<u8>>> {
    let Ok(mut q) = shard.prepare_cached(
        "select distinct set_id from par2_files where release_id = ?1 and bound = 1 order by set_id limit ?2",
    ) else {
        return Ok(Vec::new());
    };
    q.query_map(rusqlite::params![release_id, SETS_CAP as i64], |r| r.get(0))?.collect()
}

/// The `release_keys` row a release has. None: no row.
fn stored(shard: &Connection, release_id: i64) -> rusqlite::Result<Option<Keys>> {
    shard
        .prepare_cached(
            "select account, host, layout, lines, file_crc, body_crc, video, audio from release_keys where release_id = ?1",
        )?
        .query_row([release_id], |r| {
            Ok(Keys {
                account: r.get(0)?,
                host: r.get(1)?,
                layout: r.get(2)?,
                lines: r.get(3)?,
                file_crc: r.get(4)?,
                body_crc: r.get(5)?,
                video: r.get(6)?,
                audio: r.get(7)?,
            })
        })
        .optional()
}

/// The query over one shard (`s{shard}.`) for the releases other than `?1` sharing `kind`, newest first, with the
/// values bound from `?3` on, at most `?2`; and how many values it takes. Only releases the pages would show count:
/// one deleted or in a blocked group (`atlas_blocked`) is left out before the limit, soo the count and the list agree.
fn others_sql(kind: Kind, shard: usize, sets: usize) -> String {
    if matches!(kind, Kind::FileCrc | Kind::InnerFile) {
        // `sets` files: their keys first (?3..), then each one's size and CRC; a file posted bare, one a PAR2 set
        // describes and one inside an archive are the same bytes either way
        let keys: Vec<String> = (0..sets).map(|i| format!("?{}", i + 3)).collect();
        let pairs: Vec<String> =
            (0..sets).map(|i| format!("(size = ?{} and crc32 = ?{})", sets + 3 + 2 * i, sets + 4 + 2 * i)).collect();
        return format!(
            "select distinct k.release_id from (
                 select release_id from s{shard}.release_keys where file_crc in ({keys})
                 union select release_id from s{shard}.par2_files where bound = 1 and ({pairs})
                 union select release_id from s{shard}.archive_inner_files
                     where source in {ARCHIVE_SOURCES} and crc32 is not null and ({pairs})
             ) k join s{shard}.releases r on r.id = k.release_id
             where k.release_id != ?1 and not atlas_blocked(r.group_name)
             order by k.release_id desc limit ?2",
            keys = keys.join(", "),
            pairs = pairs.join(" or ")
        );
    }
    if let Some(col) = kind.archive_column() {
        return format!(
            "select distinct k.release_id from s{shard}.archive_release k join s{shard}.releases r on r.id = k.release_id
             where k.{col} = ?3 and k.release_id != ?1 and not atlas_blocked(r.group_name)
             order by k.release_id desc limit ?2"
        );
    }
    let (from, matches) = match kind.column() {
        Some(col) => (format!("s{shard}.release_keys k"), format!("k.{col} = ?3")),
        None if kind == Kind::Par2 => {
            let marks: Vec<String> = (0..sets).map(|i| format!("?{}", i + 3)).collect();
            (format!("s{shard}.par2_files k"), format!("k.set_id in ({}) and k.bound = 1", marks.join(", ")))
        }
        None => (format!("s{shard}.release_probe k"), "k.moviehash = ?3".to_string()),
    };
    format!(
        "select distinct k.release_id from {from} join s{shard}.releases r on r.id = k.release_id
         where {matches} and k.release_id != ?1 and not atlas_blocked(r.group_name)
         order by k.release_id desc limit ?2"
    )
}

fn values(lookup: &Lookup, kind: Kind) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    match kind {
        Kind::Par2 => lookup.par2_sets.iter().map(|s| Value::Blob(s.clone())).collect(),
        Kind::MovieHash => lookup.moviehash.iter().map(|h| Value::Text(h.clone())).collect(),
        Kind::FileCrc | Kind::InnerFile => {
            let files = if kind == Kind::FileCrc { &lookup.files } else { &lookup.inner_files };
            files
                .iter()
                .map(|f| Value::Integer(f.key()))
                .chain(files.iter().flat_map(|f| [Value::Integer(f.size), Value::Integer(i64::from(f.crc32))]))
                .collect()
        }
        Kind::ArchiveTwin | Kind::Owner | Kind::Zone => {
            lookup.archive_key(kind).map(Value::Integer).into_iter().collect()
        }
        _ => lookup.keys.get(kind).map(Value::Integer).into_iter().collect(),
    }
}

/// What `others_sql` is told it binds: the file kinds bind three values a file.
fn binds(kind: Kind, values: usize) -> usize {
    if matches!(kind, Kind::FileCrc | Kind::InnerFile) { values / 3 } else { values }
}

/// The releases other than `own` that share `kind` with it, over every shard of `conn` (the main database with
/// the shards attached), newest first, at most `limit`. A shard without the table yet has none.
pub fn others(conn: &Connection, own: i64, lookup: &Lookup, kind: Kind, limit: i64) -> Vec<i64> {
    if kind == Kind::AudioPrint {
        return if lookup.audio_print { crate::probe::audio_print::others(conn, own, limit) } else { Vec::new() };
    }
    let bound = values(lookup, kind);
    if bound.is_empty() || limit <= 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    for shard in 0..store::SHARDS {
        let sql = others_sql(kind, shard, binds(kind, bound.len()));
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let args = [rusqlite::types::Value::Integer(own), rusqlite::types::Value::Integer(limit)]
            .into_iter()
            .chain(bound.iter().cloned());
        let Ok(rows) = q.query_map(params_from_iter(args), |r| r.get::<_, i64>(0)) else { continue };
        found.extend(rows.filter_map(Result::ok));
    }
    // ids grow with time, within a shard and across them closely enough
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.dedup();
    found.truncate(limit as usize);
    found
}

/// How many releases other than `own` share `kind` with it, up to `COUNT_CAP`.
pub fn count_others(conn: &Connection, own: i64, lookup: &Lookup, kind: Kind) -> i64 {
    if kind == Kind::AudioPrint {
        return others(conn, own, lookup, kind, COUNT_CAP).len() as i64;
    }
    let bound = values(lookup, kind);
    if bound.is_empty() {
        return 0;
    }
    let mut total = 0;
    for shard in 0..store::SHARDS {
        let sql = format!("select count(*) from ({})", others_sql(kind, shard, binds(kind, bound.len())));
        let Ok(mut q) = conn.prepare(&sql) else { continue };
        let args = [rusqlite::types::Value::Integer(own), rusqlite::types::Value::Integer(COUNT_CAP - total)]
            .into_iter()
            .chain(bound.iter().cloned());
        total += q.query_row(params_from_iter(args), |r| r.get::<_, i64>(0)).unwrap_or(0);
        if total >= COUNT_CAP {
            return COUNT_CAP;
        }
    }
    total
}

// ---------------------------------------------------------------- the report

/// What `--crc-links` found over its sample.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CrcReport {
    pub sampled: i64,
    pub with_file: i64,
    pub file_linked: i64,
    /// linked by the file key and by no PAR2 set: a link nothing else made
    pub file_only: i64,
    pub with_body: i64,
    pub body_linked: i64,
}

/// Counts, over the newest `per_shard` probed releases of each shard, how many the file and first-article keys
/// link to another release, and how many of those no PAR2 set links. Reads only.
pub fn crc_report(conn: &Connection, main: &std::path::Path, per_shard: i64) -> rusqlite::Result<CrcReport> {
    let mut r = CrcReport::default();
    for n in 0..store::SHARDS {
        let Ok(shard) = crate::db::open_shard(&store::shard_path(main, n)) else { continue };
        let ids: Vec<i64> =
            match shard.prepare("select release_id from release_probe order by release_id desc limit ?1") {
                Ok(mut q) => q.query_map([per_shard], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?,
                Err(_) => continue,
            };
        for id in ids {
            let lookup = read_lookup(&shard, id, None, None)?;
            r.sampled += 1;
            if lookup.has(Kind::FileCrc) {
                r.with_file += 1;
                if count_others(conn, id, &lookup, Kind::FileCrc) > 0 {
                    r.file_linked += 1;
                    if count_others(conn, id, &lookup, Kind::Par2) == 0 {
                        r.file_only += 1;
                    }
                }
            }
            if lookup.has(Kind::BodyCrc) {
                r.with_body += 1;
                if count_others(conn, id, &lookup, Kind::BodyCrc) > 0 {
                    r.body_linked += 1;
                }
            }
        }
    }
    Ok(r)
}

/// `--crc-links [--sample N]`: the report over the newest N probed releases (20,000 by default).
pub fn run_report(args: &[String]) -> i32 {
    let sample = args
        .iter()
        .position(|a| a == "--sample")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<i64>().ok())
        .unwrap_or(20_000)
        .max(store::SHARDS as i64);
    let main = crate::paths::database();
    let conn = match crate::db::open_with_shards(&main) {
        Ok(c) => c,
        Err(e) => {
            println!("couldnt open {}: {e}", main.display());
            return 1;
        }
    };
    match crc_report(&conn, &main, sample / store::SHARDS as i64) {
        Ok(r) => {
            let pct = |n: i64, of: i64| if of > 0 { 100.0 * n as f64 / of as f64 } else { 0.0 };
            println!("newest probed releases looked at   {:>10}", r.sampled);
            println!("  with a file CRC (1 MiB or more)  {:>10}", r.with_file);
            println!(
                "  sharing it with another release  {:>10}  ({:.1}% of those with one)",
                r.file_linked,
                pct(r.file_linked, r.with_file)
            );
            println!("  of those, with no PAR2 set link  {:>10}  (no shared PAR2 set linked them)", r.file_only);
            println!("  with a first-article body CRC    {:>10}", r.with_body);
            println!(
                "  sharing it with another release  {:>10}  ({:.1}% of those with one)",
                r.body_linked,
                pct(r.body_linked, r.with_body)
            );
            0
        }
        Err(e) => {
            println!("couldnt read the shards: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------- the step

/// Keeps `release_keys` up to date: after a probe (which asks for it again), and as a release grows, for the Lines
/// key. A release with no key has no row.
#[derive(Default)]
pub struct KeysStep {
    salt: OnceLock<i64>,
}

impl KeysStep {
    /// The salt, read once from shard 0, beside the shard `conn` is on: the one the pages read.
    fn salt(&self, conn: &Connection) -> Result<i64, StepError> {
        if let Some(s) = self.salt.get() {
            return Ok(*s);
        }
        let none = || StepError::Retry("the key salt could not be read or made".into());
        let zero = conn.path().and_then(shard_zero).ok_or_else(none)?;
        let shard = crate::db::open_shard(&zero).map_err(|e| StepError::Retry(e.to_string()))?;
        let found = salt_in(&shard, "meta").ok_or_else(none)?;
        Ok(*self.salt.get_or_init(|| found))
    }
}

/// Shard 0's file, from another shard's (`atlas.s3.db` gives `atlas.s0.db`): `store::shard_path`'s names.
fn shard_zero(path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(path);
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".db")?;
    let (base, shard) = stem.rsplit_once(".s")?;
    shard.parse::<usize>().ok()?;
    Some(path.with_file_name(format!("{base}.s0.db")))
}

impl Step for KeysStep {
    fn id(&self) -> StepId {
        KEYS_STEP
    }
    fn name(&self) -> &'static str {
        "keys"
    }
    fn rank(&self) -> u16 {
        600
    }
    fn needs(&self) -> Needs {
        Needs::Database
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    /// Bit 1: the file and first-article keys, soo every release already keyed is keyed again: the file key comes
    /// from probes made long before it.
    fn config_stamp(&self) -> u64 {
        1
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let parts = Some(ctx.release().parts);
        let salt = self.salt(ctx.conn())?;
        let keys = read_keys(ctx.conn(), id, parts, Some(salt)).map_err(retry)?;
        let had = stored(ctx.conn(), id).map_err(retry)?;
        let writes = match (keys.is_empty(), had) {
            (true, None) => Vec::new(),
            (true, Some(_)) => vec![StepWrite::new(DELETE, vec![id.into()])],
            (false, Some(old)) if old == keys => Vec::new(),
            (false, _) => vec![StepWrite::new(
                UPSERT,
                vec![
                    id.into(),
                    keys.account.into(),
                    keys.host.into(),
                    keys.layout.into(),
                    keys.lines.into(),
                    keys.file_crc.into(),
                    keys.body_crc.into(),
                    keys.video.into(),
                    keys.audio.into(),
                ],
            )],
        };
        Ok(Outcome { matched: !keys.is_empty(), writes, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_print_as_the_pages_always_printed_them() {
        let d = digest8(b"abc");
        // SHA-256("abc") starts ba7816bf8f01cfea
        assert_eq!(hex16(as_key(d)), "ba7816bf8f01cfea");
        let salted = as_key(salted8(7, "user@example"));
        assert_eq!(hex12(salted).len(), 12);
        assert_eq!(hex12(salted), hex16(salted)[..12]);
        assert_ne!(salted8(7, "user@example"), salted8(8, "user@example"), "another install, another key");
    }

    #[test]
    fn keys_come_from_the_probe_and_the_timing() {
        let mut probe = ProbeInfo::default();
        probe.headers.insert(
            "Injection-Info".into(),
            "news.example; posting-account=\"acc-1\"; posting-host=\"10.0.0.1\"; logging-data=\"7\"".into(),
        );
        let keys = keys_of(Some(&probe), &[], None, Some(5));
        assert_eq!(keys.account, Some(as_key(salted8(5, "acc-1"))));
        assert_eq!(keys.host, Some(as_key(salted8(5, "10.0.0.1"))));
        assert_eq!((keys.layout, keys.lines), (None, None));
        assert_eq!(keys_of(Some(&probe), &[], None, None).account, None, "no salt, no salted key");

        // NNTP-Posting-Host stands in when Injection-Info names no host
        let mut old = ProbeInfo::default();
        old.headers.insert("NNTP-Posting-Host".into(), " 192.0.2.4 ".into());
        assert_eq!(keys_of(Some(&old), &[], None, Some(5)).host, Some(as_key(salted8(5, "192.0.2.4"))));

        // the Lines key needs every article timed and every file with lines
        let files = [(Some(1000), 10), (Some(500), 5)];
        let lines = keys_of(None, &files, Some(15), None).lines;
        assert_eq!(lines, Some(as_key(digest8(b"1000,500"))));
        assert_eq!(keys_of(None, &files, Some(16), None).lines, None);
        assert_eq!(keys_of(None, &files, None, None).lines, None);
        assert_eq!(keys_of(None, &[(None, 10), (Some(500), 5)], Some(15), None).lines, None);
        assert!(keys_of(None, &[], Some(0), None).is_empty());
    }

    #[test]
    fn the_audio_key_is_the_first_audio_tracks() {
        use crate::probe::media::{AudioTrack, MediaInfo};
        let probe = |keys: &[Option<&str>]| ProbeInfo {
            media: Some(MediaInfo {
                audio: keys
                    .iter()
                    .map(|k| AudioTrack { stream_key: k.map(str::to_string), ..Default::default() })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let keys = keys_of(Some(&probe(&[Some("ba7816bf8f01cfea"), Some("0123456789abcdef")])), &[], None, None);
        assert_eq!(keys.audio, Some(as_key(digest8(b"abc"))));
        assert_eq!(keys_of(Some(&probe(&[None, Some("0123456789abcdef")])), &[], None, None).audio, None);
        assert_eq!(keys_of(Some(&probe(&[])), &[], None, None).audio, None);
        assert!(others_sql(Kind::Audio, 2, 1).contains("k.audio = ?3"));
        assert_eq!(Kind::parse("audio"), Some(Kind::Audio));
    }

    #[test]
    fn the_video_key_is_the_first_keyframes() {
        use crate::probe::media::{MediaInfo, VideoTrack};
        let probe = |key: Option<&str>| ProbeInfo {
            media: Some(MediaInfo {
                video: vec![VideoTrack { keyframe_key: key.map(str::to_string), ..Default::default() }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let keys = keys_of(Some(&probe(Some("ba7816bf8f01cfea"))), &[], None, None);
        assert_eq!(keys.video, Some(as_key(digest8(b"abc"))));
        assert_eq!(hex16(keys.video.unwrap()), "ba7816bf8f01cfea", "the page prints what the probe kept");
        assert_eq!(keys_of(Some(&probe(None)), &[], None, None).video, None);
        assert_eq!(keys_of(Some(&probe(Some("nothex"))), &[], None, None).video, None);
        assert!(
            others_sql(Kind::Video, 2, 1).contains("s2.release_keys k")
                && others_sql(Kind::Video, 2, 1).contains("k.video = ?3")
        );
    }

    #[test]
    fn shard_zero_sits_beside_any_shard() {
        let main = std::path::Path::new("/data/atlas.db");
        for n in 0..store::SHARDS {
            let shard = store::shard_path(main, n);
            assert_eq!(shard_zero(shard.to_str().unwrap()), Some(store::shard_path(main, 0)));
        }
        assert_eq!(shard_zero("/data/atlas.db"), None);
        assert_eq!(shard_zero(""), None);
    }

    #[test]
    fn kinds_round_trip_their_words() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.word()), Some(kind));
            assert!(!kind.about().is_empty() && !kind.heading().is_empty());
        }
        assert_eq!(Kind::parse("raw"), None);
    }

    #[test]
    fn par2_lookups_bind_each_set() {
        let sql = others_sql(Kind::Par2, 3, 2);
        assert!(sql.contains("s3.par2_files k") && sql.contains("set_id in (?3, ?4)"), "{sql}");
        assert!(sql.contains("join s3.releases r") && sql.contains("not atlas_blocked(r.group_name)"), "{sql}");
        assert!(others_sql(Kind::Lines, 0, 1).contains("s0.release_keys k join s0.releases r"));
        assert!(others_sql(Kind::Lines, 0, 1).contains("k.lines = ?3"));
        assert!(others_sql(Kind::MovieHash, 7, 1).contains("k.moviehash = ?3"));
    }

    #[test]
    fn only_bound_par2_files_of_shown_releases_match() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch(&crate::archive::store::PAR2_FILES.create.replace("par2_files (", "s0.par2_files ("))
            .unwrap();
        conn.execute_batch("create table s0.releases (id integer primary key, group_name text not null)").unwrap();
        let blocked = "alt.binaries.pictures.erotica.teen";
        assert!(crate::filters::group_blocked(blocked));
        let set = vec![7u8; 16];
        // 1 asks; 2 shares it; 3 only holds an unbound copy; 4 is in a blocked group; 5 has no release row
        for (release, bound, group) in
            [(1, 1, Some("a.b.c")), (2, 1, Some("a.b.c")), (3, 0, Some("a.b.c")), (4, 1, Some(blocked)), (5, 1, None)]
        {
            if let Some(group) = group {
                conn.execute(
                    "insert into s0.releases (id, group_name) values (?1, ?2)",
                    rusqlite::params![release, group],
                )
                .unwrap();
            }
            conn.execute(
                "insert into s0.par2_files (release_id, file_id, set_id, md5, md5_16k, size, name, bound)
                 values (?1, ?2, ?3, x'', x'', 1, 'a.mkv', ?4)",
                rusqlite::params![release, vec![release as u8], set, bound],
            )
            .unwrap();
        }
        let lookup = Lookup { par2_sets: vec![set], ..Default::default() };
        assert_eq!(others(&conn, 1, &lookup, Kind::Par2, 1), vec![2], "the limit counts only the shown ones");
        assert_eq!(count_others(&conn, 1, &lookup, Kind::Par2), 1);
    }

    #[test]
    fn the_server_body_crc_reads_decimal_and_hex() {
        assert_eq!(received_body_crc(" 3735928559 "), Some(0xdead_beef));
        assert_eq!(received_body_crc("0xDEADBEEF"), Some(0xdead_beef));
        assert_eq!(received_body_crc("0"), Some(0));
        assert_eq!(received_body_crc("4294967296"), None, "past 32 bits");
        assert_eq!(received_body_crc("-1"), None);
        assert_eq!(received_body_crc(""), None);
    }

    #[test]
    fn the_first_article_key_needs_the_header_and_the_part() {
        let mut probe = ProbeInfo::default();
        probe.headers.insert("x-received-body-crc".into(), "3735928559".into());
        assert_eq!(body_text(&probe), None, "no yEnc lines, no key");
        probe.yenc = Some(crate::probe::yenc::YencLines {
            part: Some(1),
            begin: Some(1),
            end: Some(716_800),
            file_size: Some(5_000_000),
            ..Default::default()
        });
        assert_eq!(body_text(&probe).as_deref(), Some("body:deadbeef:1:716800"));
        let keys = keys_of(Some(&probe), &[], None, None);
        assert_eq!(keys.body_crc, Some(as_key(digest8(b"body:deadbeef:1:716800"))));
        probe.headers.clear();
        assert_eq!(keys_of(Some(&probe), &[], None, None).body_crc, None);
    }

    #[test]
    fn the_file_key_skips_small_files() {
        let mut probe = ProbeInfo::default();
        probe.file_crc32 = Some(0x1234_5678);
        probe.yenc = Some(crate::probe::yenc::YencLines { file_size: Some(CRC_MIN_SIZE as u64), ..Default::default() });
        let file = probe_file(&probe).expect("1 MiB is enough");
        assert_eq!(file.key(), as_key(digest8(format!("file:{}:12345678", CRC_MIN_SIZE).as_bytes())));
        assert_eq!(keys_of(Some(&probe), &[], None, None).file_crc, Some(file.key()));
        probe.yenc =
            Some(crate::probe::yenc::YencLines { file_size: Some(CRC_MIN_SIZE as u64 - 1), ..Default::default() });
        assert!(probe_file(&probe).is_none(), "a CRC over a small file says too little");
        probe.yenc = Some(crate::probe::yenc::YencLines { file_size: Some(5 << 20), ..Default::default() });
        probe.file_crc32 = None;
        assert!(probe_file(&probe).is_none());
    }

    #[test]
    fn the_body_crc_header_is_kept() {
        let kept = crate::probe::keep_headers(&[
            ("X-Received-Body-CRC".into(), "123".into()),
            ("X-Received-Bytes".into(), "740000".into()),
            ("X-Something-Else".into(), "1".into()),
        ]);
        assert_eq!(kept.get(crate::probe::BODY_CRC_HEADER).map(String::as_str), Some("123"));
        assert!(kept.contains_key("X-Received-Bytes"));
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn a_file_matches_by_its_key_or_a_par2_file_list() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch(&crate::archive::store::PAR2_FILES.create.replace("par2_files (", "s0.par2_files ("))
            .unwrap();
        conn.execute_batch(&RELEASE_KEYS.create.replace("release_keys (", "s0.release_keys (")).unwrap();
        conn.execute_batch(INNER_FILES_FIXTURE).unwrap();
        conn.execute_batch("create table s0.releases (id integer primary key, group_name text not null)").unwrap();
        let blocked = "alt.binaries.pictures.erotica.teen";
        for (id, group) in [(1, "a.b.c"), (2, "a.b.c"), (3, "a.b.d"), (4, "a.b.c"), (5, blocked), (6, "a.b.c")] {
            conn.execute("insert into s0.releases (id, group_name) values (?1, ?2)", rusqlite::params![id, group])
                .unwrap();
        }
        let file = FileCrc::new(8 << 20, 0xfeed_beef).unwrap();
        // 1 asks; 2 probed the same file; 3's PAR2 set lists it; 4's PAR2 copy is unbound; 5 is blocked;
        // 6 has the same CRC at another size
        for (id, key) in [(1, file.key()), (2, file.key()), (5, file.key())] {
            conn.execute(
                "insert into s0.release_keys (release_id, file_crc) values (?1, ?2)",
                rusqlite::params![id, key],
            )
            .unwrap();
        }
        for (id, size, bound) in [(3, 8i64 << 20, 1), (4, 8 << 20, 0), (6, 9 << 20, 1)] {
            conn.execute(
                "insert into s0.par2_files (release_id, file_id, set_id, md5, md5_16k, size, name, crc32, bound)
                 values (?1, x'01', x'02', x'', x'', ?2, 'a.mkv', ?3, ?4)",
                rusqlite::params![id, size, i64::from(0xfeed_beef_u32), bound],
            )
            .unwrap();
        }
        for (_, index) in crate::archive::store::PAR2_FILES.indexes {
            conn.execute_batch(&index.replacen("exists ", "exists s0.", 1)).unwrap();
        }
        let lookup = Lookup { files: vec![file], ..Default::default() };
        assert!(lookup.has(Kind::FileCrc));
        let plan: Vec<String> = conn
            .prepare(&format!("explain query plan {}", others_sql(Kind::FileCrc, 0, 1)))
            .unwrap()
            .query_map(rusqlite::params![1, 10, file.key(), file.size, i64::from(file.crc32)], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(plan.iter().any(|p| p.contains("par2_files_crc")), "the PAR2 side seeks size and CRC: {plan:?}");
        assert_eq!(others(&conn, 1, &lookup, Kind::FileCrc, 10), vec![3, 2]);
        assert_eq!(count_others(&conn, 1, &lookup, Kind::FileCrc), 2);
        let none = Lookup::default();
        assert!(others(&conn, 1, &none, Kind::FileCrc, 10).is_empty());

        // 6 also holds the file inside a RAR: the file key and the inner-file key find it there, and a PAR2 or NZB
        // list of the same size and CRC (sources 1 and 5) isn't an archive's header
        for (id, seq, source) in [(6, 1, crate::archive::store::SOURCE_RAR), (4, 2, crate::archive::store::SOURCE_PAR2)]
        {
            conn.execute(
                "insert into s0.archive_inner_files (release_id, seq, name, size, source, crc32) values (?1, ?2, 'a.mkv', ?3, ?4, ?5)",
                rusqlite::params![id, seq, file.size, source, i64::from(file.crc32)],
            )
            .unwrap();
        }
        assert_eq!(others(&conn, 1, &lookup, Kind::FileCrc, 10), vec![6, 3, 2]);
        let inner = Lookup { inner_files: vec![file], ..Default::default() };
        assert!(inner.has(Kind::InnerFile) && !inner.has(Kind::FileCrc));
        assert_eq!(others(&conn, 1, &inner, Kind::InnerFile, 10), vec![6, 3, 2], "a bare, a PAR2 and an archived copy");
    }

    /// `archive_inner_files` as a shard has it, on `s0`, without the trigger on `releases`.
    const INNER_FILES_FIXTURE: &str = "create table s0.archive_inner_files (
        release_id INTEGER NOT NULL, seq INTEGER NOT NULL, name TEXT NOT NULL, size INTEGER, source INTEGER NOT NULL,
        crc32 INTEGER, mtime INTEGER, host_os INTEGER, primary key (release_id, seq)) without rowid";

    #[test]
    fn an_archives_own_keys_match_only_their_own_column() {
        let conn = Connection::open_in_memory().unwrap();
        crate::filters::register_sql(&conn).unwrap();
        conn.execute("attach ':memory:' as s0", []).unwrap();
        conn.execute_batch(
            "create table s0.archive_release (release_id INTEGER PRIMARY KEY, password INTEGER NOT NULL,
                packer TEXT, twin_key INTEGER, owner_key INTEGER, zone_key INTEGER);
             create table s0.releases (id integer primary key, group_name text not null)",
        )
        .unwrap();
        // 1 asks; 2 is the same archive; 3 has the same owner only; 4 the same zone only
        for (id, twin, owner, zone) in [(1, 10, 20, 30), (2, 10, 21, 31), (3, 11, 20, 32), (4, 12, 22, 30)] {
            conn.execute("insert into s0.releases (id, group_name) values (?1, 'a.b.c')", [id]).unwrap();
            conn.execute(
                "insert into s0.archive_release (release_id, password, twin_key, owner_key, zone_key) values (?1, 0, ?2, ?3, ?4)",
                rusqlite::params![id, twin, owner, zone],
            )
            .unwrap();
        }
        let lookup = Lookup {
            archive: crate::archive::store::ArchiveKeys {
                packer: None,
                twin_key: Some(10),
                owner_key: Some(20),
                zone_key: Some(30),
            },
            ..Default::default()
        };
        assert_eq!(others(&conn, 1, &lookup, Kind::ArchiveTwin, 10), vec![2]);
        assert_eq!(others(&conn, 1, &lookup, Kind::Owner, 10), vec![3]);
        assert_eq!(others(&conn, 1, &lookup, Kind::Zone, 10), vec![4]);
        assert!(!Lookup::default().has(Kind::ArchiveTwin));
        assert_eq!(archive_keys(&conn, 2).twin_key, Some(10), "read back from the shard's table");
        assert_eq!(archive_keys(&conn, 99), crate::archive::store::ArchiveKeys::default());
    }
}
