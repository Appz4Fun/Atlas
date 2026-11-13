//! The processing step: a few articles of a release, read whole.

use std::sync::Mutex;

use crate::archive::{self, crc32};
use crate::catalog_facts::{FileInfo, SegmentPick};
use crate::config::Config;
use crate::nntp::yenc_decode;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite,
    mask,
};
use crate::release_info;

use super::{
    ProbeInfo, agent_of, derive, imdb_ids, keep_headers, media, moviehash, nest, nfo_text, parse_sfv, xref_groups,
    yenc, yenc_quirks,
};

pub const PROBE_STEP: StepId = StepId(4);

/// The most text kept from an NFO.
const MAX_NFO: usize = super::nfo_store::MAX_CHARS;

#[derive(Default)]
pub struct ProbeStep {
    on: Mutex<bool>,
}

fn is_par2(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".par2")
}

fn is_small_text(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".nfo") || lower.ends_with(".sfv") || lower.ends_with(".txt")
}

/// The file the probe's about: the one with the most parts, and of those, the first by name, soo a set of RAR
/// volumes gives its first volume.
pub(crate) fn main_file(files: &[FileInfo]) -> Option<&FileInfo> {
    let parts = |f: &FileInfo| f.expected.unwrap_or(f.parts_seen);
    let candidates: Vec<&FileInfo> =
        files.iter().filter(|f| !is_par2(&f.filename) && !is_small_text(&f.filename)).collect();
    let most = candidates.iter().map(|f| parts(f)).max()?;
    let pick = candidates
        .iter()
        .copied()
        .filter(|f| parts(f) >= most.saturating_sub(1))
        .min_by_key(|f| first_volume_key(&f.filename))?;
    Some(first_of_set(pick, &candidates))
}

