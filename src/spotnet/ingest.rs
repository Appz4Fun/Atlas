//! Reads new spots from `free.pt` every `poll_minutes`: the overview first, keeping the signed movie and TV spots,
//! then each of those whole (HEAD for its XML, BODY for its NZB in `alt.binaries.ftd`), saved as a release and
//! tagged with its spot. About 75 spots a day pass the filter. Spots are fetched `concurrency` at a time: each
//! request queues for a connection behind indexing, so one at a time was minutes a spot on a busy indexer.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use crate::config::SpotsConfig;
use crate::indexer::{Db, on_db};
use crate::nntp::{Pool, says_absent};

use super::store::{self, SAVED, SKIPPED, Settled, SpotRow};
use super::{MAX_XML_BYTES, SPOT_GROUP, binary, full, header, release};

/// Overview lines asked for in one XOVER.
const XOVER_STEP: u64 = 5_000;
/// The largest article of a spot's NZB read, as it comes off the wire: they are a few hundred KB.
const MAX_NZB_ARTICLE: usize = 4 * 1024 * 1024;
/// A spot's NZB articles read at most, and their bytes together. A real spot names one or two; past these the
/// spot is no real one, and nothing more is read.
const MAX_NZB_ARTICLES: usize = 16;
const MAX_NZB_POSTED: usize = 16 * 1024 * 1024;
/// What `body_capped` may read past its cap before it gives up: one long line.
const CAP_OVERSHOOT: usize = 64 * 1024;
/// NZB article bytes one pass reads at most; the spots left wait for the next pass.
const MAX_PASS_POSTED: usize = 256 * 1024 * 1024;

/// What one pass did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub read: u64,
    pub noted: usize,
    pub saved: usize,
    pub skipped: usize,
    pub retry: usize,
    /// left for a later pass: the pass's byte budget ran out, or a spot with the same NZB was being saved
    pub pending: usize,
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
            Ok(p) if p.noted + p.saved + p.skipped + p.retry + p.pending > 0 => println!(
                "spots: read {} overview lines, {} new movie/TV spots, saved {}, skipped {}, to retry {}, left for later {}",
                p.read, p.noted, p.saved, p.skipped, p.retry, p.pending
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
pub async fn pass(pool: &Arc<Pool>, db: &Db, config: &SpotsConfig, stopping: &impl Fn() -> bool) -> Result<Pass> {
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
        let wanted: Vec<store::Noted> = rows
            .iter()
            .filter_map(|r| {
                let h = header::parse(&r.subject, &r.from).filter(header::SpotHeader::wanted)?;
                Some((r.message_id.trim_matches(['<', '>']).to_string(), r.number, h.spotter_id()))
            })
            .collect();
        let key = host.clone();
        let now = chrono::Utc::now().timestamp();
        done.noted += on_db(db, move |conn| Ok(store::note_read(conn, &key, to, &wanted, now)?)).await?;
        from = to + 1;
    }

    let now = chrono::Utc::now().timestamp();
    on_db(db, move |conn| Ok(store::prune(conn, now)?)).await?;
    let limit = config.per_pass;
    let todo = on_db(db, move |conn| Ok(store::todo(conn, limit)?)).await?;
    if todo.is_empty() {
        return Ok(done);
    }
    let waiting = on_db(db, |conn| Ok(store::waiting(conn)?)).await?;
    println!("spots: {waiting} waiting, fetching {} ({} at a time)", todo.len(), config.concurrency);
    let shared = Arc::new(Shared::new(MAX_PASS_POSTED));
    let slots = Arc::new(tokio::sync::Semaphore::new(config.concurrency));
    let mut tasks = tokio::task::JoinSet::new();
    for message_id in todo {
        // stopping, or too little budget left for a spot's first NZB article: the rest wait for the next pass
        if stopping() || shared.budget.load(Ordering::Relaxed) < MAX_NZB_ARTICLE + CAP_OVERSHOOT {
            break;
        }
        let slot = slots.clone().acquire_owned().await.expect("the pass's semaphore is never closed");
        let (pool, db, shared) = (pool.clone(), db.clone(), shared.clone());
        tasks.spawn(async move {
            let _slot = slot;
            // held until the outcome is in spots_seen, where the next spot with this NZB finds it
            let mut claims = Vec::new();
            let outcome = match fetch(&pool, &db, &message_id, &shared, &mut claims).await {
                // left for a later pass: nothing to record
                Ok(None) => return Ok(Waited::Pending),
                Ok(Some(settled)) => Some(settled),
                Err(e) => {
                    eprintln!("spots: {message_id}: {e:#}");
                    None
                }
            };
            let state = outcome.as_ref().map(|s| s.state);
            let now = chrono::Utc::now().timestamp();
            on_db(&db, move |conn| Ok(store::settle(conn, &message_id, outcome.as_ref(), now)?)).await?;
            drop(claims);
            Ok(Waited::Done(state))
        });
        while let Some(joined) = tasks.try_join_next() {
            count(&mut done, joined);
        }
    }
    // every task runs to its end: one that failed doesn't cut the others short. Once stopping, the ones still at
    // work leave their spots for later at their next step
    loop {
        if stopping() {
            shared.stopping.store(true, Ordering::Relaxed);
        }
        match tokio::time::timeout(Duration::from_secs(1), tasks.join_next()).await {
            Ok(Some(joined)) => count(&mut done, joined),
            Ok(None) => break,
            Err(_) => {}
        }
    }
    Ok(done)
}

