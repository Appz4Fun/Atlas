//! The processing step: a few articles of a release, read whole.

use std::sync::Mutex;

use crate::archive::{self, crc32};
use crate::config::Config;
use crate::nntp::yenc_decode;
use crate::postprocess::step::{
    BodyRequest, Got, Needs, Outcome, ReleaseInfo, Reruns, Step, StepCtx, StepError, StepId, StepWrite, mask,
};
use crate::release_info;
use crate::store::{FileInfo, SegmentPick};

use super::{
    DROP_COVER, ProbeInfo, SET_COVER, SET_PROBE, agent_of, derive, imdb_ids, keep_headers, media, moviehash, nest,
    nfo_text, parse_sfv, xref_groups, yenc, yenc_quirks,
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

/// Asks for the probe again on releases whose probe found the main file's first article too big to read, before
/// `fetch_head`: it saw only that the article was there, no yEnc header. Once, from the stage's seed.
pub(crate) fn rerun_big_heads(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_probes(conn, "json_extract(info, '$.yenc') is null and json_extract(info, '$.health.first') = 1", now)
}

/// Asks for the probe again on AVI files it found no tracks in, before `avi` read them: posted bare, split into
/// `.avi.001` and on, or stored in an archive. Once, from the stage's seed.
pub(crate) fn rerun_avi(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_unread(conn, &["avi"], now)
}

/// The same for WMV and ASF files, before `asf` read them.
pub(crate) fn rerun_asf(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_unread(conn, &["wmv", "asf"], now)
}

/// Asks for the probe again on MP4s posted whole whose front gave no tracks, before `read_tail_moov` read the end
/// of the file. Once, from the stage's seed.
pub(crate) fn rerun_mp4_tail(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_probes(conn, MP4_WITHOUT_TRACKS, now)
}

/// A probe row of an MP4 posted whole with no tracks.
const MP4_WITHOUT_TRACKS: &str = "json_extract(info, '$.media.container') = 'mp4'
     and json_extract(info, '$.archive') is null
     and coalesce(json_array_length(json_extract(info, '$.media.video')), 0) = 0
     and coalesce(json_array_length(json_extract(info, '$.media.audio')), 0) = 0";

/// The same for MPEG program streams, before `mpegps` read them.
pub(crate) fn rerun_mpeg_ps(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_unread(conn, &["mpg", "mpeg", "vob"], now)
}

/// The same for DVD-Video discs and folders, before `dvd` read their title sets: an ISO or IMG image, or a
/// `VIDEO_TS` folder whose first file is an IFO or BUP.
pub(crate) fn rerun_dvd(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    Ok(rerun_unread(conn, &["iso", "img", "ifo", "bup"], now)? + rerun_probes(conn, DVD_FOLDER_AS_VOB, now)?)
}

/// A probe row of a `VIDEO_TS` folder in an archive read from its VOB only, before its IFO was looked for.
const DVD_FOLDER_AS_VOB: &str = "json_extract(info, '$.media.container') = 'mpeg-ps'
     and lower(json_extract(info, '$.archive.name')) like '%video_ts%'";
/// The same for Blu-ray discs and folders, before `bluray` read their playlists: a disc image, or a `BDMV` folder
/// whose first file is one of its small ones. Images again too: the DVD step's rerun may have run before this.
pub(crate) fn rerun_bluray(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_unread(conn, &["iso", "img", "bdmv", "clpi", "mpls", "bdjo", "m2ts"], now)
}

/// Asks for the probe again on Dolby Vision video whose RPU wasn't read, or was read before the probe kept that it
/// had been: only an RPU says whether a profile 7 enhancement layer is FEL or MEL, and which content mapping it
/// carries. One front read a release, and only Dolby Vision ones. Once, from the stage's seed.
pub(crate) fn rerun_dv_without_rpu(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_probes(conn, DV_WITHOUT_RPU, now)
}

/// A probe row whose first video track is Dolby Vision, saved before `from_rpu` was kept. A row that has it, true
/// or false, was probed since and isn't asked for again.
const DV_WITHOUT_RPU: &str = "json_extract(info, '$.media.video[0].dv_profile') is not null
     and json_type(info, '$.media.video[0].dolby_vision.from_rpu') is null";

/// Asks for the probe again on Dolby Vision video probed before the first keyframe was keyed, soo the remuxes the
/// owner looks for most can be found by their video stream. One front read a release, Dolby Vision only: other
/// releases get the key from their next probe. Once, from the stage's seed.
pub(crate) fn rerun_dv_without_keyframe_key(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_probes(conn, DV_WITHOUT_KEYFRAME_KEY, now)
}

/// A probe row whose first video track is Dolby Vision, saved before `keyframe_key` was kept (a probe since keeps
/// it, null when it found no whole keyframe).
const DV_WITHOUT_KEYFRAME_KEY: &str = "json_extract(info, '$.media.video[0].dv_profile') is not null
     and json_type(info, '$.media.video[0].keyframe_key') is null";

/// Asks for the probe again on Dolby Vision video whose first audio track is AC-3, E-AC-3 or FLAC, probed before
/// that track's stream was keyed. One front read a release, as for the keyframe key. Once, from the stage's seed.
pub(crate) fn rerun_dv_without_audio_key(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_probes(conn, DV_WITHOUT_AUDIO_KEY, now)
}

/// The codecs `stream_key` is taken for, as Matroska and MP4 name them.
const KEYED_AUDIO: &[&str] = &["A_AC3", "A_EAC3", "A_FLAC", "ac-3", "ec-3", "fLaC"];

/// A probe row whose first video track is Dolby Vision and first audio track a keyed codec, saved before
/// `stream_key` was kept (a probe since keeps it, null when the frames read ran out).
const DV_WITHOUT_AUDIO_KEY: &str = "json_extract(info, '$.media.video[0].dv_profile') is not null
     and json_extract(info, '$.media.audio[0].codec') in ('A_AC3', 'A_EAC3', 'A_FLAC', 'ac-3', 'ec-3', 'fLaC')
     and json_type(info, '$.media.audio[0].stream_key') is null";

/// The same for MPEG transport streams, before `mpegts` read their program map: a capture's `.ts` or `.mts`, or a
/// Blu-ray's `.m2ts`.
pub(crate) fn rerun_mpeg_ts(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    rerun_unread(conn, &["ts", "mts", "m2ts"], now)
}

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

/// Asks for the probe again where it found no media in a file with one of these extensions: the file itself, or the
/// first one of the archive it read.
fn rerun_unread(conn: &rusqlite::Connection, exts: &[&str], now: i64) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = conn
        .prepare(
            "select release_id, json_extract(info, '$.file'), json_extract(info, '$.archive.name') from release_probe
             where json_extract(info, '$.media') is null",
        )?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))?
        .filter_map(|row| match row {
            Ok((id, file, inner)) => [file, inner]
                .iter()
                .flatten()
                .any(|n| exts.iter().any(|e| named(n, e)))
                .then_some(Ok(id)),
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::postprocess::rerun_on(conn, *id, mask(PROBE_STEP), now)?;
    }
    Ok(ids.len())
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

/// Asks for `archive_list` and `probe` again on encrypted releases that now have a known password in
/// `release_nfo_password` (`names_hidden = 1` with no inner files, or a probe that stopped at encryption).
pub(crate) fn rerun_encrypted_with_password(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    let mut count = 0usize;
    let list_ids: Vec<i64> = conn
        .prepare(
            "select ar.release_id from archive_release ar
             join release_nfo_password pw on pw.release_id = ar.release_id
             where (ar.names_hidden = 1
                    and not exists (
                        select 1 from archive_inner_files f
                        where f.release_id = ar.release_id and f.source in (1, 2, 3)
                    ))
                or (ar.password = 1
                    and exists (
                        select 1 from archive_inner_files f
                        where f.release_id = ar.release_id and f.source in (1, 2, 3) and lower(f.name) like '%.nzb'
                    )
                    and not exists (
                        select 1 from archive_inner_files f
                        where f.release_id = ar.release_id and f.source = 5
                    ))",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &list_ids {
        crate::postprocess::rerun_on(conn, *id, mask(archive::steps::LIST_STEP), now)?;
        count += 1;
    }
    let probe_ids: Vec<i64> = conn
        .prepare(
            "select p.release_id, p.info from release_probe p
             join release_nfo_password pw on pw.release_id = p.release_id
             where json_extract(p.info, '$.media') is null",
        )?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|row| match row {
            Ok((id, info)) if encrypted_without_media(&info) => Some(Ok(id)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?;
    for id in &probe_ids {
        crate::postprocess::rerun_on(conn, *id, mask(PROBE_STEP), now)?;
        count += 1;
    }
    Ok(count)
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
    // and before `stream_key` was kept, the rows `DV_WITHOUT_AUDIO_KEY` picks
    let dv_before_audio_key = !p["media"]["video"][0]["dv_profile"].is_null()
        && p["media"]["audio"][0]["codec"].as_str().is_some_and(|c| KEYED_AUDIO.contains(&c))
        && p["media"]["audio"][0].get("stream_key").is_none();
    mp4_without_tracks
        || dvd_folder_as_vob
        || dv_before_audio_key
        || dv_before_from_rpu
        || dv_before_keyframe_key
        || p["media"].is_null()
            && (newly_read(p["file"].as_str())
            || newly_read(p["archive"]["name"].as_str())
            || p["file"].as_str().is_some_and(later_volume))
}

/// Asks for the probe again on releases whose probe read a later volume of a set and found no media, before
/// `first_of_set`. Once, from the stage's seed.
pub(crate) fn rerun_later_volumes(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = conn
        .prepare(
            "select release_id, json_extract(info, '$.file') from release_probe
             where json_extract(info, '$.media') is null",
        )?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?
        .filter_map(|row| match row {
            Ok((id, Some(file))) if later_volume(&file) => Some(Ok(id)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::postprocess::rerun_on(conn, *id, mask(PROBE_STEP), now)?;
    }
    Ok(ids.len())
}

/// The probe again, past its saved result, on the releases whose probe row matches `filter`.
fn rerun_probes(conn: &rusqlite::Connection, filter: &str, now: i64) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = conn
        .prepare(&format!("select release_id from release_probe where {filter}"))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::postprocess::rerun_on(conn, *id, mask(PROBE_STEP), now)?;
    }
    Ok(ids.len())
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
    /// SegmentUID, editions, default flags and the encoder's settings. Bit 32: mkvmerge's per-track statistics from
    /// the Tags at a Matroska file's end, and a Blu-ray's `id.bdmv` disc ID. Bit 64: the yEnc encoder's quirks.
    fn config_stamp(&self) -> u64 {
        1 | 2 | 4 | 8 | 32 | 64
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let release_id = ctx.release().id;
        let rel_name = ctx.release().name.clone();
        let rel_group = ctx.release().group.clone();
        let rel_parts = ctx.release().parts;
        let rel_complete = ctx.release().complete;
        let stamp = self.config_stamp();
        let content_id = crate::raw_store::release_content_id(ctx.conn(), release_id).unwrap_or_default();
        let existing_db_pw = archive::store::nfo_password(ctx.conn(), release_id);
        let known_pw = existing_db_pw
            .clone()
            .or_else(|| crate::raw_store::get_nfo_password(&rel_name, &rel_group, &content_id));
        if !crate::raw_store::is_explicit_rerun(ctx.conn(), release_id, PROBE_STEP)
            && let Some(saved) =
                crate::raw_store::get_probe(&rel_name, &rel_group, &content_id, rel_parts, rel_complete, stamp).filter(
                    |s| {
                        let has_pw = s.nfo_password.is_some() || known_pw.is_some();
                        s.cover
                            .as_ref()
                            .is_none_or(|(_, sha)| super::cover_file(sha).is_some_and(|p| p.exists()))
                            && !stale_saved(&s.info)
                            && !(has_pw && encrypted_without_media(&s.info))
                    },
                )
        {
            // a video probed before the probe kept the server's body CRC gets it from one HEAD
            let backfill = super::body_crc::wants_backfill(&saved.info);
            let mut writes = Vec::new();
            if let Some(pw) = saved.nfo_password {
                writes.push(StepWrite::new(archive::store::SET_NFO_PASSWORD, vec![release_id.into(), pw.into()]));
            }
            writes.push(match saved.cover {
                Some((mime, sha256)) => {
                    StepWrite::new(SET_COVER, vec![release_id.into(), mime.into(), sha256.into()])
                }
                None => StepWrite::new(DROP_COVER, vec![release_id.into()]),
            });
            let hash = saved.moviehash.clone();
            writes.push(StepWrite::new(
                SET_PROBE,
                vec![release_id.into(), saved.crc32.into(), saved.agent.into(), saved.info.into(), hash.clone().into()],
            ));
            // the account, host and layout keys come from what the probe holds
            let rerun = mask(crate::keys::KEYS_STEP)
                | if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 }
                | if backfill { mask(super::body_crc::BODY_CRC_STEP) } else { 0 };
            let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
            return Ok(Outcome { matched: true, writes, facts, rerun, ..Outcome::default() });
        }
        let files = ctx.files().map_err(retry)?;
        let par2_names = archive::select::par2_name_map(ctx.conn(), release_id, &files);
        let par2_all = archive::select::par2_file_names(ctx.conn(), release_id);
        let mut check = archive::store::local_password_check(ctx.conn(), release_id);
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
            crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, pw);
            writes.push(StepWrite::new(
                archive::store::SET_NFO_PASSWORD,
                vec![release_id.into(), pw.clone().into()],
            ));
        }

        // the NFO and SFV come first, being the smallest: a budget that runs out still leaves them
        let is_text_or_par2_text = |f: &FileInfo| {
            is_small_text(&f.filename)
                || par2_names.get(&f.filename).is_some_and(|p| is_small_text(p))
                || f.subject.as_deref().is_some_and(crate::nfo::is_nfo)
        };
        // an NFO read before, by this probe's last run or by a step that came first, isn't fetched again
        let mut kept = super::nfo_store::stored(ctx.conn(), release_id);
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
                    let Some(seg) = ctx.segments(file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next()
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
                    crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &password);
                    found_nfo_password = Some(password.clone());
                    writes.push(StepWrite::new(
                        archive::store::SET_NFO_PASSWORD,
                        vec![release_id.into(), password.into()],
                    ));
                }
                if info.nfo.is_none() {
                    info.imdb_ids = imdb_ids(&text);
                    info.nfo = Some(text.chars().take(MAX_NFO).collect());
                    // a later article's text isn't the NFO: no file, soo nothing reuses it and the next run reads
                    // the first article once it's there
                    if from_start {
                        info.nfo_file = Some(file.filename.clone());
                        // the probe's JSON holds it now: a copy a step kept goes
                        writes.push(StepWrite::new(
                            super::nfo_store::DROP,
                            vec![release_id.into(), file.filename.clone().into()],
                        ));
                    }
                }
            }
        }
        if found_nfo_password.is_none() {
            for nzb_file in archive::select::nzb_sources_with_par2(&files, &par2_names) {
                let Some(seg) = ctx.segments(nzb_file.id, &SegmentPick::First(1)).map_err(retry)?.into_iter().next()
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
                    crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &password);
                    found_nfo_password = Some(password.clone());
                    writes.push(StepWrite::new(
                        archive::store::SET_NFO_PASSWORD,
                        vec![release_id.into(), password.into()],
                    ));
                    break;
                }
            }
        }

        let mut last_lines = None;
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
                                archive::Format::Rar4 | archive::Format::Rar5 => archive::rar::list(&front).password_check,
                                archive::Format::Zip => archive::zip::list_head(&front).password_check,
                                _ => None,
                            };
                        }
                        if found_nfo_password.is_none()
                            && check.is_some()
                            && let Some(ref nfo_s) = info.nfo
                            && let Some(password) = release_info::find_verified_password([nfo_s.as_str()], &[], check.as_ref())
                        {
                            crate::raw_store::put_nfo_password(&rel_name, &rel_group, &content_id, &password);
                            found_nfo_password = Some(password.clone());
                            writes.push(StepWrite::new(
                                archive::store::SET_NFO_PASSWORD,
                                vec![release_id.into(), password.into()],
                            ));
                        }
                        inspect(&front, found_nfo_password.as_deref(), &mut info);
                    }
                }
            }
            let total = file.expected.unwrap_or(file.parts_seen);
            if total > 2 {
                let middle = ctx.segments(file.id, &SegmentPick::Parts(vec![(total + 1) / 2])).map_err(retry)?;
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
            info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last.clone())?;
            // last, being the most it may read: a release's article budget goes to the checks above first
            if !front.is_empty() {
                let before = info.layers.clone();
                read_more_front(ctx, file, total, &mut front, found_nfo_password.as_deref(), &mut info)?;
                // more of the front can show a bigger file than the one hashed
                if info.layers != before {
                    info.moviehash = moviehash_of(ctx, file, &files, &front, &info, known_last.clone())?;
                }
                read_tail_moov(ctx, file, &front, &mut info)?;
                read_tail_tags(ctx, file, &front, known_last.as_ref(), &mut info)?;
            }
        }

        let empty = info.yenc.is_none() && info.nfo.is_none() && info.sfv.is_empty() && info.health.is_empty();
        if empty {
            return Ok(Outcome { writes, ..Outcome::default() });
        }
        // a look that finds no cover drops one an earlier look kept
        let cover = info.media.as_mut().and_then(|m| m.cover.take());
        let saved_cover = cover.and_then(|c| super::keep_cover(&c.bytes).map(|sha256| (c.mime.to_string(), sha256)));
        writes.push(match &saved_cover {
            Some((mime, sha256)) => {
                StepWrite::new(SET_COVER, vec![release_id.into(), mime.clone().into(), sha256.clone().into()])
            }
            None => StepWrite::new(DROP_COVER, vec![release_id.into()]),
        });
        let crc = info.file_crc32.map(i64::from);
        let agent = agent_of(&info.headers).map(str::to_string);
        let hash = info.moviehash.clone();
        let json = serde_json::to_string(&info).map_err(|e| StepError::Permanent(e.to_string()))?;
        crate::raw_store::put_probe(&crate::raw_store::ProbeRecord {
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
        });
        writes.push(StepWrite::new(
            SET_PROBE,
            vec![release_id.into(), crc.into(), agent.into(), json.into(), hash.clone().into()],
        ));
        // a hash found on a later look is asked about then too
        let needs_list_decrypt = found_nfo_password.is_some()
            && (existing_db_pw != found_nfo_password
                || info.archive.as_ref().is_some_and(|a| a.name.is_some()))
            && ctx
                .conn()
                .query_row(
                    "select 1 from archive_release ar
                     where ar.release_id = ?1
                       and (
                           (ar.names_hidden = 1
                            and not exists (
                                select 1 from archive_inner_files f
                                where f.release_id = ar.release_id and f.source in (1, 2, 3)
                            ))
                           or (ar.password = 1
                               and exists (
                                   select 1 from archive_inner_files f
                                   where f.release_id = ar.release_id and f.source in (1, 2, 3) and lower(f.name) like '%.nzb'
                               )
                               and not exists (
                                   select 1 from archive_inner_files f
                                   where f.release_id = ar.release_id and f.source = 5
                               ))
                       )",
                    [release_id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
        let rerun = mask(crate::keys::KEYS_STEP)
            | if hash.is_some() { mask(super::opensubtitles::OPENSUBTITLES_STEP) } else { 0 }
            | if needs_list_decrypt { mask(archive::steps::LIST_STEP) } else { 0 };
        let facts = hash.map(|h| vec![(MOVIEHASH_FACT, h)]).unwrap_or_default();
        Ok(Outcome { matched: true, writes, facts, rerun, ..Outcome::default() })
    }
}

