//! The step that reads a release's pictures, and the table and folder they're kept in.
//!
//! A release's pictures are read whole as it's processed while they're few and small: at most
//! `archive.pictures_auto_max` of them, together at most `archive.pictures_auto_max_mb`, each a few articles. Past
//! either, only its thumbnail or cover is, and the release page offers each of the rest: one asked for there is kept in
//! `picture_requests` and read at the release's next visit, which the request moves to the front of the net lane. Its bytes go to the
//! `images` folder under their SHA-256, soo the same picture posted in many groups is kept once, and its headers go to
//! `release_images` beside the release. A JPEG's EXIF and XMP ids are kept as columns of their own: the same id on two
//! releases says they carry one picture, whatever it was called and wherever it was posted.
//!
//! A post of nothing but pictures (a picture set, a lone cover) is outside the body lane's gate
//! (`postprocess::rules::body_gate`), soo this step reads the pictures that come with a release, never a picture post.

use sha2::Digest;

use crate::config::Config;
use crate::postprocess::step::{ImageRecord, Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepMutation, StepWrite};
use crate::catalog_facts::{FileInfo, SegmentPick};

use super::{is_image_name, jpeg, sniff};

pub const IMAGES_STEP: StepId = StepId(13);

/// Articles one picture read on its own may take: a cover is one or two.
const MAX_PARTS: usize = 8;
/// Articles one run may fetch for pictures, all told: a run that has more to read goes on in a later one.
const RUN_PARTS: usize = 64;
/// Pictures inside books and comics kept, at most.
const MAX_BOOK_IMAGES: usize = 6;
/// The net lane's rank a picture asked for on the page gives its release: above every rank `schema::NET_RANK` gives,
/// soo its turn comes in the lane's main order straight away.
pub const ASKED_RANK: i64 = 9;
/// A run that couldn't get to every picture asked for goes on this soon.
const ASKED_RESUME_SECS: i64 = 60;
/// Articles a book or comic may take to be read whole for the pictures inside it.
/// A whole read stops past 8 MiB, which is about 11 articles: past that the articles would be fetched for nothing.
const MAX_CONTAINER_PARTS: usize = 10;
/// Past this, the oldest pictures go from the folder.
const MAX_IMAGE_CACHE: u64 = 2 << 30;
const TRIM_EVERY: u64 = 64;

fn image_dir() -> std::path::PathBuf {
    crate::paths::app_dir().join("images")
}

fn image_file(sha256: &str) -> Option<std::path::PathBuf> {
    (sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())).then(|| image_dir().join(sha256))
}

fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Keeps a picture, named by its hash.
fn keep(bytes: &[u8]) -> Option<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let sha256 = sha256_hex(bytes);
    let file = image_file(&sha256)?;
    if !file.exists() {
        static WRITTEN: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(image_dir()).ok()?;
        crate::atomic::write_atomic(&file, bytes).ok()?;
        if WRITTEN.fetch_add(1, Ordering::Relaxed).is_multiple_of(TRIM_EVERY) {
            crate::media::details::trim_posters(&image_dir(), MAX_IMAGE_CACHE);
        }
    }
    Some(sha256)
}

/// One kept picture of a release.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub position: i64,
    pub file: String,
    pub sha256: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub bytes: i64,
    pub jpeg: Option<jpeg::JpegInfo>,
}

/// How another release is tied to one of this one's pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tie {
    /// the same bytes
    Same,
    /// the same EXIF ImageUniqueID
    ExifId,
    /// the same XMP document id: the same picture, edited or saved again
    DocumentId,
}

impl Tie {
    pub fn label(self) -> &'static str {
        match self {
            Tie::Same => "the same file",
            Tie::ExifId => "the same EXIF image id",
            Tie::DocumentId => "the same XMP document id",
        }
    }
}

/// Whether a picture's name says it's the one that stands for the release: a thumbnail, cover or poster.
fn stands_for_release(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["thumb", "cover", "poster", "folder", "front"].iter().any(|w| lower.contains(w))
}

