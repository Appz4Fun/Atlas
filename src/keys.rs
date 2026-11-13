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
//! Three more come from what the probe already reads: the Matroska SegmentUID with the mux's DateUTC (the same mux,
//! byte for byte, whatever the file is called), the encoder's settings string with the length (the same encode,
//! remuxed or not), and the id a post's thread hangs on (its probed article's first References id, else its own
//! Message-ID: the parts of one post its poster threaded, wherever they were indexed). And a file whose every article a
//! step read whole gets its whole-file CRC32 folded from theirs (`gf2::crc32_combine`), for the file key.
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

use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite};
use crate::probe::ProbeInfo;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const KEYS_STEP: StepId = StepId(40);
pub const COUNT_CAP: i64 = 1_000;
pub const LIST_CAP: i64 = 100;
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
    Mux,
    Encode,
    Thread,
    InnerFile,
    ArchiveTwin,
    Owner,
    Zone,
    /// the same sound by audio fingerprint (`probe::audio_print`), kept only for releases nothing else ties
    AudioPrint,
    /// the same subtitle tracks, by their headers (an ASS script's styles, a VobSub's palette)
    Subs,
    /// the same fonts attached, by name and size
    Fonts,
    /// the same chapter times
    Chapters,
    /// an encrypted archive set's volume size, count and last volume's size, from its PAR2 file list
    Volumes,
}

impl Kind {
    pub const ALL: [Kind; 22] = [
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
        Kind::Mux,
        Kind::Encode,
        Kind::Thread,
        Kind::InnerFile,
        Kind::ArchiveTwin,
        Kind::Owner,
        Kind::Zone,
        Kind::AudioPrint,
        Kind::Subs,
        Kind::Fonts,
        Kind::Chapters,
        Kind::Volumes,
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
            Kind::Mux => "mux",
            Kind::Encode => "encode",
            Kind::Thread => "thread",
            Kind::InnerFile => "inner",
            Kind::ArchiveTwin => "archive",
            Kind::Owner => "owner",
            Kind::Zone => "zone",
            Kind::AudioPrint => "audioprint",
            Kind::Subs => "subs",
            Kind::Fonts => "fonts",
            Kind::Chapters => "chapters",
            Kind::Volumes => "volumes",
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
            Kind::Mux => "Same Matroska mux",
            Kind::Encode => "Same encoder settings and length",
            Kind::Thread => "Same post thread",
            Kind::InnerFile => "Same file inside an archive, by unpacked size and CRC32",
            Kind::ArchiveTwin => "Same archive, by its own random values",
            Kind::Owner => "Same Unix owner in the archive",
            Kind::Zone => "Same packer settings and clock zone",
            Kind::AudioPrint => "Same sound, by audio fingerprint",
            Kind::Subs => "Same subtitle headers",
            Kind::Fonts => "Same attached fonts",
            Kind::Chapters => "Same chapter times",
            Kind::Volumes => "Same encrypted volume sizes",
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
                "A file of the same size and whole-file CRC32, from the yEnc trailer of a probed post, a PAR2 set's file list or a RAR recovery volume's list of the volumes it repairs: the same bytes, as a rule, whatever the file is called or how it was cut into articles. A 32-bit CRC can match by chance, soo check the size and names."
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
            Kind::Mux => {
                "The same Matroska SegmentUID and mux date: 128 random bits mkvmerge picks for each file it writes, soo the same file, renamed, repacked or posted again. A remux of it has a new one."
            }
            Kind::Encode => {
                "The same x264 or x265 settings string and the same length to the second: the same encode as a rule, in another container or remuxed with other audio. A group encodes many films with one preset, which the length tells apart, but a series' episodes can share both."
            }
            Kind::Thread => {
                "Probed articles that hang on the same thread: one names the other's first article in its References, or both name a third. Parts of one post as its poster threaded it, indexed as separate releases. References is the poster's to write, soo check the names."
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
            Kind::Subs => {
                "The same subtitle tracks by their own headers: an ASS script's info and styles (Aegisub's saved state left out) or a VobSub's size and palette. The same subtitles muxed again, as a rule, often by the same group or from the same source; a stock template or the default DVD palette is shared by many, soo check the names."
            }
            Kind::Fonts => {
                "The same set of fonts attached, by file name and size: typesetting a group ships with its subtitles, soo the same release or the same group's work. A series shares one set across its episodes."
            }
            Kind::Chapters => {
                "The same chapter start times, to the millisecond, three or more of them and not evenly spaced: chapters cut from the same disc or by the same hand. Episodes of one series rarely share them; another encode of the same disc often does."
            }
            Kind::Volumes => {
                "An encrypted archive set of the same volume size, volume count and last volume size, to the byte, as its PAR2 set lists them: a password-protected repost of the same files packed the same way often has exactly these, whatever the password, names or posting. Only a hint: many sets of one disc size or one volume cap share a count, soo check the names."
            }
        }
    }

