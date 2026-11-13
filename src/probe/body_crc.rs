//! The server's body CRC for probes made before the probe kept it: one HEAD of the probed article, for the movie
//! and TV releases a one-off seed names (`seed`), and for a video probe put back from `atlas.raw.db` without it
//! (`wants_backfill`). A probe since keeps `X-Received-Body-CRC` itself, soo this step does nothing for a release
//! nobody asked it about.
//!
//! The header is stamped by the server an article was posted through (Highwinds-family servers and eweka), soo most
//! articles have none: a HEAD that finds none still marks the step done, and the probe is left as it was.

use crate::postprocess::step::{BodyRequest, Got, Needs, Outcome, Step, StepCtx, StepError, StepId, StepMutation, StepWrite, mask};
use crate::store;

use super::{BODY_CRC_HEADER, ProbeInfo};

/// Not 3: that id was the archive NFO step's, and a retired id is never given out again.
pub const BODY_CRC_STEP: StepId = StepId(10);

/// The headers a HEAD is read for, as `keep_headers` keeps them.
const WANTED: [&str; 2] = [BODY_CRC_HEADER, "X-Received-Bytes"];

/// A HEAD is a few KiB; anything past this is not one worth reading.
const HEAD_CAP: usize = 64 * 1024;

/// The probes this step is for: a movie or TV release whose probe read an article but kept no body CRC.
const WANTING: &str = "select p.release_id from release_probe p join release_category c on c.release_id = p.release_id
     where (c.category between 2000 and 2999 or c.category between 5000 and 5999)
       and json_extract(p.info, '$.file') is not null
       and json_extract(p.info, '$.headers.\"X-Received-Body-CRC\"') is null";

#[derive(Default)]
pub struct BodyCrcStep;

/// Asks the step about every probe `WANTING` names, once per shard. The step is marked seen in `pp_steps_seen` by
/// the caller, soo no walk over every release starts for it. Returns how many.
pub(crate) fn seed(conn: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = conn.prepare(WANTING)?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        crate::postprocess::rerun_on(conn, *id, mask(BODY_CRC_STEP), now)?;
    }
    Ok(ids.len())
}

/// Whether a probe put back from the raw cache should ask this step: it read a video and kept no body CRC.
pub(crate) fn wants_backfill(info: &str) -> bool {
    serde_json::from_str::<ProbeInfo>(info).is_ok_and(|p| {
        p.file.is_some()
            && p.media.as_ref().is_some_and(|m| !m.video.is_empty())
            && !p.headers.keys().any(|k| k.eq_ignore_ascii_case(BODY_CRC_HEADER))
    })
}

/// The wanted headers out of a HEAD reply, under `WANTED`'s spelling. Folded lines join their header.
pub fn wanted_headers(head: &[u8]) -> Vec<(&'static str, String)> {
    let text = String::from_utf8_lossy(head);
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(at) = current
                && let Some((_, value)) = out.get_mut(at)
            {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        current = None;
        let Some((name, value)) = line.split_once(':') else { continue };
        if let Some(wanted) = WANTED.iter().find(|w| w.eq_ignore_ascii_case(name.trim()))
            && !out.iter().any(|(n, _)| n == wanted)
        {
            out.push((wanted, value.trim().chars().take(512).collect()));
            current = Some(out.len() - 1);
        }
    }
    out
}

/// The message-id of the article the probe read its yEnc lines from: its file's part `yenc.part`, or its first
/// article when that is part 1 or says none (as `archive::steps::first_article` takes it).
fn probed_article(ctx: &StepCtx<'_>, probe: &ProbeInfo) -> Result<Option<(String, String)>, StepError> {
    let Some(name) = probe.file.as_deref() else { return Ok(None) };
    let part = probe.yenc.as_ref().and_then(|y| y.part).map_or(1, i64::from);
    let files = ctx.files().map_err(StepError::from_inventory)?;
    let Some(file) = files.iter().find(|f| f.filename == name) else { return Ok(None) };
    let segment = if part <= 1 {
        crate::archive::steps::first_article(ctx, file)?
    } else {
        ctx.segments(file.id, &store::SegmentPick::Parts(vec![part])).map_err(StepError::from_inventory)?.into_iter().next()
    };
    Ok(segment.map(|s| (s.message_id, file.filename.clone())))
}