/// The first volume posted of the set `pick` is in. Only it holds the archive's header, and it can be smaller than
/// the rest: a 7z `.001` of 220 parts before 120 volumes of 293, or a `.part01.rar` cut shorter, which the size rule
/// above leaves out.
fn first_of_set<'a>(pick: &'a FileInfo, candidates: &[&'a FileInfo]) -> &'a FileInfo {
    let Some((set, _)) = volume(&pick.filename) else { return pick };
    candidates
        .iter()
        .copied()
        .filter_map(|f| volume(&f.filename).filter(|(s, _)| *s == set).map(|(_, n)| (n, f)))
        .min_by_key(|(n, _)| *n)
        .map_or(pick, |(_, f)| f)
}

/// A volume's set and its place in the set: `x.rar` 0 then `x.r00` on from 1 (and `.s00` past `.r99`), `x.part3.rar`
/// 3, `x.7z.003` and `x.003` 3. None for a name that isn't a volume.
fn volume(name: &str) -> Option<(String, u32)> {
    let lower = name.to_ascii_lowercase();
    let (stem, ext) = lower.rsplit_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if ext.len() == 3 && digits(ext) {
        return Some((format!("{stem}.#"), ext.parse().ok()?));
    }
    if ext == "rar" {
        if let Some((set, part)) = stem.rsplit_once(".part")
            && digits(part)
        {
            return Some((format!("{set}.part#.rar"), part.parse().ok()?));
        }
        return Some((format!("{stem}.r#"), 0));
    }
    match ext.as_bytes() {
        [letter @ b'r'..=b'z', d1, d2] if d1.is_ascii_digit() && d2.is_ascii_digit() => {
            let n = u32::from(letter - b'r') * 100 + u32::from(d1 - b'0') * 10 + u32::from(d2 - b'0');
            Some((format!("{stem}.r#"), n + 1))
        }
        _ => None,
    }
}

/// Whether `name` is a volume after its set's first, which only a probe from before `first_of_set` could have read.
fn later_volume(name: &str) -> bool {
    volume(name).is_some_and(|(set, n)| if set.ends_with(".r#") { n > 0 } else { n > 1 })
}

/// `.rar` and `.part01.rar` and `.001` before the volumes after them.
fn first_volume_key(name: &str) -> (u8, String) {
    let lower = name.to_ascii_lowercase();
    let rank = if lower.ends_with(".rar") { 0 } else { 1 };
    (rank, lower)
}

pub(super) fn fetch_raw(ctx: &mut StepCtx<'_>, message_id: &str, file: &str) -> Result<Got, StepError> {
    Ok(ctx.fetch_article(&capped(message_id, file))?.got)
}

/// A file's first article, or its first `ARTICLE_CAP` when it's bigger: some posters put 40 MB in an article, and
/// a media header is in the first few hundred KB of it.
fn fetch_head(ctx: &mut StepCtx<'_>, message_id: &str, file: &str) -> Result<Got, StepError> {
    Ok(ctx.fetch_article_prefix(&capped(message_id, file))?.got)
}

fn capped(message_id: &str, file: &str) -> BodyRequest {
    BodyRequest { message_id: message_id.to_string(), file: file.to_string(), max_bytes: Some(archive::ARTICLE_CAP) }
}

/// The codecs `stream_key` is taken for, as Matroska and MP4 name them.
const KEYED_AUDIO: &[&str] = &["A_AC3", "A_EAC3", "A_FLAC", "ac-3", "ec-3", "fLaC"];

/// Extensions of the files a probe reads tracks from since it first ran: a saved probe of one with no media is
/// from before.
const NEWLY_READ: &[&str] = &[
    "avi", "wmv", "asf", "mpg", "mpeg", "vob", "iso", "img", "ifo", "bup", "bdmv", "clpi", "mpls", "bdjo", "m2ts",
    "ts", "mts",
];

/// Whether `name` ends in `.ext`, or is a split piece of one: `.ext.001` and on.
fn named(name: &str, ext: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(ext).is_some_and(|s| s.ends_with('.')) || lower.contains(&format!(".{ext}.0"))
}

/// Whether a saved probe stopped at encryption without extracting media tracks.
fn encrypted_without_media(info: &str) -> bool {
    let Ok(p) = serde_json::from_str::<serde_json::Value>(info) else { return false };
    if !p["media"].is_null() {
        return false;
    }
    p["archive"]["encrypted"] == true
        || p["layers"]
            .as_array()
            .is_some_and(|ls| ls.iter().any(|l| l["encrypted"] == true || l["stop"] == "encrypted"))
}

/// A saved probe with no media of a file in `NEWLY_READ`, from before its reader, or of a set's later volume, from
/// before `first_of_set`: probed again rather than reused, as when the shards were made again and the seed's reruns
/// found nothing to ask for.
fn stale_saved(info: &str) -> bool {
    let Ok(p) = serde_json::from_str::<serde_json::Value>(info) else { return false };
    let newly_read = |name: Option<&str>| name.is_some_and(|n| NEWLY_READ.iter().any(|e| named(n, e)));
    let empty = |k: &str| p["media"][k].as_array().is_none_or(Vec::is_empty);
    let mp4_without_tracks =
        p["media"]["container"] == "mp4" && p["archive"].is_null() && empty("video") && empty("audio");
    let dvd_folder_as_vob = p["media"]["container"] == "mpeg-ps"
        && p["archive"]["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().contains("video_ts"));
    // Dolby Vision saved before `from_rpu` was kept, the same rows `DV_WITHOUT_RPU` picks
    let dv_before_from_rpu = !p["media"]["video"][0]["dv_profile"].is_null()
        && p["media"]["video"][0]["dolby_vision"].get("from_rpu").is_none();
    // and before `keyframe_key` was kept, the rows `DV_WITHOUT_KEYFRAME_KEY` picks: a null one was looked for
    let dv_before_keyframe_key =
        !p["media"]["video"][0]["dv_profile"].is_null() && p["media"]["video"][0].get("keyframe_key").is_none();
    // These are missing producer fields, not a config-version guess. Only a
    // currently visited target with this saved payload asks for missing facts;
    // the preserved cache row is not deleted or relabelled as a genuine miss.
    let media=&p["media"];
    let matroska=media["container"].as_str().is_some_and(|c|matches!(c,"matroska"|"webm"));
    let old_media_coverage=matroska && (
        media.get("header_complete").is_none()
        || (media["attachments"].as_array().is_some_and(|files|!files.is_empty()) && media.get("attachments_complete").is_none())
        || media["subtitles"].as_array().is_some_and(|tracks|tracks.iter().any(|track|track.get("header_key").is_none()))
        || (media["chapters"].as_array().is_some_and(|chapters|!chapters.is_empty()) && media.get("chapters_complete").is_none()));
    // and before `stream_key` was kept, the rows `DV_WITHOUT_AUDIO_KEY` picks
    let dv_before_audio_key = !p["media"]["video"][0]["dv_profile"].is_null()
        && p["media"]["audio"][0]["codec"].as_str().is_some_and(|c| KEYED_AUDIO.contains(&c))
        && p["media"]["audio"][0].get("stream_key").is_none();
    old_media_coverage
        || mp4_without_tracks
        || dvd_folder_as_vob
        || dv_before_audio_key
        || dv_before_from_rpu
        || dv_before_keyframe_key
        || p["media"].is_null()
            && (newly_read(p["file"].as_str())
                || newly_read(p["archive"]["name"].as_str())
                || p["file"].as_str().is_some_and(later_volume))
}

/// A preserved row can exist without the fields needed by the current target.
/// This state is deliberately distinct from a natural-cache miss or decode error.
pub(crate) enum SavedCoverage {
    Current(CheckedSavedProbe),
    NeedsTargetRefresh,
}

/// Non-deserializable admission result, minted only by the real bounded decode.
/// It binds the exact JSON bytes so an actor can recheck its PG association
/// without allocating a second expanded ProbeInfo on the same worker frame.
pub(crate) struct CheckedSavedProbe {
    probe: ProbeInfo,
    length: usize,
    digest: [u8; 32],
}
impl CheckedSavedProbe {
    pub(crate) fn matches(&self,json:&str)->bool {
        use sha2::Digest;
        self.length==json.len()&&self.digest==<[u8;32]>::from(sha2::Sha256::digest(json.as_bytes()))
    }
    fn into_probe(self) -> ProbeInfo {
        self.probe
    }
}
impl std::ops::Deref for CheckedSavedProbe {
    type Target = ProbeInfo;
    fn deref(&self) -> &ProbeInfo {
        &self.probe
    }
}

/// Bound expanded serde ownership before any of the legacy JSON predicates can
/// allocate a Value tree. The shared admission reserves four struct widths and
/// 64 bytes per decoded string byte, covering the temporary predicate tree while
/// the typed probe remains live. No preserved bytes or stamps are rewritten.
pub(crate) fn native_saved_probe(info: &str, remaining: &mut u64) -> anyhow::Result<SavedCoverage> {
    let decoded = crate::pg::probe_decode::decode(info.as_bytes(), remaining)?;
    use sha2::Digest;
    Ok(if stale_saved(info) {
        SavedCoverage::NeedsTargetRefresh
    } else {
        SavedCoverage::Current(CheckedSavedProbe {
            probe: decoded,
            length: info.len(),
            digest: sha2::Sha256::digest(info.as_bytes()).into(),
        })
    })
}

// The parser recognizes cover names independently of declared MIME. Its cover
// bytes are serde(skip), so serialized media alone cannot prove asset absence.
fn native_media_needs_asset(media: Option<&media::MediaInfo>) -> bool {
    media.is_some_and(|media| {
        media.cover.is_some()
            || media.attachments.iter().any(|a| {
                media::cover_rank(&a.name).is_some()
                    || a.mime
                        .as_deref()
                        .and_then(|mime| mime.get(..6))
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
            })
    })
}

/// Metadata-only SameMux reuse. The caller must select and observe the candidate
/// on its original owner's RR snapshot; the returned ID is not write authority.
/// This never authorizes article-byte lending. Until retained image assets have
/// a qualified worker grant, candidates with image attachments are ineligible.
/// A missing/old semantic proof is a known ineligible candidate, whereas malformed
/// or over-budget saved data is an error and must not trigger provider fallback.
pub(crate) fn native_same_mux_candidate(
    own: i64,
    candidate: i64,
    group: &str,
    info: &ProbeInfo,
    json: &str,
    moviehash: Option<&str>,
    cover: Option<(&str, &str)>,
    stamp: u64,
    remaining: &mut u64,
) -> anyhow::Result<Option<SameMux>> {
    if own == candidate || crate::filters::group_blocked(group) {
        return Ok(None);
    }
    let SavedCoverage::Current(other) = native_saved_probe(json, remaining)? else { return Ok(None) };
    let other = other.into_probe();
    if cover.is_some() || native_media_needs_asset(other.media.as_ref()) {
        return Ok(None);
    }
    let Some(mine) = info.media.as_ref() else { return Ok(None) };
    let Some(mux) = crate::keys::mux_text(mine) else { return Ok(None) };
    // Packed probes do not currently bind the inner payload's whole checksum.
    // Refuse their cross-release metadata reuse rather than compare outer CRCs.
    if info.archive.is_some()
        || !info.layers.is_empty()
        || other.archive.is_some()
        || !other.layers.is_empty()
        || info.file_crc32.zip(other.file_crc32).is_none_or(|(a, b)| a != b)
        || content_size(info).is_none()
        || content_size(info) != content_size(&other)
        || moviehash
            .filter(|h| !h.is_empty())
            .is_none_or(|h| info.moviehash.as_deref() != Some(h) || other.moviehash.as_deref() != Some(h))
        || other.media_whole != Some(whole_mark(stamp))
    {
        return Ok(None);
    }
    let Some(media) = other.media else { return Ok(None) };
    if !media.header_complete
        || !media.encoder_complete
        || !first_frame_read(&media)
        || (!media.attachments.is_empty() && !media.attachments_complete)
        || (!media.chapters.is_empty() && !media.chapters_complete)
        || crate::keys::mux_text(&media).as_ref() != Some(&mux)
        || media.video.is_empty()
        || !same_header(mine, &media)
    {
        return Ok(None);
    }
    Ok(Some(SameMux { release_id: candidate, media, cover: None }))
}

/// Whether a server has it. None: this doesn't say, as when the article's too big to read, or the budget's gone.
fn there(got: &Got) -> Option<bool> {
    match got {
        Got::Body(_) | Got::TooBig => Some(true),
        Got::Missing => Some(false),
        Got::Refused(_) => None,
    }
}

impl Step for ProbeStep {
    fn id(&self) -> StepId {
        PROBE_STEP
    }
    fn name(&self) -> &'static str {
        "probe"
    }
    fn rank(&self) -> u16 {
        120
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.on.lock().unwrap_or_else(|e| e.into_inner()) = cfg.archive.enabled;
        cfg.archive.enabled
    }
    fn applies(&self, release: &ReleaseInfo) -> bool {
        !release.name.to_ascii_lowercase().ends_with(".jpg")
    }
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    /// What a probe reads, a bit per version, soo a release probed before a version gets probed again. Bit 1:
    /// chapters, commentary, more than one article of a header, and the OpenSubtitles hash. Bit 2: archives inside
    /// archives, followed down. Bit 4: attachments, cover art, when it was muxed, MaxCLL and MaxFALL. Bit 8:
    /// SegmentUID, editions, default flags and the encoder's settings. Bit 16: the article's header names in order,
    /// its Message-ID and References, and the title ids a Matroska file's tags give. Bit 32: mkvmerge's per-track
    /// statistics from the Tags at a Matroska file's end, and a Blu-ray's `id.bdmv` disc ID. Bit 64: the yEnc
    /// encoder's quirks.
    fn config_stamp(&self) -> u64 {
        1 | 2 | 4 | 8 | 16 | 32 | 64
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let release_id = ctx.release().id;
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let rel_parts = ctx.release().parts;
        let rel_complete = ctx.release().complete;
        let stamp = self.config_stamp();
        let content_id = ctx.data().release_content_id(release_id)?.unwrap_or_default();
        let existing_db_pw = ctx.data().nfo_password(release_id)?;
        let known_pw = match existing_db_pw.clone() {
            Some(password) => Some(password),
            None => crate::raw_store::get_nfo_password_checked(&rel_name, &rel_group, &content_id)
                .map_err(StepError::from_cache)?,
        };
        let saved = if ctx.data().explicit_rerun(release_id, PROBE_STEP)? {
            None
        } else {
            crate::raw_store::get_probe_checked(&rel_name, &rel_group, &content_id, rel_parts, rel_complete, stamp)
                .map_err(StepError::from_cache)?
        };
        let mut asset_hold: Option<Box<dyn std::any::Any + Send>> = None;
        let saved = match saved {
            Some(saved) => match native_saved_probe(&saved.info, &mut (32 << 20)).map_err(StepError::from_cache)? {
                SavedCoverage::NeedsTargetRefresh => None,
                SavedCoverage::Current(decoded) => {
                    if saved.cover.is_some() {
                        let grant =
                            crate::pg::worker_assets::current().map_err(StepError::from_cache)?.ok_or_else(|| {
                                StepError::Transient("native cover namespace has not been selected".into())
                            })?;
                        match grant
                            .verify_probe(
                                release_id,
                                crate::pg::natural_cache::Key {
                                    name: &rel_name,
                                    group: &rel_group,
                                    content_id: &content_id,
                                },
                                rel_parts,
                                rel_complete,
                                stamp,
                                &saved,
                                &decoded,
                            )
                            .map_err(StepError::from_cache)?
                        {
                            crate::pg::worker_assets::CoverStatus::Verified(hold) => asset_hold = Some(Box::new(hold)),
                            crate::pg::worker_assets::CoverStatus::Changed => {
                                return Err(StepError::Transient("saved probe association changed".into()));
                            }
                            crate::pg::worker_assets::CoverStatus::Missing => {
                                return Err(StepError::Transient("selected native cover bytes are unavailable".into()));
                            }
                        }
                    } else if native_media_needs_asset(decoded.media.as_ref()) {
                        return Err(StepError::Transient("saved probe has no verified cover association".into()));
                    }
                    Some((saved, decoded.into_probe()))
                }
            },
            None => None,
        };
        if let Some((saved, decoded)) = saved.filter(|(saved, _)| {
            let has_pw = saved.nfo_password.is_some() || known_pw.is_some();
            (asset_hold.is_some() || saved.cover.is_none()) && !(has_pw && encrypted_without_media(&saved.info))
        }) {
            // a video probed before the probe kept the server's body CRC gets it from one HEAD
            let backfill = super::body_crc::wants_backfill(&saved.info);
            let mut writes = Vec::new();
            if let Some(pw) = saved.nfo_password {
                writes.push(StepWrite::for_mutation(release_id, StepMutation::SetNfoPassword { password: pw }));
            }
            writes.push(match saved.cover {
                Some((mime, sha256)) => StepWrite::for_mutation(release_id, StepMutation::KeepCover { mime, sha256 }),
                None => StepWrite::for_mutation(release_id, StepMutation::ClearCover),
            });
            let hash = saved.moviehash.clone();
            writes.extend(title_writes(release_id, decoded.media.as_ref()));
            writes.push(StepWrite::for_mutation(
                release_id,
                StepMutation::KeepProbe {
                    crc32: saved.crc32,
                    agent: saved.agent,
                    json: saved.info,
                    moviehash: hash.clone(),
                },
            ));
            // the account, host and layout keys come from what the probe holds
            let rerun = mask(crate::keys::KEYS_STEP)
                | if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 }
                | if backfill { mask(super::body_crc::BODY_CRC_STEP) } else { 0 };
            let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
            return Ok(Outcome { matched: true, writes, facts, rerun, hold: asset_hold, ..Outcome::default() });
        }
        let files = ctx.files().map_err(StepError::from_inventory)?;
        let par2_names = ctx.data().par2_names(release_id, &files)?;
        let par2_all = ctx.data().par2_file_names(release_id)?;
        let mut check = ctx.data().local_password_check(release_id)?;
        let mut info = ProbeInfo::default();
        let mut writes = Vec::new();
        let mut found_nfo_password = known_pw.or_else(|| {
            let rel = ctx.release();
            let texts = std::iter::once(rel.name.as_str())
                .chain(rel.display_name.as_deref())
                .chain(files.iter().map(|f| f.filename.as_str()))
                .chain(files.iter().filter_map(|f| f.subject.as_deref()))
                .chain(par2_all.iter().map(String::as_str))
                .chain(par2_names.values().map(String::as_str));
            release_info::find_verified_password(texts, &[], check.as_ref())
        });
        if let Some(ref pw) = found_nfo_password {
            crate::raw_store::put_nfo_password_checked(&rel_name, &rel_group, &content_id, pw)
                .map_err(StepError::from_cache)?;
            writes.push(StepWrite::for_mutation(release_id, StepMutation::SetNfoPassword { password: pw.clone() }));
        }

        // the NFO and SFV come first, being the smallest: a budget that runs out still leaves them
        let is_text_or_par2_text = |f: &FileInfo| {
            is_small_text(&f.filename)
                || par2_names.get(&f.filename).is_some_and(|p| is_small_text(p))
                || f.subject.as_deref().is_some_and(crate::nfo::is_nfo)
        };
        // an NFO read before, by this probe's last run or by a step that came first, isn't fetched again
        let mut kept = ctx.data().stored_nfo(release_id)?;
        for file in files.iter().filter(|f| is_text_or_par2_text(f)).take(3) {
            let par2_name = par2_names.get(&file.filename);
            let eff_name = par2_name.unwrap_or(&file.filename);
            let is_sfv = eff_name.to_ascii_lowercase().ends_with(".sfv");
            let reused = match &kept {
                Some(k) if !is_sfv && k.is_of(&file.filename, par2_name.map(String::as_str)) => kept.take(),
                _ => None,
            };
            // `from_start`: read from the file's first article, the NFO's start, soo a reader after it can use it
            let (text, from_start) = match reused {
                Some(k) => (k.text, true),
                None => {
                    let Some(seg) = ctx
                        .segments(file.id, &SegmentPick::First(1))
                        .map_err(StepError::from_inventory)?
                        .into_iter()
                        .next()
                    else {
                        continue;
                    };
                    let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { continue };
                    let article = yenc::split_article(&raw);
                    let body = yenc_decode(&article.lines).unwrap_or_else(|| article.lines.join(&b'\n'));
                    (nfo_text(&body), seg.part.is_none_or(|p| p <= 1))
                }
            };
            if is_sfv {
                info.sfv = parse_sfv(&text);
            } else {
                if found_nfo_password.is_none()
                    && let Some(password) = release_info::find_verified_password([text.as_str()], &[], check.as_ref())
                {
                    crate::raw_store::put_nfo_password_checked(&rel_name, &rel_group, &content_id, &password)
                        .map_err(StepError::from_cache)?;
                    found_nfo_password = Some(password.clone());
                    writes.push(StepWrite::for_mutation(release_id, StepMutation::SetNfoPassword { password }));
                }
                if info.nfo.is_none() {
                    info.imdb_ids = imdb_ids(&text);
                    info.nfo = Some(text.chars().take(MAX_NFO).collect());
                    // a later article's text isn't the NFO: no file, soo nothing reuses it and the next run reads
                    // the first article once it's there
                    if from_start {
                        info.nfo_file = Some(file.filename.clone());
                        // the probe's JSON holds it now: a copy a step kept goes
                        writes.push(StepWrite::for_mutation(
                            release_id,
                            StepMutation::DropNfoCopy { file: file.filename.clone() },
                        ));
                    }
                }
            }
        }
        if found_nfo_password.is_none() {
            for nzb_file in archive::select::nzb_sources_with_par2(&files, &par2_names) {
                let Some(seg) = ctx
                    .segments(nzb_file.id, &SegmentPick::First(1))
                    .map_err(StepError::from_inventory)?
                    .into_iter()
                    .next()
                else {
                    continue;
                };
                let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &nzb_file.filename)? else { continue };
                let article = yenc::split_article(&raw);
                let body = yenc_decode(&article.lines).unwrap_or_else(|| article.lines.join(&b'\n'));
                if archive::identify(&body) == archive::Format::Nzb
                    && let Some(meta_pw) = archive::nzb::meta_password(&body)
                    && let Some(password) =
                        release_info::find_verified_password(std::iter::empty(), &[meta_pw], check.as_ref())
                {
                    crate::raw_store::put_nfo_password_checked(&rel_name, &rel_group, &content_id, &password)
                        .map_err(StepError::from_cache)?;
                    found_nfo_password = Some(password.clone());
                    writes.push(StepWrite::for_mutation(release_id, StepMutation::SetNfoPassword { password }));
                    break;
                }
            }
        }

        let mut last_lines = None;
        // the cover kept for a release whose media came from another's probe, when its own start had none
        let mut twin_cover = None;
        if let Some(file) = main_file(&files) {
            info.file = Some(file.filename.clone());
            // only the real ends count: an incomplete file's earliest article isn't its first
            let first = archive::steps::first_article(ctx, file)?;
            let last = archive::steps::tail_articles(ctx, file)?.pop();
            let wire_bytes = first.as_ref().map_or(0, |f| f.bytes);
            let mut front = Vec::new();
            if let Some(first) = &first {
                let got = fetch_head(ctx, &first.message_id, &file.filename)?;
                if let Some(there) = there(&got) {
                    info.health.insert("first".into(), there);
                }
                if let Got::Body(raw) = got {
                    let article = yenc::split_article(&raw);
                    info.headers = keep_headers(&article.headers);
                    info.header_order = crate::probe::header_order(&article.headers);
                    if let Some(xref) = info.headers.get("Xref") {
                        info.crossposted_to = xref_groups(xref);
                    }
                    info.yenc = yenc::parse(&article.lines);
                    info.quirks = yenc_quirks::Quirks::read(&article.lines);
                    if let Some(body) = yenc_decode(&article.lines) {
                        if let Some(expected) = info.yenc.as_ref().and_then(|y| y.pcrc32) {
                            info.part_crc_ok = Some(crc32(&body) == expected);
                        }
                        front = body;
                        if check.is_none() {
                            check = match archive::identify(&front) {
                                archive::Format::Rar4 | archive::Format::Rar5 => {
                                    archive::rar::list(&front).password_check
                                }
                                archive::Format::Zip => archive::zip::list_head(&front).password_check,
                                _ => None,
                            };
                        }
                        if found_nfo_password.is_none()
                            && check.is_some()
                            && let Some(ref nfo_s) = info.nfo
                            && let Some(password) =
                                release_info::find_verified_password([nfo_s.as_str()], &[], check.as_ref())
                        {
                            crate::raw_store::put_nfo_password_checked(&rel_name, &rel_group, &content_id, &password)
                                .map_err(StepError::from_cache)?;
                            found_nfo_password = Some(password.clone());
                            writes.push(StepWrite::for_mutation(release_id, StepMutation::SetNfoPassword { password }));
                        }
                        inspect(&front, found_nfo_password.as_deref(), &mut info);
                    }
                }
            }
            let total = file.expected.unwrap_or(file.parts_seen);
            if total > 2 {
                let middle = ctx
                    .segments(file.id, &SegmentPick::Parts(vec![(total + 1) / 2]))
                    .map_err(StepError::from_inventory)?;
                if let Some(middle) = middle.into_iter().next()
                    && let Some(there) = ctx.article_exists(&middle.message_id, &file.filename)?
                {
                    info.health.insert("middle".into(), there);
                }
            }
            let mut last_body = None;
            if let Some(last) = last.filter(|l| first.as_ref().is_none_or(|f| l.message_id != f.message_id)) {
                let got = fetch_raw(ctx, &last.message_id, &file.filename)?;
                if let Some(there) = there(&got) {
                    info.health.insert("last".into(), there);
                }
                if let Got::Body(raw) = got {
                    let article = yenc::split_article(&raw);
                    last_lines = yenc::parse(&article.lines);
                    last_body = yenc_decode(&article.lines);
                    // the tail article read is the file's last only when its own yEnc lines say soo
                    if let Some(seen) = yenc_quirks::Quirks::read(&article.lines) {
                        info.quirks.get_or_insert_with(Default::default).add(&seen);
                    }
                }
            }
            derive(&mut info, wire_bytes, file.expected, last_lines.as_ref());
            let known_last = last_part(&info, last_lines.as_ref(), last_body);
            // the file's own OpenSubtitles hash, over its size and first and last 64 KiB: a lender's must agree with
            // it (`same_mux_media`), which catches a header, or end Tags in the last 64 KiB, rewritten in place
            info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last.clone())?;
            // the same mux already read whole by another release's probe: its media is taken, and nothing more of
            // this file is read for it (an explicit rerun reads its own)
            let shared = if front.is_empty() || ctx.data().explicit_rerun(release_id, PROBE_STEP)? {
                None
            } else {
                ctx.data().same_mux_media(release_id, &info, stamp)?
            };
            if let Some(twin) = shared {
                crate::metrics::names::MEDIA_SHARED.inc(&[("key", crate::keys::Kind::Mux.word())], 1);
                twin_cover = take_lent(&mut info, twin, stamp);
            }
            // last, being the most it may read: a release's article budget goes to the checks above first. A release
            // whose media came from another's keeps the archive and layers its first article shows: the lender's
            // matched its unpacked size, soo more of the front would show no other file
            if !front.is_empty() && info.media_from.is_none() {
                let before = info.layers.clone();
                let encoder_read =
                    read_more_front(ctx, file, total, &mut front, found_nfo_password.as_deref(), &mut info)?;
                // more of the front can show a bigger file than the one hashed
                if info.layers != before {
                    info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last.clone())?;
                }
                read_tail_moov(ctx, file, &front, &mut info)?;
                let tags_read = read_tail_tags(ctx, file, &front, known_last.as_ref(), &mut info)?;
                if media_read_whole(encoder_read, tags_read, info.media.as_ref()) {
                    info.media_whole = Some(whole_mark(stamp));
                }
            }
        }

        let empty = info.yenc.is_none() && info.nfo.is_none() && info.sfv.is_empty() && info.health.is_empty();
        if empty {
            return Ok(Outcome { writes, ..Outcome::default() });
        }
        // a look that finds no cover drops one an earlier look kept
        let cover = info.media.as_mut().and_then(|m| m.cover.take());
        let saved_cover = if cover.is_some() {
            let grant = crate::pg::worker_assets::current()
                .map_err(StepError::from_cache)?
                .ok_or_else(|| StepError::Transient("native cover namespace has not been selected".into()))?;
            let hold = grant
                .produce_cover(release_id, cover.as_ref().expect("present native cover"))
                .map_err(StepError::from_cache)?;
            let descriptor = hold.descriptor();
            asset_hold = Some(Box::new(hold));
            Some(descriptor)
        } else {
            twin_cover
        };
        writes.push(match &saved_cover {
            Some((mime, sha256)) => StepWrite::for_mutation(
                release_id,
                StepMutation::KeepCover { mime: mime.clone(), sha256: sha256.clone() },
            ),
            None => StepWrite::for_mutation(release_id, StepMutation::ClearCover),
        });
        let crc = info.file_crc32.map(i64::from);
        let agent = agent_of(&info.headers).map(str::to_string);
        let hash = info.moviehash.clone();
        let json = serde_json::to_string(&info).map_err(|e| StepError::Permanent(e.to_string()))?;
        crate::raw_store::put_probe_checked(&crate::raw_store::ProbeRecord {
            name: &rel_name,
            group_name: &rel_group,
            content_id: &content_id,
            crc32: crc,
            agent: agent.as_deref(),
            info: &json,
            moviehash: hash.as_deref(),
            parts: rel_parts,
            complete: rel_complete,
            config_stamp: stamp,
            nfo_password: found_nfo_password.as_deref(),
            cover: saved_cover.as_ref().map(|(m, s)| (m.as_str(), s.as_str())),
        })
        .map_err(StepError::from_cache)?;
        writes.extend(title_writes(release_id, info.media.as_ref()));
        writes.push(StepWrite::for_mutation(
            release_id,
            StepMutation::KeepProbe { crc32: crc, agent, json, moviehash: hash.clone() },
        ));
        // a hash found on a later look is asked about then too
        let needs_list_decrypt = found_nfo_password.is_some()
            && (existing_db_pw != found_nfo_password || info.archive.as_ref().is_some_and(|a| a.name.is_some()))
            && ctx.data().archive_requires_password_list(release_id)?;
        let rerun = mask(crate::keys::KEYS_STEP)
            | if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 }
            | if needs_list_decrypt { mask(archive::steps::LIST_STEP) } else { 0 };
        let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
        Ok(Outcome { matched: true, writes, facts, rerun, hold: asset_hold, ..Outcome::default() })
    }
}

