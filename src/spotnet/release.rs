//! A spot's NZB as a release: its files and articles, the way indexing would have saved the post, under the
//! spot's title. Saved through the same path as indexed releases, so search, the API, NZBs and post-processing
//! treat it like any other. A post the indexer already saved under the same name and group gets the spot's
//! articles folded into it rather than a second row.

use crate::archive::nzb::Parsed;
use crate::parser::{Article, Release, is_complete, parse_subject};

use super::full::FullSpot;

/// Segments read out of one spot's NZB at most: about 200 GB of 700 KB articles. Each becomes an article in
/// memory until the release is saved, so this bounds what one spot can take.
pub const MAX_SEGMENTS: usize = 300_000;

/// The release a spot's NZB is, or None when the NZB is cut, damaged or left files out, lost segments past
/// `MAX_SEGMENTS`, or has a file posted only to blocked groups (`filters::group_blocked`), or to none.
pub fn release_of(spot: &FullSpot, nzb: &Parsed) -> Option<Release> {
    if !nzb.complete || nzb.damaged || nzb.left_out > 0 || nzb.files.is_empty() {
        return None;
    }
    // every file in a group that may be saved, the release under the first file's first one
    if !nzb.files.iter().all(|f| allowed(f).is_some()) {
        return None;
    }
    let group = allowed(&nzb.files[0])?.clone();
    let title = clean_title(&spot.title)?;
    let file_total = i64::try_from(nzb.files.len()).ok()?;
    let mut articles = Vec::new();
    for (at, file) in nzb.files.iter().enumerate() {
        // every segment was kept, or the release would pass for whole with holes in it
        if file.segments.len() != file.articles || file.segments.iter().any(|s| s.message_id.is_empty()) {
            return None;
        }
        let parsed = parse_subject(&file.subject);
        let filename =
            parsed.as_ref().map(|p| p.filename.clone()).filter(|f| !f.is_empty()).unwrap_or_else(|| {
                if file.name.is_empty() { format!("{title}.{:03}", at + 1) } else { file.name.clone() }
            });
        let (file_index, files) = match parsed.as_ref().and_then(|p| Some((p.file_index?, p.file_total?))) {
            Some(counter) => counter,
            None => (at as i64 + 1, file_total),
        };
        let mut segments = file.segments.clone();
        segments.sort_by_key(|s| s.number);
        // what the subject says the file has, when that's more than the NZB lists: a file the NZB lost the end of
        // must not pass for whole
        let total_parts = (segments.len() as i64).max(parsed.as_ref().map_or(0, |p| p.total_parts));
        let date = iso_date(if file.date > 0 { file.date } else { spot.created });
        let author = if file.poster.is_empty() { spot.poster.clone() } else { file.poster.clone() };
        for (n, segment) in segments.iter().enumerate() {
            articles.push(Article {
                subject: file.subject.clone(),
                author: author.clone(),
                date: date.clone(),
                message_id: format!("<{}>", segment.message_id),
                bytes: i64::try_from(segment.bytes).unwrap_or(i64::MAX),
                filename: Some(filename.clone()),
                release_name: Some(title.clone()),
                part: Some(if segment.number > 0 { segment.number } else { n as i64 + 1 }),
                total_parts: Some(total_parts),
                file_index: Some(file_index),
                file_total: Some(files),
                ..Article::default()
            });
        }
    }
    let first = articles.first()?;
    let (poster, date) = (first.author.clone(), first.date.clone());
    Some(Release {
        name: title,
        size: articles.iter().map(|a| a.bytes).sum(),
        complete: is_complete(&articles),
        is_obfuscated: false,
        display_name: None,
        group,
        poster,
        date,
        collection: None,
        articles,
    })
}

/// Gives a release, and each of its articles, another name.
pub fn rename(release: &mut Release, name: &str) {
    release.name = name.to_string();
    for article in &mut release.articles {
        article.release_name = Some(name.to_string());
    }
}

/// Which NZB articles a spot points at, whatever order it lists them in: a SHA-256 of their ids, sorted, hex. Two
/// spots with the same one share an NZB, read once.
pub fn ids_key(ids: &[String]) -> String {
    sha_of_sorted(ids.iter().map(String::as_str).collect())
}

