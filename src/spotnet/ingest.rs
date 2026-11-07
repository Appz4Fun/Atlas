//! Reads new spots from `free.pt` every `poll_minutes`: the overview first, keeping the movie and TV spots whose
//! signatures hold (`sign`), then each of those whole (HEAD for its XML, BODY for its NZB in `alt.binaries.ftd`),
//! saved as a release and tagged with its spot. About 75 spots a day pass the filter. Spots are fetched
//! `concurrency` at a time: each request queues for a connection behind indexing, so one at a time was minutes a
//! spot on a busy indexer.
//!
//! Comments and spam reports on the spots kept (`comments`) are read by a task of their own, `run_comments`:
//! with a backlog of spots a spot pass takes an hour, and comments read after it waited that long.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use crate::config::SpotsConfig;
use crate::indexer::{Db, on_db};
use crate::nntp::{Pool, says_absent};

use super::store::{self, SAVED, SKIPPED, Settled, SpotRow};
use super::{MAX_XML_BYTES, SPOT_GROUP, binary, blacklist, comments, full, header, release, sign};

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
/// Spots decoded and on their way to being saved at once. Each save waits behind indexing's on its shard's writer
/// (seconds to minutes on a busy indexer), so one at a time was the pass's pace.
const DECODING: usize = 4;
/// NZB article bytes one pass reads at most; the spots left wait for the next pass.
const MAX_PASS_POSTED: usize = 256 * 1024 * 1024;
/// Spots saved before their XML was kept, read again per pass (a HEAD each, a few KB).
const XML_BACKFILL: usize = 50;

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

/// Runs spot passes until `stopping`.
pub async fn run(pool: Arc<Pool>, db: Db, config: SpotsConfig, stopping: impl Fn() -> bool + Send + 'static) {
    if let Err(e) = on_db(&db, |conn| Ok(store::create_main(conn)?)).await {
        eprintln!("spots: {e:#}");
        return;
    }
    for shard in 0..crate::store::SHARDS {
        match on_shard(&db, shard, store::recategorize_all).await {
            Ok(0) => {}
            Ok(n) => println!("spots: {n} releases of shard {shard} queued for their category, with their spot's kind"),
            Err(e) => eprintln!("spots: queueing shard {shard}'s spot releases for their category: {e:#}"),
        }
    }
    while !stopping() {
        match pass(&pool, &db, &config, &stopping).await {
            Ok(p) if p.noted + p.saved + p.skipped + p.retry + p.pending > 0 => println!(
                "spots: read {} overview lines, {} new movie/TV spots, saved {}, skipped {}, to retry {}, left for later {}",
                p.read, p.noted, p.saved, p.skipped, p.retry, p.pending
            ),
            Ok(_) => {}
            Err(e) => eprintln!("spots: {e:#}"),
        }
        crate::bg_indexer::nap(poll(&config), &stopping).await;
    }
}

/// Runs comment passes until `stopping`, beside the spots' (`run`) and never behind them.
/// Its tables are made with the main database's (`store::create_main`).
pub async fn run_comments(pool: Arc<Pool>, db: Db, config: SpotsConfig, stopping: impl Fn() -> bool + Send + 'static) {
    while !stopping() {
        let more = match said(&pool, &db, &config, &stopping).await {
            Ok(s) => {
                if s != Said::default() {
                    println!(
                        "spots: {} new comments, {} new spam reports, read {} comments' text ({} gone, {} to retry)",
                        s.comments, s.reports, s.read, s.gone, s.retry
                    );
                }
                s.more
            }
            Err(e) => {
                eprintln!("spots: comments: {e:#}");
                false
            }
        };
        if !more {
            crate::bg_indexer::nap(poll(&config), &stopping).await;
        }
    }
}

/// The time between passes.
fn poll(config: &SpotsConfig) -> Duration {
    Duration::from_secs(u64::from(config.poll_minutes) * 60)
}