fn parts_of(f: &FileInfo) -> i64 {
    f.expected.unwrap_or(0).max(f.parts_seen)
}

/// A release's pictures, best first: a thumbnail, cover or poster before the rest, then by name. Each name once: a
/// post cut across groups can have it in two rows.
pub fn pictures(files: &[FileInfo]) -> Vec<&FileInfo> {
    let mut seen = std::collections::HashSet::new();
    let mut images: Vec<&FileInfo> = files
        .iter()
        .filter(|f| is_image_name(&f.filename) && parts_of(f) >= 1)
        .filter(|f| seen.insert(f.filename.to_ascii_lowercase()))
        .collect();
    images.sort_by(|a, b| {
        (!stands_for_release(&a.filename))
            .cmp(&!stands_for_release(&b.filename))
            .then_with(|| a.filename.cmp(&b.filename))
    });
    images
}

/// The limits on what's read without being asked (`archive.pictures_auto_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoLimits {
    pub count: usize,
    pub bytes: i64,
}

impl AutoLimits {
    pub fn of(cfg: &crate::config::ArchiveConfig) -> AutoLimits {
        AutoLimits { count: cfg.pictures_auto_max.max(0) as usize, bytes: cfg.pictures_auto_max_mb.max(0) << 20 }
    }
}

/// The pictures read without being asked, from `pictures` (best first) and each one's posted bytes: all of them while
/// they're within `limits` and each a few articles, else the first when it stands for the release.
pub fn auto_picked<'a>(
    pictures: &[&'a FileInfo],
    bytes: impl Fn(&FileInfo) -> i64,
    limits: AutoLimits,
) -> Vec<&'a FileInfo> {
    let small = |f: &FileInfo| (1..=MAX_PARTS as i64).contains(&parts_of(f));
    if pictures.len() <= limits.count && pictures.iter().all(|f| small(f)) {
        let total: i64 = pictures.iter().map(|f| bytes(f)).sum();
        if total <= limits.bytes {
            return pictures.to_vec();
        }
    }
    pictures.first().filter(|f| small(f) && stands_for_release(&f.filename)).copied().into_iter().collect()
}

/// How a picture asked for came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Shown,
    NotAPicture,
    Unreadable,
    TooBig,
    Gone,
}

impl Answer {
    pub fn word(self) -> &'static str {
        match self {
            Answer::Shown => "shown",
            Answer::NotAPicture => "not a picture",
            Answer::Unreadable => "unreadable",
            Answer::TooBig => "too big",
            Answer::Gone => "gone",
        }
    }
}

/// A picture asked for on the release page, and where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub release_id: i64,
    pub file: String,
    pub requested_at: i64,
    pub done_at: Option<i64>,
    pub outcome: Option<String>,
    /// unanswered, but the pictures step isn't owed on its release all the same: the release was passed over (turned
    /// down for reading, not complete enough yet, of no size, or the step is off), and no run is on its way to answer it
    pub passed_over: bool,
}