/// What a spot task came to: settled (`SAVED`, `SKIPPED`, or None for one more try), or left for a later pass.
enum Waited {
    Done(Option<i64>),
    Pending,
}

/// What a pass's spot tasks share.
struct Shared {
    /// NZB article bytes the pass may still read: each BODY reserves the most it can take first, and gives back
    /// what it didn't read
    budget: AtomicUsize,
    /// one spot at a time from inflating its NZB to tagging its release: decoding is the memory a spot takes (an
    /// NZB of up to 64 MB, up to 300,000 articles), and the copy checks, save and tag must not interleave
    decoding: tokio::sync::Mutex<()>,
    /// NZB keys (and NZB article sets, `ref:`) of spots being read, saved or tagged in this pass whose outcome isn't
    /// in spots_seen yet
    claims: std::sync::Mutex<std::collections::HashSet<String>>,
    /// set once the indexer is stopping: tasks leave their spots for later at their next step
    stopping: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn new(budget: usize) -> Shared {
        Shared {
            budget: AtomicUsize::new(budget),
            decoding: Default::default(),
            claims: Default::default(),
            stopping: Default::default(),
        }
    }

    /// Takes `n` bytes of the budget, or nothing when fewer are left.
    fn reserve(&self, n: usize) -> bool {
        self.budget.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(n)).is_ok()
    }

    fn give_back(&self, n: usize) {
        self.budget.fetch_add(n, Ordering::Relaxed);
    }

    /// Claims an NZB key for this spot until the guard goes, or None when another spot holds it.
    fn claim(self: &Arc<Self>, key: &str) -> Option<Claim> {
        let mut claims = self.claims.lock().unwrap();
        claims.insert(key.to_string()).then(|| Claim { shared: self.clone(), key: key.to_string() })
    }
}

/// An NZB key claimed in `Shared::claims`, released when dropped.
struct Claim {
    shared: Arc<Shared>,
    key: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.shared.claims.lock().unwrap().remove(&self.key);
    }
}

/// One spot task into the pass's counts. One whose outcome couldn't be recorded is logged, and its spot is fetched
/// again next pass.
fn count(done: &mut Pass, joined: std::result::Result<Result<Waited>, tokio::task::JoinError>) {
    match joined.map_err(|e| anyhow!("spot task failed: {e}")).and_then(|r| r) {
        Ok(Waited::Done(Some(SAVED))) => done.saved += 1,
        Ok(Waited::Done(Some(_))) => done.skipped += 1,
        Ok(Waited::Done(None)) => done.retry += 1,
        Ok(Waited::Pending) => done.pending += 1,
        Err(e) => {
            eprintln!("spots: {e:#}");
            done.retry += 1;
        }
    }
}

/// Where a pass starts reading: after the cursor, or `first` articles back from the newest on the first pass and
/// when the cursor is past what the server has (it renumbered).
pub fn start_at(cursor: Option<u64>, low: u64, high: u64, first: u64) -> u64 {
    match cursor {
        Some(c) if c <= high => c + 1,
        _ => high.saturating_sub(first).max(low).max(1),
    }
}