/// The rules a probe meets to mark its media whole (`media_whole`), kept with the config stamp above its 32 bits:
/// raised whenever they get stricter, soo a probe marked under looser ones never lends.
const MEDIA_WHOLE_RULES: u64 = 1;

/// What `media_whole` holds for a probe run under config `stamp`.
fn whole_mark(stamp: u64) -> u64 {
    stamp | MEDIA_WHOLE_RULES << 32
}

/// What another release's probe already read of the same Matroska mux.
#[derive(Debug, PartialEq)]
pub(crate) struct SameMux {
    pub release_id: i64,
    pub media: media::MediaInfo,
    /// its cover art, `(mime, sha256)`, while the folder still has it
    pub cover: Option<(String, String)>,
}

/// Puts what another probe read of the same file (`same_mux_media`) on this one: its media, keeping this file's own
/// cover when its first article had one, where it came from, and the whole mark. The lender's cover, `(mime,
/// sha256)`, when this file had none of its own.
fn take_lent(info: &mut ProbeInfo, twin: SameMux, stamp: u64) -> Option<(String, String)> {
    let own_cover = info.media.as_mut().and_then(|m| m.cover.take());
    let lent_cover = if own_cover.is_none() { twin.cover } else { None };
    let mut media = twin.media;
    media.cover = own_cover;
    info.media = Some(media);
    info.media_from = Some(twin.release_id);
    info.media_whole = Some(whole_mark(stamp));
    lent_cover
}

