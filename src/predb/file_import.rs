//! Pure line decoder retained after retirement of the SQLite file-import service.

use super::import::Entry;
use super::keys::valid_title;

/// a line as an entry, or None when it's no title
fn entry_of(line: &[u8]) -> Option<Entry> {
    let text = std::str::from_utf8(line).ok()?;
    let mut fields = text.trim_end_matches(['\r', '\n']).split('\t');
    let title = fields.next()?.trim();
    if !valid_title(title) {
        return None;
    }
    Some(Entry {
        title: title.to_string(),
        category: fields.next().map(str::trim).filter(|c| !c.is_empty()).map(String::from),
        pre_at: fields.next().and_then(|p| p.trim().parse().ok()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_decoder_keeps_title_category_timestamp_and_rejects_invalid_bytes() {
        assert_eq!(
            entry_of(b" Alpha.Beta.Title-GRP\t TV \t123\r\n"),
            Some(Entry {
                title: "Alpha.Beta.Title-GRP".into(),
                category: Some("TV".into()),
                pre_at: Some(123),
                ..Default::default()
            })
        );
        assert!(entry_of(b"bad/slash-GRP").is_none());
        assert!(entry_of(&[255]).is_none());
    }
}
