//! A posted NZB, read as a list: the files of the post it points to, by the names their subjects give and the
//! article bytes the NZB counts for each. What that post holds is for the release it is, not this one.

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

/// Each `<file>` once, by its subject's file name and its segments' bytes summed. `complete` once `</nzb>` is read;
/// a cut or broken NZB lists what came before the break. Bytes that aren't UTF-8 are read as replacement characters.
pub fn list(body: &[u8]) -> Listing {
    let text = String::from_utf8_lossy(body);
    let mut reader = Reader::from_str(&text);
    let mut listing = Listing::default();
    // the file being read: its name and bytes so far
    let mut file: Option<(String, u64)> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "file" => file = Some((file_name(&e), 0)),
                "segment" => add_bytes(&mut file, &e),
                _ => {}
            },
            Ok(Event::Empty(e)) if e.local_name().as_ref() == "segment" => add_bytes(&mut file, &e),
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "file" => {
                    if let Some((name, bytes)) = file.take()
                        && !name.is_empty()
                        && listing.files.len() < MAX_ENTRIES
                    {
                        listing.files.push(InnerFile { name, size: Some(bytes) });
                    }
                }
                "nzb" => {
                    listing.complete = true;
                    break;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => {
                listing.damaged = true;
                break;
            }
            Ok(_) => {}
        }
    }
    listing
}

/// the name a subject gives its file, or the subject itself when no pattern reads one
fn file_name(e: &BytesStart<'_>) -> String {
    let subject = attribute(e, "subject").unwrap_or_default();
    let name = crate::parser::parse_subject(&subject).map_or(subject, |p| p.filename);
    clean_name(&name).unwrap_or_default()
}

fn add_bytes(file: &mut Option<(String, u64)>, e: &BytesStart<'_>) {
    if let Some((_, total)) = file {
        let bytes = attribute(e, "bytes").and_then(|b| b.trim().parse::<u64>().ok()).unwrap_or(0);
        *total = total.saturating_add(bytes);
    }
}

/// An attribute's text, unescaped; as it is written when an entity can't be.
fn attribute(e: &BytesStart<'_>, key: &str) -> Option<String> {
    let attr = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == key)?;
    Some(attr.normalized_value(XmlVersion::Implicit1_0).map_or_else(|_| attr.value.to_string(), |v| v.into_owned()))
}

/// The release names an NZB's files give, as a save names a release from a file (`strip_release_suffixes`), PAR2
/// files last, each once. At most 3.
pub fn release_names<'a>(files: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut files: Vec<&str> = files.collect();
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