/// The size of the video file a probe read its media from: the innermost file the layers were followed down to (an
/// archive's first entry may be an NFO, a nested set's an inner volume), else the file as posted. None when a layer
/// stopped short or doesn't say.
fn content_size(p: &ProbeInfo) -> Option<u64> {
    match p.layers.last() {
        Some(_) if p.layers.iter().any(|l| l.stop.is_some()) => None,
        Some(inner) => inner.unpacked,
        None if p.archive.is_none() => p.yenc.as_ref().and_then(|y| y.file_size),
        None => None,
    }
}

/// Whether what a first article shows of a file's header (`mine`) is all in another probe's (`other`) as it is: its
/// title, each track's codec, language and name, and the attachments so far. mkvpropedit rewrites these in place
/// and keeps the SegmentUID and DateUTC, soo the same mux key can name a file whose header was changed.
fn same_header(mine: &media::MediaInfo, other: &media::MediaInfo) -> bool {
    // a header the first article holds whole has every item there is; one it cut off may show its last item only in
    // part, soo that one isn't compared, and the other probe may have more
    let whole = mine.header_complete;
    fn prefix<T, K: PartialEq>(a: &[T], b: &[T], whole: bool, label: impl Fn(&T) -> K) -> bool {
        let a = if whole { a } else { &a[..a.len().saturating_sub(1)] };
        (if whole { a.len() == b.len() } else { a.len() <= b.len() })
            && a.iter().zip(b).all(|(x, y)| label(x) == label(y))
    }
    mine.title == other.title
        && mine.tags.iter().all(|(k, v)| other.tags.get(k) == Some(v))
        && prefix(&mine.video, &other.video, whole, |t| {
            (
                t.codec.clone(),
                t.language.clone(),
                (t.width, t.height, t.display_width, t.display_height),
                (t.hdr.clone(), t.primaries, t.transfer, t.max_cll, t.max_fall),
            )
        })
        && prefix(&mine.audio, &other.audio, whole, |t| {
            (t.codec.clone(), t.language.clone(), t.name.clone(), t.default, t.commentary)
        })
        && prefix(&mine.subtitles, &other.subtitles, whole, |t| {
            (t.codec.clone(), t.language.clone(), t.name.clone(), t.default, t.forced, t.hearing_impaired, t.commentary)
        })
        && prefix(&mine.attachments, &other.attachments, whole, |a| (a.name.clone(), a.size))
        && prefix(&mine.chapters, &other.chapters, whole, |c| (c.start_secs.to_bits(), c.name.clone()))
}