/// The pictures a release's run reads: the ones read without asking, then the ones asked for (`asked`, by name) up to
/// `max_parts` articles each.
fn wanted<'a>(
    pictures: &[&'a FileInfo],
    auto: &[&'a FileInfo],
    asked: &[String],
    max_parts: i64,
) -> Vec<(&'a FileInfo, bool)> {
    let mut out: Vec<(&FileInfo, bool)> = auto.iter().map(|f| (*f, asked.contains(&f.filename))).collect();
    for f in pictures {
        if asked.contains(&f.filename) && !auto.iter().any(|a| a.filename == f.filename) && parts_of(f) <= max_parts {
            out.push((*f, true));
        }
    }
    // what was asked for first: a picture read without asking that never comes to anything is fetched again on every
    // run, and mustn't use up the articles a picture asked for needs
    out.sort_by_key(|(_, asked)| !asked);
    out
}

/// A file's posted bytes, from its articles' sizes: an upper bound on what it decodes to.
fn posted_bytes(ctx: &StepCtx<'_>, file: &FileInfo) -> anyhow::Result<i64> {
    let parts = parts_of(file);
    if !(1..=MAX_PARTS as i64).contains(&parts) {
        return Ok(i64::MAX / 4);
    }
    let mut by_part: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    for seg in ctx.segments(file.id, &SegmentPick::Parts((1..=parts).collect()))? {
        by_part.entry(seg.part.unwrap_or(1)).or_insert(seg.bytes);
    }
    Ok(by_part.values().sum::<i64>().max(1))
}

/// Books and comics small enough to read whole for the pictures inside them, at most two.
pub fn containers(files: &[FileInfo]) -> Vec<&FileInfo> {
    files
        .iter()
        .filter(|f| super::container::is_container_name(&f.filename))
        .filter(|f| (1..=MAX_CONTAINER_PARTS as i64).contains(&f.expected.unwrap_or(0).max(f.parts_seen)))
        .take(2)
        .collect()
}

/// Document ids that say nothing of one picture: empty uuids and the default some editors write on every file.
const SHARED_DOCUMENT_IDS: [&str; 2] = ["faf5bdd5-ba3d-11da-ad31-d33d75182f1b", "00000000-0000-0000-0000-000000000000"];

/// An ImageUniqueID that names one picture: 16 hex digits or more, not all one digit. Some cameras write their model
/// or firmware there, the same on every shot.
fn is_telling_image_id(id: &str) -> bool {
    let hex: Vec<char> = id.chars().filter(|c| !c.is_whitespace()).collect();
    hex.len() >= 16 && hex.iter().all(char::is_ascii_hexdigit) && hex.iter().any(|c| !c.eq_ignore_ascii_case(&hex[0]))
}

fn is_telling_document_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    let bare = lower.rsplit(':').next().unwrap_or(&lower);
    bare.chars().filter(char::is_ascii_alphanumeric).count() >= 16
        && bare.chars().any(|c| c.is_ascii_alphanumeric() && c != '0')
        && !SHARED_DOCUMENT_IDS.contains(&bare)
}

fn image_record(file: &str, bytes: &[u8], sha256: &str) -> Option<ImageRecord> {
    // a JPEG's headers read once: its size comes from them too
    let (sniffed, info) = if jpeg::is_jpeg(bytes) {
        let info = jpeg::read(bytes)?;
        (super::Sniffed { format: "jpeg", width: u32::from(info.width), height: u32::from(info.height) }, Some(info))
    } else {
        (sniff(bytes)?, None)
    };
    let meta = info.as_ref().and_then(|i| serde_json::to_string(i).ok());
    let exif_id = info.as_ref().and_then(|i| i.exif.unique_id.clone()).filter(|id| is_telling_image_id(id));
    // the original's id names the picture through every save; an editor that writes only its own takes that. One
    // stamped on every file a tool makes ties nothing
    let document_id = info
        .as_ref()
        .and_then(|i| i.xmp.original_document_id.clone().or_else(|| i.xmp.document_id.clone()))
        .filter(|id| is_telling_document_id(id));
    Some(ImageRecord { file: file.to_string(),sha256: sha256.to_string(),format: sniffed.format.to_string(),
        width: sniffed.width,height: sniffed.height,bytes: bytes.len() as i64,meta,exif_id,document_id })
}

fn image_error(error:anyhow::Error)->StepError {
    if error.is::<crate::pg::worker_assets::ImageChanged>() || error.downcast_ref::<std::io::Error>().is_some() {
        StepError::Transient(error.to_string())
    }else{StepError::from_cache(error)}
}

#[derive(Default)]
pub struct ImagesStep {
    limits: std::sync::Mutex<crate::config::ArchiveConfig>,
}

