//! Parts no listing showed, asked for by the ids their file's own articles name. An upload whose ids run
//! `part{N}of{T}.{token}@domain` (`nzb::inferred_segments`) tells us each missing part's id, and an id needs no
//! listing: every server is asked for its headers (the same all-servers read a body takes, which calls an article
//! missing only when each one says so), and what turns up is saved like any other header, soo the part joins its
//! file and counts towards whole. An id no server has is asked again later, less and less often; a post with more
//! missing parts than a round asks is asked over several rounds.

#[cfg(unix)]
mod head_journal;
#[cfg(unix)]
pub(crate) mod native;
pub(crate) mod template;

use std::sync::Arc;
use std::time::Duration;

use crate::nntp::Overview;
use crate::search::ArticleRow;

/// posts younger than this may still be arriving, by upload or by listing
const SETTLED_SECS: i64 = 2 * 3600;
/// how often a round looks for parts to ask for
const EVERY: Duration = Duration::from_secs(300);
/// releases this big or bigger: smaller posts aren't worth the reads
const MIN_BYTES: i64 = 64 << 20;
/// posted this recently: past it the parts a server lacks are rarely anywhere
const DAYS: i64 = 7;
/// a post less whole than this (in thousandths) isn't one a few parts make whole, nor one whose ids prove much
const MIN_PERMILLE: i64 = 500;
/// ids asked for per round, and at once
const PER_ROUND: usize = 3_000;
const AT_ONCE: usize = 8;
/// a post with nothing to ask for is looked at again after this: more parts may have come and prove its ids
const NOTHING_AGAIN: i64 = 86_400;
/// an id no server had (or whose headers were no use): asked again after this, doubling with each try, up to `LONGEST`
const ABSENT_AGAIN: i64 = 6 * 3600;
const LONGEST: i64 = 2 * 86_400;
/// an id a server couldn't be asked for, or whose headers couldn't be saved
const FAILED_AGAIN: i64 = 1800;

/// One part to ask for: its post (by the release it was found by), the group it's saved in unless its headers say
/// another, and the row `nzb::inferred_segments` made for it.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub release_id: i64,
    pub group: String,
    /// every group of its post: where else it may be saved
    pub groups: Arc<[String]>,
    pub row: ArticleRow,
}

impl Wanted {
    /// its id as `inferred_ids` keeps it, without the `<>`
    fn key(&self) -> String {
        self.row.message_id.trim().trim_matches(['<', '>']).to_string()
    }
}

/// What became of one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// a server had it, and it was saved. It waits like an absent one: a part saved that didn't join its file (its
    /// subject read another way) would be asked for every round
    Found,
    /// every server said it hasn't it
    Absent,
    /// a server answered with headers that aren't a listing row (none of the article's, no subject, too big), which
    /// asking again won't change
    Unusable,
    /// a server couldn't be asked, or what it gave couldn't be saved
    Failed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Found => "found",
            Outcome::Absent => "absent",
            Outcome::Unusable => "unusable",
            Outcome::Failed => "failed",
        }
    }
}

/// The retry time for an answered MID; transient failures retain their own short delay.
pub fn backoff(now: i64, tries: i64) -> i64 {
    let shift = tries.clamp(1, 16) - 1;
    now + (ABSENT_AGAIN << shift).min(LONGEST)
}