impl Step for BodyCrcStep {
    fn id(&self) -> StepId {
        BODY_CRC_STEP
    }
    fn name(&self) -> &'static str {
        "probe_body_crc"
    }
    /// after the probe and naming, before the keys read what it found
    fn rank(&self) -> u16 {
        220
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let release_id = ctx.release().id;
        if !ctx.data().explicit_rerun(release_id, BODY_CRC_STEP)? {
            return Ok(Outcome::default());
        }
        let info = ctx.data().probe_info(release_id)?;
        let Some(mut probe) = info.and_then(|i| serde_json::from_str::<ProbeInfo>(&i).ok()) else {
            return Ok(Outcome::default());
        };
        if probe.headers.keys().any(|k| k.eq_ignore_ascii_case(BODY_CRC_HEADER)) {
            return Ok(Outcome::default());
        }
        let Some((message_id, file)) = probed_article(ctx, &probe)? else { return Ok(Outcome::default()) };
        let got = ctx.fetch_head(&BodyRequest { message_id, file, max_bytes: Some(HEAD_CAP) })?.got;
        let head = match got {
            Got::Body(head) => head,
            // gone from every server, or too big to be a HEAD: nothing more to learn
            Got::Missing | Got::TooBig => return Ok(Outcome::default()),
            Got::Refused(why) => return Err(StepError::Retry(why.to_string())),
        };
        let found = wanted_headers(&head);
        if !found.iter().any(|(n, _)| *n == BODY_CRC_HEADER) {
            return Ok(Outcome::default());
        }
        for (name, value) in found {
            probe.headers.insert(name.to_string(), value);
        }
        let json = serde_json::to_string(&probe).map_err(|e| StepError::Permanent(e.to_string()))?;
        Ok(Outcome {
            matched: true,
            writes: vec![StepWrite::for_mutation(release_id, StepMutation::SetProbeInfo { json })],
            // the first-article key comes from what the probe holds
            rerun: mask(crate::keys::KEYS_STEP),
            ..Outcome::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_gives_up_its_body_crc() {
        let head = b"Path: news.example!not-for-mail\nFrom: poster <p@x>\nx-received-body-crc: 3735928559\n\
                     X-Received-Bytes: 740312\n  more\nSubject: a\n";
        let found = wanted_headers(head);
        assert_eq!(
            found,
            vec![(BODY_CRC_HEADER, "3735928559".to_string()), ("X-Received-Bytes", "740312 more".to_string())]
        );
        assert!(wanted_headers(b"Path: x\nSubject: y\n").is_empty());
        assert_eq!(wanted_headers(b"X-Received-Body-CRC: 1\nX-Received-Body-CRC: 2\n").len(), 1, "the first one");
    }

    #[test]
    fn a_restored_video_probe_without_the_header_wants_it() {
        let info = |video: bool, crc: bool| {
            let mut p = ProbeInfo { file: Some("a.mkv".into()), ..ProbeInfo::default() };
            if video {
                let mut m = super::super::media::MediaInfo::default();
                m.video.push(super::super::media::VideoTrack::default());
                p.media = Some(m);
            }
            if crc {
                p.headers.insert(BODY_CRC_HEADER.into(), "1".into());
            }
            serde_json::to_string(&p).unwrap()
        };
        assert!(wants_backfill(&info(true, false)));
        assert!(!wants_backfill(&info(true, true)), "it has it");
        assert!(!wants_backfill(&info(false, false)), "no video");
        assert!(!wants_backfill("not json"));
    }

    #[test]
    fn the_seed_names_movie_and_tv_probes_without_the_header() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(super::super::PROBE.create).unwrap();
        conn.execute_batch(crate::category::RELEASE_CATEGORY.create).unwrap();
        let probe = |file: Option<&str>, crc: bool| {
            let mut p = ProbeInfo { file: file.map(str::to_string), ..ProbeInfo::default() };
            if crc {
                p.headers.insert(BODY_CRC_HEADER.into(), "1".into());
            }
            serde_json::to_string(&p).unwrap()
        };
        // 1 wants it; 2 has it; 3 is music; 4 read no article; 5 is TV
        for (id, cat, file, crc) in [
            (1, 2040, Some("a.mkv"), false),
            (2, 2040, Some("a.mkv"), true),
            (3, 3010, Some("a.flac"), false),
            (4, 2040, None, false),
            (5, 5040, Some("b.mkv"), false),
        ] {
            conn.execute(
                "insert into release_probe (release_id, info) values (?1, ?2)",
                rusqlite::params![id, probe(file, crc)],
            )
            .unwrap();
            conn.execute("insert into release_category values (?1, ?2)", rusqlite::params![id, cat]).unwrap();
        }
        let ids: Vec<i64> =
            conn.prepare(WANTING).unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(ids, vec![1, 5]);
    }
}