    /// The `release_keys` column, for the kinds kept there.
    pub(crate) fn column(self) -> Option<&'static str> {
        match self {
            Kind::Account => Some("account"),
            Kind::Host => Some("host"),
            Kind::Layout => Some("layout"),
            Kind::Lines => Some("lines"),
            Kind::BodyCrc => Some("body_crc"),
            Kind::Video => Some("video"),
            Kind::Audio => Some("audio"),
            Kind::Mux => Some("mux"),
            Kind::Encode => Some("encode"),
            Kind::Thread => Some("thread"),
            Kind::Subs => Some("subs"),
            Kind::Fonts => Some("fonts"),
            Kind::Chapters => Some("chapters"),
            Kind::Volumes => Some("volumes"),
            Kind::Par2 | Kind::MovieHash | Kind::FileCrc | Kind::InnerFile => None,
            Kind::ArchiveTwin | Kind::Owner | Kind::Zone | Kind::AudioPrint => None,
        }
    }

    /// The `archive_release` column, for the archive's own keys.
    pub(crate) fn archive_column(self) -> Option<&'static str> {
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
    pub mux: Option<i64>,
    pub encode: Option<i64>,
    pub thread: Option<i64>,
    pub subs: Option<i64>,
    pub fonts: Option<i64>,
    pub chapters: Option<i64>,
    pub volumes: Option<i64>,
}

impl Keys {
    pub fn is_empty(&self) -> bool {
        *self == Keys::default()
    }

    pub(crate) fn get(&self, kind: Kind) -> Option<i64> {
        match kind {
            Kind::Account => self.account,
            Kind::Host => self.host,
            Kind::Layout => self.layout,
            Kind::Lines => self.lines,
            Kind::BodyCrc => self.body_crc,
            Kind::Video => self.video,
            Kind::Audio => self.audio,
            Kind::Mux => self.mux,
            Kind::Encode => self.encode,
            Kind::Thread => self.thread,
            Kind::Subs => self.subs,
            Kind::Fonts => self.fonts,
            Kind::Chapters => self.chapters,
            Kind::Volumes => self.volumes,
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

/// What the same-mux key is taken over: the Matroska SegmentUID and the mux's DateUTC, when there's a SegmentUID.
pub(crate) fn mux_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    let uid = media.segment_uid.as_deref().filter(|u| !u.is_empty() && u.bytes().any(|b| b != b'0'))?;
    Some(format!("mux:{uid}:{}", media.muxed_at.map(|t| t.to_string()).unwrap_or_default()))
}

/// What the same-encode key is taken over: the encoder's settings string and the length to the second, both needed.
pub(crate) fn encode_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    let options = media.encoder_options.as_deref().map(str::trim).filter(|o| !o.is_empty())?;
    let secs = media.duration_secs.filter(|d| *d >= 1.0)?.round() as i64;
    Some(format!("encode:{}:{options}:{secs}", media.encoder.as_deref().unwrap_or("")))
}

/// What the subtitle key is taken over: every subtitle track's header key (`media::subtitle_header_key`), in track
/// order. None when no track has one.
pub(crate) fn subs_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    let keys: Vec<&str> = media.subtitles.iter().filter_map(|t| t.header_key.as_deref()).collect();
    (!keys.is_empty()).then(|| format!("subs:{}", keys.join(",")))
}