/// A spot fetched whole and saved, or skipped for good. Ok(None): left for a later pass, untouched. Err: try again
/// later. `claims` keep its NZB article set and NZB key claimed until the caller has recorded the outcome.
async fn fetch(
    pool: &Pool,
    db: &Db,
    message_id: &str,
    shared: &Arc<Shared>,
    claims: &mut Vec<Claim>,
) -> Result<Option<Settled>> {
    let skipped = |nzb_ref: Option<String>| Settled { state: SKIPPED, nzb_ref, ..Settled::default() };
    if shared.stopping.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let head = match pool.fetch_head(&format!("<{message_id}>"), MAX_XML_BYTES * 4).await {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(Some(skipped(None))),
        Err(e) if says_absent(&e) => return Ok(Some(skipped(None))),
        Err(e) => return Err(e.into()),
    };
    let Some(spot) = full::xml_from_headers(&head).as_deref().and_then(full::parse) else {
        return Ok(Some(skipped(None)));
    };
    let Some(kind) = spot.kind() else { return Ok(Some(skipped(None))) };
    // an id named twice is no real spot's, and would be read twice
    if spot.nzb.is_empty() || !nzb_ids_usable(&spot.nzb) {
        return Ok(Some(skipped(None)));
    }
    let spotter_id = header::spotter_id_of_head(&head);
    let row = |nzb_key: String| SpotRow {
        message_id: message_id.to_string(),
        kind,
        spot: spot.clone(),
        spotter_id: spotter_id.clone(),
        nzb_key,
    };

    // another spot pointing at the same NZB articles was settled already: no need to read them again
    let nzb_ref = release::ids_key(&spot.nzb);
    // another spot of this pass is reading the same articles: this one waits for the next pass, where `known_ref`
    // has the other's outcome, rather than reading them twice
    let Some(claimed) = shared.claim(&format!("ref:{nzb_ref}")) else { return Ok(None) };
    claims.push(claimed);
    let r = nzb_ref.clone();
    match on_db(db, move |conn| Ok(store::known_ref(conn, &r)?)).await? {
        Some((SAVED, Some(id))) => return copy_of(db, id, row(String::new()), nzb_ref, None).await.map(Some),
        Some(_) => return Ok(Some(skipped(Some(nzb_ref)))),
        None => {}
    }

    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(spot.nzb.len());
    for segment in &spot.nzb {
        let left = MAX_NZB_POSTED.saturating_sub(bodies.iter().map(Vec::len).sum::<usize>());
        let cap = MAX_NZB_ARTICLE.min(left);
        // the most this read can take off the wire, reserved before it's sent
        let most = cap + CAP_OVERSHOOT;
        if shared.stopping.load(Ordering::Relaxed) || !shared.reserve(most) {
            return Ok(None);
        }
        match pool.fetch_body_capped(&format!("<{segment}>"), cap).await {
            Ok(Some(body)) => {
                shared.give_back(most.saturating_sub(body.len()));
                bodies.push(body);
            }
            // a body past the cap was read up to it (and a line more) before it was given up: it keeps its reservation
            Ok(None) => return Ok(Some(skipped(Some(nzb_ref)))),
            Err(e) => {
                shared.give_back(most);
                if says_absent(&e) {
                    return Ok(Some(skipped(Some(nzb_ref))));
                }
                return Err(e.into());
            }
        }
    }
    // one spot at a time from here (`Shared::decoding`)
    let _decoding = shared.decoding.lock().await;
    let parts: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
    let Some(nzb) = binary::nzb(&parts) else { return Ok(Some(skipped(Some(nzb_ref)))) };
    let parsed = crate::archive::nzb::parse_keeping(&nzb, release::MAX_SEGMENTS);
    let Some(release) = release::release_of(&spot, &parsed) else { return Ok(Some(skipped(Some(nzb_ref)))) };
    let nzb_key = release::nzb_key(&parsed);
    let settled = |release_id: Option<i64>| Settled {
        state: if release_id.is_some() { SAVED } else { SKIPPED },
        release_id,
        nzb_ref: Some(nzb_ref.clone()),
        nzb_key: Some(nzb_key.clone()),
    };

    let group = release.group.clone();
    let shard = crate::store::shard_of(&group);
    // a spot with this NZB was saved or tagged in this pass and isn't in spots_seen yet: this one waits for the next
    // pass, where `known_key` finds it
    if shared.stopping.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let Some(claimed) = shared.claim(&nzb_key) else { return Ok(None) };
    claims.push(claimed);
    // an edit or repost of a spot already saved (its release maybe frozen since): that release gets the newer
    // spot, and nothing is saved
    let k = nzb_key.clone();
    if let Some(id) = on_db(db, move |conn| Ok(store::known_key(conn, &k)?)).await? {
        return copy_of(db, id, row(nzb_key.clone()), nzb_ref.clone(), Some(nzb_key.clone())).await.map(Some);
    }
    let row = row(nzb_key.clone());
    let copy_row = row.clone();
    if let Some(id) = on_shard(db, shard, move |conn| store::tag_copy(conn, &copy_row)).await? {
        return Ok(Some(settled(Some(id))));
    }
    // the title and group may already be a release. It is this post only when it holds every article of the NZB,
    // each in the file of the same name: then the spot adds nothing and just tags it. Any other release with the
    // name is another post, and this one gets a name of its own rather than adding articles or files to it.
    let mut by_file: Vec<(String, Vec<String>)> = Vec::new();
    for a in &release.articles {
        let file = a.filename.clone().unwrap_or_default();
        let id = a.message_id.trim_matches(['<', '>']).to_string();
        match by_file.last_mut() {
            Some((f, ids)) if *f == file => ids.push(id),
            _ => by_file.push((file, vec![id])),
        }
    }
    let (check_name, check_group, key) = (release.name.clone(), group.clone(), nzb_key.clone());
    let placed =
        on_shard(db, shard, move |conn| store::placement(conn, &check_name, &check_group, &key, &by_file)).await?;
    if let store::Placement::SamePost(id) = placed {
        on_shard(db, crate::store::shard_of_id(id), move |conn| store::tag_release(conn, id, &row)).await?;
        return Ok(Some(settled(Some(id))));
    }
    let mut release = release;
    if placed == store::Placement::Taken {
        let own = format!("{} [{}]", release.name, &nzb_key[..8]);
        release::rename(&mut release, &own);
    }
    let name = release.name.clone();
    db.save(vec![release]).await?;
    // tagged on its shard's writer after the save: the save doesn't say which id the release got
    let title = row.spot.title.clone();
    let id = on_shard(db, shard, move |conn| store::tag(conn, &name, &group, &row)).await?;
    if let Some(id) = id {
        println!("spots: saved \"{title}\" as release {id}");
    }
    Ok(Some(settled(id)))
}

