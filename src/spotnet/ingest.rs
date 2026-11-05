//! Reads new spots from `free.pt` every `poll_minutes`: the overview first, keeping the signed movie and TV spots,
//! then each of those whole (HEAD for its XML, BODY for its NZB in `alt.binaries.ftd`), saved as a release and
//! tagged with its spot. About 75 spots a day pass the filter, so this is a few requests a minute at most.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::config::SpotsConfig;
use crate::indexer::{Db, on_db};
use crate::nntp::{Pool, says_absent};

use super::store::{self, SAVED, SKIPPED, SpotRow};
use super::{MAX_XML_BYTES, SPOT_GROUP, binary, full, header, release};

/// Overview lines asked for in one XOVER.
const XOVER_STEP: u64 = 5_000;
/// The largest article of a spot's NZB read, as it comes off the wire: they are a few hundred KB.
const MAX_NZB_ARTICLE: usize = 4 * 1024 * 1024;
/// A spot's NZB articles read at most, and their bytes together. A real spot names one or two; past these the
/// spot is no real one, and nothing more is read.
const MAX_NZB_ARTICLES: usize = 16;
const MAX_NZB_POSTED: usize = 16 * 1024 * 1024;

/// What one pass did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub read: u64,
    pub noted: usize,
    pub saved: usize,
    pub skipped: usize,
    pub retry: usize,
}