/// Fewer attached fonts than this make no font key: one stock font is in thousands of unrelated files.
const MIN_FONTS: usize = 2;

/// Whether an attachment is a font, by its MIME type or its name.
fn is_font(a: &crate::probe::media::Attachment) -> bool {
    let mime = a.mime.as_deref().unwrap_or("").to_ascii_lowercase();
    let name = a.name.to_ascii_lowercase();
    mime.contains("font")
        || mime.contains("truetype")
        || mime.contains("opentype")
        || [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"].iter().any(|e| name.ends_with(e))
}

/// What the font key is taken over: every attached font's name (lower case) and size, sorted. None for fewer than
/// `MIN_FONTS`, or without whole-header and complete attachment metadata proof:
/// capped, truncated, unknown or ambiguous names never qualify.
pub(crate) fn fonts_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    if !media.header_complete || !media.attachments_complete {
        return None;
    }
    // The existing textual key uses ':' and ',' separators. Such names are
    // retained as facts but cannot enter this ambiguous key representation.
    if media.attachments.iter().filter(|a| is_font(a)).any(|a| a.name.contains([':', ','])) {
        return None;
    }
    let mut fonts: Vec<String> = media
        .attachments
        .iter()
        .filter(|a| is_font(a))
        .map(|a| format!("{}:{}", a.name.to_ascii_lowercase(), a.size))
        .collect();
    fonts.sort_unstable();
    fonts.dedup();
    (fonts.len() >= MIN_FONTS).then(|| format!("fonts:{}", fonts.join(",")))
}

/// Fewer chapters than this make no chapter key: a start and an end credits mark are in every other film.
const MIN_CHAPTERS: usize = 3;

/// What the chapter key is taken over: each chapter's start in milliseconds, in order. None for fewer than
/// `MIN_CHAPTERS`, or when every gap between them is the same to the second: chapters made every few minutes by a
/// tool, which every file of that length gets.
pub(crate) fn chapters_text(media: &crate::probe::media::MediaInfo) -> Option<String> {
    if !media.chapters_complete {
        return None;
    }
    let starts: Vec<i64> = media
        .chapters
        .iter()
        .map(|c| c.start_secs)
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| (s * 1000.0).round() as i64)
        .collect();
    if starts.len() < MIN_CHAPTERS || starts.len() != media.chapters.len() {
        return None;
    }
    let gaps: Vec<i64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    let (lo, hi) = (gaps.iter().min()?, gaps.iter().max()?);
    // two chapters at one time are an edition's leftovers, not a cut
    if *lo <= 0 {
        return None;
    }
    // the last chapter can be cut short by the film's end: even spacing is judged without it
    let inner = &gaps[..gaps.len() - 1];
    let even = |g: &[i64]| g.iter().max().zip(g.iter().min()).is_some_and(|(a, b)| a - b <= 1_000);
    if hi - lo <= 1_000 || (inner.len() >= 2 && even(inner)) {
        return None;
    }
    let list: Vec<String> = starts.iter().map(i64::to_string).collect();
    Some(format!("chapters:{}", list.join(",")))
}

/// The fewest volumes a set must have for a volume key: two equal sizes and a last one say little.
const MIN_VOLUMES: usize = 3;

