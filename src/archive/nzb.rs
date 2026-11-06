//! A posted NZB, read as a list: the files of the post it points to, by the names their subjects give and the
//! article bytes the NZB counts for each. What that post holds is for the release it is, not this one.

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use super::names::clean_name;
use super::{InnerFile, Listing, MAX_ENTRIES};

/// the decoded bytes of a posted NZB read at most, from every article of its file
pub const MAX_NZB_BYTES: usize = 8 * 1024 * 1024;
/// and its articles, all of which it takes
pub const MAX_NZB_PARTS: i64 = 16;

/// Whether these bytes start an NZB: an XML prolog or doctype, or the root itself, and the root soon after.
pub fn is_nzb(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let start = head.iter().position(|b| !b.is_ascii_whitespace()).map_or(&[][..], |at| &head[at..]);
    let opens = [&b"<?xml"[..], b"<!DOCTYPE nzb", b"<nzb"].iter().any(|p| starts_with_ignore_case(start, p));
    opens && start.get(..4096).unwrap_or(start).windows(4).any(|w| w.eq_ignore_ascii_case(b"<nzb"))
}

fn starts_with_ignore_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.get(..prefix.len()).is_some_and(|b| b.eq_ignore_ascii_case(prefix))
}

/// Segments an NZB's files keep at most between them: a post with more is listed and not read whole, soo their
/// message-ids aren't needed, and a crafted NZB of millions can't fill memory with them.
pub const MAX_KEPT_SEGMENTS: usize = 4_096;

/// One `<file>` of an NZB: the name its subject gives, and its articles: all of them counted, the first ones kept
/// (`MAX_KEPT_SEGMENTS` across the NZB).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NzbFile {
    pub name: String,
    pub segments: Vec<Segment>,
    /// its `<segment>`s, kept or not
    pub articles: usize,
    /// and their bytes
    pub total_bytes: u64,
    /// the groups its `<groups>` names, the first `MAX_GROUPS`
    pub groups: Vec<String>,
    /// its `subject`, `poster` and `date` (unix seconds) attributes, as written
    pub subject: String,
    pub poster: String,
    pub date: i64,
}

/// Groups of a file kept at most.
const MAX_GROUPS: usize = 32;
/// The longest message-id or group name read: one longer is no real one, and is dropped rather than cut.
const MAX_TEXT: usize = 512;

impl NzbFile {
    /// what its articles count in all
    pub fn bytes(&self) -> u64 {
        self.total_bytes
    }

    fn add(&mut self, segment: Segment, kept: &mut usize, max_kept: usize) {
        self.articles += 1;
        self.total_bytes = self.total_bytes.saturating_add(segment.bytes);
        if *kept < max_kept {
            *kept += 1;
            self.segments.push(segment);
        }
    }
}

/// One `<segment>`: its number in the file, its article bytes and its message-id, without brackets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub number: i64,
    pub bytes: u64,
    pub message_id: String,
}

/// What an NZB says: its files, `complete` once `</nzb>` is read, `damaged` when it broke off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub files: Vec<NzbFile>,
    pub complete: bool,
    pub damaged: bool,
    /// `<file>`s not in `files`: past `MAX_ENTRIES`, or with no name
    pub left_out: usize,
}

/// Each `<file>` with a name, at most `MAX_ENTRIES`, and its segments. A cut or broken NZB gives what came before
/// the break. Bytes that aren't UTF-8 are read as replacement characters.
pub fn parse(body: &[u8]) -> Parsed {
    parse_keeping(body, MAX_KEPT_SEGMENTS)
}

