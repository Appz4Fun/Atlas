//! The step that reads a release's pictures, and the table and folder they're kept in.
//!
//! A picture is read whole when it's a few articles at most: a cover or a thumbnail is one. Its bytes go to the
//! `images` folder under their SHA-256, soo the same picture posted in many groups is kept once, and its headers go to
//! `release_images` beside the release. A JPEG's EXIF and XMP ids are kept as columns of their own: the same id on two
//! releases says they carry one picture, whatever it was called and wherever it was posted.
//!
//! A post of nothing but pictures (a picture set, a lone cover) is outside the body lane's gate
//! (`postprocess::rules::body_gate`), soo this step reads the pictures that come with a release, never a picture post.

use sha2::Digest;

use crate::config::Config;
use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::{Needs, Outcome, Reruns, Step, StepCtx, StepError, StepId, StepWrite};
use crate::store::{self, FileInfo};

use super::{is_image_name, jpeg, sniff};

pub const IMAGES_STEP: StepId = StepId(13);

/// Pictures read for one release at most, and articles one may take: a cover is one or two, and a set of a hundred
/// photos isn't read through.
pub const MAX_IMAGES: usize = 6;
const MAX_PARTS: usize = 8;
/// Articles a book or comic may take to be read whole for the pictures inside it.
/// A whole read stops past 8 MiB, which is about 11 articles: past that the articles would be fetched for nothing.
const MAX_CONTAINER_PARTS: usize = 10;
/// Past this, the oldest pictures go from the folder.
const MAX_IMAGE_CACHE: u64 = 2 << 30;
const TRIM_EVERY: u64 = 64;

pub const IMAGES: ShardTable = ShardTable {
    name: "release_images",
    create: "create table if not exists release_images (
        release_id  INTEGER NOT NULL,
        position    INTEGER NOT NULL,
        file        TEXT NOT NULL,
        sha256      TEXT NOT NULL,
        format      TEXT NOT NULL,
        width       INTEGER NOT NULL,
        height      INTEGER NOT NULL,
        bytes       INTEGER NOT NULL,
        meta        TEXT,
        exif_id     TEXT,
        document_id TEXT,
        PRIMARY KEY (release_id, position)
    )",
    indexes: &[
        (
            "release_images_sha256",
            "create index if not exists release_images_sha256 on release_images(sha256, release_id)",
        ),
        (
            "release_images_exif_id",
            "create index if not exists release_images_exif_id on release_images(exif_id, release_id) where exif_id is not null",
        ),
        (
            "release_images_document_id",
            "create index if not exists release_images_document_id on release_images(document_id, release_id)
                where document_id is not null",
        ),
    ],
    release_key: Some("release_id"),
    added: &[],
};

const DROP_IMAGES: &str = "delete from release_images where release_id = ?1";
const SET_IMAGE: &str = "insert or replace into release_images
    (release_id, position, file, sha256, format, width, height, bytes, meta, exif_id, document_id)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";

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