/// Runs passes until `stopping`.
pub async fn run(pool: Arc<Pool>, db: Db, config: SpotsConfig, stopping: impl Fn() -> bool + Send + 'static) {
    if let Err(e) = on_db(&db, |conn| Ok(store::create_main(conn)?)).await {
        eprintln!("spots: {e:#}");
        return;
    }
    let every = Duration::from_secs(u64::from(config.poll_minutes) * 60);
    while !stopping() {
        match pass(&pool, &db, &config, &stopping).await {
            Ok(p) if p.noted + p.saved + p.skipped + p.retry > 0 => println!(
                "spots: read {} overview lines, {} new movie/TV spots, saved {}, skipped {}, to retry {}",
                p.read, p.noted, p.saved, p.skipped, p.retry
            ),
            Ok(_) => {}
            Err(e) => eprintln!("spots: {e:#}"),
        }
        let started = std::time::Instant::now();
        while !stopping() && started.elapsed() < every {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// One pass: the overview since the cursor, then up to `per_pass` spots fetched whole.
pub async fn pass(pool: &Pool, db: &Db, config: &SpotsConfig, stopping: &impl Fn() -> bool) -> Result<Pass> {
    let mut done = Pass::default();
    let prefer = pool.indexing_tier().first().copied().unwrap_or(0);
    let (server, (_, low, high, _)) = pool.select_group_on(prefer, SPOT_GROUP).await?;
    let host = pool.host(server);
    let key = host.clone();
    let cursor = on_db(db, move |conn| Ok(store::cursor(conn, &key)?)).await?;
    let mut from = start_at(cursor, low, high, config.first_articles);
    while from <= high && !stopping() {
        let to = high.min(from + XOVER_STEP - 1);
        let rows = match pool.xover_on(server, SPOT_GROUP, from, to).await {
            Ok(rows) => rows,
            Err(e) if e.is_empty_range() => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        done.read += rows.len() as u64;
        let wanted: Vec<(String, u64)> = rows
            .iter()
            .filter(|r| header::parse(&r.subject, &r.from).is_some_and(|h| h.signed && h.kind().is_some()))
            .map(|r| (r.message_id.trim_matches(['<', '>']).to_string(), r.number))
            .collect();
        let key = host.clone();
        let now = chrono::Utc::now().timestamp();
        done.noted += on_db(db, move |conn| Ok(store::note_read(conn, &key, to, &wanted, now)?)).await?;
        from = to + 1;
    }

    let limit = config.per_pass;
    let todo = on_db(db, move |conn| Ok(store::todo(conn, limit)?)).await?;
    for message_id in todo {
        if stopping() {
            break;
        }
        let outcome = match fetch(pool, db, &message_id).await {
            Ok(Some(release_id)) => {
                done.saved += 1;
                Some((SAVED, Some(release_id)))
            }
            Ok(None) => {
                done.skipped += 1;
                Some((SKIPPED, None))
            }
            Err(e) => {
                eprintln!("spots: {message_id}: {e:#}");
                done.retry += 1;
                None
            }
        };
        let now = chrono::Utc::now().timestamp();
        on_db(db, move |conn| Ok(store::settle(conn, &message_id, outcome, now)?)).await?;
    }
    Ok(done)
}

/// Where a pass starts reading: after the cursor, or `first` articles back from the newest on the first pass and
/// when the cursor is past what the server has (it renumbered).
pub fn start_at(cursor: Option<u64>, low: u64, high: u64, first: u64) -> u64 {
    match cursor {
        Some(c) if c <= high => c + 1,
        _ => high.saturating_sub(first).max(low).max(1),
    }
}

/// A spot fetched whole and saved. Ok(None): it isn't one to save, and never will be. Err: try again later.
async fn fetch(pool: &Pool, db: &Db, message_id: &str) -> Result<Option<i64>> {
    let head = match pool.fetch_head(&format!("<{message_id}>"), MAX_XML_BYTES * 4).await {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(None),
        Err(e) if says_absent(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let Some(spot) = full::xml_from_headers(&head).as_deref().and_then(full::parse) else { return Ok(None) };
    let Some(kind) = spot.kind() else { return Ok(None) };
    if spot.nzb.is_empty() {
        return Ok(None);
    }
    // an id named twice is no real spot's, and would be read twice
    if !nzb_ids_usable(&spot.nzb) {
        return Ok(None);
    }
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(spot.nzb.len());
    for segment in &spot.nzb {
        let left = MAX_NZB_POSTED.saturating_sub(bodies.iter().map(Vec::len).sum::<usize>());
        match pool.fetch_body_capped(&format!("<{segment}>"), MAX_NZB_ARTICLE.min(left)).await {
            Ok(Some(body)) => bodies.push(body),
            Ok(None) => return Ok(None),
            Err(e) if says_absent(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    let parts: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
    let Some(nzb) = binary::nzb(&parts) else { return Ok(None) };
    let parsed = crate::archive::nzb::parse_keeping(&nzb, release::MAX_SEGMENTS);
    let Some(release) = release::release_of(&spot, &parsed) else { return Ok(None) };
    if crate::filters::group_blocked(&release.group) {
        return Ok(None);
    }

    let group = release.group.clone();
    let shard = crate::store::shard_of(&group);
    let spotter_id = header::spotter_id_of_head(&head);
    let nzb_key = release::nzb_key(&parsed);
    let row = SpotRow { message_id: message_id.to_string(), kind, spot, spotter_id, nzb_key };

    // an edit or repost of a spot already saved: tag that release with the newer spot, and save nothing
    let copy_row = row.clone();
    let copy = on_shard(db, shard, move |conn| store::tag_copy(conn, &copy_row)).await?;
    if copy.is_some() {
        return Ok(copy);
    }
    // another upload already saved under this spot title and group: this one gets a name of its own, rather than
    // having its articles folded into that release
    let (check_name, check_group, key) = (release.name.clone(), group.clone(), row.nzb_key.clone());
    let taken = on_shard(db, shard, move |conn| store::name_taken(conn, &check_name, &check_group, &key)).await?;
    let mut release = release;
    if taken {
        let own = format!("{} [{}]", release.name, &row.nzb_key[..8]);
        release::rename(&mut release, &own);
    }
    let name = release.name.clone();
    db.save(vec![release]).await?;
    // tagged on its shard's writer after the save: the save doesn't say which id the release got
    on_shard(db, shard, move |conn| store::tag(conn, &name, &group, &row)).await
}

/// Runs `work` on `shard`'s writer, in its own transaction, and hands back what it returned.
async fn on_shard<T: Send + 'static>(
    db: &Db,
    shard: usize,
    work: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T> {
    let out = Arc::new(std::sync::Mutex::new(None));
    let into = out.clone();
    let answer = db.on_writer(
        shard,
        "db.stage.spots",
        Box::new(move |conn| {
            *into.lock().unwrap() = Some(work(conn)?);
            Ok(())
        }),
    )?;
    tokio::task::spawn_blocking(move || answer.recv())
        .await
        .map_err(|e| anyhow!("spot task failed: {e}"))?
        .map_err(|_| anyhow!("db writer stopped"))?
        .map_err(|e| anyhow!("spots: {e}"))?;
    let value = out.lock().unwrap().take();
    value.ok_or_else(|| anyhow!("spots: the writer gave no answer"))
}

/// Whether a spot's NZB articles can be read: at most `MAX_NZB_ARTICLES` of them, each named once.
fn nzb_ids_usable(ids: &[String]) -> bool {
    let mut seen = std::collections::HashSet::new();
    ids.len() <= MAX_NZB_ARTICLES && ids.iter().all(|id| seen.insert(id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_pass_starts() {
        // the first pass goes `first` back, but not before the group's first article
        assert_eq!(start_at(None, 2, 4_450_733, 20_000), 4_430_733);
        assert_eq!(start_at(None, 4_440_000, 4_450_733, 20_000), 4_440_000);
        assert_eq!(start_at(None, 0, 10, 20_000), 1);
        // then after the cursor
        assert_eq!(start_at(Some(4_450_000), 2, 4_450_733, 20_000), 4_450_001);
        assert_eq!(start_at(Some(4_450_733), 2, 4_450_733, 20_000), 4_450_734);
        // a cursor past the newest article: the server renumbered
        assert_eq!(start_at(Some(9_000_000), 2, 4_450_733, 20_000), 4_430_733);
    }

    #[test]
    fn nzb_ids_are_few_and_named_once() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(nzb_ids_usable(&ids(&["a@spot.net", "b@spot.net"])));
        assert!(!nzb_ids_usable(&ids(&["a@spot.net", "a@spot.net"])));
        let many: Vec<String> = (0..=MAX_NZB_ARTICLES).map(|n| format!("{n}@spot.net")).collect();
        assert!(!nzb_ids_usable(&many));
        assert!(nzb_ids_usable(&many[..MAX_NZB_ARTICLES]));
    }
}