/// `parse`, keeping up to `max_kept` segments across the NZB rather than `MAX_KEPT_SEGMENTS`: for an NZB that is
/// to be saved whole, such as a spot's.
pub fn parse_keeping(body: &[u8], max_kept: usize) -> Parsed {
    let text = String::from_utf8_lossy(body);
    let mut reader = Reader::from_str(&text);
    let mut parsed = Parsed::default();
    let mut file: Option<NzbFile> = None;
    // the segment being read, its message-id so far
    let mut segment: Option<Segment> = None;
    // a `<group>` being read, its name so far
    let mut group: Option<String> = None;
    // the text being read went past `MAX_TEXT`
    let mut overlong = false;
    let mut kept = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "file" => file = Some(file_of(&e)),
                "segment" => {
                    segment = Some(segment_of(&e));
                    overlong = false;
                }
                "group" => {
                    group = Some(String::new());
                    overlong = false;
                }
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == "segment" => {
                if let Some(f) = file.as_mut() {
                    f.add(segment_of(&e), &mut kept, max_kept);
                }
            }
            Ok(Event::Text(t)) => append(text_of(&mut segment, &mut group), &t, &mut overlong),
            Ok(Event::CData(t)) => append(text_of(&mut segment, &mut group), &t, &mut overlong),
            Ok(Event::GeneralRef(r)) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => resolve_predefined_entity(&r).unwrap_or_default().to_string(),
                };
                append(text_of(&mut segment, &mut group), &resolved, &mut overlong);
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "segment" => {
                    if let (Some(mut s), Some(f)) = (segment.take(), file.as_mut()) {
                        // an overlong id is no id: such a file can't be read
                        s.message_id = if overlong {
                            String::new()
                        } else {
                            s.message_id.trim().trim_matches(['<', '>']).to_string()
                        };
                        f.add(s, &mut kept, max_kept);
                    }
                }
                "group" => {
                    if let (Some(g), Some(f)) = (group.take(), file.as_mut())
                        && !overlong
                        && !g.trim().is_empty()
                        && f.groups.len() < MAX_GROUPS
                    {
                        f.groups.push(g.trim().to_string());
                    }
                }
                "file" => {
                    if let Some(f) = file.take() {
                        if !f.name.is_empty() && parsed.files.len() < MAX_ENTRIES {
                            parsed.files.push(f);
                        } else {
                            parsed.left_out += 1;
                        }
                    }
                }
                "nzb" => {
                    parsed.complete = true;
                    break;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => {
                parsed.damaged = true;
                break;
            }
            Ok(_) => {}
        }
    }
    parsed
}

/// What text goes to: the segment's message-id, or the group's name, being read.
fn text_of<'a>(segment: &'a mut Option<Segment>, group: &'a mut Option<String>) -> Option<&'a mut String> {
    segment.as_mut().map(|s| &mut s.message_id).or(group.as_mut())
}

/// `piece` onto `to`, up to `MAX_TEXT` in all; past it nothing more is kept and `overlong` says so.
fn append(to: Option<&mut String>, piece: &str, overlong: &mut bool) {
    let Some(to) = to else { return };
    if *overlong || to.len() + piece.len() > MAX_TEXT {
        *overlong = true;
        return;
    }
    to.push_str(piece);
}

/// Each `<file>` once, by its subject's file name and its segments' bytes summed, as `parse` reads it.
pub fn list(body: &[u8]) -> Listing {
    let parsed = parse(body);
    Listing {
        files: parsed.files.iter().map(|f| InnerFile { name: f.name.clone(), size: Some(f.bytes()) }).collect(),
        complete: parsed.complete,
        damaged: parsed.damaged,
        ..Default::default()
    }
}

/// the name a subject gives its file, or the subject itself when no pattern reads one
fn file_name(e: &BytesStart<'_>) -> String {
    let subject = attribute(e, "subject").unwrap_or_default();
    let name = crate::parser::parse_subject(&subject).map_or(subject, |p| p.filename);
    clean_name(&name).unwrap_or_default()
}

/// The longest `subject` or `poster` kept, in bytes: a crafted NZB can't make every article of a release that is
/// saved from it carry megabytes.
pub const MAX_ATTRIBUTE: usize = 1_024;

fn file_of(e: &BytesStart<'_>) -> NzbFile {
    let bounded = |key| attribute(e, key).map(|v| cut_to(v, MAX_ATTRIBUTE)).unwrap_or_default();
    NzbFile {
        name: file_name(e),
        subject: bounded("subject"),
        poster: bounded("poster"),
        date: attribute(e, "date").and_then(|d| d.trim().parse().ok()).unwrap_or(0),
        ..NzbFile::default()
    }
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut_to(mut s: String, max: usize) -> String {
    if s.len() > max {
        let at = (0..=max).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        s.truncate(at);
    }
    s
}

fn segment_of(e: &BytesStart<'_>) -> Segment {
    let number = |key| attribute(e, key).and_then(|b| b.trim().parse::<u64>().ok());
    Segment {
        number: number("number").and_then(|n| i64::try_from(n).ok()).unwrap_or(0),
        bytes: number("bytes").unwrap_or(0),
        message_id: String::new(),
    }
}

/// An attribute's text, unescaped; as it is written when an entity can't be.
fn attribute(e: &BytesStart<'_>, key: &str) -> Option<String> {
    let attr = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key)?;
    Some(attr.normalized_value(XmlVersion::Implicit1_0).map_or_else(|_| attr.value.to_string(), |v| v.into_owned()))
}

