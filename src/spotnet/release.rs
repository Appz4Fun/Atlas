//! A spot's NZB as a release: its files and articles, the way indexing would have saved the post, under the
//! spot's title. Saved through the same path as indexed releases, so search, the API, NZBs and post-processing
//! treat it like any other. A post the indexer already saved under the same name and group gets the spot's
//! articles folded into it rather than a second row.

use crate::archive::nzb::Parsed;
use crate::parser::{Article, Release, is_complete, parse_subject};

use super::full::FullSpot;

/// Segments read out of one spot's NZB at most: about 1.4 TB of 700 KB articles.
pub const MAX_SEGMENTS: usize = 2_000_000;

/// The release a spot's NZB is, or None when the NZB is cut, damaged, lists no group, or lost segments past
/// `MAX_SEGMENTS`.
pub fn release_of(spot: &FullSpot, nzb: &Parsed) -> Option<Release> {
    if !nzb.complete || nzb.damaged || nzb.files.is_empty() {
        return None;
    }
    let group = nzb.files.iter().find_map(|f| f.groups.first())?.clone();
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
        let total_parts = segments.len() as i64;
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

/// What an NZB holds, whatever order it lists its files in: a SHA-256 of its message-ids, sorted, hex.
pub fn nzb_key(nzb: &Parsed) -> String {
    use sha2::{Digest, Sha256};
    let mut ids: Vec<&str> = nzb.files.iter().flat_map(|f| f.segments.iter().map(|s| s.message_id.as_str())).collect();
    ids.sort_unstable();
    let mut hash = Sha256::new();
    for id in ids {
        hash.update(id.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The title as a release name: one line, no control characters, at most 255 characters.
fn clean_title(title: &str) -> Option<String> {
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
fn iso_date(unix: i64) -> String {
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
        let untitled = FullSpot { title: "\t ".into(), ..spot() };
        assert!(release_of(&untitled, &parse_keeping(NZB.as_bytes(), MAX_SEGMENTS)).is_none());
    }
}