/// Articles read from the start of a file, at most, when its header runs past the first one: Matroska puts
/// attachments and chapters before the video, and a few MB of fonts is common.
const FRONT_ARTICLES: i64 = 6;

/// The title a file's tags name, put on its `media_release` row before any lookup by name: an IMDb id the same way
/// an NFO's is (`imdb_from_nfo`), and a TMDB or TVDB id pinned the same way (`tmdb_from_tag`, `tvdb_from_tag`), each
/// only when the row's kind agrees (a TMDB movie id means nothing for a show). No row yet: nothing, and the next
/// probe of the release writes them.
pub(crate) fn title_writes(release_id: i64, media: Option<&media::MediaInfo>) -> Vec<StepWrite> {
    let Some(ids) = media.map(media::MediaInfo::title_ids) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(imdb) = ids.imdb {
        out.push(StepWrite::for_mutation(release_id, StepMutation::PinTagImdb { imdb_id: imdb }));
    }
    if let Some((movie, tmdb)) = ids.tmdb {
        let kind = if movie { crate::media::title::Kind::Movie } else { crate::media::title::Kind::Tv };
        out.push(StepWrite::for_mutation(release_id, StepMutation::PinTagTmdb { tmdb_id: tmdb,kind }));
    }
    if let Some(tvdb) = ids.tvdb {
        out.push(StepWrite::for_mutation(release_id, StepMutation::PinTagTvdb { tvdb_id: tvdb }));
    }
    out
}

/// A later step's way to the hash this run found, before it's written.
pub const MOVIEHASH_FACT: &str = "moviehash";

/// More of the file's start, an article at a time, until the media header is all there. Whether the encoder's
/// settings were looked for to their end: found whole, not wanted, or the article past the header read and none
/// there (a remux or a hardware encode writes none).
fn read_more_front(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    total: i64,
    front: &mut Vec<u8>,
    password: Option<&str>,
    info: &mut ProbeInfo,
) -> Result<bool, StepError> {
    let mut part = 2;
    while info.media.as_ref().is_some_and(|m| !m.header_complete) && part <= total.min(FRONT_ARTICLES) {
        let Some(body) = read_part(ctx, file, part, front.len() as u64)? else { break };
        front.extend(body);
        inspect(front, password, info);
        part += 1;
    }
    // the encoder's settings are in the first frame, just past the header: one more article when they're not here yet
    if !info.media.as_ref().is_some_and(wants_encoder) {
        return Ok(true);
    }
    if part <= total.min(FRONT_ARTICLES)
        && let Some(body) = read_part(ctx, file, part, front.len() as u64)?
    {
        front.extend(body);
        inspect(front, password, info);
        // settings begun but cut off by the read's end aren't whole
        return Ok(info.media.as_ref().is_some_and(|m| !wants_encoder(m) || m.encoder.is_none()));
    }
    Ok(false)
}

/// Whether a probe's own reads of its media ran to their end: the encoder's settings looked for to theirs, a bare
/// Matroska file's end Tags read whole, the header complete and the first frame's keys there.
fn media_read_whole(encoder_read: bool, tags_read: bool, media: Option<&media::MediaInfo>) -> bool {
    // only Matroska is lent (by its mux key); an MP4's tail moov read says nothing of its end
    encoder_read
        && tags_read
        && media.is_some_and(|m| {
            matches!(m.container.as_str(), "matroska" | "webm") && m.header_complete && first_frame_read(m)
        })
}

/// Whether the keys taken over the first frame are there: an AVC or HEVC track's keyframe key (and with it, a Dolby
/// Vision RPU), and a keyed first audio track's stream key. Whether they are depends on how far the posted articles
/// reach past the header, not on the file, soo a probe without them doesn't lend its media.
fn first_frame_read(m: &media::MediaInfo) -> bool {
    let video = m.video.first().is_none_or(|v| {
        let c = v.codec.to_ascii_lowercase();
        !["avc", "hevc", "h264", "hvc1", "hev1"].iter().any(|k| c.contains(k)) || v.keyframe_key.is_some()
    });
    let audio = m.audio.first().is_none_or(|a| !KEYED_AUDIO.contains(&a.codec.as_str()) || a.stream_key.is_some());
    video && audio
}

/// Whether the header is all read but the encoder's settings, in the first frame just past it, aren't yet: an AVC or
/// HEVC video track.
fn wants_encoder(m: &media::MediaInfo) -> bool {
    m.header_complete
        && !m.encoder_complete
        && m.video.iter().any(|v| {
            // Matroska's codec ids, and MP4's sample entry fourccs
            let c = v.codec.to_ascii_lowercase();
            ["avc", "hevc", "h264", "hvc1", "hev1"].iter().any(|k| c.contains(k))
        })
}

/// The most of an MP4's `moov` read from the end of the file: a feature film's sample tables are a few MB.
const MAX_TAIL_MOOV: u64 = 8 << 20;

/// An MP4 posted whole whose `moov` comes after its `mdat` (not made for streaming): the front has no tracks, soo
/// the parts holding the `moov` are read, from where the `mdat` ends to the end of the file.
fn read_tail_moov(ctx: &mut StepCtx<'_>, file: &FileInfo, front: &[u8], info: &mut ProbeInfo) -> Result<(), StepError> {
    let wants = info.media.as_ref().is_some_and(|m| m.container == "mp4" && m.video.is_empty() && m.audio.is_empty());
    // a bare file only: in an archive the moov's place in the post would depend on the volumes
    if !wants || info.archive.is_some() || !info.layers.is_empty() {
        return Ok(());
    }
    let segment = info.segment_size.filter(|s| *s > 0);
    let (Some(segment), Some(size)) = (segment, info.yenc.as_ref().and_then(|y| y.file_size)) else { return Ok(()) };
    let Some(moov_at) = media::Mp4Prober::find_moov_offset(front).filter(|at| *at < size) else { return Ok(()) };
    let end = size.min(moov_at.saturating_add(MAX_TAIL_MOOV));
    let first = moov_at / segment + 1;
    let last = (end - 1) / segment + 1;
    let mut tail = Vec::new();
    for part in first..=last {
        let Ok(part_no) = i64::try_from(part) else { return Ok(()) };
        let Some(body) = read_part(ctx, file, part_no, (part - 1) * segment)? else { return Ok(()) };
        tail.extend(body);
    }
    let skip = usize::try_from(moov_at - (first - 1) * segment).unwrap_or(usize::MAX);
    let Some(moov) = tail.get(skip..) else { return Ok(()) };
    // the sample offsets count from the file's start, which `front` is
    let found = media::Mp4Prober::probe_tail_moov_with(moov, front);
    if let Some(found) = found.filter(|m| !m.video.is_empty() || !m.audio.is_empty()) {
        info.media = Some(found);
    }
    Ok(())
}

/// The most parts read for a Matroska file's end Tags: mkvmerge's are a few KB, but may straddle two parts.
const TAIL_TAGS_PARTS: u64 = 2;

/// A Matroska file posted whole whose Tags lie after its Clusters, as mkvmerge writes them with every track's
/// statistics (exact bitrate, frame and byte counts): the front's SeekHead says where, and the part holding them is
/// read, one more when they run into it. The last part, read already, isn't fetched again. Whether the Tags it
/// wanted were read, true when it wanted none.
fn read_tail_tags(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    front: &[u8],
    known_last: Option<&(i64, Vec<u8>)>,
    info: &mut ProbeInfo,
) -> Result<bool, StepError> {
    let wants = info.media.as_ref().is_some_and(|m| {
        matches!(m.container.as_str(), "matroska" | "webm") && m.header_complete && !m.track_uids.is_empty()
    });
    // a bare file only, as for the moov: in an archive the Tags' place in the post would depend on the volumes
    if !wants || info.archive.is_some() || !info.layers.is_empty() {
        return Ok(true);
    }
    let size = info.yenc.as_ref().and_then(|y| y.file_size);
    let Some(at) = media::matroska_tags_past(front).into_iter().find(|at| size.is_none_or(|size| *at < size)) else {
        return Ok(true);
    };
    let whole = |b: &[u8]| media::element_len(b).and_then(|n| usize::try_from(n).ok()).is_some_and(|n| n <= b.len());
    // past the first Cluster but inside what was read
    let read = usize::try_from(at).ok().and_then(|at| front.get(at..)).filter(|b| whole(b));
    let mut tail = Vec::new();
    let tags = match read {
        Some(read) => read,
        None => {
            let segment = info.segment_size.filter(|s| *s > 0);
            let (Some(segment), Some(size)) = (segment, size) else { return Ok(false) };
            let first = at / segment + 1;
            let skip = usize::try_from(at - (first - 1) * segment).unwrap_or(usize::MAX);
            for part in first..=size.div_ceil(segment).min(first + TAIL_TAGS_PARTS - 1) {
                let Ok(part_no) = i64::try_from(part) else { break };
                let body = match known_last.filter(|(p, _)| *p == part_no) {
                    Some((_, body)) => Some(body.clone()),
                    None => read_part(ctx, file, part_no, (part - 1) * segment)?,
                };
                let Some(body) = body else { break };
                tail.extend(body);
                if tail.get(skip..).is_some_and(whole) {
                    break;
                }
            }
            let Some(tags) = tail.get(skip..) else { return Ok(false) };
            tags
        }
    };
    let Some(m) = info.media.as_mut() else { return Ok(false) };
    media::tail_tags(tags, m);
    // what a missing or short part cut off is read as far as it goes, but isn't the whole of them
    Ok(whole(tags))
}