/// Articles read from the start of a file, at most, when its header runs past the first one: Matroska puts
/// attachments and chapters before the video, and a few MB of fonts is common.
const FRONT_ARTICLES: i64 = 6;

/// A later step's way to the hash this run found, before it's written.
pub const MOVIEHASH_FACT: &str = "moviehash";

/// More of the file's start, an article at a time, until the media header is all there.
fn read_more_front(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    total: i64,
    front: &mut Vec<u8>,
    password: Option<&str>,
    info: &mut ProbeInfo,
) -> Result<(), StepError> {
    let mut part = 2;
    while info.media.as_ref().is_some_and(|m| !m.header_complete) && part <= total.min(FRONT_ARTICLES) {
        let Some(body) = read_part(ctx, file, part, front.len() as u64)? else { break };
        front.extend(body);
        inspect(front, password, info);
        part += 1;
    }
    // the encoder's settings are in the first frame, just past the header: one more article when they're not here yet
    let wants_encoder = info.media.as_ref().is_some_and(|m| {
        m.header_complete
            && !m.encoder_complete
            && m.video.iter().any(|v| {
                // Matroska's codec ids, and MP4's sample entry fourccs
                let c = v.codec.to_ascii_lowercase();
                ["avc", "hevc", "h264", "hvc1", "hev1"].iter().any(|k| c.contains(k))
            })
    });
    if wants_encoder
        && part <= total.min(FRONT_ARTICLES)
        && let Some(body) = read_part(ctx, file, part, front.len() as u64)?
    {
        front.extend(body);
        inspect(front, password, info);
    }
    Ok(())
}