/// The listing row a server's headers for `w` make, and the group to save it in: `w`'s, unless the headers' `Xref:`
/// names other groups of its post and not it (a post cut across groups, its file in another of them). Subject and From
/// are made as a listing makes them: a folded line's tab a space, and mojibake undone. None when they aren't the article
/// asked for, or have no subject. Its size is what `Bytes:` says, else the size `w` was given (its file's parts' own),
/// else what `Lines:` makes of it.
pub fn overview_of(head: &[u8], w: &Wanted) -> Option<(String, Overview)> {
    let value = |name: &str| crate::spotnet::header::header_value(head, name);
    let message_id = value("Message-ID")?;
    if message_id.trim().trim_matches(['<', '>']) != w.row.message_id.trim().trim_matches(['<', '>']) {
        return None;
    }
    let listed = |v: String| crate::nntp::undo_mojibake(v.replace('\t', " "));
    let subject = listed(value("Subject")?).trim().to_string();
    if subject.is_empty() {
        return None;
    }
    let number_of = |n: Option<String>| n.and_then(|n| n.trim().parse::<i64>().ok()).filter(|n| *n > 0);
    let bytes = number_of(value("Bytes")).unwrap_or(0);
    let lines = number_of(value("Lines")).unwrap_or(0);
    let xref = value("Xref").unwrap_or_default();
    let numbers: Vec<(&str, &str)> = xref.split_whitespace().skip(1).filter_map(|e| e.split_once(':')).collect();
    let ours = numbers.iter().any(|(g, _)| *g == w.group);
    let group = match numbers
        .iter()
        .find(|(g, _)| !ours && w.groups.iter().any(|p| p == g) && !crate::filters::group_blocked(g))
    {
        Some((g, _)) => g.to_string(),
        None => w.group.clone(),
    };
    let row = Overview {
        // the answering server's numbers aren't the listing's: none (0, which the store's file timing passes over)
        number: 0,
        subject,
        from: listed(value("From").unwrap_or_default()),
        date: value("Date").unwrap_or_default(),
        message_id: format!("<{}>", message_id.trim().trim_matches(['<', '>'])),
        references: value("References").unwrap_or_default(),
        bytes: if bytes > 0 { bytes } else { w.row.bytes.filter(|b| *b > 0).unwrap_or(0) },
        lines,
        xref_groups: (!xref.is_empty()).then(|| crate::nntp::xref_groups(&format!("Xref: {xref}"))).flatten(),
    };
    Some((group, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(id: &str, group: &str) -> Wanted {
        Wanted {
            release_id: 1,
            group: group.into(),
            groups: vec![group.to_string()].into(),
            row: ArticleRow {
                message_id: id.into(),
                filename: Some("a.rar".into()),
                part: Some(3),
                total_parts: Some(6),
                bytes: Some(640),
                subject: None,
                poster: None,
                posted_date: None,
            },
        }
    }

    const HEAD: &[u8] =
        b"Path: x!not-for-mail\r\nFrom: DAX <dax@domain.com>\r\nSubject: [1/2] - \"a.rar\" yEnc\r\n (3/6)\r\n\
Date: Sat, 07 Nov 2026 06:22:22 GMT\r\nMessage-ID: <part3of6.tok@bzh29>\r\nLines: 5022\r\n\
Xref: news.example alt.binaries.movies:11666417361 alt.binaries.hdtv:42\r\n";

    #[test]
    fn a_servers_headers_make_the_listing_row() {
        let (group, o) = overview_of(HEAD, &wanted("<part3of6.tok@bzh29>", "alt.binaries.movies")).unwrap();
        assert_eq!(group, "alt.binaries.movies");
        assert_eq!(o.subject, "[1/2] - \"a.rar\" yEnc (3/6)");
        assert_eq!(o.from, "DAX <dax@domain.com>");
        assert_eq!(o.message_id, "<part3of6.tok@bzh29>");
        assert_eq!(o.number, 0);
        assert_eq!(o.lines, 5022);
        // no Bytes: the size its file's parts have, not the line count's guess
        assert_eq!(o.bytes, 640);
        assert_eq!(o.xref_groups, Some(2));
        // and without that either, the line count gives it (`Overview::into_article`)
        let mut sizeless = wanted("<part3of6.tok@bzh29>", "alt.binaries.movies");
        sizeless.row.bytes = None;
        let o = overview_of(HEAD, &sizeless).unwrap().1;
        assert_eq!((o.bytes, o.into_article().bytes), (0, 5022 * 130));
        // a post cut across groups: saved in the group of the post the article is in
        let mut cut = wanted("<part3of6.tok@bzh29>", "alt.binaries.teevee");
        cut.groups = vec!["alt.binaries.teevee".to_string(), "alt.binaries.movies".to_string()].into();
        assert_eq!(overview_of(HEAD, &cut).unwrap().0, "alt.binaries.movies");
        // but never in a group the post isn't in
        let alone = wanted("<part3of6.tok@bzh29>", "alt.binaries.teevee");
        assert_eq!(overview_of(HEAD, &alone).unwrap().0, "alt.binaries.teevee");
        // a folded line's tab is a space, as in a listing
        let tabbed = b"Subject: \"a.rar\" yEnc\r\n\t(3/6)\r\nMessage-ID: <part3of6.tok@bzh29>\r\n";
        assert_eq!(overview_of(tabbed, &alone).unwrap().1.subject, "\"a.rar\" yEnc (3/6)");
    }

    #[test]
    fn headers_without_a_size_keep_the_one_the_part_was_given() {
        let head = b"Subject: \"a.rar\" yEnc (3/6)\nMessage-ID: <part3of6.tok@bzh29>\nFrom: x\n";
        let (group, o) = overview_of(head, &wanted("part3of6.tok@bzh29", "g")).unwrap();
        assert_eq!((group.as_str(), o.bytes, o.lines, o.number, o.xref_groups), ("g", 640, 0, 0, None));
        let sized = b"Subject: s\nMessage-ID: <part3of6.tok@bzh29>\nBytes: 661524\n";
        assert_eq!(overview_of(sized, &wanted("<part3of6.tok@bzh29>", "g")).unwrap().1.bytes, 661524);
    }

    #[test]
    fn headers_of_another_article_or_without_a_subject_are_no_row() {
        let other = b"Subject: s\nMessage-ID: <part4of6.tok@bzh29>\n";
        assert!(overview_of(other, &wanted("<part3of6.tok@bzh29>", "g")).is_none());
        let bare = b"Message-ID: <part3of6.tok@bzh29>\n";
        assert!(overview_of(bare, &wanted("<part3of6.tok@bzh29>", "g")).is_none());
        let empty = b"Subject:  \nMessage-ID: <part3of6.tok@bzh29>\n";
        assert!(overview_of(empty, &wanted("<part3of6.tok@bzh29>", "g")).is_none());
    }

    #[test]
    fn an_id_on_no_server_is_asked_again_less_often() {
        assert_eq!(backoff(0, 1), ABSENT_AGAIN);
        assert_eq!(backoff(0, 2), 2 * ABSENT_AGAIN);
        assert_eq!(backoff(0, 3), 4 * ABSENT_AGAIN);
        assert_eq!(backoff(0, 9), LONGEST);
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "explicit isolated PG/NXV and owned NNTP; actual native retry acceptance"]
    fn native_each_id_keeps_answered_tries_and_transient_failure_waits() {
        use native::{
            Service,
            tests::{fixture, net, peer, seed},
        };
        let (schema, runtime, _root, store, headers) = fixture();
        let now = 1_800_000_000;
        seed(&runtime, now, &[1, 2, 4]);
        let server = peer(now, true, None);
        let net = net();
        let pool =
            Arc::new(crate::nntp::Pool::new(&[server.config.clone()]).with_body_tries(Duration::from_secs(1), 0));
        let mut service = Service::new(&runtime.owner, &runtime.db, pool.clone(), store.clone(), &headers).unwrap();
        let mut observer = schema.pool.get().unwrap();
        let mut retry = || {
            let row = observer
                .query_one(
                    "select next_at,tries from inferred_ids where message_id='part3of4.own@example.invalid'",
                    &[],
                )
                .unwrap();
            (row.get::<_, i64>(0), row.get::<_, i64>(1))
        };
        service.start_without_delay_for_test();
        assert_eq!(service.tick(net.handle(), now).unwrap().unusable, 1);
        assert_eq!(retry(), (now + ABSENT_AGAIN, 1));
        service.start_without_delay_for_test();
        assert_eq!(service.tick(net.handle(), now + 301).unwrap().requested, 0);
        let second = now + ABSENT_AGAIN + 1;
        service.start_without_delay_for_test();
        assert_eq!(service.tick(net.handle(), second).unwrap().unusable, 1);
        assert_eq!(retry(), (second + 2 * ABSENT_AGAIN, 2));
        drop(service);
        let failed_at = second + 2 * ABSENT_AGAIN + 1;
        let mut unavailable =
            Service::new(&runtime.owner, &runtime.db, Arc::new(crate::nntp::Pool::new(&[])), store.clone(), &headers)
                .unwrap();
        unavailable.start_without_delay_for_test();
        assert_eq!(unavailable.tick(net.handle(), failed_at).unwrap().failed, 1);
        assert_eq!(retry(), (failed_at + FAILED_AGAIN, 2), "a provider failure is not an answered try");
        drop(unavailable);
        let answered_at = failed_at + FAILED_AGAIN + 1;
        let mut service = Service::new(&runtime.owner, &runtime.db, pool, store.clone(), &headers).unwrap();
        service.start_without_delay_for_test();
        assert_eq!(service.tick(net.handle(), answered_at).unwrap().unusable, 1);
        assert_eq!(retry(), (answered_at + 4 * ABSENT_AGAIN, 3));
        assert_eq!(server.hits.load(std::sync::atomic::Ordering::Acquire), 3);
        assert_eq!(observer.query_one("select count(*) from segments", &[]).unwrap().get::<_, i64>(0), 3);
        drop(service);
        runtime.owner.close();
        store.close().unwrap();
    }
}