/// One part of a file, decoded, when it holds the file's bytes from `offset` (0 based) on.
pub(super) fn read_part(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    part: i64,
    offset: u64,
) -> Result<Option<Vec<u8>>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Parts(vec![part])).map_err(StepError::from_inventory)?;
    let Some(seg) = segs.into_iter().next().filter(|s| s.part.is_none_or(|p| p == part)) else { return Ok(None) };
    let Got::Body(raw) = fetch_raw(ctx, &seg.message_id, &file.filename)? else { return Ok(None) };
    let article = yenc::split_article(&raw);
    if yenc::parse(&article.lines).and_then(|l| l.begin) != Some(offset + 1) {
        return Ok(None);
    }
    Ok(yenc_decode(&article.lines))
}

/// The last article already read, as (part, bytes), when it sits where its part number says.
fn last_part(info: &ProbeInfo, lines: Option<&yenc::YencLines>, body: Option<Vec<u8>>) -> Option<(i64, Vec<u8>)> {
    let segment = info.segment_size.filter(|s| *s > 0)?;
    let begin = lines?.begin?.checked_sub(1)?;
    if begin % segment != 0 {
        return None;
    }
    Some((i64::try_from(begin / segment).ok()? + 1, body?))
}

/// A file's bytes `from..to`, from the parts that hold them. `known` is a part already read.
fn file_range(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    segment: u64,
    from: u64,
    to: u64,
    known: Option<&(i64, Vec<u8>)>,
) -> Result<Option<Vec<u8>>, StepError> {
    let Some(parts) = moviehash::parts_for(segment, from, to) else { return Ok(None) };
    // a hash's end is 64 KiB: more than two parts means parts too small to be real
    if parts.end() - parts.start() > 1 {
        return Ok(None);
    }
    let start = (*parts.start() as u64 - 1) * segment;
    let mut out = Vec::new();
    for part in parts {
        let body = match known.filter(|(p, _)| *p == part) {
            Some((_, body)) => body.clone(),
            None => match read_part(ctx, file, part, (part as u64 - 1) * segment)? {
                Some(body) => body,
                None => return Ok(None),
            },
        };
        out.extend(body);
    }
    let (lo, hi) = (usize::try_from(from - start).ok(), usize::try_from(to - start).ok());
    Ok(lo.zip(hi).and_then(|(lo, hi)| out.get(lo..hi)).map(<[u8]>::to_vec))
}

/// The OpenSubtitles hash of the video: of the file itself, or of the one a stored RAR set holds, whose end is
/// in the set's last volume.
fn moviehash_of(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    files: &[FileInfo],
    front: &[u8],
    info: &ProbeInfo,
    known_last: Option<(i64, Vec<u8>)>,
) -> Result<Option<String>, StepError> {
    const CHUNK: u64 = moviehash::CHUNK as u64;
    if info.media.is_none() {
        return Ok(None);
    }
    // the walk follows the biggest file, which needn't be the first one the old way reads
    let other_file = info.layers.first().is_some_and(|l| info.archive.as_ref().is_none_or(|a| a.name != l.name));
    if info.layers.len() > 1 || other_file {
        return chain_moviehash(ctx, file, files, front, info.segment_size, known_last);
    }
    let Some(head) = &info.archive else {
        let Some(size) = info.yenc.as_ref().and_then(|y| y.file_size) else { return Ok(None) };
        if size < 2 * CHUNK {
            return Ok(None);
        }
        // posted whole in one article: both ends are already here
        if front.len() as u64 == size {
            return Ok(moviehash::moviehash(size, front, front));
        }
        let Some(segment) = info.segment_size else { return Ok(None) };
        let tail = file_range(ctx, file, segment, size - CHUNK, size, known_last.as_ref())?;
        return Ok(tail.and_then(|tail| moviehash::moviehash(size, front, &tail)));
    };
    let (Some(at), Some(size), Some(name)) = (head.stored_at, head.unpacked, head.name.as_deref()) else {
        return Ok(None);
    };
    let Some(inner_front) = front.get(at..) else { return Ok(None) };
    if size < 2 * CHUNK {
        return Ok(None);
    }
    let Some(volume) = moviehash::last_volume(&file.filename, files) else { return Ok(None) };
    let tail = if volume.id == file.id {
        // one volume: the file ends where its stored bytes do
        let (Some(packed), Some(segment)) = (head.packed, info.segment_size) else { return Ok(None) };
        let end = at as u64 + packed;
        // the volumes after it aren't indexed
        if packed < CHUNK || head.continues {
            return Ok(None);
        }
        file_range(ctx, file, segment, end - CHUNK, end, known_last.as_ref())?
    } else {
        let Some(first) = archive::steps::first_article(ctx, volume)? else { return Ok(None) };
        let Got::Body(raw) = fetch_raw(ctx, &first.message_id, &volume.filename)? else { return Ok(None) };
        let article = yenc::split_article(&raw);
        let lines = yenc::parse(&article.lines);
        let (Some(1), Some(end_byte)) = (lines.as_ref().and_then(|l| l.begin), lines.as_ref().and_then(|l| l.end))
        else {
            return Ok(None);
        };
        let Some(body) = yenc_decode(&article.lines) else { return Ok(None) };
        let Some(last_head) = super::archive::parse(&body) else { return Ok(None) };
        let (Some(last_at), Some(packed)) = (last_head.stored_at, last_head.packed) else { return Ok(None) };
        // the same file, still stored, and its last piece holds a whole 64 KiB,
        // and this is its end: the last volume indexed needn't be the set's last
        if last_head.name.as_deref() != Some(name) || packed < CHUNK || last_head.continues {
            return Ok(None);
        }
        let end = last_at as u64 + packed;
        file_range(ctx, volume, end_byte, end - CHUNK, end, Some(&(1, body)))?
    };
    Ok(tail.and_then(|tail| moviehash::moviehash(size, inner_front, &tail)))
}

/// The hash of a video under more than one layer, or under a tar or the like: only when every layer is stored,
/// soo the video's end is a known place in this one file, and the file is all there is.
fn chain_moviehash(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    files: &[FileInfo],
    front: &[u8],
    segment: Option<u64>,
    known_last: Option<(i64, Vec<u8>)>,
) -> Result<Option<String>, StepError> {
    const CHUNK: u64 = moviehash::CHUNK as u64;
    let nest = nest::unwrap(front);
    let (Some(at), Some(last)) = (nest.outer_at, nest.layers.last()) else { return Ok(None) };
    if nest.layers.iter().any(|l| !l.stored || l.continues || l.stop.is_some()) {
        return Ok(None);
    }
    let Some(size) = last.unpacked.filter(|s| *s >= 2 * CHUNK) else { return Ok(None) };
    // the volumes after this one would hold the end: not followed yet
    if moviehash::last_volume(&file.filename, files).is_some_and(|v| v.id != file.id) {
        return Ok(None);
    }
    let end = at as u64 + size;
    let tail = match usize::try_from(end).ok().and_then(|e| front.get(e - moviehash::CHUNK..e)) {
        Some(tail) => Some(tail.to_vec()),
        None => match segment {
            Some(segment) => file_range(ctx, file, segment, end - CHUNK, end, known_last.as_ref())?,
            None => None,
        },
    };
    Ok(tail.and_then(|tail| moviehash::moviehash(size, front.get(at..)?, &tail)))
}