/// What the volume key is taken over: an archive set's format, its volume size, how many volumes and the last one's
/// size, from `(name, size)` files as a PAR2 set lists them; the biggest set when there are several. None unless the
/// RAR set has `MIN_VOLUMES` or more numbered from its first with none missing, every volume but the last the same
/// size and the last smaller: the shape a packer's `-v` cut leaves, and the same whatever the password.
pub(crate) fn volumes_text(files: &[(String, i64)]) -> Option<String> {
    use crate::archive::select::{Kind as File, classify, set_name};
    use std::collections::BTreeMap;
    // (format, set) -> volumes, and whether the set is named the old way (`.rar`, `.r00`, ...), which starts at 0
    type Set = (Vec<(u32, i64)>, bool);
    let mut sets: BTreeMap<(&str, String), Set> = BTreeMap::new();
    for (name, size) in files {
        // RAR only: the probe can't tell an encrypted 7z, and a `.001` split could be anything
        let (format, volume) = match classify(name) {
            File::Rar(n) => ("rar", n),
            _ => continue,
        };
        let set = set_name(name, false).to_lowercase();
        let entry = sets.entry((format, set)).or_default();
        entry.0.push((volume, *size));
        entry.1 |= !name.to_ascii_lowercase().ends_with(".rar");
    }
    let ((format, _), (mut vols, old)) =
        sets.into_iter().max_by_key(|(_, (v, _))| v.iter().map(|(_, s)| *s).sum::<i64>())?;
    vols.sort_unstable();
    // one volume listed at two sizes is two sets under one name: no shape to go by
    if vols.windows(2).any(|w| w[0].0 == w[1].0 && w[0].1 != w[1].1) {
        return None;
    }
    vols.dedup_by_key(|(n, _)| *n);
    // from the set's own first volume (`.rar` is 0, `.part1.rar` 1), none missing
    let first = vols.first()?.0;
    if first > 1
        || (old && first != 0)
        || vols.len() < MIN_VOLUMES
        || vols.iter().enumerate().any(|(i, (n, _))| *n != first + i as u32)
    {
        return None;
    }
    let (body, last) = vols.split_at(vols.len() - 1);
    let size = body.first()?.1;
    let last = last.first()?.1;
    // a last volume as big as the cut is more often a list cut short than a set that came out even
    if size <= 0 || last <= 0 || last >= size || body.iter().any(|(_, s)| *s != size) {
        return None;
    }
    Some(format!("volumes:{format}:{size}:{}:{last}", vols.len()))
}

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
        keys.mux = p.media.as_ref().and_then(mux_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.encode = p.media.as_ref().and_then(encode_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.thread =
            crate::probe::thread_root(&p.headers).map(|id| as_key(digest8(format!("thread:{id}").as_bytes())));
        keys.subs = p.media.as_ref().and_then(subs_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.fonts = p.media.as_ref().and_then(fonts_text).map(|t| as_key(digest8(t.as_bytes())));
        keys.chapters = p.media.as_ref().and_then(chapters_text).map(|t| as_key(digest8(t.as_bytes())));
    }
    keys.lines = lines_text(timing, parts).map(|t| as_key(digest8(t.as_bytes())));
    keys
}

pub fn salt_id(salt: i64) -> i64 {
    as_key(digest8(format!("salt:{salt}").as_bytes()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedCrc {
    pub file: FileCrc,
    pub name: String,
    /// the whole-file MD5 a set of this release gives the file, when one does: a donor's set that says another MD5
    /// for the same size and CRC is another file, a CRC32 collision or a set that lies
    pub md5: Option<Vec<u8>>,
}

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

/// Keeps `release_keys` up to date: after a probe (which asks for it again), and as a release grows, for the Lines
/// key. A release with no key has no row.
#[derive(Default)]
pub struct KeysStep {
    salt: OnceLock<i64>,
}

impl KeysStep {
    /// The original native owner reads and verifies the catalog salt; the Step retains that value.
    fn salt(&self, data: &crate::postprocess::data::StepData<'_>) -> Result<i64, StepError> {
        let found = data.key_salt(self.salt.get().copied())?;
        Ok(*self.salt.get_or_init(|| found))
    }
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
    /// from probes made long before it. Bit 2: the mux, encode and thread keys, and file CRCs folded from articles.
    /// Bit 4: the subtitle header, font and chapter keys. Bit 8: the encrypted volume sizes key.
    fn config_stamp(&self) -> u64 {
        1 | 2 | 4 | 8
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let parts = Some(ctx.release().parts);
        let salt = self.salt(ctx.data())?;
        let keys = ctx.data().release_keys(id, parts, salt)?;
        let had = ctx.data().stored_release_keys(id)?;
        let writes = match (keys.is_empty(), had) {
            (true, None) => Vec::new(),
            (true, Some(_)) => vec![StepWrite::for_mutation(id, StepMutation::ClearReleaseKeys)],
            (false, Some(old)) if old == keys => Vec::new(),
            (false, _) => vec![StepWrite::for_mutation(id, StepMutation::KeepReleaseKeys { keys })],
        };
        Ok(Outcome { matched: !keys.is_empty(), writes, ..Outcome::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_key_is_a_clean_v_cut() {
        let set = |sizes: &[i64], name: &dyn Fn(usize) -> String| -> Vec<(String, i64)> {
            sizes.iter().enumerate().map(|(i, s)| (name(i), *s)).collect()
        };
        let part = |i: usize| format!("x.part{:02}.rar", i + 1);
        assert_eq!(
            volumes_text(&set(&[100, 100, 100, 40], &part)).as_deref(),
            Some("volumes:rar:100:4:40"),
            "names play no part"
        );
        let old = |i: usize| if i == 0 { "Y.rar".to_string() } else { format!("Y.r{:02}", i - 1) };
        assert_eq!(volumes_text(&set(&[100, 100, 100, 40], &old)).as_deref(), Some("volumes:rar:100:4:40"));
        assert_eq!(volumes_text(&set(&[100, 100, 40], &part)).as_deref(), Some("volumes:rar:100:3:40"));
        assert_eq!(volumes_text(&set(&[100, 40], &part)), None, "too few");
        assert_eq!(volumes_text(&set(&[100, 90, 100, 40], &part)), None, "not one cut");
        assert_eq!(volumes_text(&set(&[100, 100, 100, 140], &part)), None, "a last bigger than the cut");
        assert_eq!(volumes_text(&set(&[100, 100, 100, 100], &part)), None, "a list cut short looks even");
        let later = |i: usize| format!("x.part{:02}.rar", i + 2);
        assert_eq!(volumes_text(&set(&[100, 100, 100, 40], &later)), None, "the first volume missing");
        let no_rar = |i: usize| format!("Y.r{i:02}");
        assert_eq!(volumes_text(&set(&[100, 100, 100, 40], &no_rar)), None, "an old set without its .rar");
        let mut twice = set(&[100, 100, 100, 40], &part);
        twice.push(("X.part02.rar".into(), 90));
        assert_eq!(volumes_text(&twice), None, "two sets under one name");
        let mut gap = set(&[100, 100, 100, 40], &part);
        gap.remove(1);
        assert_eq!(volumes_text(&gap), None, "a volume missing");
        let mut with_par2 = set(&[100, 100, 100, 40], &part);
        with_par2.push(("x.vol00+01.par2".into(), 7));
        assert_eq!(volumes_text(&with_par2).as_deref(), Some("volumes:rar:100:4:40"));
    }

    #[test]
    fn subtitle_font_and_chapter_keys_need_enough_to_tell_releases_apart() {
        use crate::probe::media::{Attachment, Chapter, MediaInfo, SubtitleTrack};
        let mut m = MediaInfo { header_complete: true, attachments_complete: true, ..Default::default() };
        assert_eq!((subs_text(&m), fonts_text(&m), chapters_text(&m)), (None, None, None));
        m.subtitles = vec![
            SubtitleTrack { codec: "S_TEXT/UTF8".into(), ..Default::default() },
            SubtitleTrack { codec: "S_TEXT/ASS".into(), header_key: Some("00aa".into()), ..Default::default() },
        ];
        assert_eq!(subs_text(&m).as_deref(), Some("subs:00aa"));
        let font = |name: &str, size| Attachment { name: name.into(), size, ..Default::default() };
        m.attachments = vec![font("A.ttf", 10), font("cover.jpg", 99)];
        assert_eq!(fonts_text(&m), None, "one font is everyone's");
        m.attachments.push(Attachment { mime: Some("application/x-font-otf".into()), ..font("b", 20) });
        assert_eq!(fonts_text(&m).as_deref(), Some("fonts:a.ttf:10,b:20"));
        m.attachments.reverse();
        assert_eq!(fonts_text(&m).as_deref(), Some("fonts:a.ttf:10,b:20"), "in any order");
        m.header_complete = false;
        assert_eq!(fonts_text(&m), None, "a header cut short may list only some");
        let at = |secs: &[f64]| secs.iter().map(|s| Chapter { start_secs: *s, name: None }).collect::<Vec<_>>();
        m.chapters_complete = true;
        m.chapters = at(&[0.0, 312.5]);
        assert_eq!(chapters_text(&m), None);
        m.chapters = at(&[0.0, 300.0, 600.0, 900.0, 1000.0]);
        assert_eq!(chapters_text(&m), None, "every five minutes, the last cut short by the end");
        m.chapters = at(&[0.0, 92.342, 1301.1, 2544.0]);
        assert_eq!(chapters_text(&m).as_deref(), Some("chapters:0,92342,1301100,2544000"));
        assert_eq!(chapters_text(&MediaInfo { chapters: at(&[0.0, 0.0, 1301.1]), ..Default::default() }), None);
        m.chapters.push(Chapter { start_secs: f64::NAN, name: None });
        assert_eq!(chapters_text(&m), None, "a start not read leaves the list unknown");
    }

    #[test]
    fn font_names_cannot_alias_key_field_or_inventory_boundaries() {
        use crate::probe::media::{Attachment, MediaInfo};
        let font = |name: &str, size| Attachment { name: name.into(), size, ..Default::default() };
        let a = MediaInfo {
            header_complete: true,
            attachments_complete: true,
            attachments: vec![font("a.ttf", 10), font("b.ttf", 20), font("c.ttf", 30)],
            ..Default::default()
        };
        let b = MediaInfo { attachments: vec![font("a.ttf:10,b.ttf", 20), font("c.ttf", 30)], ..a.clone() };
        assert!(fonts_text(&a).is_some());
        assert!(fonts_text(&b).is_none(), "retained names cannot concatenate into another inventory's accepted key");
    }

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
    fn the_mux_encode_and_thread_keys_need_their_parts() {
        let mut media = crate::probe::media::MediaInfo {
            segment_uid: Some("0102030405060708090a0b0c0d0e0f10".into()),
            muxed_at: Some(1_700_000_000),
            encoder: Some("x264".into()),
            encoder_options: Some("cabac=1 ref=4 crf=18.0".into()),
            duration_secs: Some(5400.4),
            ..Default::default()
        };
        assert_eq!(mux_text(&media).as_deref(), Some("mux:0102030405060708090a0b0c0d0e0f10:1700000000"));
        assert_eq!(encode_text(&media).as_deref(), Some("encode:x264:cabac=1 ref=4 crf=18.0:5400"));
        let mut probe = ProbeInfo { media: Some(media.clone()), ..Default::default() };
        probe.headers.insert("References".into(), "<root@post> <other@post>".into());
        let keys = keys_of(Some(&probe), &[], None, None);
        assert!(keys.mux.is_some() && keys.encode.is_some());
        assert_eq!(keys.thread, Some(as_key(digest8(b"thread:<root@post>"))));
        // the root's own article hangs on itself, soo it meets the parts that name it
        let mut root = ProbeInfo::default();
        root.headers.insert("Message-ID".into(), "<root@post>".into());
        assert_eq!(keys_of(Some(&root), &[], None, None).thread, keys.thread);
        media.segment_uid = Some("00000000000000000000000000000000".into());
        media.duration_secs = None;
        assert_eq!((mux_text(&media), encode_text(&media)), (None, None), "no UID, no length: no key");
    }
}