/// A spot whose NZB an earlier spot's release `id` already has: that release gets this spot when it is live.
async fn copy_of(db: &Db, id: i64, mut row: SpotRow, nzb_ref: String, nzb_key: Option<String>) -> Result<Settled> {
    let shard = crate::store::shard_of_id(id);
    if row.nzb_key.is_empty() {
        let known = on_shard(db, shard, move |conn| {
            conn.query_row("select nzb_key from spots where release_id = ?", [id], |r| r.get::<_, Option<String>>(0))
                .optional()
                .map(Option::flatten)
        })
        .await?;
        row.nzb_key = known.unwrap_or_default();
    }
    let key = nzb_key.or_else(|| (!row.nzb_key.is_empty()).then(|| row.nzb_key.clone()));
    on_shard(db, shard, move |conn| store::tag_release(conn, id, &row)).await?;
    Ok(Settled { state: SAVED, release_id: Some(id), nzb_ref: Some(nzb_ref), nzb_key: key })
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
    fn the_pass_budget_is_reserved_before_reading_and_given_back_after() {
        let shared = Shared::new(100);
        assert!(shared.reserve(60));
        assert!(!shared.reserve(60), "only 40 left");
        shared.give_back(50);
        assert!(shared.reserve(90));
        assert_eq!(shared.budget.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_nzb_key_is_claimed_by_one_spot_until_its_guard_goes() {
        let shared = Arc::new(Shared::new(0));
        let first = shared.claim("k").unwrap();
        assert!(shared.claim("k").is_none());
        assert!(shared.claim("other").is_some());
        drop(first);
        assert!(shared.claim("k").is_some());
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
