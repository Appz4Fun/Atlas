//! Comments on spots, and spam reports, as Spotweb reads them. A comment is an article in `free.usenet` whose
//! message-id starts with its spot's (`<spot>.<rating>.<random>.<random>@spot.net`); its text is the article's
//! body. A report is an article in `free.willey` whose subject is `REPORT <spot message-id> ...`. Every comment
//! and report is noted (its header only, from the overview), in the main database, whether or not its spot is in
//! `spots_seen` yet: the spot overview is read by another task, and articles don't reach a server in the order they
//! were posted. A comment's body is read once its spot is kept (waiting or saved). Those whose spot isn't kept
//! `UNKEPT_DAYS` after they were noted are pruned.

use crate::pg::sql::{Sql, vals};

use super::store::{SAVED, TODO, read_cursor, write_cursor};

/// Where comments are posted.
pub const COMMENT_GROUP: &str = "free.usenet";
/// Where spam reports are posted.
pub const REPORT_GROUP: &str = "free.willey";

/// The longest comment body read: a longer one is no comment but an NZB or the like posted by mistake, and is
/// left out (`GONE`).
pub const MAX_BODY: usize = 10 * 1024;

/// Waiting for its body.
pub const WAITING: i64 = 0;
/// Body read.
pub const READ: i64 = 1;
/// The article is gone, too big to be a comment, or wouldn't come in `MAX_TRIES` passes.
pub const GONE: i64 = 2;
/// Days a comment or report on a spot not kept stays, for its spot to be noted: the spot overview reads behind the
/// group by an overview pass at most, so a day would do.
pub const UNKEPT_DAYS: i64 = 7;

/// Passes a comment's body is tried in before it's given up on (`GONE`): a pass stops at the first failure, so an
/// outage costs each pass a try for the few comments in flight, not the whole queue.
pub const MAX_TRIES: i64 = 20;

/// Whether a spot message-id is a Spotnet one: `...@spot.net`. Comments and reports name their spots that way;
/// anything else in the groups is no comment or report.
fn spotnet_id(id: &str) -> bool {
    super::valid_message_id(id) && id.rsplit_once('@').is_some_and(|(_, d)| d.eq_ignore_ascii_case("spot.net"))
}

/// The spot a comment is on and the rating it gives (0 for none, else 1 to 10), from its message-id, as Spotweb
/// reads them: not every server passes `References` on. None when the message-id isn't a comment's.
pub fn comment_ref(message_id: &str) -> Option<(String, u8)> {
    let id = message_id.trim().trim_matches(['<', '>']);
    let (local, domain) = id.split_once('@')?;
    let parts: Vec<&str> = local.split('.').collect();
    if parts.len() < 2 || parts[0].is_empty() {
        return None;
    }
    let spot = format!("{}@{domain}", parts[0]);
    if !spotnet_id(&spot) {
        return None;
    }
    // older comments have no rating, or a word where it goes
    let rating = if parts.len() == 4 { parts[1].parse::<u8>().ok().filter(|r| *r <= 10).unwrap_or(0) } else { 0 };
    Some((spot, rating))
}

/// The spot a report is about: the message-id its subject names after `REPORT`, else its `References`.
pub fn report_ref(subject: &str, references: &str) -> Option<String> {
    let named = subject
        .trim()
        .strip_prefix("REPORT")
        .and_then(|rest| {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('<')?;
            Some(&rest[..rest.find('>')?])
        })
        .or_else(|| references.split_whitespace().last().map(|r| r.trim_matches(['<', '>'])))?;
    spotnet_id(named).then(|| named.to_string())
}

/// The name a `From` value gives, before its address.
pub fn poster_name(from: &str) -> String {
    let name = from.split('<').next().unwrap_or_default().trim().trim_matches('"').trim();
    name.chars().take(128).collect()
}

/// A comment the overview lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Noted {
    pub message_id: String,
    pub spot: String,
    pub at: i64,
    pub rating: u8,
    pub poster: String,
    pub spotter_id: Option<String>,
}

fn cursor_key(group: &str, server: &str) -> String {
    format!("spot_said_cursor:{group}:{server}")
}

/// The last article of `group` read on `server`, by its numbers there.
pub fn cursor(db: &dyn Sql, group: &str, server: &str) -> crate::pg::sql::Result<Option<u64>> {
    read_cursor(db, &cursor_key(group, server))
}