/// One pass: the overview since the cursor, then up to `per_pass` spots fetched whole.
pub async fn pass(pool: &Arc<Pool>, db: &Db, config: &SpotsConfig, stopping: &impl Fn() -> bool) -> Result<Pass> {
    let mut done = Pass::default();
    (done.read, done.noted) = read_overview(pool, db, config, stopping).await?;

    let now = chrono::Utc::now().timestamp();
    // the comment task prunes its own tables when it runs
    let comments_off = !config.comments;
    on_db(db, move |conn| Ok(store::prune(conn, now)? + if comments_off { comments::prune(conn, now)? } else { 0 }))
        .await?;
    if config.blacklist_url.is_empty() {
        // turned off: the list kept goes too, so it no longer removes anything
        match on_db(db, |conn| Ok(blacklist::clear(conn)?)).await {
            Ok(0) => {}
            Ok(n) => println!("spots: the blacklist is off, {n} spotters taken off it"),
            Err(e) => eprintln!("spots: blacklist: {e:#}"),
        }
    } else {
        refresh_blacklist(db, &config.blacklist_url, now).await;
    }
    match backfill_xml(pool, db, stopping).await {
        Ok(0) => {}
        Ok(n) => println!("spots: read the XML of {n} spots saved before it was kept"),
        Err(e) => eprintln!("spots: reading saved spots' XML: {e:#}"),
    }
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

/// Reads the blacklist again when it's a day old. A failed read is logged, the list kept stays, and it's tried
/// again a day later.
async fn refresh_blacklist(db: &Db, url: &str, now: i64) {
    match on_db(db, move |conn| Ok(blacklist::due(conn, now)?)).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => return eprintln!("spots: blacklist: {e:#}"),
    }
    let u = url.to_string();
    let read = tokio::task::spawn_blocking(move || blacklist::fetch(&u).map(|xml| blacklist::parse(&xml)))
        .await
        .map_err(|e| anyhow!("{e}"))
        .and_then(|r| r);
    let done = match read {
        Ok(spotters) => on_db(db, move |conn| blacklist::replace(conn, &spotters, now)).await,
        Err(e) => Err(e),
    };
    match done {
        Ok(n) => println!("spots: the blacklist names {n} spotters"),
        Err(e) => {
            eprintln!("spots: blacklist {url}: {e:#}");
            if let Err(e) = on_db(db, move |conn| Ok(blacklist::tried(conn, now)?)).await {
                eprintln!("spots: blacklist: {e:#}");
            }
        }
    }
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
    /// `DECODING` spots at a time from inflating their NZB to tagging their release: decoding is the memory a spot
    /// takes (an NZB of up to 64 MB, up to 300,000 articles), held until its save is done
    decoding: tokio::sync::Semaphore,
    /// one spot of a shard at a time through the copy checks, save and tag: spots of one title go to the same group,
    /// and so the same shard (copies of one NZB in other shards are kept apart by `claims`)
    saving: Vec<tokio::sync::Mutex<()>>,
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
            decoding: tokio::sync::Semaphore::new(DECODING),
            saving: (0..crate::store::SHARDS).map(|_| Default::default()).collect(),
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
    // each step timed (`atlas_function_duration_seconds{function="spots.*"}`), to see where a slow pass waits
    let head = timed("spots.head", pool.fetch_head(&format!("<{message_id}>"), MAX_XML_BYTES * 4)).await;
    let head = match head {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(Some(skipped(None))),
        Err(e) if says_absent(&e) => return Ok(Some(skipped(None))),
        Err(e) => return Err(e.into()),
    };
    // checked again here, for spots noted before the overview was checked
    let (subject, from) = (header::header_value(&head, "Subject"), header::header_value(&head, "From"));
    let verdict = match (subject, from) {
        (Some(subject), Some(from)) => {
            header::parse(&subject, &from).map(|h| sign::verify(&h.title, &from, message_id)).unwrap_or_default()
        }
        _ => sign::Verdict::default(),
    };
    if !verdict.verified {
        return Ok(Some(skipped(None)));
    }
    let Some(xml) = full::xml_from_headers(&head) else { return Ok(Some(skipped(None))) };
    let Some(spot) = full::parse(&xml) else { return Ok(Some(skipped(None))) };
    let Some(kind) = spot.kind() else { return Ok(Some(skipped(None))) };
    // an id named twice is no real spot's, and would be read twice
    if spot.nzb.is_empty() || !nzb_ids_usable(&spot.nzb) {
        return Ok(Some(skipped(None)));
    }
    let spotter_id = verdict.spotter_id;
    let row = |nzb_key: String| SpotRow {
        message_id: message_id.to_string(),
        kind,
        spot: spot.clone(),
        spotter_id: spotter_id.clone(),
        nzb_key,
        xml: xml.clone(),
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
        match timed("spots.body", pool.fetch_body_capped(&format!("<{segment}>"), cap)).await {
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
    // `DECODING` spots at a time from here (`Shared::decoding`)
    let span = crate::metrics::span("spots.wait_turn");
    let _decoding = shared.decoding.acquire().await.expect("the pass's semaphore is never closed");
    drop(span);
    let parts: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
    let Some(nzb) = binary::nzb(&parts) else { return Ok(Some(skipped(Some(nzb_ref)))) };
    let parsed = crate::archive::nzb::parse_keeping(&nzb, release::MAX_SEGMENTS);
    let Some(release) = release::release_of(&spot, &parsed) else { return Ok(Some(skipped(Some(nzb_ref)))) };
    let nzb_key = release::nzb_key(&parsed);
    // the release and its key are all that's needed from here: the rest of what was decoded goes now, not after the save
    drop(parts);
    drop((parsed, nzb, bodies));
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
    let span = crate::metrics::span("spots.wait_shard");
    let _saving = shared.saving[shard].lock().await;
    drop(span);
    if shared.stopping.load(Ordering::Relaxed) {
        return Ok(None);
    }
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
    let (check_name, check_group) = (release.name.clone(), group.clone());
    let in_use = on_shard(db, shard, move |conn| store::name_in_use(conn, &check_name, &check_group)).await?;
    let placed = if in_use {
        // rare, and can be heavy (reading a big release's ids): with the stage work, in its share
        let (check_name, check_group, key) = (release.name.clone(), group.clone(), nzb_key.clone());
        on_shard_queued(db, shard, move |conn| store::placement(conn, &check_name, &check_group, &key, &by_file))
            .await?
    } else {
        store::Placement::Free
    };
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
    timed("spots.save", db.save(vec![release])).await?;
    // tagged on its shard's writer after the save: the save doesn't say which id the release got
    let title = row.spot.title.clone();
    let id = on_shard(db, shard, move |conn| store::tag(conn, &name, &group, &row)).await?;
    if let Some(id) = id {
        println!("spots: saved \"{title}\" as release {id}");
    }
    Ok(Some(settled(id)))
}

/// Reads the `free.pt` overview since its cursor and notes the spots worth fetching. Returns the overview lines read
/// and the spots noted.
async fn read_overview(
    pool: &Arc<Pool>,
    db: &Db,
    config: &SpotsConfig,
    stopping: &impl Fn() -> bool,
) -> Result<(u64, usize)> {
    let (mut read, mut noted) = (0, 0);
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
        read += rows.len() as u64;
        let now = chrono::Utc::now().timestamp();
        let removals: Vec<store::Disposal> = rows.iter().filter_map(|r| removal(r, now)).collect();
        let wanted: Vec<store::Noted> = rows
            .iter()
            .filter_map(|r| {
                let h = header::parse(&r.subject, &r.from).filter(header::SpotHeader::wanted)?;
                // a spot whose signatures don't hold is one anyone could have posted: Spotweb drops it too
                let verdict = sign::verify(&h.title, &r.from, &r.message_id);
                if !verdict.verified {
                    return None;
                }
                Some((r.message_id.trim_matches(['<', '>']).to_string(), r.number, verdict.spotter_id))
            })
            .collect();
        let key = host.clone();
        // the removals first: the cursor moves with the spots
        noted += on_db(db, move |conn| {
            let removed = store::note_disposals(conn, &removals)?;
            if removed > 0 {
                println!("spots: {removed} spots or comments removed by a moderator or their spotter");
            }
            Ok(store::note_read(conn, &key, to, &wanted, now)?)
        })
        .await?;
        from = to + 1;
    }
    Ok((read, noted))
}

/// The removal an overview line is, when it's a moderator's (or a spotter's own) `DISPOSE` whose signatures hold.
/// A spotter's own needs their key to have signed its title and message-id: it removes only their posts.
fn removal(r: &crate::nntp::Overview, now: i64) -> Option<store::Disposal> {
    let h = header::parse(&r.subject, &r.from)?;
    let message_id = h.removes()?;
    let verdict = sign::verify(&h.title, &r.from, &r.message_id);
    if !verdict.verified {
        return None;
    }
    // their key must sign both the title, which names the target, and the message-id: the header signature alone
    // could be replayed onto another message-id, the message-id's alone onto another title
    let spotter = match verdict.personal {
        true if verdict.message_id_signed && verdict.title_signed => verdict.spotter_id?,
        true => return None,
        false => String::new(),
    };
    let at = crate::dates::posted_timestamp(&r.date).unwrap_or(now);
    Some(store::Disposal { message_id, spotter, at })
}

/// What one pass read of what's said about spots, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
struct Said {
    comments: usize,
    reports: usize,
    read: usize,
    gone: usize,
    retry: usize,
    /// the texts stopped at the pass's time, with more waiting: the next pass comes without a nap
    more: bool,
}

/// The comments and spam reports posted since the last pass, then up to `comments_per_pass` texts of comments on
/// kept spots (a BODY each, a few hundred bytes), for a poll at most. A group that can't be read is logged and
/// skipped, and the texts are read whatever came of the groups. The first failed read ends the texts for this pass.
async fn said(pool: &Arc<Pool>, db: &Db, config: &SpotsConfig, stopping: &impl Fn() -> bool) -> Result<Said> {
    let mut done = Said::default();
    // these tables are this task's to prune; a failed prune doesn't hold up the rest
    let now = chrono::Utc::now().timestamp();
    if let Err(e) = on_db(db, move |conn| Ok(comments::prune(conn, now)?)).await {
        eprintln!("spots: comments: prune: {e:#}");
    }
    for (group, reports) in [(comments::COMMENT_GROUP, false), (comments::REPORT_GROUP, true)] {
        match read_group(pool, db, group, reports, config.comment_days, stopping).await {
            Ok(n) if reports => done.reports = n,
            Ok(n) => done.comments = n,
            Err(e) => eprintln!("spots: {group}: {e:#}"),
        }
    }

    let limit = config.comments_per_pass;
    let todo = on_db(db, move |conn| Ok(comments::todo(conn, limit)?)).await?;
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // a quarter of the spots' share: texts can wait, spots' saves shouldn't
    let slots = Arc::new(tokio::sync::Semaphore::new((config.concurrency / 4).max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    let mut outcomes: Vec<(String, comments::Outcome)> = Vec::new();
    // no longer than a poll: the groups are read again at the next pass, whatever texts are left
    let started = std::time::Instant::now();
    for message_id in todo {
        if stopping() || failed.load(Ordering::Relaxed) {
            break;
        }
        if started.elapsed() >= poll(config) {
            done.more = true;
            break;
        }
        let slot = slots.clone().acquire_owned().await.expect("the pass's semaphore is never closed");
        let (pool, failed) = (pool.clone(), failed.clone());
        tasks.spawn(async move {
            let _slot = slot;
            let id = format!("<{message_id}>");
            let outcome = match timed("spots.comment", pool.fetch_body_capped(&id, comments::MAX_BODY)).await {
                Ok(Some(raw)) => comments::Outcome::Read(comments::body_text(&raw)),
                // too big to be a comment
                Ok(None) => comments::Outcome::Gone,
                Err(e) if says_absent(&e) => comments::Outcome::Gone,
                Err(e) => {
                    eprintln!("spots: comment {message_id}: {e:#}");
                    failed.store(true, Ordering::Relaxed);
                    comments::Outcome::Failed
                }
            };
            (message_id, outcome)
        });
        while let Some(joined) = tasks.try_join_next() {
            outcomes.extend(joined.map_err(|e| eprintln!("spots: comment task failed: {e}")).ok());
        }
        if outcomes.len() >= SETTLE_BATCH {
            settle_comments(db, std::mem::take(&mut outcomes), &mut done).await?;
        }
    }
    while let Some(joined) = tasks.join_next().await {
        outcomes.extend(joined.map_err(|e| eprintln!("spots: comment task failed: {e}")).ok());
    }
    settle_comments(db, outcomes, &mut done).await?;
    Ok(done)
}

/// Comments' texts read before they're written, together.
const SETTLE_BATCH: usize = 200;

/// What came of reading these comments' texts, in one transaction, counted into `done`.
async fn settle_comments(db: &Db, outcomes: Vec<(String, comments::Outcome)>, done: &mut Said) -> Result<()> {
    if outcomes.is_empty() {
        return Ok(());
    }
    for (_, outcome) in &outcomes {
        match outcome {
            comments::Outcome::Read(_) => done.read += 1,
            comments::Outcome::Gone => done.gone += 1,
            comments::Outcome::Failed => done.retry += 1,
        }
    }
    on_db(db, move |conn| Ok(comments::settle(conn, &outcomes)?)).await
}

/// Reads `group`'s overview since its cursor (`days` back on the first pass) and notes every comment, or with
/// `reports` every spam report, on any spot: whether its spot is kept is settled later (`comments::todo`,
/// `comments::prune`). Returns how many were new.
async fn read_group(
    pool: &Arc<Pool>,
    db: &Db,
    group: &'static str,
    reports: bool,
    days: u32,
    stopping: &impl Fn() -> bool,
) -> Result<usize> {
    let prefer = pool.indexing_tier().first().copied().unwrap_or(0);
    let (server, (_, low, high, _)) = pool.select_group_on(prefer, group).await?;
    let host = pool.host(server);
    let key = host.clone();
    let cursor = on_db(db, move |conn| Ok(comments::cursor(conn, group, &key)?)).await?;
    let mut from = match cursor {
        Some(c) if c <= high => (c + 1).max(low),
        // the first pass, or the server renumbered: from `days` ago
        _ => {
            let when = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
            let from = pool.article_at(server, group, low.max(1), high, when).await?;
            println!(
                "spots: reading {group} on {host} from article {from} ({days} days back), {} to go",
                (high + 1).saturating_sub(from)
            );
            if from > high {
                // nothing that recent: the cursor still goes to the end, so the next pass doesn't search again
                let key = host.clone();
                let now = chrono::Utc::now().timestamp();
                on_db(db, move |conn| Ok(comments::note_read(conn, group, &key, high, &[], reports, now)?)).await?;
                return Ok(0);
            }
            from
        }
    };
    let mut added = 0;
    while from <= high && !stopping() {
        let to = high.min(from + XOVER_STEP - 1);
        let rows = match pool.xover_on(server, group, from, to).await {
            Ok(rows) => rows,
            Err(e) if e.is_empty_range() => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let now = chrono::Utc::now().timestamp();
        let found: Vec<comments::Noted> = rows
            .iter()
            .filter_map(|r| {
                let (spot, rating) = if reports {
                    (comments::report_ref(&r.subject, &r.references)?, 0)
                } else {
                    comments::comment_ref(&r.message_id)?
                };
                Some(comments::Noted {
                    message_id: r.message_id.trim().trim_matches(['<', '>']).to_string(),
                    spot,
                    at: crate::dates::posted_timestamp(&r.date).unwrap_or(now),
                    rating,
                    poster: comments::poster_name(&r.from),
                    spotter_id: header::spotter_id_of_from(&r.from),
                })
            })
            .collect();
        let key = host.clone();
        added += on_db(db, move |conn| Ok(comments::note_read(conn, group, &key, to, &found, reports, now)?)).await?;
        from = to + 1;
    }
    Ok(added)
}

/// Reads again the XML of up to `XML_BACKFILL` spots saved before it was kept, so their release pages get the
/// description and the rest. A spot whose article is gone, or whose XML won't read, gets an empty one and isn't
/// asked for again. The first other error ends it for this pass; spots are taken in no order (`missing_xml`), so
/// one that keeps failing doesn't keep the rest waiting. Returns how many were filled.
async fn backfill_xml(pool: &Pool, db: &Db, stopping: &impl Fn() -> bool) -> Result<usize> {
    let mut filled = 0;
    let mut left = XML_BACKFILL;
    for shard in 0..crate::store::SHARDS {
        if left == 0 || stopping() {
            break;
        }
        let limit = left;
        let missing = on_shard(db, shard, move |conn| store::missing_xml(conn, limit)).await?;
        left -= missing.len();
        for (release_id, message_id) in missing {
            if stopping() {
                return Ok(filled);
            }
            let head = timed("spots.head", pool.fetch_head(&format!("<{message_id}>"), MAX_XML_BYTES * 4)).await;
            let xml = match head {
                Ok(Some(head)) => full::xml_from_headers(&head).filter(|x| full::parse(x).is_some()),
                Ok(None) => None,
                Err(e) if says_absent(&e) => None,
                // the servers aren't answering: the rest wait for the next pass, rather than each waiting out the
                // deadline in turn ahead of the pass's new spots
                Err(e) => {
                    eprintln!("spots: {message_id}: {e:#}");
                    return Ok(filled);
                }
            };
            let xml = xml.unwrap_or_default();
            let whole = !xml.is_empty();
            let set = on_shard(db, shard, move |conn| store::set_xml(conn, release_id, &message_id, &xml)).await?;
            if set && whole {
                filled += 1;
            }
        }
    }
    Ok(filled)
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

/// Runs `work` on `shard`'s writer, in its own transaction, and hands back what it returned. On the writer's lane
/// for small jobs someone waits on (`Db::on_writer_first`): behind the processing stage's queued work, one of these
/// waited many minutes on prod.
async fn on_shard<T: Send + 'static>(
    db: &Db,
    shard: usize,
    work: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T> {
    on_writer(db, shard, true, work).await
}

/// `on_shard` for a job that can be heavy: queued with the stage work, in its share of the writer's time.
async fn on_shard_queued<T: Send + 'static>(
    db: &Db,
    shard: usize,
    work: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T> {
    on_writer(db, shard, false, work).await
}

async fn on_writer<T: Send + 'static>(
    db: &Db,
    shard: usize,
    first: bool,
    work: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T> {
    let out = Arc::new(std::sync::Mutex::new(None));
    let into = out.clone();
    let job: crate::indexer::StageWork = Box::new(move |conn| {
        *into.lock().unwrap() = Some(work(conn)?);
        Ok(())
    });
    let db = db.clone();
    // the queued kind blocks while the writer's queue is full: off the runtime's threads, like the wait for the answer
    let answered = timed("spots.writer", async move {
        tokio::task::spawn_blocking(move || {
            let answer = if first {
                db.on_writer_first(shard, "db.stage.spots", job)
            } else {
                db.on_writer(shard, "db.stage.spots", job)
            }?;
            answer.recv().map_err(|_| anyhow!("db writer stopped"))?.map_err(|e| anyhow!("spots: {e}"))
        })
        .await
        .map_err(|e| anyhow!("spot task failed: {e}"))?
    })
    .await;
    answered?;
    let value = out.lock().unwrap().take();
    value.ok_or_else(|| anyhow!("spots: the writer gave no answer"))
}

/// `fut`, timed as `function` in `atlas_function_duration_seconds`, and counted as failed when it errs.
async fn timed<T, E>(
    function: &'static str,
    fut: impl std::future::Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, E> {
    let span = crate::metrics::span(function);
    let result = fut.await;
    if result.is_err() {
        span.failed();
    }
    result
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