/// A release's pictures, in the order they were read.
pub fn images(conn: &rusqlite::Connection, release_id: i64) -> Vec<Image> {
    let s = store::shard_of_id(release_id);
    let sql = format!(
        "select position, file, sha256, format, width, height, bytes, meta from s{s}.release_images
         where release_id = ?1 order by position"
    );
    let Ok(mut q) = conn.prepare(&sql) else { return Vec::new() };
    q.query_map([release_id], |r| {
        let meta: Option<String> = r.get(7)?;
        Ok(Image {
            position: r.get(0)?,
            file: r.get(1)?,
            sha256: r.get(2)?,
            format: r.get(3)?,
            width: r.get(4)?,
            height: r.get(5)?,
            bytes: r.get(6)?,
            jpeg: meta.and_then(|m| serde_json::from_str(&m).ok()),
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

/// One picture's bytes and format, while the folder still has it.
pub fn image(conn: &rusqlite::Connection, release_id: i64, position: i64) -> Option<(Vec<u8>, String)> {
    let s = store::shard_of_id(release_id);
    let (sha256, format): (String, String) = conn
        .query_row(
            &format!("select sha256, format from s{s}.release_images where release_id = ?1 and position = ?2"),
            rusqlite::params![release_id, position],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    Some((std::fs::read(image_file(&sha256)?).ok()?, format))
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

/// Other releases, in every shard, that carry one of this release's pictures: (release id, its picture's position
/// here, how they're tied), the strongest tie first, at most `limit`.
pub fn same_pictures(conn: &rusqlite::Connection, release_id: i64, limit: usize) -> Vec<(i64, i64, Tie)> {
    let own = images_keys(conn, release_id);
    let mut found: Vec<(i64, i64, Tie)> = Vec::new();
    for (position, sha256, exif_id, document_id) in own {
        let keys = [
            (Tie::Same, "sha256", Some(sha256)),
            (Tie::ExifId, "exif_id", exif_id),
            (Tie::DocumentId, "document_id", document_id),
        ];
        for (tie, column, value) in keys {
            let Some(value) = value else { continue };
            for s in 0..store::SHARDS {
                let sql = format!(
                    "select distinct release_id from s{s}.release_images where {column} = ?1 and release_id != ?2
                     order by release_id desc limit ?3"
                );
                let Ok(mut q) = conn.prepare(&sql) else { continue };
                let ids: Vec<i64> = q
                    .query_map(rusqlite::params![value, release_id, limit as i64], |r| r.get(0))
                    .map(|rows| rows.filter_map(Result::ok).collect())
                    .unwrap_or_default();
                for id in ids {
                    match found.iter_mut().find(|(other, _, _)| *other == id) {
                        // found again by a stronger tie: shown by that one
                        Some(seen) if (tie as u8) < (seen.2 as u8) => *seen = (id, position, tie),
                        Some(_) => {}
                        None => found.push((id, position, tie)),
                    }
                }
            }
        }
    }
    // the strongest tie first, each kept in the order found
    found.sort_by_key(|&(_, _, tie)| tie as u8);
    found.truncate(limit);
    found
}

fn images_keys(conn: &rusqlite::Connection, release_id: i64) -> Vec<(i64, String, Option<String>, Option<String>)> {
    let s = store::shard_of_id(release_id);
    let sql = format!(
        "select position, sha256, exif_id, document_id from s{s}.release_images where release_id = ?1 order by position"
    );
    let Ok(mut q) = conn.prepare(&sql) else { return Vec::new() };
    q.query_map([release_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// The pictures to read, best first: a thumbnail, cover or poster before the rest, then by name. Only those of a few
/// articles.
pub fn picked(files: &[FileInfo]) -> Vec<&FileInfo> {
    let mut images: Vec<&FileInfo> = files
        .iter()
        .filter(|f| is_image_name(&f.filename))
        .filter(|f| (1..=MAX_PARTS as i64).contains(&f.expected.unwrap_or(0).max(f.parts_seen)))
        .collect();
    let rank = |f: &FileInfo| {
        let lower = f.filename.to_ascii_lowercase();
        !["thumb", "cover", "poster", "folder", "front"].iter().any(|w| lower.contains(w))
    };
    images.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.filename.cmp(&b.filename)));
    images.truncate(MAX_IMAGES);
    images
}

/// The files of the rows linked into the upload `lead` leads, its own left out, and of the small files found beside
/// it, up to 50 a row.
fn linked_files(conn: &rusqlite::Connection, lead: i64) -> rusqlite::Result<Vec<FileInfo>> {
    let members: Vec<i64> = conn
        .prepare_cached("select release_id from upload_link where lead_id = ?1 and release_id != ?1 limit 16")?
        .query_map([lead], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut files = Vec::new();
    // and the small files found beside it
    // this shard's own table, by its bare name: a step's connection has no other shard attached
    let beside: Vec<i64> = conn
        .prepare_cached("select row_id from beside_files where release_id = ?1 limit 16")?
        .query_map([lead], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for member in members.into_iter().chain(beside) {
        files.extend(store::release_files(conn, member, 50)?);
    }
    Ok(files)
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

/// What's kept of one picture read: its row's values after the release id and position.
pub fn row_values(file: &str, bytes: &[u8], sha256: &str) -> Option<Vec<rusqlite::types::Value>> {
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
    Some(vec![
        file.to_string().into(),
        sha256.to_string().into(),
        sniffed.format.to_string().into(),
        i64::from(sniffed.width).into(),
        i64::from(sniffed.height).into(),
        (bytes.len() as i64).into(),
        meta.into(),
        exif_id.into(),
        document_id.into(),
    ])
}

#[derive(Default)]
pub struct ImagesStep;

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
        let mut files = ctx.files().map_err(|e| StepError::Retry(e.to_string()))?;
        // the small files found beside this release, as a thumbnail posted under a subject of its own, and the rows
        // of an upload it leads (`archive::link`)
        files.extend(linked_files(ctx.conn(), ctx.release().id).map_err(|e| StepError::Retry(e.to_string()))?);
        let wanted = picked(&files);
        let books = containers(&files);
        if wanted.is_empty() && books.is_empty() {
            return Ok(Outcome::default());
        }
        let release_id = ctx.release().id;
        let mut left = MAX_PARTS * MAX_IMAGES;
        let mut writes = vec![StepWrite::new(DROP_IMAGES, vec![release_id.into()])];
        let mut kept = 0i64;
        let mut row = |name: &str, body: &[u8], writes: &mut Vec<StepWrite>| {
            // read before it's kept: a file named .jpg that isn't a picture never reaches the folder
            let sha256 = sha256_hex(body);
            let Some(values) = row_values(name, body, &sha256) else { return };
            if keep(body).is_none() {
                return;
            }
            let mut params = vec![release_id.into(), kept.into()];
            params.extend(values);
            writes.push(StepWrite::new(SET_IMAGE, params));
            kept += 1;
        };
        for file in wanted {
            if let Some(body) = crate::archive::steps::whole_file(ctx, file, &mut left)? {
                row(&file.filename, &body, &mut writes);
            }
        }
        // a book's own pictures, while there's room for them
        for file in books {
            let room = MAX_IMAGES.saturating_sub(writes.len() - 1);
            let mut book_left = MAX_CONTAINER_PARTS;
            if room == 0 {
                break;
            }
            let Some(body) = crate::archive::steps::whole_file(ctx, file, &mut book_left)? else { continue };
            for inside in super::container::pictures(&body, room) {
                row(&format!("{} › {}", file.filename, inside.name), &inside.bytes, &mut writes);
            }
        }
        if kept == 0 {
            return Ok(Outcome::default());
        }
        Ok(Outcome { matched: true, writes, ..Outcome::default() })
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

    #[test]
    fn a_thumbnail_or_cover_is_read_first_and_big_pictures_not_at_all() {
        let files = [
            file("Some.Film.2160p.mkv", 9_000),
            file("b-shot.png", 1),
            file("a-shot.jpg", 1),
            file("00-thumbnail.jpg", 1),
            file("huge.jpg", 40),
        ];
        let names: Vec<&str> = picked(&files).iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["00-thumbnail.jpg", "a-shot.jpg", "b-shot.png"]);
        let many: Vec<FileInfo> = (0..20).map(|i| file(&format!("{i:03}.jpg"), 1)).collect();
        assert_eq!(picked(&many).len(), MAX_IMAGES);
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
        let values = row_values("00-thumbnail.jpg", &bytes, "ab").unwrap();
        assert_eq!(values[2], "jpeg".to_string().into());
        assert_eq!(values[3], 16i64.into());
        let rusqlite::types::Value::Text(meta) = &values[6] else { panic!("meta") };
        let info: jpeg::JpegInfo = serde_json::from_str(meta).unwrap();
        assert_eq!(info.quality, Some(90));
        assert_eq!(values[7], rusqlite::types::Value::Null, "no EXIF id");
        assert!(row_values("x.jpg", b"not a picture", "ab").is_none());
    }
}