/// Notes the comments (or, with `reports`, the reports) of one stretch of `group`, seen `now`, and that it was read
/// up to `last`, together. Returns how many were new.
pub fn note_read(
    db: &dyn Sql,
    group: &str,
    server: &str,
    last: u64,
    found: &[Noted],
    reports: bool,
    now: i64,
) -> crate::pg::sql::Result<usize> {
    db.tx(|tx| {
        let mut added = 0;
        for n in found {
            added += if reports {
                tx.execute(
                    "insert into spot_reports (message_id, spot, at, poster, seen) values ($1, $2, $3, $4, $5)
                     on conflict (message_id) do nothing",
                    vals![&n.message_id, &n.spot, n.at, &n.poster, now],
                )?
            } else {
                tx.execute(
                    "insert into spot_comments (message_id, spot, at, rating, poster, spotter_id, seen)
                     values ($1, $2, $3, $4, $5, $6, $7)
                     on conflict (message_id) do nothing",
                    vals![&n.message_id, &n.spot, n.at, i64::from(n.rating), &n.poster, n.spotter_id.as_deref(), now],
                )?
            };
        }
        write_cursor(tx, &cursor_key(group, server), last)?;
        Ok(added)
    })
}

/// Comments on kept spots waiting for their body, those on saved spots first, newest first.
pub fn todo(db: &dyn Sql, limit: usize) -> crate::pg::sql::Result<Vec<String>> {
    db.column(
        "select c.message_id from spot_comments c join spots_seen s on s.message_id = c.spot
         where c.state = 0 and s.state in ($1, $2)
         order by case when s.state = $2 then 0 else 1 end, c.at desc, c.message_id limit $3",
        vals![TODO, SAVED, limit as i64],
    )
}

/// What came of reading a comment's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Read(String),
    Gone,
    /// one more try (gone after `MAX_TRIES`)
    Failed,
}

/// What came of reading these comments' bodies, in one transaction.
pub fn settle(db: &dyn Sql, outcomes: &[(String, Outcome)]) -> crate::pg::sql::Result<()> {
    db.tx(|tx| {
        for (message_id, outcome) in outcomes {
            match outcome {
                Outcome::Read(body) => tx.execute(
                    "update spot_comments set state = $1, body = $2 where message_id = $3",
                    vals![READ, body, message_id],
                )?,
                Outcome::Gone => {
                    tx.execute("update spot_comments set state = $1 where message_id = $2", vals![GONE, message_id])?
                }
                Outcome::Failed => tx.execute(
                    "update spot_comments set tries = tries + 1,
                        state = case when tries + 1 >= $1 then $2 else state end
                     where message_id = $3",
                    vals![MAX_TRIES, GONE, message_id],
                )?,
            };
        }
        Ok(())
    })
}

/// Forgets the comments and reports of spots not kept (never noted, came to nothing, forgotten since) once they
/// were noted `UNKEPT_DAYS` ago. A saved spot is kept for good (`store::prune`).
pub fn prune(db: &dyn Sql, now: i64) -> crate::pg::sql::Result<usize> {
    let mut n = 0;
    for table in ["spot_comments", "spot_reports"] {
        n += db.execute(
            &format!(
                "delete from {table} where seen < $3
                 and spot not in (select message_id from spots_seen where state in ($1, $2))"
            ),
            vals![TODO, SAVED, now - UNKEPT_DAYS * 86_400],
        )?;
    }
    Ok(n)
}

/// A comment's body as text: UTF-8, or Latin-1 as older clients wrote it, its line ends as `\n`, cut at
/// `MAX_BODY` (the read is capped there; this is for one that isn't).
pub fn body_text(raw: &[u8]) -> String {
    let cut = raw.len() > MAX_BODY;
    let raw = &raw[..raw.len().min(MAX_BODY)];
    let text = match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        // UTF-8 cut inside a character: what came before it
        Err(e) if cut && e.error_len().is_none() => String::from_utf8_lossy(&raw[..e.valid_up_to()]).into_owned(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    };
    text.replace("\r\n", "\n").trim().to_string()
}

/// A comment as the release page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comment {
    pub at: i64,
    pub rating: u8,
    pub poster: String,
    pub spotter_id: Option<String>,
    pub state: i64,
    pub body: String,
}