/// Extracts a password from an NZB's `<head><meta type="password">...</meta></head>` or `<file subject="...">`.
pub fn meta_password(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let mut reader = Reader::from_str(&text);
    let mut in_password_meta = false;
    let mut meta_buf = String::new();
    let mut subject_hit: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "meta" => {
                    let ty = attribute(&e, "type").unwrap_or_default().to_ascii_lowercase();
                    in_password_meta = ty == "password"
                        || ty == "pwd"
                        || ty == "x-dnzb-password"
                        || ty.contains("password");
                    meta_buf.clear();
                }
                "file" if subject_hit.is_none() => {
                    if let Some(subj) = attribute(&e, "subject") {
                        subject_hit = crate::release_info::find_password(&subj)
                            .or_else(|| crate::release_info::harvested_password(&subj));
                    }
                }
                _ => {}
            },
            Ok(Event::Text(t)) if in_password_meta => {
                meta_buf.push_str(&t.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if in_password_meta => {
                if let Ok(Some(ch)) = r.resolve_char_ref() {
                    meta_buf.push(ch);
                } else if let Some(val) = quick_xml::escape::resolve_xml_entity(r.as_ref()) {
                    meta_buf.push_str(val);
                }
            }
            Ok(Event::CData(c)) if in_password_meta => {
                meta_buf.push_str(c.as_ref());
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "meta" if in_password_meta => {
                    in_password_meta = false;
                    let trimmed = meta_buf.trim();
                    if !trimmed.is_empty()
                        && trimmed.len() <= 128
                        && !super::PasswordHarvester::is_negative_phrase(trimmed)
                        && !crate::release_info::is_false_positive_password(trimmed)
                    {
                        return Some(trimmed.to_string());
                    }
                }
                "nzb" => break,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    subject_hit
}

/// The release names an NZB's files give, as a save names a release from a file (`strip_release_suffixes`), PAR2
/// files last, each once. At most 3.
pub fn release_names<'a>(files: impl Iterator<Item = &'a str>) -> Vec<String> {
    // an NZB on the way is a hop, not where it leads
    let mut files: Vec<&str> = files.filter(|name| super::select::classify(name) != super::select::Kind::Nzb).collect();
    files.sort_by_key(|name| name.to_ascii_lowercase().ends_with(".par2"));
    let mut names: Vec<String> = Vec::new();
    for file in files {
        let name = crate::parser::strip_release_suffixes(super::names::base_name(file));
        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) && names.len() < 3 {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_left_out_are_counted_and_attributes_are_bounded() {
        let long = "\u{e9}".repeat(MAX_ATTRIBUTE);
        let nzb = format!(
            r#"<nzb><file subject="{long}" poster="p"><groups><group>a.b.c</group></groups><segments><segment number="1" bytes="1">a@x</segment></segments></file><file subject=""><segments><segment number="1" bytes="1">b@x</segment></segments></file></nzb>"#
        );
        let parsed = parse(nzb.as_bytes());
        assert_eq!((parsed.files.len(), parsed.left_out), (1, 1));
        let subject = &parsed.files[0].subject;
        assert!(subject.len() <= MAX_ATTRIBUTE && subject.len() >= MAX_ATTRIBUTE - 1, "{}", subject.len());
        assert_eq!(parsed.files[0].poster, "p");
    }

    const NZB: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 0.9//EN" "http://www.newzbin.com/DTD/nzb/nzb-0.9.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p@x (p)" date="1517591902" subject="Some.Movie.2016.2160p-GRP.rar (01/02)">
    <groups><group>alt.binaries.a</group><group>alt.binaries.b</group></groups>
    <segments>
      <segment bytes="640000" number="1">part1of2.abc$&#x26;@powerpost.local</segment>
      <segment bytes="1000" number="2">part2of2.abc$&#x26;@powerpost.local</segment>
    </segments>
  </file>
  <file poster="p@x (p)" date="1517591902" subject="[1/2] - &quot;Some.Movie.2016.2160p-GRP.par2&quot; yEnc (1/1)">
    <groups><group>alt.binaries.a</group></groups>
    <segments><segment bytes="8600" number="1">x@y</segment></segments>
  </file>
</nzb>"#;

    #[test]
    fn lists_each_file_by_its_subjects_name_with_its_bytes() {
        let l = list(NZB.as_bytes());
        let got: Vec<(&str, Option<u64>)> = l.files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        assert_eq!(
            got,
            [("Some.Movie.2016.2160p-GRP.rar", Some(641_000)), ("Some.Movie.2016.2160p-GRP.par2", Some(8_600))]
        );
        assert!(l.complete && !l.damaged);
    }

    #[test]
    fn a_cut_nzb_lists_what_came_before_and_is_never_complete() {
        let whole = list(NZB.as_bytes());
        for at in 0..NZB.len() {
            let part = list(&NZB.as_bytes()[..at]);
            assert!(!part.complete, "cut {at}");
            assert!(part.files.iter().zip(&whole.files).all(|(a, b)| a == b), "cut {at}");
        }
    }

    #[test]
    fn parse_keeps_each_segments_number_bytes_and_message_id() {
        let p = parse(NZB.as_bytes());
        assert_eq!(p.files.len(), 2);
        assert_eq!(
            p.files[0].segments,
            [
                Segment { number: 1, bytes: 640_000, message_id: "part1of2.abc$&@powerpost.local".into() },
                Segment { number: 2, bytes: 1000, message_id: "part2of2.abc$&@powerpost.local".into() },
            ]
        );
        assert_eq!(p.files[1].segments[0].message_id, "x@y");
        let bracketed = parse(b"<nzb><file subject=\"a.rar (1/1)\"><segments><segment bytes=\"1\" number=\"1\"> &lt;a@b&gt; </segment></segments></file></nzb>");
        assert_eq!(bracketed.files[0].segments[0].message_id, "a@b");
        let long = format!(
            "<nzb><file subject=\"a.rar (1/1)\"><groups><group>a.b.c</group></groups><segments><segment bytes=\"1\" number=\"1\">{}</segment></segments></file></nzb>",
            "x".repeat(10_000)
        );
        let long = parse(long.as_bytes());
        assert_eq!(
            (long.files[0].segments[0].message_id.as_str(), long.files[0].groups.as_slice()),
            ("", &["a.b.c".to_string()][..])
        );
        let cdata = parse(b"<nzb><file subject=\"a.rar (1/1)\"><segments><segment bytes=\"1\" number=\"1\"><![CDATA[c@d]]></segment></segments></file></nzb>");
        assert_eq!(cdata.files[0].segments[0].message_id, "c@d");
    }

    #[test]
    fn segments_past_the_cap_are_counted_not_kept() {
        let segments: String = (1..=MAX_KEPT_SEGMENTS + 10)
            .map(|n| format!("<segment bytes=\"2\" number=\"{n}\">{n}@x</segment>"))
            .collect();
        let p = parse(
            format!("<nzb><file subject=\"a.rar (1/1)\"><segments>{segments}</segments></file></nzb>").as_bytes(),
        );
        let f = &p.files[0];
        assert_eq!(
            (f.segments.len(), f.articles, f.bytes()),
            (MAX_KEPT_SEGMENTS, MAX_KEPT_SEGMENTS + 10, 2 * (MAX_KEPT_SEGMENTS as u64 + 10))
        );
        assert!(!crate::archive::follow::small(&p.files));
    }

    #[test]
    fn release_names_leave_nzbs_on_the_way_out() {
        let names = release_names(
            ["A.nzb", "B-{{pw}}.nzb", "Final.Movie.2024-GRP.part01.rar", "Final.Movie.2024-GRP.par2"].into_iter(),
        );
        assert_eq!(names, ["Final.Movie.2024-GRP"]);
    }

    #[test]
    fn a_subject_no_pattern_reads_is_the_name() {
        let nzb =
            r#"<nzb><file subject="a3f9c2d1e4b5"><segments><segment bytes="5">x@y</segment></segments></file></nzb>"#;
        assert_eq!(list(nzb.as_bytes()).files, [InnerFile { name: "a3f9c2d1e4b5".into(), size: Some(5) }]);
    }

    #[test]
    fn is_nzb_by_its_start() {
        assert!(is_nzb(NZB.as_bytes()));
        assert!(is_nzb(b"\xef\xbb\xbf\r\n<nzb xmlns=\"x\">"));
        assert!(is_nzb(b"<!DOCTYPE nzb><NZB>"));
        assert!(!is_nzb(b"<?xml version=\"1.0\"?><rss></rss>"));
        assert!(!is_nzb(b"Rar!\x1a\x07\x01\x00"));
        assert!(!is_nzb(b"see the <nzb> inside"));
        assert!(!is_nzb(b""));
    }
}