impl Step for ImagesStep {
    fn id(&self) -> StepId {
        IMAGES_STEP
    }
    fn name(&self) -> &'static str {
        "images"
    }
    fn rank(&self) -> u16 {
        240
    }
    fn needs(&self) -> Needs {
        Needs::Bodies
    }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.limits.lock().unwrap() = cfg.archive.clone();
        cfg.archive.enabled
    }
    fn applies(&self, r: &crate::postprocess::step::ReleaseInfo) -> bool {
        r.size > 0
    }
    // a picture can come after the rest of the post; one linked into it later asks for a run itself (`archive::link`)
    fn reruns(&self) -> Reruns {
        Reruns { on_rename: false, on_growth: true }
    }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        // pictures are what this step reads: the fetcher refuses them to every other
        ctx.allow_pictures();
        let limits = self.limits.lock().unwrap().clone();
        let mut files = ctx.files().map_err(StepError::from_inventory)?;
        // the small files found beside this release, as a thumbnail posted under a subject of its own, and the rows
        // of an upload it leads (`archive::link`)
        files.extend(ctx.data().linked_image_files(ctx.release().id)?);
        let release_id = ctx.release().id;
        // what's still owed an answer, and what was shown (kept again from the folder); one that came to nothing waits
        // for "Try again" on the page, which clears its answer
        let asked = ctx.data().image_requests(release_id)?;
        // one asked for that the rows read above don't reach (a member past the first few, a file past a row's first
        // fifty): looked for in the upload's rows by its name
        for name in &asked {
            if !files.iter().any(|f| f.filename == *name) {
                files.extend(
                    ctx.data().requested_image_file(release_id, name)?,
                );
            }
        }
        let all = pictures(&files);
        let auto_limits = AutoLimits::of(&limits);
        // the sizes are read only when they could keep the pictures within the limits
        let sizes: std::collections::HashMap<i64, i64> = if all.len() <= auto_limits.count {
            let mut sizes = std::collections::HashMap::new();
            for f in &all {
                sizes.insert(f.id, posted_bytes(ctx, f).map_err(StepError::from_inventory)?);
            }
            sizes
        } else {
            std::collections::HashMap::new()
        };
        let auto = auto_picked(&all, |f| sizes.get(&f.id).copied().unwrap_or(i64::MAX / 4), auto_limits);
        let wanted = wanted(&all, &auto, &asked, limits.picture_max_parts);
        let books = containers(&files);
        let now = chrono::Utc::now().timestamp();
        let mut answers: Vec<StepWrite> = Vec::new();
        let answer = |file: &str, a: Answer, answers: &mut Vec<StepWrite>| {
            answers.push(StepWrite::for_mutation(release_id, StepMutation::AnswerImageRequest { file: file.to_string(),done_at: now,answer: a }));
        };
        // asked for, but no longer among its files, or past what one may take
        for name in &asked {
            match all.iter().find(|f| f.filename == *name) {
                None => answer(name, Answer::Gone, &mut answers),
                Some(f) if !wanted.iter().any(|(w, _)| w.filename == f.filename) => {
                    answer(name, Answer::TooBig, &mut answers)
                }
                Some(_) => {}
            }
        }
        if wanted.is_empty() && books.is_empty() {
            return Ok(Outcome { writes: answers, ..Outcome::default() });
        }
        let native=matches!(ctx.data(),crate::postprocess::data::StepData::Native(_));
        let max_bytes = (limits.picture_max_parts.max(MAX_PARTS as i64) as usize).saturating_mul(1 << 20);
        let mut image_frame=if native {
            let grant=crate::pg::worker_assets::current().map_err(image_error)?.ok_or_else(||StepError::Transient("native image namespace has not been selected".into()))?;
            Some(grant.image_frame(release_id,max_bytes as u64).map_err(image_error)?)
        }else{None};
        let before = ctx.data().kept_images(release_id)?;
        // room for the biggest picture one may ask for, whatever `picture_max_parts` says
        let mut left = RUN_PARTS.max(limits.picture_max_parts.max(1) as usize);
        let mut writes = vec![StepWrite::for_mutation(release_id, StepMutation::ClearImages)];
        let mut kept = 0i64;
        let mut row = |name: &str, body: &[u8], writes: &mut Vec<StepWrite>, frame:&mut Option<crate::pg::worker_assets::ImageFrame>| -> Result<bool,StepError> {
            // read before it's kept: a file named .jpg that isn't a picture never reaches the folder
            let sha256 = sha256_hex(body);
            let Some(image) = image_record(name, body, &sha256) else { return Ok(false) };
            if let Some(frame)=frame {
                if !frame.keep(body).map_err(image_error)? {return Err(StepError::Transient("native image cache could not reach its 2GiB target".into()));}
            }else if keep(body).is_none(){return Ok(false);}
            writes.push(StepWrite::for_mutation(release_id, StepMutation::KeepImage { position: kept,image }));
            kept += 1;
            Ok(true)
        };
        let mut unfinished = false;
        // out of time or budget, or a server's trouble, after some pictures came: those are kept and the rest wait
        // for the next run, which finds these in the folder, instead of all of them being fetched again for ever
        let mut stopped = false;
        // a picture read without asking may be as big as `MAX_PARTS` articles, whatever `picture_max_parts` says
        for (file, was_asked) in wanted {
            // read on an earlier run and still in the folder: not fetched again
            let cached=if let Some(frame)=image_frame.as_mut() {
                match before.get(&file.filename) {Some(sha)=>frame.cached(&file.filename,sha).map_err(image_error)?,None=>None}
            }else {before.get(&file.filename).and_then(|sha|image_file(sha)).and_then(|path|std::fs::read(path).ok())};
            let body = match cached {
                Some(body) => Some(body),
                None if stopped => {
                    unfinished = true;
                    continue;
                }
                None if parts_of(file) as usize > left => {
                    unfinished |= was_asked;
                    continue;
                }
                // a picture asked for on the page is bounded by `left`, not by `bodies_per_release`: one bigger than
                // that could never be read, as a run's fetches start from none
                None if was_asked => {
                    let used = ctx.used_bodies;
                    // as good as none used, whatever the read takes: `left` is what bounds it
                    ctx.used_bodies = i64::MIN / 2;
                    let body = crate::archive::steps::whole_file_within(ctx, file, &mut left, max_bytes);
                    ctx.used_bodies = used;
                    match body {
                        Err(StepError::OutOfBudget | StepError::Transient(_)) if writes.len() > 1 || !answers.is_empty() => {
                            (stopped, unfinished) = (true, true);
                            continue;
                        }
                        other => other?,
                    }
                }
                None => match crate::archive::steps::whole_file_within(ctx, file, &mut left, max_bytes) {
                    Err(StepError::OutOfBudget | StepError::Transient(_)) if writes.len() > 1 || !answers.is_empty() => {
                        (stopped, unfinished) = (true, true);
                        continue;
                    }
                    other => other?,
                },
            };
            let shown=match body.as_deref(){Some(body)=>row(&file.filename,body,&mut writes,&mut image_frame)?,None=>false};
            if was_asked {
                let a = match (&body, shown) {
                    (_, true) => Answer::Shown,
                    (Some(_), false) => Answer::NotAPicture,
                    (None, _) => Answer::Unreadable,
                };
                answer(&file.filename, a, &mut answers);
            }
        }
        // a book's own pictures, while there's room for them
        let mut book_images = 0;
        for file in books.into_iter().filter(|_| !stopped) {
            let room = MAX_BOOK_IMAGES.saturating_sub(book_images);
            let mut book_left = MAX_CONTAINER_PARTS;
            if room == 0 {
                break;
            }
            let Some(body) = crate::archive::steps::whole_file(ctx, file, &mut book_left)? else { continue };
            for inside in super::container::pictures(&body, room) {
                if row(&format!("{} › {}", file.filename, inside.name), &inside.bytes, &mut writes,&mut image_frame)? {
                    book_images += 1;
                }
            }
        }
        let wait_until = unfinished.then_some(now + ASKED_RESUME_SECS);
        let hold=image_frame.map(|frame|frame.finish().map(|frame|Box::new(frame) as Box<dyn std::any::Any+Send>)).transpose().map_err(image_error)?;
        if kept == 0 {return Ok(Outcome {writes:answers,wait_until,hold,..Outcome::default()});}
        writes.extend(answers);
        Ok(Outcome {matched:true,writes,wait_until,hold,..Outcome::default()})
    }
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

    fn file_id(name: &str, parts: i64, id: i64) -> FileInfo {
        FileInfo { id, ..file(name, parts) }
    }

    const LIMITS: AutoLimits = AutoLimits { count: 12, bytes: 16 << 20 };

    #[test]
    fn a_thumbnail_or_cover_comes_first_and_few_small_pictures_are_all_read() {
        let files = [
            file_id("Some.Film.2160p.mkv", 9_000, 1),
            file_id("b-shot.png", 1, 2),
            file_id("a-shot.jpg", 1, 3),
            file_id("00-thumbnail.jpg", 1, 4),
            file_id("a-shot.jpg", 1, 5),
        ];
        let all = pictures(&files);
        let names: Vec<&str> = all.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["00-thumbnail.jpg", "a-shot.jpg", "b-shot.png"], "each name once");
        let auto = auto_picked(&all, |_| 500_000, LIMITS);
        assert_eq!(auto.len(), 3);
    }

    // Port of the legacy asked_for fixture: native body-gate scheduling reads this exact owner interface.
    #[test]
    #[ignore = "explicit isolated PG; actual pending image request body-gate interface"]
    fn an_open_request_lets_the_step_past_the_body_gate_and_an_answered_one_doesnt() {
        use crate::postprocess::{
            data::StepData,
            rules::{BodyGate, RowFacts, Rules},
        };
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated PG required");
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let id = runtime
            .owner
            .with(|client| {
                crate::pg::release_writer::save(
                    client,
                    &[crate::parser::Release {
                        name: "Picture.Set".into(),
                        group: "example.pictures".into(),
                        size: 43,
                        articles: vec![crate::parser::Article {
                            filename: Some("a.gif".into()),
                            message_id: "<picture-gate@fixture.test>".into(),
                            part: Some(1),
                            total_parts: Some(1),
                            bytes: 43,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    -100_000,
                )?;
                Ok(client.query_one("select id from releases", &[])?.get(0))
            })
            .unwrap();
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        let data = StepData::Native(&reads);
        let snapshot = reads.snapshot(id).unwrap().unwrap();
        let row = RowFacts {
            group: &snapshot.info.group,
            name: &snapshot.info.name,
            size: snapshot.info.size,
            parts: snapshot.info.parts,
            complete: snapshot.info.complete,
            file_total: snapshot.info.file_total,
        };
        assert_eq!(
            crate::postprocess::rules::body_gate(&Rules::new(&Config::default().postprocess), &row, &snapshot.summary),
            BodyGate::Outside
        );
        assert!(!reads.pending_picture_request(id).unwrap());
        assert!(data.image_requests(id).unwrap().is_empty());
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "insert into picture_requests(release_id,file,requested_at) values($1,'a.gif',1)",
                    &[&id],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(reads.pending_picture_request(id).unwrap());
        assert_eq!(data.image_requests(id).unwrap(), ["a.gif"]);
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "update picture_requests set done_at=2,outcome='unreadable' where release_id=$1",
                    &[&id],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(!reads.pending_picture_request(id).unwrap(), "answered: no body-gate bypass remains");
        assert!(data.image_requests(id).unwrap().is_empty());
        runtime
            .owner
            .with(|client| {
                client.execute("update picture_requests set outcome='shown' where release_id=$1", &[&id])?;
                Ok(())
            })
            .unwrap();
        assert!(!reads.pending_picture_request(id).unwrap(), "shown pictures do not reopen the body gate");
        assert_eq!(
            data.image_requests(id).unwrap(),
            ["a.gif"],
            "a previously shown picture remains eligible for cache reuse"
        );
        runtime.owner.close();
        assert!(reads.pending_picture_request(id).is_err());
        assert!(matches!(data.image_requests(id), Err(StepError::Native(_))));
    }

    #[test]
    fn many_or_big_pictures_leave_all_but_the_thumbnail_to_be_asked_for() {
        let mut many: Vec<FileInfo> = (0..20).map(|i| file_id(&format!("{i:03}.jpg"), 1, i)).collect();
        let all = pictures(&many);
        assert!(auto_picked(&all, |_| 1, LIMITS).is_empty(), "no thumbnail among them");
        many.push(file_id("cover.jpg", 1, 99));
        let all = pictures(&many);
        let auto = auto_picked(&all, |_| 1, LIMITS);
        assert_eq!(auto.iter().map(|f| f.filename.as_str()).collect::<Vec<_>>(), ["cover.jpg"]);
        // few, but too many bytes together
        let few = [file_id("thumb.jpg", 1, 1), file_id("big1.jpg", 8, 2), file_id("big2.jpg", 8, 3)];
        let all = pictures(&few);
        let auto = auto_picked(&all, |_| 6 << 20, LIMITS);
        assert_eq!(auto.len(), 1);
        // one picture too many articles to read without asking
        let huge = [file_id("huge.jpg", 40, 1)];
        let all = pictures(&huge);
        assert!(auto_picked(&all, |_| 1, LIMITS).is_empty());
        let asked = vec!["huge.jpg".to_string()];
        assert_eq!(wanted(&all, &[], &asked, 64).len(), 1, "asked for, within the articles one may take");
        assert!(wanted(&all, &[], &asked, 32).is_empty());
    }

    #[test]
    fn image_ids_that_name_a_camera_tie_nothing() {
        assert!(is_telling_image_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_telling_image_id("E08LLKL01AM"));
        assert!(!is_telling_image_id("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"));
    }

    #[test]
    fn shared_document_ids_tie_nothing() {
        assert!(is_telling_document_id("xmp.did:8a1f3c2e-55b0-4c6d-9e71-0b2d4f6a8c13"));
        assert!(!is_telling_document_id("uuid:faf5bdd5-ba3d-11da-ad31-d33d75182f1b"));
        assert!(!is_telling_document_id("uuid:00000000-0000-0000-0000-000000000000"));
        assert!(!is_telling_document_id("xmp.did:1"));
    }

    #[test]
    fn a_jpeg_row_keeps_its_headers_and_ids() {
        let y = vec![200u8; 16 * 16];
        let c = vec![128u8; 64];
        let bytes = crate::video::jpeg::encode(&y, &c, &c, 16, 16, 90);
        // The old SQLite value-vector fixture now asserts the actual typed native result.
        let record = image_record("00-thumbnail.jpg", &bytes, "ab").unwrap();
        assert_eq!(record.format, "jpeg");
        assert_eq!((record.width, record.height), (16, 16));
        assert_eq!(record.file, "00-thumbnail.jpg");
        assert_eq!(record.sha256, "ab");
        assert_eq!(record.bytes, bytes.len() as i64);
        let info: jpeg::JpegInfo = serde_json::from_str(record.meta.as_deref().expect("meta")).unwrap();
        assert_eq!(info.quality, Some(90));
        assert_eq!(record.exif_id, None, "no EXIF id");
        assert!(image_record("x.jpg", b"not a picture", "ab").is_none());
    }
}

#[cfg(all(test,unix))]
#[path="native_image_step_tests.rs"]
mod native_tests;