/// What the first bytes are. A file in an archive, or in an archive in that, and soo on down, is read as itself.
fn inspect(body: &[u8], password: Option<&str>, info: &mut ProbeInfo) {
    info.archive = super::archive::parse_with_password(body, password);
    let nest = nest::unwrap_with_password(body, password);
    info.media = media::parse(&nest.inner);
    // a DVD folder in a stored archive: the walk follows its biggest file, a VOB, but the title set's IFO, with the
    // languages and the playing time, is beside it in what was read; and of several title sets read, the walk's
    // pick needn't be the longest
    if info.media.as_ref().is_none_or(|m| matches!(m.container.as_str(), "dvd-video" | "mpeg-ps")) {
        if super::dvd::has_title_set(body) {
            // an ID worked out from an unwrapped disc image stands if the raw bytes can't give one
            let inner_id = info.media.as_mut().and_then(|m| m.disc_id.take());
            let mut dvd = super::dvd::dvd(body);
            dvd.disc_id = dvd.disc_id.or(inner_id);
            info.media = Some(dvd);
        } else if super::dvd::is_dvd(body) {
            // only the manager IFO or the disc's names so far: what was read stands, and more of the front is read
            // for a title set
            match info.media.as_mut() {
                Some(m) => m.header_complete = false,
                None => info.media = Some(super::dvd::dvd(body)),
            }
        }
    }
    // and a BDMV folder's playlists, beside the stream file (a transport stream) the walk picked, the longest of all
    // read; with only its index or the disc's names so far, a Blu-ray whose header is further on
    if info.media.as_ref().is_none_or(|m| matches!(m.container.as_str(), "bluray" | "m2ts" | "mpeg-ts"))
        && super::bluray::is_bluray(body)
    {
        // as for a DVD: an ID from an unwrapped disc image stands if the raw bytes can't give one
        let inner_id = info.media.as_mut().and_then(|m| m.disc_id.take());
        let mut bluray = super::bluray::bluray(body);
        bluray.disc_id = bluray.disc_id.or(inner_id);
        info.media = Some(bluray);
    }
    info.layers = nest.layers;
}