/// The most of an MP4's `moov` read from the end of the file: a feature film's sample tables are a few MB.
const MAX_TAIL_MOOV: u64 = 8 << 20;

/// An MP4 posted whole whose `moov` comes after its `mdat` (not made for streaming): the front has no tracks, soo
/// the parts holding the `moov` are read, from where the `mdat` ends to the end of the file.
fn read_tail_moov(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    front: &[u8],
    info: &mut ProbeInfo,
) -> Result<(), StepError> {
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
/// read, one more when they run into it. The last part, read already, isn't fetched again.
fn read_tail_tags(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    front: &[u8],
    known_last: Option<&(i64, Vec<u8>)>,
    info: &mut ProbeInfo,
) -> Result<(), StepError> {
    let wants = info.media.as_ref().is_some_and(|m| {
        matches!(m.container.as_str(), "matroska" | "webm") && m.header_complete && !m.track_uids.is_empty()
    });
    // a bare file only, as for the moov: in an archive the Tags' place in the post would depend on the volumes
    if !wants || info.archive.is_some() || !info.layers.is_empty() {
        return Ok(());
    }
    let segment = info.segment_size.filter(|s| *s > 0);
    let (Some(segment), Some(size)) = (segment, info.yenc.as_ref().and_then(|y| y.file_size)) else { return Ok(()) };
    let Some(at) = media::matroska_tags_past(front).into_iter().find(|at| *at < size) else { return Ok(()) };
    let whole = |b: &[u8]| media::element_len(b).and_then(|n| usize::try_from(n).ok()).is_some_and(|n| n <= b.len());
    // past the first Cluster but inside what was read
    let read = usize::try_from(at).ok().and_then(|at| front.get(at..)).filter(|b| whole(b));
    let mut tail = Vec::new();
    let tags = match read {
        Some(read) => read,
        None => {
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
            let Some(tags) = tail.get(skip..) else { return Ok(()) };
            tags
        }
    };
    if let Some(m) = info.media.as_mut() {
        media::tail_tags(tags, m);
    }
    Ok(())
}

/// One part of a file, decoded, when it holds the file's bytes from `offset` (0 based) on.
pub(super) fn read_part(
    ctx: &mut StepCtx<'_>,
    file: &FileInfo,
    part: i64,
    offset: u64,
) -> Result<Option<Vec<u8>>, StepError> {
    let segs = ctx.segments(file.id, &SegmentPick::Parts(vec![part])).map_err(|e| StepError::Retry(e.to_string()))?;
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
mod tests {
    use super::*;

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
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, 0)).unwrap();
        for (id, info) in [
            // stored before from_rpu was kept
            (1i64, r#"{"media":{"video":[{"dv_profile":7,"dolby_vision":{"profile":7,"fel":true}}]}}"#),
            (2, r#"{"media":{"video":[{"dv_profile":8,"dolby_vision":{"profile":8,"from_rpu":false}}]}}"#),
            (3, r#"{"media":{"video":[{"dv_profile":7,"dolby_vision":{"profile":7,"from_rpu":true}}]}}"#),
            (4, r#"{"media":{"video":[{"hdr":"HDR10"}]}}"#),
            (5, r#"{"media":null}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(PROBE_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        // 1 has no from_rpu; 2 and 3 were probed since, whatever they found
        assert_eq!(rerun_dv_without_rpu(&conn, 100).unwrap(), 1);
        assert!(stale_saved(r#"{"media":{"video":[{"dv_profile":7,"dolby_vision":{"profile":7,"fel":true}}]}}"#));
        assert!(stale_saved(r#"{"media":{"video":[{"dv_profile":7}]}}"#));
        assert!(!stale_saved(
            r#"{"media":{"video":[{"dv_profile":8,"dolby_vision":{"from_rpu":false},"keyframe_key":null}]}}"#
        ));
        assert!(!stale_saved(r#"{"media":{"video":[{"hdr":"HDR10"}]}}"#));
    }

    #[test]
    fn dolby_vision_probes_from_before_the_audio_key_are_asked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, 0)).unwrap();
        for (id, info) in [
            (1i64, r#"{"media":{"video":[{"dv_profile":8}],"audio":[{"codec":"A_EAC3"}]}}"#),
            (2, r#"{"media":{"video":[{"dv_profile":8}],"audio":[{"codec":"A_EAC3","stream_key":null}]}}"#),
            (3, r#"{"media":{"video":[{"dv_profile":8}],"audio":[{"codec":"A_TRUEHD"}]}}"#),
            (4, r#"{"media":{"video":[{"codec":"V_MPEGH/ISO/HEVC"}],"audio":[{"codec":"A_AC3"}]}}"#),
            (5, r#"{"media":{"video":[{"dv_profile":5}],"audio":[{"codec":"fLaC"}]}}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(PROBE_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        assert_eq!(rerun_dv_without_audio_key(&conn, 100).unwrap(), 2, "1 and 5");
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
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, 0)).unwrap();
        for (id, info) in [
            (1i64, r#"{"media":{"video":[{"dv_profile":8}]}}"#),
            (2, r#"{"media":{"video":[{"dv_profile":8,"keyframe_key":null}]}}"#),
            (3, r#"{"media":{"video":[{"dv_profile":7,"keyframe_key":"0123456789abcdef"}]}}"#),
            (4, r#"{"media":{"video":[{"codec":"V_MPEGH/ISO/HEVC"}]}}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(PROBE_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        assert_eq!(rerun_dv_without_keyframe_key(&conn, 100).unwrap(), 1);
        let saved =
            |v: &str| format!(r#"{{"media":{{"video":[{{"dv_profile":8,"dolby_vision":{{"from_rpu":true}}{v}}}]}}}}"#);
        assert!(stale_saved(&saved("")), "a restored probe from before the key is probed again");
        assert!(!stale_saved(&saved(r#","keyframe_key":null"#)));
        assert!(!stale_saved(&saved(r#","keyframe_key":"0123456789abcdef""#)));
    }

    #[test]
    fn probes_that_couldnt_read_the_first_article_are_asked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, 0)).unwrap();
        for (id, info) in [
            (1i64, r#"{"health":{"first":true},"yenc":null}"#),
            (2, r#"{"health":{"first":true},"yenc":{"part":1}}"#),
            (3, r#"{"health":{"first":false},"yenc":null}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(PROBE_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        assert_eq!(rerun_big_heads(&conn, 100).unwrap(), 1);
        for (id, info) in [
            (4i64, r#"{"file":"Show.S01E01.XviD.avi","media":null}"#),
            (5, r#"{"file":"Movie.part01.rar","archive":{"name":"Movie.CD1.avi"},"media":null}"#),
            (6, r#"{"file":"Movie.avi.001","media":null}"#),
            (7, r#"{"file":"Movie.avi","media":{"container":"avi"}}"#),
            (8, r#"{"file":"Movie.mkv","media":null}"#),
        ] {
            conn.execute("insert into releases (id, name, group_name) values (?, ?, 'g')", rusqlite::params![id, id])
                .unwrap();
            conn.execute(
                "insert into pp_state (release_id, done) values (?, ?)",
                rusqlite::params![id, mask(PROBE_STEP)],
            )
            .unwrap();
            conn.execute("insert into release_probe (release_id, info) values (?, ?)", rusqlite::params![id, info])
                .unwrap();
        }
        assert_eq!(rerun_avi(&conn, 100).unwrap(), 3, "a bare one, one in an archive, a split one");
        conn.execute("insert into releases (id, name, group_name) values (9, '9', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (9, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info) values (9, '{\"file\":\"x.7z.002\",\"media\":null}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_later_volumes(&conn, 100).unwrap(), 1, "the 7z's second volume, not the mkv");
        conn.execute("insert into releases (id, name, group_name) values (10, '10', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (10, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute("insert into release_probe (release_id, info) values (10, '{\"file\":\"Clip.wmv\"}')", [])
            .unwrap();
        assert_eq!(rerun_asf(&conn, 100).unwrap(), 1);
        conn.execute("insert into releases (id, name, group_name) values (12, '12', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (12, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info) values (12, '{\"media\":{\"container\":\"mp4\"}}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_mp4_tail(&conn, 100).unwrap(), 1);
        conn.execute("insert into releases (id, name, group_name) values (11, '11', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (11, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info) values (11, '{\"archive\":{\"name\":\"VTS_01_1.VOB\"}}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_mpeg_ps(&conn, 100).unwrap(), 1);
        conn.execute("insert into releases (id, name, group_name) values (13, '13', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (13, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info) values (13, '{\"archive\":{\"name\":\"VIDEO_TS.BUP\"}}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_dvd(&conn, 100).unwrap(), 1);
        conn.execute("insert into releases (id, name, group_name) values (15, '15', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (15, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info)
             values (15, '{\"archive\":{\"name\":\"VIDEO_TS/X.VOB\"},\"media\":{\"container\":\"mpeg-ps\"}}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_dvd(&conn, 100).unwrap(), 2, "the folder read as a VOB too, and the earlier one again");
        conn.execute("insert into releases (id, name, group_name) values (14, '14', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (14, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute(
            "insert into release_probe (release_id, info) values (14, '{\"archive\":{\"name\":\"BDMV/index.bdmv\"}}')",
            [],
        )
        .unwrap();
        assert_eq!(rerun_bluray(&conn, 100).unwrap(), 1, "the BDMV folder");
                conn.execute("insert into releases (id, name, group_name) values (16, '16', 'g')", []).unwrap();
        conn.execute("insert into pp_state (release_id, done) values (16, ?)", [mask(PROBE_STEP)]).unwrap();
        conn.execute("insert into release_probe (release_id, info) values (16, '{\"file\":\"Match.ts\"}')", [])
            .unwrap();
        assert_eq!(rerun_mpeg_ts(&conn, 100).unwrap(), 1);
        let state = |id: i64| -> (i64, i64) {
            conn.query_row("select done, rerun_mask from pp_state where release_id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        assert_eq!(state(1), (0, mask(PROBE_STEP)), "too big: probed again, past the saved probe");
        assert_eq!(state(2), (mask(PROBE_STEP), 0), "read");
        assert_eq!(state(3), (mask(PROBE_STEP), 0), "missing");
        let queued: i64 = conn.query_row("select count(*) from pp_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(queued, 12);
        assert_eq!(state(7), (mask(PROBE_STEP), 0), "read already");
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
        assert!(!stale_saved(r#"{"file":"x.7z.002","media":{"container":"matroska"}}"#));
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