fn sha_of_sorted(mut ids: Vec<&str>) -> String {
    use sha2::{Digest, Sha256};
    ids.sort_unstable();
    let mut hash = Sha256::new();
    for id in ids {
        hash.update(id.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What an NZB holds, whatever order it lists its files in: a SHA-256 of its message-ids, sorted, hex.
pub fn nzb_key(nzb: &Parsed) -> String {
    sha_of_sorted(nzb.files.iter().flat_map(|f| f.segments.iter().map(|s| s.message_id.as_str())).collect())
}

/// A group of `file`'s that may be saved, the first one.
pub(super) fn allowed(file: &crate::archive::nzb::NzbFile) -> Option<&String> {
    file.groups.iter().find(|g| !crate::filters::group_blocked(g))
}

/// The title as a release name: one line, no control characters, at most 255 characters.
pub(super) fn clean_title(title: &str) -> Option<String> {
    let one_line: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = one_line.chars().take(255).collect();
    let cut = cut.trim().to_string();
    (!cut.is_empty()).then_some(cut)
}

/// Unix seconds as indexing stores a date: `%Y-%m-%d %H:%M:%S`, UTC.
pub(super) fn iso_date(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0).unwrap_or_default().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::nzb::parse_keeping;

    const NZB: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
 <file poster="up@loader" date="1762300000" subject="[1/2] - &quot;Movie.2026.1080p.mkv&quot; yEnc (1/2)">
  <groups><group>alt.binaries.example</group></groups>
  <segments>
   <segment bytes="700" number="2">b@x</segment>
   <segment bytes="700" number="1">a@x</segment>
  </segments>
 </file>
 <file poster="up@loader" date="1762300000" subject="3c9a7f86cd1142d7a0fc06df591ee45f">
  <groups><group>alt.binaries.example</group></groups>
  <segments><segment bytes="100" number="1">c@x</segment></segments>
 </file>
</nzb>"#;

    fn spot() -> FullSpot {
        FullSpot { title: "Movie (2026)\t1080p".into(), poster: "spotter".into(), created: 1, ..FullSpot::default() }
    }

    #[test]
    fn a_spot_nzb_is_a_release() {
        let r = release_of(&spot(), &parse_keeping(NZB.as_bytes(), MAX_SEGMENTS)).unwrap();
        assert_eq!(r.name, "Movie (2026) 1080p");
        assert_eq!(r.group, "alt.binaries.example");
        assert_eq!(r.poster, "up@loader");
        assert_eq!(r.date, "2025-11-04 23:46:40");
        assert_eq!(r.size, 1500);
        assert!(r.complete);
        assert!(!r.is_obfuscated);
        let parts: Vec<(&str, i64, &str)> = r
            .articles
            .iter()
            .map(|a| (a.filename.as_deref().unwrap(), a.part.unwrap(), a.message_id.as_str()))
            .collect();
        // a subject no rule reads keeps it as the file's name
        assert_eq!(
            parts,
            [
                ("Movie.2026.1080p.mkv", 1, "<a@x>"),
                ("Movie.2026.1080p.mkv", 2, "<b@x>"),
                ("3c9a7f86cd1142d7a0fc06df591ee45f", 1, "<c@x>")
            ]
        );
        assert!(r.articles.iter().all(|a| a.release_name.as_deref() == Some("Movie (2026) 1080p")));
    }

    #[test]
    fn a_file_the_nzb_cut_short_keeps_its_subjects_total() {
        // the subject says 3 parts, the NZB lists 2
        let cut = NZB.replace("yEnc (1/2)", "yEnc (1/3)");
        let r = release_of(&spot(), &parse_keeping(cut.as_bytes(), MAX_SEGMENTS)).unwrap();
        let first = r.articles.iter().find(|a| a.filename.as_deref() == Some("Movie.2026.1080p.mkv")).unwrap();
        assert_eq!(first.total_parts, Some(3));
        assert!(!r.complete);
    }

    #[test]
    fn the_ids_key_is_the_ids_in_any_order() {
        let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(ids_key(&ids(&["a@x", "b@x"])), ids_key(&ids(&["b@x", "a@x"])));
        assert_ne!(ids_key(&ids(&["a@x"])), ids_key(&ids(&["a@x", "b@x"])));
    }

    #[test]
    fn a_rename_reaches_every_article() {
        let mut r = release_of(&spot(), &parse_keeping(NZB.as_bytes(), MAX_SEGMENTS)).unwrap();
        rename(&mut r, "Movie (2026) 1080p [0123abcd]");
        assert_eq!(r.name, "Movie (2026) 1080p [0123abcd]");
        assert!(r.articles.iter().all(|a| a.release_name.as_deref() == Some("Movie (2026) 1080p [0123abcd]")));
    }

    #[test]
    fn the_nzb_key_is_the_ids_in_any_order() {
        let a = parse_keeping(NZB.as_bytes(), MAX_SEGMENTS);
        let mut b = a.clone();
        b.files.reverse();
        assert_eq!(nzb_key(&a), nzb_key(&b));
        assert_eq!(nzb_key(&a).len(), 64);
        b.files[0].segments[0].message_id = "other@x".into();
        assert_ne!(nzb_key(&a), nzb_key(&b));
    }

    #[test]
    fn no_release_from_a_broken_nzb() {
        let cut = &NZB[..NZB.len() - 20];
        assert!(release_of(&spot(), &parse_keeping(cut.as_bytes(), MAX_SEGMENTS)).is_none());
        // segments past what was kept
        assert!(release_of(&spot(), &parse_keeping(NZB.as_bytes(), 1)).is_none());
        let no_group = NZB.replace("<group>alt.binaries.example</group>", "");
        assert!(release_of(&spot(), &parse_keeping(no_group.as_bytes(), MAX_SEGMENTS)).is_none());
        let left_out = Parsed { left_out: 1, ..parse_keeping(NZB.as_bytes(), MAX_SEGMENTS) };
        assert!(release_of(&spot(), &left_out).is_none());
        // a file posted only to a blocked group can't ride in under the first file's group
        let blocked = NZB
            .replacen("<group>alt.binaries.example</group>", "<group>alt.binaries.14yo</group>", 2)
            .replacen("<group>alt.binaries.14yo</group>", "<group>alt.binaries.example</group>", 1);
        assert!(crate::filters::group_blocked("alt.binaries.14yo"));
        assert!(release_of(&spot(), &parse_keeping(blocked.as_bytes(), MAX_SEGMENTS)).is_none());
        // one allowed group among blocked ones is enough, and names the release
        let mixed = NZB.replacen(
            "<group>alt.binaries.example</group>",
            "<group>alt.binaries.14yo</group><group>alt.binaries.example</group>",
            1,
        );
        assert_eq!(
            release_of(&spot(), &parse_keeping(mixed.as_bytes(), MAX_SEGMENTS)).unwrap().group,
            "alt.binaries.example"
        );
        let untitled = FullSpot { title: "\t ".into(), ..spot() };
        assert!(release_of(&untitled, &parse_keeping(NZB.as_bytes(), MAX_SEGMENTS)).is_none());
    }
}