#[cfg(test)]
mod buffered_tags_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A Matroska mux with `video` video tracks and `audio` audio tracks, as a first article shows it.
    fn mkv(uid: &str, video: usize, audio: usize) -> media::MediaInfo {
        media::MediaInfo {
            container: "matroska".into(),
            segment_uid: Some(uid.into()),
            muxed_at: Some(1_700_000_000),
            video: vec![media::VideoTrack { codec: "V_MPEGH/ISO/HEVC".into(), ..Default::default() }; video],
            audio: vec![media::AudioTrack { codec: "A_EAC3".into(), ..Default::default() }; audio],
            ..Default::default()
        }
    }

    const STAMP: u64 = 127;
    const HASH: &str = "1d9da161e916b2fd";
    const SIZE: u64 = 4_000_000_000;

    /// A bare file of `SIZE` bytes whose probe read `media` to the end under `STAMP`.
    fn whole_bare(media: media::MediaInfo) -> ProbeInfo {
        ProbeInfo {
            yenc: Some(super::super::yenc::YencLines { file_size: Some(SIZE), ..Default::default() }),
            moviehash: Some(HASH.into()),
            media: Some(media),
            media_whole: Some(whole_mark(STAMP)),
            ..Default::default()
        }
    }

    /// The first article of a bare file of `SIZE` bytes, showing `media` with its header all there.
    fn first_bare(mut media: media::MediaInfo) -> ProbeInfo {
        media.header_complete = true;
        ProbeInfo { media_whole: None, ..whole_bare(media) }
    }

    #[test]
    fn same_mux_header_comparison_preserves_exact_tracks_tags_and_partial_prefixes() {
        let mut whole = mkv("ab12", 1, 2);
        whole.header_complete = true;
        whole.chapters = vec![media::Chapter::default(); 3];
        let front = whole.clone();
        assert!(same_header(&front, &whole));
        let mut changed = front.clone();
        changed.title = Some("Retitled".into());
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.audio[0].language = Some("ger".into());
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.audio[0].default = true;
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.video[0].hdr = Some("HDR10".into());
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.tags.insert("IMDB".into(), "tt0133093".into());
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.chapters.pop();
        assert!(!same_header(&changed, &whole));
        let mut changed = front.clone();
        changed.audio.push(Default::default());
        assert!(!same_header(&changed, &whole));
        let mut partial = front;
        partial.header_complete = false;
        partial.audio[1].language = Some("ger".into());
        assert!(same_header(&partial, &whole), "only the incomplete final track is omitted");
        partial.audio[0].language = Some("ger".into());
        assert!(!same_header(&partial, &whole), "complete preceding tracks must still agree");
    }

    #[test]
    fn what_is_lent_keeps_the_files_own_cover() {
        let lent =
            || SameMux { release_id: 7, media: mkv("ab12", 1, 2), cover: Some(("image/jpeg".into(), "c0ffee".into())) };
        let mut info = first_bare(mkv("ab12", 1, 1));
        assert_eq!(
            take_lent(&mut info, lent(), STAMP),
            Some(("image/jpeg".into(), "c0ffee".into())),
            "none of its own"
        );
        assert_eq!(info.media.as_ref().map(|m| m.audio.len()), Some(2));
        assert_eq!((info.media_from, info.media_whole), (Some(7), Some(whole_mark(STAMP))));
        assert_eq!(info.moviehash.as_deref(), Some(HASH), "its own hash stays");

        let mut own = mkv("ab12", 1, 1);
        own.cover = Some(media::Cover { mime: "image/png", bytes: vec![1, 2, 3] });
        let mut info = first_bare(own);
        assert_eq!(take_lent(&mut info, lent(), STAMP), None, "its own first");
        assert_eq!(info.media.as_ref().and_then(|m| m.cover.as_ref()).map(|c| c.bytes.len()), Some(3));
    }

    #[test]
    fn only_a_probe_that_read_the_first_frame_keys_is_whole() {
        let mut m = mkv("ab12", 1, 1);
        assert!(!first_frame_read(&m), "HEVC without its keyframe key, E-AC-3 without its stream key");
        m.video[0].keyframe_key = Some("k".into());
        assert!(!first_frame_read(&m), "the audio key still missing");
        m.audio[0].stream_key = Some("s".into());
        assert!(first_frame_read(&m));
        m.audio[0] = media::AudioTrack { codec: "A_DTS".into(), ..Default::default() };
        assert!(first_frame_read(&m), "no key is taken over DTS");
        m.video[0] = media::VideoTrack { codec: "V_MPEG2".into(), ..Default::default() };
        assert!(first_frame_read(&m), "nor over MPEG-2 video");

        // the whole of what marks a probe's media whole
        let mut keyed = mkv("ab12", 1, 1);
        keyed.video[0].keyframe_key = Some("k".into());
        keyed.audio[0].stream_key = Some("s".into());
        assert!(!media_read_whole(true, true, Some(&keyed)), "header not complete");
        keyed.header_complete = true;
        assert!(media_read_whole(true, true, Some(&keyed)));
        assert!(!media_read_whole(false, true, Some(&keyed)), "encoder settings not looked for to their end");
        assert!(!media_read_whole(true, false, Some(&keyed)), "end Tags cut off");
        assert!(!media_read_whole(true, true, None), "no media");
        keyed.container = "mp4".into();
        assert!(!media_read_whole(true, true, Some(&keyed)), "only Matroska is lent");
    }

    fn file(name: &str, parts: i64) -> FileInfo {
        FileInfo {
            id: 1,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: Some(parts),
            file_total: None,
            parts_seen: parts,
            touched_at: None,
            sealed: false,
        }
    }

    #[test]
    fn dolby_vision_probes_without_an_rpu_are_asked_for_again() {
        assert!(stale_saved(r#"{"media":{"video":[{"dv_profile":7,"dolby_vision":{"profile":7,"fel":true}}]}}"#));
        assert!(stale_saved(r#"{"media":{"video":[{"dv_profile":7}]}}"#));
        assert!(!stale_saved(
            r#"{"media":{"video":[{"dv_profile":8,"dolby_vision":{"from_rpu":false},"keyframe_key":null}]}}"#
        ));
        assert!(!stale_saved(r#"{"media":{"video":[{"hdr":"HDR10"}]}}"#));
    }

    #[test]
    fn dolby_vision_probes_from_before_the_audio_key_are_asked_for_again() {
        let saved = |codec: &str, v: &str| {
            format!(
                r#"{{"media":{{"video":[{{"dv_profile":8,"dolby_vision":{{"from_rpu":true}},"keyframe_key":null}}],"audio":[{{"codec":"{codec}"{v}}}]}}}}"#
            )
        };
        assert!(stale_saved(&saved("A_AC3", "")), "a restored probe from before the key is probed again");
        assert!(!stale_saved(&saved("A_AC3", r#","stream_key":null"#)));
        assert!(!stale_saved(&saved("A_DTS", "")));
    }

    #[test]
    fn dolby_vision_probes_from_before_the_keyframe_key_are_asked_for_again() {
        let saved =
            |v: &str| format!(r#"{{"media":{{"video":[{{"dv_profile":8,"dolby_vision":{{"from_rpu":true}}{v}}}]}}}}"#);
        assert!(stale_saved(&saved("")), "a restored probe from before the key is probed again");
        assert!(!stale_saved(&saved(r#","keyframe_key":null"#)));
        assert!(!stale_saved(&saved(r#","keyframe_key":"0123456789abcdef""#)));
    }

    #[test]
    fn old_missing_probe_coverage_is_not_current_known_absence_or_a_partial_current_read() {
        let old=r#"{"media":{"container":"matroska","subtitles":[{"codec":"S_TEXT/ASS"}]}}"#;
        assert!(stale_saved(old),"the current target needs missing producer facts, not a global stamp sweep");
        let mut current=media::MediaInfo {container:"matroska".into(),..Default::default()};
        current.subtitles.push(media::SubtitleTrack {codec:"S_TEXT/ASS".into(),..Default::default()});
        let partial=serde_json::to_string(&ProbeInfo {media:Some(current.clone()),..Default::default()}).unwrap();
        assert!(!stale_saved(&partial),"explicit partial read retains known facts without inventing whole-header proof");
        current.header_complete=true;
        let negative=serde_json::to_string(&ProbeInfo {media:Some(current),..Default::default()}).unwrap();
        assert!(!stale_saved(&negative),"a completed header with no supported subtitle key is a genuine known absence");
        let mut old:serde_json::Value=serde_json::from_str(&negative).unwrap();
        old["media"]["subtitles"][0].as_object_mut().unwrap().remove("header_key");
        assert!(stale_saved(&old.to_string()));
        let mut old:serde_json::Value=serde_json::from_str(&negative).unwrap();
        old["media"]["chapters"]=serde_json::json!([{"start_secs":0},{"start_secs":92},{"start_secs":1301}]);
        old["media"].as_object_mut().unwrap().remove("chapters_complete");
        assert!(stale_saved(&old.to_string()),"old chapter list is not complete-element evidence");
    }

    #[test]
    fn a_saved_avi_probe_with_no_tracks_isnt_reused() {
        assert!(stale_saved(r#"{"file":"Show.S01E01.XviD.avi","media":null}"#));
        assert!(stale_saved(r#"{"file":"a.part01.rar","archive":{"name":"Movie.CD1.AVI"},"media":null}"#));
        assert!(stale_saved(r#"{"file":"Movie.avi.001"}"#));
        assert!(stale_saved(r#"{"file":"Clip.WMV","media":null}"#));
        assert!(!stale_saved(r#"{"file":"Clip.navi","media":null}"#), "not an avi");
        assert!(stale_saved(r#"{"file":"Film.mp4","media":{"container":"mp4","video":[],"audio":[]}}"#));
        assert!(!stale_saved(r#"{"file":"Film.mp4","media":{"container":"mp4","video":[{"codec":"avc1"}]}}"#));
        assert!(stale_saved(r#"{"archive":{"name":"VIDEO_TS/VTS_01_1.VOB"},"media":{"container":"mpeg-ps"}}"#));
        assert!(!stale_saved(r#"{"file":"Film.mpg","media":{"container":"mpeg-ps"}}"#), "a plain mpeg");
        assert!(!stale_saved(r#"{"file":"Movie.avi","media":{"container":"avi"}}"#), "read");
        assert!(!stale_saved(r#"{"file":"Movie.mkv","media":null}"#));
    }

    #[test]
    fn a_smaller_first_volume_is_still_the_one_read() {
        let mut files = vec![file("x.7z.001", 220)];
        for n in 2..=5 {
            files.push(file(&format!("x.7z.{n:03}"), 293));
        }
        files.push(file("x.par2", 1));
        assert_eq!(main_file(&files).unwrap().filename, "x.7z.001");
        let parts = [file("M.part1.rar", 50), file("M.part2.rar", 100), file("M.part3.rar", 100)];
        assert_eq!(main_file(&parts).unwrap().filename, "M.part1.rar");
        let old = [file("a.r00", 100), file("a.r01", 100), file("a.rar", 80)];
        assert_eq!(main_file(&old).unwrap().filename, "a.rar");
        // a sample beside a bigger file isn't a volume of it
        let raw = [file("movie.mkv", 3000), file("movie.sample.mkv", 10)];
        assert_eq!(main_file(&raw).unwrap().filename, "movie.mkv");
    }

    #[test]
    fn later_volumes_are_known() {
        for later in ["x.7z.002", "x.002", "M.part02.rar", "a.r00", "a.s01"] {
            assert!(later_volume(later), "{later}");
        }
        for first in ["x.7z.001", "x.001", "M.part01.rar", "M.part1.rar", "a.rar", "movie.mkv", "x.000"] {
            assert!(!later_volume(first), "{first}");
        }
        assert!(stale_saved(r#"{"file":"x.7z.002","media":null}"#), "a later volume with no media");
        let current=ProbeInfo {file:Some("x.7z.002".into()),media:Some(media::MediaInfo {container:"matroska".into(),..Default::default()}),..Default::default()};
        assert!(!stale_saved(&serde_json::to_string(&current).unwrap()),"already read current media is not stale merely because it is a later volume");
        assert!(stale_saved(r#"{"file":"x.7z.002","media":{"container":"matroska"}}"#),"older missing coverage is a separate reason to refresh this target");
    }

    #[test]
    fn a_dvd_folder_in_a_stored_archive_is_read_from_its_ifo_not_its_vob() {
        use crate::archive::testkit::{r4_file, r4_main, rar4};
        let mut vob = vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 0x01, 0x89, 0xC3, 0xF8];
        vob.resize(200_000, 0);
        let body = rar4(&[
            r4_main(0),
            r4_file(0, b"VIDEO_TS\\VTS_01_0.IFO", &super::super::dvd::tests::ifo()),
            r4_file(0, b"VIDEO_TS\\VTS_01_1.VOB", &vob),
        ]);
        let mut info = ProbeInfo::default();
        inspect(&body, None, &mut info);
        let media = info.media.expect("a dvd");
        assert_eq!(media.container, "dvd-video");
        assert_eq!(media.audio.len(), 3);
        assert_eq!(media.duration_secs, Some(6750.0));
    }

    #[test]
    fn a_dvd_folder_with_only_its_manager_ifo_read_wants_more() {
        use crate::archive::testkit::{r4_file, r4_main, rar4};
        let mut vmg = b"DVDVIDEO-VMG".to_vec();
        vmg.resize(4096, 0);
        // a menu VOB with a whole sequence header
        let mut vob = vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 0x01, 0x89, 0xC3, 0xF8];
        vob.extend([0, 0, 1, 0xE0, 0, 21, 0x81, 0, 0]);
        vob.extend([0, 0, 1, 0xB3, 0x2D, 0x02, 0x40, 0x33, 0xFF, 0xFF, 0xE0, 0x18, 0, 0, 1, 0xB5, 0x14, 0x8A]);
        vob.resize(100_000, 0);
        let body = rar4(&[
            r4_main(0),
            r4_file(0, b"VIDEO_TS\\VIDEO_TS.IFO", &vmg),
            r4_file(0, b"VIDEO_TS\\VIDEO_TS.VOB", &vob),
        ]);
        let mut info = ProbeInfo::default();
        inspect(&body, None, &mut info);
        let media = info.media.expect("the menu's stream");
        assert!(!media.header_complete, "a title set may be in the next article");
    }

    #[test]
    fn of_several_title_sets_read_the_longest_wins_whichever_the_walk_picked() {
        use crate::archive::testkit::{r4_file, r4_main, rar4};
        // a bigger extras IFO of 10 minutes, then the feature's
        let mut extra = super::super::dvd::tests::ifo();
        extra[2048 + 0x40 + 4..2048 + 0x40 + 8].copy_from_slice(&[0x00, 0x10, 0x00, 0x40]);
        extra[2048 + 0x200 + 4..2048 + 0x200 + 8].copy_from_slice(&[0x00, 0x00, 0x00, 0x40]);
        extra.resize(8192, 0);
        let body = rar4(&[
            r4_main(0),
            r4_file(0, b"VIDEO_TS\\VTS_01_0.IFO", &extra),
            r4_file(0, b"VIDEO_TS\\VTS_02_0.IFO", &super::super::dvd::tests::ifo()),
        ]);
        let mut info = ProbeInfo::default();
        inspect(&body, None, &mut info);
        assert_eq!(info.media.expect("a dvd").duration_secs, Some(6750.0));
    }

    #[test]
    fn a_bdmv_folder_in_a_stored_archive_is_read_from_its_playlists() {
        use crate::archive::testkit::{r4_file, r4_main, rar4};
        let body = rar4(&[
            r4_main(0),
            r4_file(0, b"BDMV\\PLAYLIST\\00800.mpls", &super::super::bluray::tests::mpls(2, 3_600)),
            r4_file(0, b"BDMV\\STREAM\\00800.m2ts", &vec![0x47; 300_000]),
        ]);
        let mut info = ProbeInfo::default();
        inspect(&body, None, &mut info);
        let media = info.media.expect("a blu-ray");
        assert_eq!((media.container.as_str(), media.duration_secs), ("bluray", Some(7_200.0)));
        assert_eq!(media.audio.len(), 2);
    }

    #[test]
    fn the_main_file_is_the_big_one_and_the_first_volume() {
        let files = [
            file("a.par2", 1),
            file("a.nfo", 1),
            file("a.r00", 100),
            file("a.rar", 100),
            file("a.r01", 100),
            file("a.r02", 40),
        ];
        assert_eq!(main_file(&files).unwrap().filename, "a.rar");
        let raw = [file("movie.mkv", 3000), file("sample.mkv", 10)];
        assert_eq!(main_file(&raw).unwrap().filename, "movie.mkv");
        assert!(main_file(&[file("a.nfo", 1)]).is_none());
    }
}

#[cfg(test)]
#[path = "native_reuse_tests.rs"]
mod native_reuse_tests;