/// What's been said about release `release_id`'s spot: its comments, oldest first (but those removed by a
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_names_its_spot_and_rating() {
        assert_eq!(
            comment_ref("<VHdhUzRwTGpFCf5RnRReipyKhb8.0.71uPABCy0Y8ZcaEag.p6FS@spot.net>"),
            Some(("VHdhUzRwTGpFCf5RnRReipyKhb8@spot.net".into(), 0))
        );
        assert_eq!(comment_ref("abc.7.x.y@spot.net"), Some(("abc@spot.net".into(), 7)));
        // an older comment: no rating, or a word where it goes, or one past 10
        assert_eq!(comment_ref("abc.x.y@spot.net"), Some(("abc@spot.net".into(), 0)));
        assert_eq!(comment_ref("abc.zz.x.y@spot.net"), Some(("abc@spot.net".into(), 0)));
        assert_eq!(comment_ref("abc.11.x.y@spot.net"), Some(("abc@spot.net".into(), 0)));
        // not a comment's: no dot, another domain
        assert_eq!(comment_ref("abc@spot.net"), None);
        assert_eq!(comment_ref("abc.def@news.host"), None);
        assert_eq!(comment_ref("no-at-sign.1.2.3"), None);
        assert_eq!(comment_ref(".1.2.3@spot.net"), None);
    }

    #[test]
    fn a_report_names_its_spot() {
        assert_eq!(
            report_ref("REPORT <6SFiUcUBnnsJGHragGYNP@spot.net> - Spotnet Classic 3.0.0.7", ""),
            Some("6SFiUcUBnnsJGHragGYNP@spot.net".into())
        );
        assert_eq!(report_ref("Spam", "<a@spot.net> <b@spot.net>"), Some("b@spot.net".into()));
        assert_eq!(report_ref("REPORT nothing", ""), None);
        assert_eq!(report_ref("REPORT <bad id@x>", ""), None);
        assert_eq!(report_ref("REPORT <a@news.host>", ""), None);
    }

    #[test]
    fn names_and_bodies() {
        assert_eq!(poster_name("OlliWanKenobi <key.sig@spot.net>"), "OlliWanKenobi");
        assert_eq!(poster_name("\"Some One\" <a@b>"), "Some One");
        assert_eq!(body_text(b"Thanks!\r\nPassword: abc\r\n\r\n"), "Thanks!\nPassword: abc");
        assert_eq!(body_text(b"caf\xe9"), "caf\u{e9}");
        let long = vec![b'x'; MAX_BODY + 50];
        assert_eq!(body_text(&long).len(), MAX_BODY);
        // UTF-8 cut inside a character: up to it
        let mut cut = vec![b'a'; MAX_BODY - 1];
        cut.extend_from_slice("\u{e9}\u{e9}".as_bytes());
        assert_eq!(body_text(&cut).len(), MAX_BODY - 1);
    }

    /// Native queue transitions, cursor isolation, retry bounds and retention.
    #[test]
    fn the_native_queue_preserves_cursor_retry_and_retention() {
        crate::pg::testing::each(|conn| {
            let spots = vec![("s1@spot.net".to_string(), 1, None), ("s2@spot.net".to_string(), 2, None)];
            super::super::store::note_read(conn, "srv", 2, &spots, 0).unwrap();
            let saved = super::super::store::Settled { state: SAVED, release_id: Some(9), ..Default::default() };
            super::super::store::settle(conn, "s1@spot.net", Some(&saved), 0).unwrap();
            let noted = |id: &str, spot: &str, at| Noted {
                message_id: id.into(),
                spot: spot.into(),
                at,
                rating: 0,
                poster: "p".into(),
                spotter_id: None,
            };
            let found =
                vec![noted("s1.8.a.b@spot.net", "s1@spot.net", 20), noted("s2.0.a.b@spot.net", "s2@spot.net", 30)];
            assert_eq!(note_read(conn, COMMENT_GROUP, "srv", 100, &found, false, 0).unwrap(), 2);
            assert_eq!(note_read(conn, COMMENT_GROUP, "srv", 120, &found, false, 0).unwrap(), 0);
            assert_eq!(cursor(conn, COMMENT_GROUP, "srv").unwrap(), Some(120));
            assert_eq!(cursor(conn, REPORT_GROUP, "srv").unwrap(), None);
            // nothing new: the cursor still moves
            assert_eq!(note_read(conn, COMMENT_GROUP, "srv", 130, &[], false, 0).unwrap(), 0);
            assert_eq!(cursor(conn, COMMENT_GROUP, "srv").unwrap(), Some(130));
            let report = vec![noted("r1@spot.net", "s1@spot.net", 25)];
            assert_eq!(note_read(conn, REPORT_GROUP, "srv", 7, &report, true, 0).unwrap(), 1);
            // a comment on a spot not noted yet (it reached the server first): kept, its text not read for now
            let day = 86_400;
            let early = vec![noted("s3.0.a.b@spot.net", "s3@spot.net", 40)];
            note_read(conn, COMMENT_GROUP, "srv", 140, &early, false, 10 * day).unwrap();
            // the saved spot's comment first, though the other is newer
            assert_eq!(todo(conn, 10).unwrap(), ["s1.8.a.b@spot.net", "s2.0.a.b@spot.net"]);
            let read = ("s1.8.a.b@spot.net".to_string(), Outcome::Read("Password in comment: hunter2".into()));
            let failed = ("s2.0.a.b@spot.net".to_string(), Outcome::Failed);
            settle(conn, &[read, failed.clone()]).unwrap();
            for _ in 1..MAX_TRIES - 1 {
                settle(conn, std::slice::from_ref(&failed)).unwrap();
            }
            assert_eq!(todo(conn, 10).unwrap(), ["s2.0.a.b@spot.net"], "tried, not given up on yet");
            settle(conn, &[failed]).unwrap();
            assert!(todo(conn, 10).unwrap().is_empty());

            // its spot comes: its text is next
            super::super::store::note_read(conn, "srv", 3, &[("s3@spot.net".to_string(), 3, None)], 0).unwrap();
            assert_eq!(todo(conn, 10).unwrap(), ["s3.0.a.b@spot.net"]);

            // `UNKEPT_DAYS` after they were noted, the comments of spots not kept go: one that came to nothing, one
            // forgotten since. A kept spot's stay
            let now = 10 * day + UNKEPT_DAYS * day;
            assert_eq!(prune(conn, now).unwrap(), 0);
            let skipped = super::super::store::Settled { state: super::super::store::SKIPPED, ..Default::default() };
            super::super::store::settle(conn, "s2@spot.net", Some(&skipped), 0).unwrap();
            assert_eq!(prune(conn, now).unwrap(), 1);
            conn.execute("delete from spots_seen where message_id = 's1@spot.net'", vals![]).unwrap();
            assert_eq!(prune(conn, now).unwrap(), 2, "its comment and its report");
            // the one noted on day 10 is past `UNKEPT_DAYS` a second later, whatever its spot did
            conn.execute("delete from spots_seen where message_id = 's3@spot.net'", vals![]).unwrap();
            assert_eq!(prune(conn, now).unwrap(), 0);
            assert_eq!(prune(conn, now + 1).unwrap(), 1);
            let left: i64 = conn.one("select count(*) from spot_comments", vals![]).unwrap();
            assert_eq!(left, 0);
        });
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "explicit isolated PG; actual producer repost and comment/report reader"]
    fn native_kept_comments_follow_repost_and_report_identity() {
        use crate::pg::fresh_reader::{Budget, ReadView};
        use crate::spotnet::{native_read_fixture_tests as fixture, native_store::Store, store};
        use std::time::{Duration, Instant};
        let (schema, runtime, _dir, id) = fixture::fixture();
        let storage = Store::new(&runtime.owner).unwrap();
        storage
            .with(|tx| {
                let db = crate::pg::sql::Pg::new(tx);
                note_read(
                    &db,
                    REPORT_GROUP,
                    "report-account",
                    7,
                    &[Noted {
                        message_id: "report@spot.net".into(),
                        spot: "real@spot.net".into(),
                        at: 2,
                        rating: 0,
                        poster: "reader".into(),
                        spotter_id: None,
                    }],
                    true,
                    2,
                )?;
                store::note_read(&db, "repost-account", 1, &[("edit@spot.net".into(), 1, None)], 2)?;
                Ok(())
            })
            .unwrap();
        let key = runtime
            .owner
            .with(|client| {
                Ok(client.query_one("select nzb_key from spots where release_id=$1", &[&id])?.get::<_, String>(0))
            })
            .unwrap();
        let row = store::SpotRow {
            message_id: "edit@spot.net".into(),
            kind: crate::spotnet::Kind::Series,
            spot: crate::spotnet::full::parse(fixture::XML).unwrap(),
            spotter_id: Some("actual".into()),
            nzb_key: key,
            xml: fixture::XML.into(),
        };
        storage.copy_of(id, &row, "edited-ref", 3).unwrap();
        let mut client = schema.pool.get().unwrap();
        let view =
            ReadView::open(&mut client, &schema.name, Budget::default(), Instant::now() + Duration::from_secs(5))
                .unwrap();
        let (detail, said, reports) = view.spot_view(id).unwrap().unwrap();
        assert_eq!(detail.message_id, "edit@spot.net");
        assert_eq!(reports, 1);
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].state, READ);
        assert_eq!(
            said[0].body,
            "é".repeat(MAX_BODY),
            "the earlier spot's original body remains attached to the repost"
        );
        assert!(view.spot_view(id + 1000).unwrap().is_none());
        drop(view);
        runtime.owner.close();
    }
}
