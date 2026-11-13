use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pg::sql::{Sql, vals};
use anyhow::{Result, anyhow};

use crate::config::{DEFAULT_BATCH_SIZE, DEFAULT_REQUEST_SIZE};
use crate::db;
use crate::nfo;
use crate::nntp::{BlockingPool, Extract, Overview, Pool, Retention, first_names, headers_to_articles};
use crate::par2;
use crate::parser::{Release, is_complete};

/// What one finished XOVER slice added, reported as it lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub articles: i64,
    pub bytes: i64,
    pub releases: i64,
    /// articles left out as obfuscated per article, also in `articles`
    pub skipped: i64,
}

impl Progress {
    fn add(&mut self, other: &Progress) {
        self.articles += other.articles;
        self.bytes += other.bytes;
        self.releases += other.releases;
        self.skipped += other.skipped;
    }
}

/// progress callback that ignores everything
pub fn no_progress(_: &Progress) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Backfill,
    Live,
}

/// per group phase/idle/backfilling
#[derive(Clone, Debug)]
pub struct GroupRunState {
    pub phase: Phase,
    pub idle: bool,
    pub backfilling: bool,
    /// a big group found unsplittable isnt probed again until then
    pub no_split_until: Option<Instant>,
    /// a split group's servers are looked at for older days again then
    pub reach_back_after: Option<Instant>,
    /// per server host (not its place in the pool, which a config reload can
    /// give to another), the oldest day it keeps of the group (as found by a
    /// day chunk too old for it), until it's looked at again
    pub keeps_from: HashMap<String, (i64, Instant)>,
    /// likewise the day of its first post, `i64::MAX` when it has none or
    /// doesnt carry the group (see `drop_unkept_days`); a server not carrying
    /// the group is noted for good (no expiry): the scheduler skips it for
    /// good too
    pub first_post_days: HashMap<String, (i64, Option<Instant>)>,
    /// XOVER slices a server refused with a 5xx, still being tried again (see `RunStates::refused`)
    pub refused: Vec<Refused>,
    /// its last backfill turn went live instead (`live_first`): the next one is its own
    pub backfill_deferred: bool,
}

/// A slice of `key`'s numbers (a cursor key, soo one server's) its server
/// answered XOVER with a 5xx: when that was first, and how many times since.
#[derive(Clone, Debug)]
pub struct Refused {
    pub key: String,
    pub start: u64,
    pub end: u64,
    pub since: Instant,
    pub tries: u32,
    /// refused last at: one not asked again for `REFUSED_IDLE` is out of reach
    /// (past retention, or the group gone) and forgotten
    pub last: Instant,
    /// passed over, last asked at: kept while it is still asked, soo a day
    /// chunk read again whole passes over it again instead of starting its hour over
    pub passed: Option<Instant>,
}

/// A slice refused with a 5xx is asked again on later passes, and only once it
/// was refused at least `REFUSED_TRIES` times over at least `REFUSED_FOR` is it
/// taken for unavailable and passed over: a provider's overview having trouble
/// for a while must not cost the articles in it for good.
pub const REFUSED_FOR: Duration = Duration::from_secs(3600);
pub const REFUSED_TRIES: u32 = 3;
/// refused slices still asked again noted per group at most; past it a
/// refusal isnt noted until there is room
const REFUSED_KEPT: usize = 1024;
/// a refusal not asked again this long is forgotten: anything still being
/// read asks again well within it
pub const REFUSED_IDLE: Duration = Duration::from_secs(6 * 3600);
/// passed over ones kept per group at most, the longest not asked go first
const PASSED_KEPT: usize = 4096;

/// the days of a split only its deepest server has that are chunks at once
/// (see `chunks::Speculative`), and the articles a day those done have to
/// hold on average to reach this many further
const SPECULATIVE_WINDOW: i64 = 60;
const SPECULATIVE_YIELD: i64 = 100;

impl Default for GroupRunState {
    fn default() -> Self {
        GroupRunState {
            phase: Phase::Backfill,
            idle: false,
            backfilling: false,
            no_split_until: None,
            reach_back_after: None,
            keeps_from: HashMap::new(),
            first_post_days: HashMap::new(),
            refused: Vec::new(),
            backfill_deferred: false,
        }
    }
}

/// Run state of every group, shared by passes running at the same time, and the groups config.toml tracks now (see
/// `groups`).
#[derive(Clone, Default)]
pub struct RunStates(
    Arc<Mutex<HashMap<String, GroupRunState>>>,
    Arc<Mutex<Option<HashSet<String>>>>,
    /// cursor key -> the time read through elsewhere its cursor was last moved up to (`time_cursor_start`)
    Arc<Mutex<HashMap<String, i64>>>,
);

impl RunStates {
    /// The time read through elsewhere the cursor `key` was last moved up to by date (`time_cursor_start`).
    fn aligned(&self, key: &str) -> Option<i64> {
        self.2.lock().unwrap().get(key).copied()
    }

    fn set_aligned(&self, key: &str, through: i64) {
        self.2.lock().unwrap().insert(key.to_string(), through);
    }

    fn with<R>(&self, group: &str, f: impl FnOnce(&mut GroupRunState) -> R) -> R {
        f(self.0.lock().unwrap().entry(group.to_string()).or_default())
    }

    pub fn is_idle(&self, group: &str) -> bool {
        self.0.lock().unwrap().get(group).is_some_and(|s| s.idle)
    }

    pub fn is_backfilling(&self, group: &str) -> bool {
        self.0.lock().unwrap().get(group).is_some_and(|s| s.backfilling)
    }

    pub fn all_idle(&self, groups: &[String]) -> bool {
        let states = self.0.lock().unwrap();
        groups.iter().all(|g| states.get(g).is_some_and(|s| s.idle))
    }

    /// Note `groups` as the ones config.toml tracks now.
    pub fn set_tracked(&self, groups: &[String]) {
        *self.1.lock().unwrap() = Some(groups.iter().cloned().collect());
    }

    /// The groups config.toml tracks now (`set_tracked`), or until those are noted the ones passes ran for: a group
    /// taken out keeps its run state, but no longer counts here.
    pub fn groups(&self) -> HashSet<String> {
        if let Some(tracked) = self.1.lock().unwrap().as_ref() {
            return tracked.clone();
        }
        self.0.lock().unwrap().keys().cloned().collect()
    }

    /// The oldest day the server `host` keeps of `group`, `i64::MIN` when not known.
    pub fn keeps_from(&self, group: &str, host: &str) -> i64 {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        let found = states.get(group).and_then(|s| s.keeps_from.get(host));
        found.filter(|(_, until)| *until > now).map_or(i64::MIN, |(day, _)| *day)
    }

    /// Note `day` as the oldest the server `host` keeps of `group`, until it's looked at again.
    fn keep_from(&self, group: &str, host: &str, day: i64) {
        self.with(group, |st| st.keeps_from.insert(host.to_string(), (day, Instant::now() + KEEPS_RECHECK)));
    }

    /// Note `day` as the day of the server `host`'s first post of `group`,
    /// until it's looked at again.
    fn first_post_on(&self, group: &str, host: &str, day: i64) {
        self.with(group, |st| st.first_post_days.insert(host.to_string(), (day, Some(Instant::now() + KEEPS_RECHECK))));
    }

    /// Note the server `host` as not carrying `group`, with no expiry.
    fn not_carrying(&self, group: &str, host: &str) {
        self.with(group, |st| st.first_post_days.insert(host.to_string(), (i64::MAX, None)));
    }

    /// The day of the server `host`'s first post of `group`, `i64::MIN` when not known.
    fn first_post_day(&self, group: &str, host: &str) -> i64 {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        let found = states.get(group).and_then(|s| s.first_post_days.get(host));
        found.filter(|(_, until)| until.is_none_or(|u| u > now)).map_or(i64::MIN, |(day, _)| *day)
    }

    /// The groups whose oldest day on the server `host` is known, with that day.
    pub fn kept_days(&self, host: &str) -> HashMap<String, i64> {
        let now = Instant::now();
        let states = self.0.lock().unwrap();
        states
            .iter()
            .filter_map(|(g, s)| s.keeps_from.get(host).filter(|(_, until)| *until > now).map(|(d, _)| (g.clone(), *d)))
            .collect()
    }

    /// `start..=end` of `key` (a cursor key of `group`) was refused with a 5xx
    /// at `now`. True: it has been, for `REFUSED_FOR` and `REFUSED_TRIES` times,
    /// soo it is passed over now. False: it is asked again later. Slice bounds
    /// move between passes, soo a refusal overlapping an earlier one of the same
    /// key counts as the same. A range passed over stays passed over while it
    /// is asked again (a day chunk is read again whole until every slice of it
    /// is done) and is forgotten once not asked for `REFUSED_FOR`.
    pub fn refused(&self, group: &str, key: &str, (start, end): (u64, u64), now: Instant) -> bool {
        self.with(group, |st| {
            st.refused.retain(|r| match r.passed {
                Some(at) => now.saturating_duration_since(at) < REFUSED_FOR,
                None => now.saturating_duration_since(r.last) < REFUSED_IDLE,
            });
            let passed = |r: &Refused| r.passed.is_some() && r.key == key && r.start <= start && end <= r.end;
            if let Some(r) = st.refused.iter_mut().find(|r| passed(r)) {
                r.passed = Some(now);
                return true;
            }
            let same = |r: &Refused| r.passed.is_none() && r.key == key && r.start <= end && start <= r.end;
            let Some(at) = st.refused.iter().position(same) else {
                // full: this one waits for room and is asked again meanwhile. Dropping
                // an older one would start its hour over, and with more refused slices
                // a pass than room none would ever reach it.
                if st.refused.iter().filter(|r| r.passed.is_none()).count() >= REFUSED_KEPT {
                    return false;
                }
                st.refused.push(Refused {
                    key: key.to_string(),
                    start,
                    end,
                    since: now,
                    tries: 1,
                    last: now,
                    passed: None,
                });
                return false;
            };
            let r = &mut st.refused[at];
            r.tries += 1;
            r.last = now;
            (r.start, r.end) = (r.start.min(start), r.end.max(end));
            let give_up = r.tries >= REFUSED_TRIES && now.saturating_duration_since(r.since) >= REFUSED_FOR;
            if give_up {
                r.passed = Some(now);
                let passed = st.refused.iter().filter(|r| r.passed.is_some()).count();
                if passed > PASSED_KEPT
                    && let Some(oldest) = (0..st.refused.len())
                        .filter(|i| st.refused[*i].passed.is_some())
                        .min_by_key(|i| st.refused[*i].passed)
                {
                    st.refused.remove(oldest);
                }
            }
            give_up
        })
    }

    /// `start..=end` of `key` was read: refusals of it are over. Only of it:
    /// slice bounds move, soo what of a refused range is left either side
    /// keeps its history.
    pub fn read(&self, group: &str, key: &str, (start, end): (u64, u64)) {
        let mut states = self.0.lock().unwrap();
        if let Some(st) = states.get_mut(group)
            && !st.refused.is_empty()
        {
            let mut left = Vec::new();
            st.refused.retain(|r| {
                if r.key != key || end < r.start || r.end < start {
                    return true;
                }
                if r.start < start {
                    left.push(Refused { end: start - 1, key: r.key.clone(), ..*r });
                }
                if end < r.end {
                    left.push(Refused { start: end + 1, key: r.key.clone(), ..*r });
                }
                false
            });
            st.refused.extend(left);
        }
    }

    /// switching modes starts every group over in the backfill phase
    pub fn reset(&self) {
        for st in self.0.lock().unwrap().values_mut() {
            *st = GroupRunState::default();
        }
    }
}

/// How a pass indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassSettings {
    pub mode: String,
    /// article numbers per pass over a group
    pub batch_size: i64,
    /// article numbers per XOVER request
    pub request_size: u64,
    /// backfill left on a group's home server before it is split into day chunks
    pub split_min_backlog: i64,
    /// article numbers a day chunk reaches past its ends (`CHUNK_SAFETY`)
    pub chunk_safety: u64,
    /// a pass on a server standing in for the group's own one while that is in trouble: GROUP there only (the group's
    /// home doesnt move), and a cursor started there leaves the group's phase alone
    pub fallback: bool,
    /// with `fallback`: the first such pass on this server since the group's own one got into trouble. Its live
    /// cursor starts at the newest article, since what came before was read on the group's own server (or will be
    /// once it is back), and a cursor left from an earlier stand-in would read all of it again
    pub from_top: bool,
}

impl Default for PassSettings {
    fn default() -> Self {
        PassSettings {
            mode: "dynamic".into(),
            batch_size: DEFAULT_BATCH_SIZE as i64,
            request_size: DEFAULT_REQUEST_SIZE,
            split_min_backlog: crate::config::SPLIT_MIN_BACKLOG,
            chunk_safety: CHUNK_SAFETY,
            fallback: false,
            from_top: false,
        }
    }
}

/// slices saved together in one transaction at most.
const MAX_BATCH: usize = 32;

/// The indexer's checked PostgreSQL sessions, shared by every pass.
/// Cursor writes and slice admission retain the original native destination.
#[derive(Clone)]
pub struct Db {
    conn: Arc<MainConnection>,
    native: Option<crate::pg::ingest::Native>,
}

pub use crate::pg::ingest::PgWriteLimits;

impl Db {
    pub(crate) fn bind_native_cache_scope(&self) -> Result<Option<crate::pg::cache_actor::NativeCacheScope>> {
        match &*self.conn {
            MainConnection::OwnedPostgres(owner) => crate::pg::cache_actor::NativeCacheScope::bind(owner).map(Some),
            MainConnection::Postgres(_) => anyhow::bail!("cache commands require the fenced native owner"),
        }
    }

    pub(crate) fn fail_native(&self, failure: crate::pg::outage::Failure) {
        if let MainConnection::OwnedPostgres(owner) = &*self.conn {
            owner.fail(failure);
        }
    }
}

enum MainConnection {
    Postgres(Arc<crate::pg::Pool>),
    OwnedPostgres(Arc<crate::pg::indexer_runtime::Owner>),
}

impl MainConnection {
    fn with_sql<T>(&self, f: impl FnOnce(&dyn Sql) -> Result<T>) -> Result<T> {
        let t = std::time::Instant::now();
        match self {
            Self::Postgres(pool) => {
                let conn = pool.get()?;
                crate::profile::DB_WAIT.add_since(t);
                f(&crate::pg::sql::Pg::new(conn))
            }
            Self::OwnedPostgres(owner) => owner.with(|conn| {
                crate::profile::DB_WAIT.add_since(t);
                f(&crate::pg::sql::Pg::new(conn))
            }),
        }
    }
}

pub(crate) fn owned_postgres_db(
    mut db: Db,
    owner: Arc<crate::pg::indexer_runtime::Owner>,
    limits: PgWriteLimits,
) -> Result<Db> {
    db.conn = Arc::new(MainConnection::OwnedPostgres(owner.clone()));
    db.native = Some(crate::pg::ingest::Native::with_owner(owner, limits)?);
    Ok(db)
}

/// Native indexer storage. The destination must already exist and be migrated; this never creates a schema.
/// Call during synchronous startup or on a blocking thread, as with the synchronous PostgreSQL pool itself.
pub fn shared_postgres_db(
    config: &crate::pg::StorageConfig,
    expected_database: &str,
    limits: PgWriteLimits,
) -> Result<Db> {
    anyhow::ensure!(config.is_postgres(), "native indexer storage requires the PostgreSQL backend");
    anyhow::ensure!(!expected_database.is_empty(), "the expected PostgreSQL database is required");
    limits.check()?;
    let pool = Arc::new(crate::pg::Pool::new(config)?);
    {
        let mut conn = pool.get()?;
        let row = conn.query_one("select current_database(), current_schema()", &[])?;
        let actual: String = row.get(0);
        let schema: Option<String> = row.get(1);
        anyhow::ensure!(actual == expected_database, "PostgreSQL database differs from the expected destination");
        anyhow::ensure!(
            schema.as_deref() == Some(config.schema.as_str()),
            "PostgreSQL schema differs from the expected destination"
        );
        let version = crate::pg::migrate::version(&mut conn, &config.schema)?;
        let latest = crate::pg::migrate::MIGRATIONS.last().expect("storage migrations").0;
        anyhow::ensure!(version == latest, "native indexer storage requires the current migrated schema");
    }
    Ok(Db {
        conn: Arc::new(MainConnection::Postgres(pool.clone())),
        native: Some(crate::pg::ingest::Native::new(pool, limits)?),
    })
}

#[cfg(test)]
mod postgres_backend_tests {
    use super::*;

    #[test]
    fn final_postgres_db_owner_can_drop_inside_an_async_task() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        eprintln!("native teardown fixture: {}", schema.name);
        let database: String = schema.pool.get().unwrap().query_one("select current_database()", &[]).unwrap().get(0);
        let db = shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            PgWriteLimits::default(),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            tokio::spawn(async move {
                drop(db);
            })
            .await
            .unwrap();
        });
    }

    #[test]
    fn cancelled_postgres_day_claim_is_released_off_the_async_worker() {
        cancelled_postgres_claim(false);
    }

    #[test]
    fn cancelled_owned_postgres_day_claim_is_released_off_the_async_worker() {
        cancelled_postgres_claim(true);
    }

    fn cancelled_postgres_claim(owned: bool) {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut admin = schema.pool.get().unwrap();
        let database: String = admin.query_one("select current_database()", &[]).unwrap().get(0);
        admin.execute("insert into backfill_chunks(grp,day,state,server,claimed_at) values ('example.test',1,1,'provider.test',50)", &[]).unwrap();
        let db = shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            PgWriteLimits::default(),
        )
        .unwrap();
        let db = if owned {
            let mut client = postgres::Client::connect(&schema.url, postgres::NoTls).unwrap();
            client.batch_execute(&format!("set search_path to {}", schema.name)).unwrap();
            owned_postgres_db(db, Arc::new(crate::pg::indexer_runtime::Owner::new(client)), PgWriteLimits::default())
                .unwrap()
        } else {
            db
        };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            let (ready, started) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let claim = crate::chunks::Claim {
                    group: "example.test".into(),
                    day: 1,
                    server: "provider.test".into(),
                    claimed_at: 50,
                    reread: false,
                };
                let guard = GiveBackOnDrop { db: &db, chunk: &claim, armed: true };
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
                drop(guard);
            });
            started.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled(), "claim cleanup must not panic on a Tokio worker");
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state: i64 = admin
                .query_one("select state from backfill_chunks where grp='example.test' and day=1", &[])
                .unwrap()
                .get(0);
            if state == 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "cancelled claim was not returned");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn postgres_indexer_saves_releases_and_cursors_without_sqlite() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        let mut conn = schema.pool.get().unwrap();
        let database: String = conn.query_one("select current_database()", &[]).unwrap().get(0);
        let config = crate::pg::StorageConfig {
            backend: crate::pg::Backend::Postgres,
            url: schema.url.clone(),
            schema: schema.name.clone(),
            allow_plaintext: true,
            ..Default::default()
        };
        assert!(shared_postgres_db(&config, "wrong_database", PgWriteLimits::default()).is_err());
        let db = shared_postgres_db(&config, &database, PgWriteLimits::default()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            on_db(&db, |sql| Ok(crate::store::put_meta(sql, "native_indexer_test", 17)?)).await.unwrap();
            db.save(vec![Release {
                name: "Example.Release".into(),
                group: "example.test".into(),
                poster: "poster".into(),
                date: "2026-01-01T00:00:00Z".into(),
                articles: vec![crate::parser::Article {
                    subject: "\"Example.Release.rar\" yEnc (1/1)".into(),
                    message_id: "<one@example.test>".into(),
                    filename: Some("Example.Release.rar".into()),
                    part: Some(1),
                    total_parts: Some(1),
                    bytes: 100,
                    ..Default::default()
                }],
                ..Default::default()
            }])
            .await
            .unwrap();
        });
        assert_eq!(
            conn.query_one("select value from meta where key='native_indexer_test'", &[]).unwrap().get::<_, i64>(0),
            17
        );
        assert_eq!(conn.query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), 1);
        assert_eq!(conn.query_one("select count(*) from files", &[]).unwrap().get::<_, i64>(0), 1);
        assert_eq!(conn.query_one("select count(*) from segments", &[]).unwrap().get::<_, i64>(0), 1);
    }
}

impl Db {
    pub(crate) fn check_native_owner(&self, owner: &Arc<crate::pg::indexer_runtime::Owner>) -> Result<()> {
        anyhow::ensure!(
            matches!(&*self.conn,MainConnection::OwnedPostgres(actual) if Arc::ptr_eq(actual,owner)),
            "native service requires its original Db owner"
        );
        self.native
            .as_ref()
            .ok_or_else(|| anyhow!("native service requires native writer admission"))?
            .check_native_owner(owner)
    }

    pub(crate) async fn reserve_native_for_owner(
        &self,
        owner: &Arc<crate::pg::indexer_runtime::Owner>,
    ) -> Result<crate::pg::ingest::NativeSavePermit> {
        self.check_native_owner(owner)?;
        self.native
            .as_ref()
            .ok_or_else(|| anyhow!("native service requires native writer admission"))?
            .reserve_native_for_owner(owner)
            .await
    }

    pub(crate) fn is_native(&self) -> bool {
        self.native.is_some()
    }

    pub(crate) async fn save_native_at(&self, releases: Vec<Release>, logical_now: i64) -> Result<()> {
        let native = self.native.as_ref().ok_or_else(|| anyhow!("replay saves require native PostgreSQL storage"))?;
        native.save_at(releases, logical_now).await
    }

    /// Save one slice through the checked native admission queue and wait for its commit.
    pub(crate) async fn save(&self, releases: Vec<Release>) -> Result<()> {
        self.native
            .as_ref()
            .ok_or_else(|| anyhow!("slice saves require native PostgreSQL storage"))?
            .save(releases)
            .await
    }
}

/// Native database work shares the already-validated indexer session.
fn native_db_work<T>(db: &Db, work: impl FnOnce(&mut postgres::Client) -> Result<T>) -> Result<T> {
    match &*db.conn {
        MainConnection::Postgres(pool) => {
            let mut conn = pool.get()?;
            work(&mut conn)
        }
        MainConnection::OwnedPostgres(owner) => owner.with(work),
    }
}

pub(crate) async fn on_native_db<T: Send + 'static>(
    db: &Db,
    work: impl FnOnce(&mut postgres::Client) -> Result<T> + Send + 'static,
) -> Result<T> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || native_db_work(&db, work))
        .await
        .map_err(|error| anyhow!("native database task failed: {error}"))?
}

/// Synchronous callbacks may be invoked from either async workers or plain threads.
/// A scoped thread keeps borrowed inputs usable without nesting the PostgreSQL runtime.
pub(crate) fn on_native_db_sync<T: Send>(
    db: &Db,
    work: impl FnOnce(&mut postgres::Client) -> Result<T> + Send,
) -> Result<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(move || native_db_work(db, work))
                .join()
                .map_err(|_| anyhow!("native database callback panicked"))?
        })
    } else {
        native_db_work(db, work)
    }
}

pub(crate) async fn on_db<T: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&dyn Sql) -> Result<T> + Send + 'static,
) -> Result<T> {
    let conn = db.conn.clone();
    tokio::task::spawn_blocking(move || conn.with_sql(f)).await.map_err(|e| anyhow!("db task failed: {e}"))?
}

/// Everything a pass needs, cheap to clone into tasks.
#[derive(Clone)]
pub struct PassContext {
    pub pool: Arc<Pool>,
    pub states: RunStates,
    pub stop: Arc<AtomicBool>,
    pub verbose: bool,
}

/// Article numbers differ between providers, soo with several servers the
/// cursors are kept per server as `group@host` (`Pool::host`, see
/// `server_keys`). One server keeps the plain group name like before, unless
/// it has an explicit `#key`: that names it wherever it is in the list, and a
/// plain cursor may be another server's left from before.
fn cursor_key(pool: &Pool, server: usize, group: &str) -> String {
    // only the host (and port) is case-insensitive, an explicit `#key` isnt
    let host = pool.host(server);
    if pool.len() <= 1 && !host.contains('#') {
        return group.to_string();
    }
    match host.split_once('#') {
        Some((h, k)) => format!("{group}@{}#{k}", h.to_lowercase()),
        None => format!("{group}@{}", host.to_lowercase()),
    }
}

/// Whose the plain (identity-less) cursors are, in the main database's
/// meta: they hold article numbers of one provider, and the order of the
/// servers says nothing about which.
const PLAIN_CURSORS_OF: &str = "plain_cursors_of";

/// With one indexing server the plain cursors are its own: recorded, soo
/// once more servers are added only it adopts them. A lone server under the
/// plain name is the one writing them, soo it takes them over from whoever
/// had them before. With several servers and nothing recorded (cursors kept
/// by a build from before) they are no one's: each server starts fresh.
pub fn claim_plain_cursors(conn: &dyn Sql, pool: &Pool) -> Result<()> {
    let indexing = pool.indexing_servers();
    let [only] = indexing.as_slice() else { return Ok(()) };
    let writes_plain = cursor_key(pool, *only, "g") == "g";
    if writes_plain || plain_cursors_owner(conn)?.is_none() {
        crate::store::put_meta_text(conn, PLAIN_CURSORS_OF, &pool.host(*only))?;
    }
    Ok(())
}

fn plain_cursors_owner(conn: &dyn Sql) -> Result<Option<String>> {
    Ok(crate::store::meta_text(conn, PLAIN_CURSORS_OF)?)
}

/// whether the server named `host` (`Pool::host`) kept the plain cursors
fn owns_plain_cursors(conn: &dyn Sql, host: &str) -> Result<bool> {
    Ok(plain_cursors_owner(conn)?.as_deref() == Some(host))
}

async fn load_cursors(
    ctx: &PassContext,
    db: &Db,
    key: &str,
    group: &str,
    server: usize,
) -> Result<Option<db::GroupState>> {
    let (key, group) = (key.to_string(), group.to_string());
    let legacy = if key == group { Vec::new() } else { ctx.pool.legacy_keys(server) };
    let host = ctx.pool.host(server);
    on_db(db, move |conn| {
        let owns_plain = key != group && owns_plain_cursors(conn, &host)?;
        cursors_or_adopted(conn, &key, &group, owns_plain, &legacy)
    })
    .await
}

/// The cursors saved under `key`, else adopted: saved under one of the
/// server's `legacy` keys (what an earlier build named it, see
/// `nntp::legacy_server_keys`) they move to `key`, once; saved under the
/// plain group name (before a second server was added) only by the server
/// that kept them (`owns_plain`, see `claim_plain_cursors`)
fn cursors_or_adopted(
    conn: &dyn Sql,
    key: &str,
    group: &str,
    owns_plain: bool,
    legacy: &[String],
) -> Result<Option<db::GroupState>> {
    if let Some(state) = db::get_group_state(conn, key)? {
        return Ok(Some(state));
    }

    for old in legacy.iter().map(|k| format!("{group}@{k}")) {
        // the move as one, soo a load of the same key at the same moment can't delete what this one just renamed
        let moved = conn.tx(|tx| {
            tx.lock("atlas.adopt_cursors")?;
            if db::get_group_state(tx, key)?.is_some() {
                return Ok(true);
            }
            if db::get_group_state(tx, &old)?.is_none() {
                return Ok(false);
            }
            tx.execute("delete from groups where name = $1", vals![key])?;
            tx.execute("update groups set name = $1 where name = $2", vals![key, old.as_str()])?;
            Ok(true)
        })?;
        if moved {
            return Ok(db::get_group_state(conn, key)?);
        }
    }

    if owns_plain
        && key != group
        && let Some(state) = db::get_group_state(conn, group)?
    {
        db::save_group_state(conn, key, state)?;
        return Ok(Some(state));
    }

    Ok(None)
}

/// With time cursors: where `group`'s live reading on `server` goes on from (its live cursor, the article before the
/// first one read). That's found by date when another server read the group further than `server` did
/// (`read_through`): the first article posted `TIME_CURSOR_MARGIN` before the time it's read through, never before
/// `own`, the server's own cursor. `own` as it is when `server` is about as far, or it was already moved up to that
/// time (`RunStates::aligned`, soo a server where nothing new turns up isn't searched again every pass). None with
/// time cursors off, or when no other server read the group (none it may move up to): then a new cursor starts at the
/// newest article as always, and an own one stays where it is (or jumps to the newest, from the top).
///
/// An own cursor (unless `jump`, a stand-in's start from the top, which leaves what's below it to the group's home
/// anyway) moves up only when its own row is read through, and only to rows that began reading
/// (`db::set_start_posted`) by the time it's read through itself, give or take the margin: else what's between its
/// cursor and where the other row began would be read by nobody. With the cursor, the time it starts reading from
/// when it's started or moved up by date (for `db::set_start_posted`).
#[allow(clippy::too_many_arguments)]
async fn time_cursor_start(
    ctx: &PassContext,
    db: &Db,
    group: &str,
    key: &str,
    server: usize,
    (first, last): (i64, i64),
    own: Option<i64>,
    jump: bool,
) -> Result<Option<(i64, Option<i64>)>> {
    if !ctx.pool.time_cursors() || last < first.max(1) {
        return Ok(None);
    }
    let guarded = own.is_some() && !jump;
    let (g, k) = (group.to_string(), key.to_string());
    let (others, here) = on_db(db, move |conn| {
        let (others, here) = read_through(conn, &g, &k)?;
        Ok(match (guarded, here) {
            (true, Some(h)) => (read_through_since(conn, &g, &k, h)?, here),
            (true, None) => (None, None),
            (false, _) => (others, here),
        })
    })
    .await?;
    let Some(through) = others else { return Ok(None) };
    let target = through - TIME_CURSOR_MARGIN;
    if let Some(own) = own
        && (here.is_some_and(|h| h >= target) || ctx.states.aligned(key) == Some(through))
    {
        return Ok(Some((own, None)));
    }
    // a server that can't answer the date search keeps its own cursor (or starts at the top), as without time cursors
    let at = match ctx.pool.article_at(server, group, first as u64, last as u64, target).await {
        Ok(at) => at as i64,
        Err(e) => {
            println!(
                "[CURSOR] {group}: {} couldnt find where it's read through elsewhere ({e})",
                ctx.pool.host(server)
            );
            return Ok(own.map(|own| (own, None)));
        }
    };
    ctx.states.set_aligned(key, through);
    let found = (at - 1).clamp(first - 1, last);
    let start = own.map_or(found, |own| own.max(found));
    if Some(start) == own {
        return Ok(Some((start, None)));
    }
    let when = chrono::DateTime::from_timestamp(through, 0).map(|d| d.format("%Y-%m-%d %H:%M").to_string());
    println!(
        "[CURSOR] {group}: {} goes on from {start}, where it's read through elsewhere ({} UTC){}",
        ctx.pool.host(server),
        when.unwrap_or_default(),
        own.map(|o| format!(", its own cursor was {o}")).unwrap_or_default(),
    );
    Ok(Some((start, Some(target))))
}

/// How far before the time a group is read through a server that hasn't been reading it starts
/// (`time_cursor_start`): posting times are only roughly in the order of article numbers, and what's read twice is
/// dropped by message-id.
pub const TIME_CURSOR_MARGIN: i64 = 3600;

/// The time `group` is read through, from its rows in `groups` (one per server, `group@server`, and the plain name
/// a lone server kept): (the newest on any server but the one `key` names, on that one). A row's time is the posting
/// time of the newest slice its passes saved (`db::save_group_dates`), and it counts only when its live cursor is past
/// that slice: a pass cut short saves the newest slices first, and those aren't read through until the ones before
/// them are.
pub fn read_through(db: &dyn Sql, group: &str, key: &str) -> Result<(Option<i64>, Option<i64>)> {
    let prefix = format!("{group}@");
    let r = db
        .row(
            "select max(case when name != $3 then high_posted end), max(case when name = $3 then high_posted end)
             from groups
             where (name = $2 or substr(name, 1, length(cast($1 as text))) = $1)
               and high_posted is not null and live_cursor is not null and high_article <= live_cursor",
            vals![prefix, group, key],
        )?
        .unwrap_or_default();
    Ok((r.get(0)?, r.get(1)?))
}

/// `read_through`'s time on the other servers, from only the rows that began reading (`db::set_start_posted`) at or
/// before `began_by`: what an own cursor read through up to then may move up to. A row from before that was kept
/// counts as begun long ago.
pub fn read_through_since(db: &dyn Sql, group: &str, key: &str, began_by: i64) -> Result<Option<i64>> {
    let prefix = format!("{group}@");
    Ok(db.one(
        "select max(high_posted)
         from groups
         where (name = $2 or substr(name, 1, length(cast($1 as text))) = $1) and name != $3
           and high_posted is not null and live_cursor is not null and high_article <= live_cursor
           and (start_posted is null or start_posted <= $4)",
        vals![prefix, group, key, began_by],
    )?)
}

/// One pass over `group`: a batch of live or backfill articles depending on
/// the mode, on server `prefer` (or whichever server carries the group).
/// `progress` hears about every slice as it is saved. Safe to run for many
/// groups at once; the pool keeps each server within its connections.
pub async fn run_pass<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    prefer: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let mut ran_on = prefer;
    run_pass_on(ctx, settings, db, group, prefer, progress, &mut ran_on).await
}

/// `run_pass`, saying in `ran_on` which server the pass went to: `prefer`, or the one a GROUP there moved the group
/// to (see `Pool::select_group_known`), soo a failure counts against the server it happened on.
pub async fn run_pass_on<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    prefer: usize,
    progress: &mut P,
    ran_on: &mut usize,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    *ran_on = prefer;
    if crate::filters::group_blocked(group) {
        return Ok(Progress::default());
    }
    // a server that indexed the group before loses it only after several replies in a row that it lacks it
    // (cursors it kept under a key of an earlier build count too, see `cursors_or_adopted`)
    let (server, (_count, first, last, _name)) = if settings.fallback {
        (prefer, ctx.pool.select_group_only(prefer, group).await?)
    } else {
        let known = load_cursors(ctx, db, &cursor_key(&ctx.pool, prefer, group), group, prefer).await?.is_some();
        ctx.pool.select_group_known(prefer, group, known).await?
    };
    *ran_on = server;
    let (first, last) = (first as i64, last as i64);
    let key = cursor_key(&ctx.pool, server, group);

    // with time cursors a group's live position is the time it's read through on any server (`time_cursor_start`):
    // a server that hasn't been reading it starts there, a stand-in too, rather than at the newest article, and one
    // whose own cursor fell behind while others read it moves up to it
    let state = match load_cursors(ctx, db, &key, group, server).await? {
        Some(mut s) if settings.fallback && settings.from_top && last >= s.live_cursor => {
            let to = time_cursor_start(ctx, db, group, &key, server, (first, last), Some(s.live_cursor), true).await?;
            let (to, began) = to.unwrap_or((last, Some(chrono::Utc::now().timestamp())));
            let began = began.filter(|_| to != s.live_cursor);
            let k = key.clone();
            on_db(db, move |conn| {
                db::update_live_cursor(conn, &k, to)?;
                if let Some(b) = began {
                    db::set_start_posted(conn, &k, b)?;
                }
                Ok(())
            })
            .await?;
            s.live_cursor = to;
            s
        }
        Some(mut s) if last >= s.live_cursor => {
            if let Some((to, began)) =
                time_cursor_start(ctx, db, group, &key, server, (first, last), Some(s.live_cursor), false).await?
                && to > s.live_cursor
            {
                let k = key.clone();
                on_db(db, move |conn| {
                    db::update_live_cursor(conn, &k, to)?;
                    if let Some(b) = began {
                        db::set_start_posted(conn, &k, b)?;
                    }
                    Ok(())
                })
                .await?;
                s.live_cursor = to;
            }
            s
        }
        // first time both cursors start at the top (or where it's read through, with time cursors). if the server
        // renumbered (last went backwards) the group got reset soo start over the same way
        _ => {
            let (start, began) = time_cursor_start(ctx, db, group, &key, server, (first, last), None, false)
                .await?
                .unwrap_or((last, None));
            let began = began.unwrap_or_else(|| chrono::Utc::now().timestamp());
            let k = key.clone();
            on_db(db, move |conn| {
                db::init_group_state(conn, &k, start)?;
                db::set_start_posted(conn, &k, began)?;
                Ok(())
            })
            .await?;
            if !settings.fallback {
                ctx.states.with(group, |st| st.phase = Phase::Backfill);
            }
            db::GroupState { live_cursor: start, backfill_cursor: start }
        }
    };

    // for the stats dashboard's progress and ETA
    let k = key.clone();
    on_db(db, move |conn| Ok(db::save_group_bounds(conn, &k, first, last)?)).await?;

    let mut phase = ctx.states.with(group, |st| st.phase);
    let split = settings.mode != "live" && settings.mode != "backfill" && {
        let g = group.to_string();
        on_db(db, move |conn| Ok(crate::chunks::is_split(conn, &g)?)).await?
    };
    if phase == Phase::Backfill {
        let deferred = ctx.states.with(group, |st| st.backfill_deferred);
        let first = live_first(&settings.mode, phase, split, last - state.live_cursor, settings.request_size, deferred);
        ctx.states.with(group, |st| st.backfill_deferred = first);
        if first {
            phase = Phase::Live;
        }
    }
    let backfilling = settings.mode == "backfill" || (settings.mode != "live" && phase == Phase::Backfill);
    if backfilling
        && maybe_split(
            ctx,
            settings,
            db,
            group,
            server,
            (first as u64, last as u64),
            state.backfill_cursor.max(0) as u64,
        )
        .await?
    {
        // a split group's backfill is day chunks, this server takes the next one: a day asked for, or one no more than
        // a day past the newest any tracked split group still has to do (see `chunks::claim_turn`), else none
        let host = ctx.pool.host(server);
        let wanted = (group.to_string(), ctx.states.keeps_from(group, &host));
        let (kept, running) = (ctx.states.kept_days(&host), ctx.states.groups());
        // the split's generation as of the claim: a sweep noted after the split reached back isnt
        let g = group.to_string();
        let (claimed, generation) = on_db(db, move |conn| {
            let generation = crate::chunks::sweep_generation(conn, &g)?;
            let split: Vec<(String, i64)> = crate::chunks::split_groups(conn)?
                .into_iter()
                .filter(|s| *s == g || running.contains(s))
                .map(|s| {
                    let day = kept.get(&s).copied().unwrap_or(i64::MIN);
                    (s, day)
                })
                .collect();
            let now = chrono::Utc::now().timestamp();
            Ok((crate::chunks::claim_turn(conn, wanted, &split, &host, now)?, generation))
        })
        .await?;
        let r = match claimed {
            // a day this server doesnt keep is left to the others, it isnt an error
            Some(chunk) => match run_chunk(ctx, settings, db, &chunk, server, progress).await {
                Err(e) if e.downcast_ref::<TooOld>().is_some() => Ok(Progress::default()),
                r => r,
            },
            // nothing for this server: once no chunk of this group it could
            // take is left, its own backfill sweeps on from where the split
            // froze the cursor down to its first article. date searches go by
            // Date headers posters can forge, and neighbouring days done on
            // other servers can leave numbers between them; the sweep picks up
            // whatever the chunks missed (what they got is dropped as a duplicate)
            // the other carriers sweep their own numbers the same way (see `run_sweep`)
            None if sweep_due(ctx, db, group, &ctx.pool.host(server)).await? => {
                if state.backfill_cursor.min(last) < first {
                    let (g, host) = (group.to_string(), ctx.pool.host(server));
                    on_db(db, move |conn| Ok(crate::chunks::set_swept(conn, &g, &host, generation)?)).await?;
                }
                let pass = Pass { ctx, settings, db, group, server, key };
                pass.backfill(state, first, last, progress).await
            }
            None => {
                // nothing pending: in backfill mode the group rests like a finished backfill
                let idle = settings.mode == "backfill";
                ctx.states.with(group, |st| {
                    st.backfilling = false;
                    st.idle |= idle;
                });
                Ok(Progress::default())
            }
        };
        // dynamic mode takes turns, the next pass is live
        ctx.states.with(group, |st| st.phase = Phase::Live);
        return r;
    }

    let pass = Pass { ctx, settings, db, group, server, key };

    match settings.mode.as_str() {
        "live" => pass.live(state, last, progress).await,
        // forward backfill reads day chunks only: a group that didn't split reads live meanwhile
        "backfill" if crate::chunks::forward_on() => pass.live(state, last, progress).await,
        "backfill" => pass.backfill(state, first, last, progress).await,
        _ if phase == Phase::Live || crate::chunks::forward_on() => {
            let r = pass.live(state, last, progress).await;
            ctx.states.with(group, |st| st.phase = Phase::Backfill);
            r
        }
        _ => {
            let r = pass.backfill(state, first, last, progress).await;
            ctx.states.with(group, |st| st.phase = Phase::Live);
            r
        }
    }
}

/// Slices a pass of `kind` streams at once (`Pool::stream_headers`): a day chunk's many more, it holds every group's
/// forward backfill behind it (`nntp::CHUNK_STREAM_SLICES`).
fn stream_width(kind: &str) -> usize {
    if kind == "CHUNK" { crate::nntp::CHUNK_STREAM_SLICES } else { crate::nntp::STREAM_SLICES }
}

/// Whether a dynamic mode pass whose turn is backfill goes live instead: a split group's whose live side is more than
/// a slice behind. Its day chunks are taken by any server's free workers too (`take_chunk`), and one can hold its home's
/// worker for over an hour, while its new posts waited: after a restart every group's first turn is backfill, and on
/// prod alias and rail claimed a day chunk each and their live cursors stood still for hours.
/// Live always wins: putting it off every other turn (`deferred`, no longer used) let a big group's frontier day hold
/// its worker for hours on prod, and its live side fell a day behind. Its frontier days go to the chunk workers
/// (`bg_indexer::chunk_share`) instead. While it stays more than a slice behind, its home's sweep and cursor backfill
/// wait (the day chunks still cover its days).
fn live_first(mode: &str, phase: Phase, split: bool, behind: i64, slice: u64, _deferred: bool) -> bool {
    mode != "live" && mode != "backfill" && phase == Phase::Backfill && split && behind > slice as i64
}

/// Whether split `group`'s cursor backfill on `host` may sweep: no day
/// chunk of it is left that the server could take or that is still running.
/// Its chunks come first; other groups' chunks dont hold its sweep back
/// (idle workers take those anyway), their retention wont wait for them.
async fn sweep_due(ctx: &PassContext, db: &Db, group: &str, host: &str) -> Result<bool> {
    let wanted = [(group.to_string(), ctx.states.keeps_from(group, host))];
    on_db(db, move |conn| Ok(!crate::chunks::waiting(conn, &wanted)?)).await
}

/// how long a big group that couldnt be split waits before it is probed again
const SPLIT_RECHECK: Duration = Duration::from_secs(3600);

/// Split `group`'s backfill into day chunks when its home server still has
/// more than `split_min_backlog` article numbers to go and another indexing
/// server carries it too. Chunks run from the day at the home cursor back to
/// the oldest day any carrying server has. True when the group is split.
async fn maybe_split(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    home: usize,
    (first, last): (u64, u64),
    cursor: u64,
) -> Result<bool> {
    let g = group.to_string();
    if on_db(db, move |conn| Ok(crate::chunks::is_split(conn, &g)?)).await? {
        // a server that goes back further (one added, or keeping more now)
        // is looked for every SPLIT_RECHECK: normal backfill never gets to
        // the days older than the split's
        let due = ctx.states.with(group, |st| {
            let due = st.reach_back_after.is_none_or(|t| Instant::now() >= t);
            if due {
                st.reach_back_after = Some(Instant::now() + SPLIT_RECHECK);
            }
            due
        });
        if due && let Err(e) = reach_back(ctx, db, group, home, (first, last)).await {
            println!("[SPLIT] {group}: couldnt look for servers going back further: {e:#}");
        }
        return Ok(true);
    }
    if (cursor.saturating_sub(first) as i64) < settings.split_min_backlog {
        return Ok(false);
    }
    if ctx.states.with(group, |st| st.no_split_until.is_some_and(|t| Instant::now() < t)) {
        return Ok(false);
    }

    // the newest day still to do is when the articles at the home cursor
    // were posted
    let at_cursor = ctx.pool.posted_dates(home, group, cursor).await?;
    let split = if at_cursor.is_empty() {
        None
    } else {
        split_days(ctx, group, home, (first, last), Newest::AtCursor(at_cursor)).await?
    };
    let Some(Split { newest_day, oldest_day, carriers, .. }) = split else {
        ctx.states.with(group, |st| st.no_split_until = Some(Instant::now() + SPLIT_RECHECK));
        return Ok(false);
    };

    let g = group.to_string();
    let added = on_db(db, move |conn| Ok(crate::chunks::add(conn, &g, newest_day, oldest_day)?)).await?;
    println!("[SPLIT] {group}: backfill split into {added} day chunks over {carriers} servers");
    ctx.states.with(group, |st| st.reach_back_after = Some(Instant::now() + SPLIT_RECHECK));
    Ok(true)
}

/// Days older than split `group`'s oldest that a carrier keeps now: chunks
/// for them, see `chunks::reach_back`. The split's newest day is its newest
/// chunk's, not the date at the home cursor: the cursor stays where the
/// split was made, and home's retention moves past it.
async fn reach_back(ctx: &PassContext, db: &Db, group: &str, home: usize, bounds: (u64, u64)) -> Result<()> {
    let g = group.to_string();
    let Some(newest_day) = on_db(db, move |conn| Ok(crate::chunks::newest_day(conn, &g)?)).await? else {
        return Ok(());
    };
    let Some(Split { oldest_day, oldest_at, .. }) =
        split_days(ctx, group, home, bounds, Newest::Day(newest_day)).await?
    else {
        return Ok(());
    };
    let g = group.to_string();
    let todo = on_db(db, move |conn| Ok(crate::chunks::reach_back(conn, &g, oldest_day, oldest_at)?)).await?;
    if todo > 0 {
        println!("[SPLIT] {group}: a server goes back further, {todo} day chunks to do");
    }
    Ok(())
}

/// What a split of a group covers.
struct Split {
    newest_day: i64,
    oldest_day: i64,
    /// when the oldest article any carrier keeps was posted (unix seconds)
    oldest_at: i64,
    /// servers carrying the group
    carriers: usize,
}

/// Where a split of a group starts.
enum Newest {
    /// when the articles at the home cursor were posted (unix seconds), for a new split
    AtCursor(Vec<i64>),
    /// the newest day of a split there is (unix days)
    Day(i64),
}

/// The days a split of `group` from `newest` covers and how many servers
/// carry it. None when it cant split: home's first date is unknown, no
/// other indexing server carries the group, or the dates make no sense (see
/// `split_bounds`). `first` and `last` are home's low and high marks.
async fn split_days(
    ctx: &PassContext,
    group: &str,
    home: usize,
    (first, last): (u64, u64),
    newest: Newest,
) -> Result<Option<Split>> {
    // the oldest day is at least home's retention. first articles are
    // searched for from the low marks, which can lag far behind what a
    // server still keeps. a server whose first dates disagree (forged) could
    // be deeper or shallower than it looks: no split until it can be told
    let unsure =
        || println!("[SPLIT] {group}: first post dates that disagree (forged Date headers?), not split for now");
    let home_oldest = match ctx.pool.retention(home, group, first, last).await? {
        Retention::Since(t) => t,
        Retention::Empty => return Ok(None),
        Retention::Unsure(_) => {
            unsure();
            return Ok(None);
        }
    };
    let mut oldest_at = home_oldest;

    // other servers are best effort: one that fails is left out. one that
    // carries the group counts, its oldest date only if it has one
    let mut carriers = 1;
    let total_conns = ctx.pool.indexing_connections();
    for other in ctx.pool.indexing_servers().into_iter().filter(|&s| s != home) {
        if total_conns >= 50 && ctx.pool.connections(other) * 20 < total_conns {
            continue;
        }
        let Ok((_, low, high, _)) = ctx.pool.group_on(other, group).await else { continue };
        carriers += 1;
        match ctx.pool.retention(other, group, low, high).await {
            Ok(Retention::Since(t)) => oldest_at = oldest_at.min(t),
            Ok(Retention::Unsure(_)) => {
                unsure();
                return Ok(None);
            }
            Ok(Retention::Empty) | Err(_) => {}
        }
    }
    if carriers < 2 {
        return Ok(None);
    }

    let today = crate::chunks::unix_day(chrono::Utc::now().timestamp());
    let Some((newest_day, oldest_day)) = split_bounds(&newest, home_oldest, oldest_at, today) else {
        println!("[SPLIT] {group}: post dates that make no sense (forged Date headers?), the split is left as it is");
        return Ok(None);
    };
    // a time from before its day (the split doesnt go further back than its
    // newest day) starts the day
    let oldest_at = oldest_at.clamp(oldest_day * 86_400, (oldest_day + 1) * 86_400 - 1);
    Ok(Some(Split { newest_day, oldest_day, oldest_at, carriers }))
}

/// 2000-01-01: binary retention doesnt reach further back than this
const SPLIT_OLDEST_DAY: i64 = 10_957;

/// A split's (newest, oldest) days from `newest`, home's first article
/// (`home_oldest`) and the first of any carrier (`oldest_at`, unix seconds).
/// Date headers are the posters' to forge, and a split made from a forged
/// one would leave the group's history unindexed for good: a date at the
/// cursor before 2000-01-01, after `today`, or more than a day before home's
/// first article is left out (the latest left answers), and None when none
/// is left, the oldest is before 2000-01-01 or the newest day is before the
/// oldest. Clamping such a date into range would still make a bogus split
/// (a too old chunk every carrier gives back).
fn split_bounds(newest: &Newest, home_oldest: i64, oldest_at: i64, today: i64) -> Option<(i64, i64)> {
    use crate::chunks::unix_day;
    let newest_day = match newest {
        Newest::AtCursor(times) => times
            .iter()
            .filter(|&&t| t >= home_oldest - 86_400)
            .map(|&t| unix_day(t))
            .filter(|day| (SPLIT_OLDEST_DAY..=today).contains(day))
            .max()?,
        Newest::Day(day) => *day,
    };
    let oldest_day = unix_day(oldest_at);
    (oldest_day >= SPLIT_OLDEST_DAY && newest_day >= oldest_day).then_some((newest_day, oldest_day))
}

/// Once every indexing server turned a day chunk of `group` down as too old
/// for it, drop its chunks of days before the first post of all of them:
/// no server has anything from those, and a split made from forged first
/// dates can reach back years. Left alone, those chunks wait for a server
/// forever and the split never completes (its sweeps never run). Best
/// effort: a failure is only logged.
async fn drop_unkept_days(ctx: &PassContext, db: &Db, group: &str) {
    // every server: a small one can still have the older days, and what this finds gets deleted
    let first =
        ctx.pool.indexing_servers().into_iter().map(|i| ctx.states.first_post_day(group, &ctx.pool.host(i))).min();
    // i64::MIN: a server not asked yet (or a while ago)
    let Some(first) = first.filter(|&d| d != i64::MIN) else { return };
    let g = group.to_string();
    match on_db(db, move |conn| Ok(crate::chunks::drop_before(conn, &g, first)?)).await {
        Ok(0) => {}
        Ok(n) => println!("[CHUNK] {group}: {n} days older than any server keeps, dropped (forged Date headers?)"),
        Err(e) => println!("[CHUNK] {group}: couldnt drop the days no server keeps: {e:#}"),
    }
}

/// A day chunk's server doesnt carry its group (GROUP answered 411).
#[derive(Debug)]
pub struct NotCarried {
    pub group: String,
    pub host: String,
}

impl std::fmt::Display for NotCarried {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} isnt on {}", self.group, self.host)
    }
}

impl std::error::Error for NotCarried {}

/// A day chunk older than anything its server keeps of the group: the chunk
/// goes back for a server that has the day.
#[derive(Debug)]
pub struct TooOld {
    pub group: String,
    pub host: String,
    /// the oldest day the server keeps (unix days), `i64::MAX` when it has nothing
    pub oldest_day: i64,
}

impl std::fmt::Display for TooOld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {} doesnt go back that far", self.group, self.host)
    }
}

impl std::error::Error for TooOld {}

/// how long a server's oldest day of a group is trusted before a chunk older
/// than it is tried there again
const KEEPS_RECHECK: Duration = Duration::from_secs(3600);

/// Claim the newest chunk for `host` of the split groups among `groups`,
/// none older than the server's oldest day in `oldest` (by group).
pub async fn claim_chunk(
    db: &Db,
    host: String,
    groups: std::collections::HashSet<String>,
    oldest: HashMap<String, i64>,
) -> Result<Option<crate::chunks::Claim>> {
    on_db(db, move |conn| {
        let split: Vec<(String, i64)> = crate::chunks::split_groups(conn)?
            .into_iter()
            .filter(|g| groups.contains(g))
            .map(|g| {
                let day = oldest.get(&g).copied().unwrap_or(i64::MIN);
                (g, day)
            })
            .collect();
        Ok(crate::chunks::claim(conn, &split, &host, chrono::Utc::now().timestamp())?)
    })
    .await
}

/// A split group whose day chunks are all done that `host`, not its home,
/// still has to sweep, among `groups`.
pub async fn sweep_to_take(db: &Db, host: String, groups: Vec<String>) -> Result<Option<String>> {
    on_db(db, move |conn| {
        for g in groups {
            if crate::chunks::chunks_done(conn, &g)? && !crate::chunks::swept(conn, &g, &host)? {
                return Ok(Some(g));
            }
        }
        Ok(None)
    })
    .await
}

/// One batch of the sweep of split `group` on `server`, a carrier that is
/// not its home, once its day chunks are done: like the home's (see
/// `run_pass`), the server's own cursor backfill from where its backfill
/// got (its high mark when it has none, where its newest chunk ended) down
/// to its first article. The chunks go by forgeable Date headers: when the
/// home keeps less than this server, what they missed of the days only this
/// one keeps is here. Noted as swept once at its first article (or when it
/// doesnt carry the group); the split is complete when every carrier is.
pub async fn run_sweep<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    if crate::filters::group_blocked(group) {
        return Ok(Progress::default());
    }
    let (g, host) = (group.to_string(), ctx.pool.host(server));
    // noted as swept only if the split hasnt reached back since (see `set_swept`)
    let generation = {
        let g = g.clone();
        on_db(db, move |conn| Ok(crate::chunks::sweep_generation(conn, &g)?)).await?
    };
    let swept = || {
        let (g, host) = (g.clone(), host.clone());
        on_db(db, move |conn| Ok(crate::chunks::set_swept(conn, &g, &host, generation)?))
    };
    // this server alone, like a day chunk
    let (_count, first, last, _name) = match ctx.pool.group_on(server, group).await {
        Ok(info) => info,
        // only 411 says the server doesnt carry it: a 403 or 430 is a failure to retry
        Err(e) if crate::nntp::carries_not(&e) => {
            swept().await?;
            return Err(NotCarried { group: g, host }.into());
        }
        Err(e) => return Err(e.into()),
    };
    let (first, last) = (first as i64, last as i64);
    let key = cursor_key(&ctx.pool, server, group);
    let cursor = match load_cursors(ctx, db, &key, group, server).await? {
        Some(s) => s.backfill_cursor.min(last),
        None => {
            let k = key.clone();
            on_db(db, move |conn| {
                db::init_group_state(conn, &k, last)?;
                db::set_start_posted(conn, &k, chrono::Utc::now().timestamp())?;
                Ok(())
            })
            .await?;
            last
        }
    };
    if cursor < first {
        swept().await?;
        println!("[SWEEP] {group} on {host} done");
        return Ok(Progress::default());
    }

    // the cursor backfill's batch, without touching the group's run state:
    // that is the home's
    let start = first.max(cursor - settings.batch_size.max(1) + 1);
    let pass = Pass { ctx, settings, db, group, server, key: key.clone() };
    let ranged = pass.process_range(start, cursor, "SWEEP", progress).await;
    // done down from the cursor: the cursor moves past it, failed or not
    if let Some(high) = ranged.high_done {
        on_db(db, move |conn| Ok(db::update_backfill_cursor(conn, &key, high as i64 - 1)?)).await?;
    }
    ranged.into_result()
}

/// a day chunk also takes this much on each side: post dates are only
/// roughly in article number order, duplicates are dropped when saved
pub const CHUNK_OVERLAP: i64 = 3600;

/// article numbers a day chunk reaches past each end its date searches put
/// it at, for neighbouring days done on other servers (see `chunk_range`)
pub const CHUNK_SAFETY: u64 = 5_000;

/// Index the day chunk `chunk` claimed (its day in unix days) on `server`,
/// for a group whose backfill is split into day chunks (see chunks.rs). The
/// chunk is marked done when the whole day is in, released again if stopped
/// part way or failing, or dropped part way (stopping drops a pass stuck on
/// the network). Either is skipped once another worker took the chunk over
/// (this one ran past CLAIM_TIMEOUT): the chunk is that worker's.
pub async fn run_chunk<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    chunk: &crate::chunks::Claim,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let mut unfinished = GiveBackOnDrop { db, chunk, armed: true };
    let r = index_chunk(ctx, settings, db, chunk, server, progress).await;
    unfinished.armed = false;
    r
}

/// Gives a claimed chunk back when its run is dropped part way, instead of
/// it staying claimed until CLAIM_TIMEOUT (a quick restart would find it
/// taken). Every way the run ends on its own finishes or gives it back already.
struct GiveBackOnDrop<'a> {
    db: &'a Db,
    chunk: &'a crate::chunks::Claim,
    armed: bool,
}

impl Drop for GiveBackOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let conn = self.db.conn.clone();
        let claim = self.chunk.clone();
        let release = move || {
            if let Err(e) = conn.with_sql(|conn| Ok(crate::chunks::release(conn, &claim)?)) {
                let (group, day) = (&claim.group, claim.day);
                println!("[CHUNK] {group} day {day}: couldnt give the chunk back when stopped: {e:#}");
            }
        };
        // Cancellation drops this guard on an async worker. PostgreSQL's synchronous client must never run
        // there; owned state also lets cleanup finish after the cancelled pass has released its Db clone.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(release);
        } else {
            release();
        }
    }
}

/// Whether a day chunk that failed with `e` rests before it is tried again: not when this server lacks the day or the
/// group, which another server takes at once.
fn chunk_failure_rests(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TooOld>().is_none() && e.downcast_ref::<NotCarried>().is_none()
}

/// A day chunk of `group` that only one server claims to have (its dates
/// reach back further than any other's) came out with no article dated in
/// that day: its dates went back further than its articles do (forged), and
/// each older day would take a chunk to find that out again. They are dropped; what those days hold is
/// the sweeps' (see `run_sweep`). Best effort: a failure is only logged.
async fn drop_older_days(db: &Db, group: &str, day: i64) {
    let g = group.to_string();
    match on_db(db, move |conn| Ok(crate::chunks::drop_before(conn, &g, day)?)).await {
        Ok(0) => {}
        Ok(n) => {
            println!("[CHUNK] {group} day {day} has no article dated in it, the {n} older days are left to the sweeps")
        }
        Err(e) => println!("[CHUNK] {group}: couldnt drop the days older than an empty one: {e:#}"),
    }
}

/// A day only this server has (`day`, of the window from `floor`, see
/// `chunks::Speculative`) is done with `dated` articles in it. When it was the
/// window's oldest and the window's days held enough a day, chunks for the
/// days before it, back to the server's first post (`oldest`). Best effort:
/// a failure is only logged.
async fn reach_further(db: &Db, group: &str, (_day, floor, oldest): (i64, i64, Option<i64>), dated: i64) {
    let first = oldest.map_or(floor, crate::chunks::unix_day);
    let g = group.to_string();
    let added = on_db(db, move |db| {
        // read, counted and written back as one: two days of the window finishing at once would else both count
        // from the same row, on Postgres
        Ok(db.tx(|conn| -> crate::pg::sql::Result<usize> {
            conn.lock(&crate::chunks::speculative_lock(&g))?;
            let Some(mut s) = crate::chunks::speculative(conn, &g)? else { return Ok(0) };
            // a day of a window the reach has moved past is not the current one's
            if floor != s.runner_up_day - s.reach {
                return Ok(0);
            }
            s.days += 1;
            s.articles += dated;
            // the window is judged when its last day is done, not when its oldest is
            let running = conn.exists(
                "select 1 from backfill_chunks where grp = $1 and day >= $2 and day < $3 and state != 2 limit 1",
                vals![g.as_str(), floor, floor + SPECULATIVE_WINDOW],
            )?;
            let further = !running && s.articles >= SPECULATIVE_YIELD * s.days && first < floor;
            if further {
                // each extension has to hold enough a day on its own: a dense
                // first window doesnt pay for the sparse ones after it
                s.reach += SPECULATIVE_WINDOW;
                s.days = 0;
                s.articles = 0;
            }
            crate::chunks::set_speculative(conn, &g, &s)?;
            if !further {
                return Ok(0);
            }
            crate::chunks::add(conn, &g, floor - 1, (floor - SPECULATIVE_WINDOW).max(first))
        })?)
    })
    .await;
    match added {
        Ok(0) => {}
        Ok(n) => println!("[CHUNK] {group}: the days only one server has hold articles, {n} more day chunks"),
        Err(e) => println!("[CHUNK] {group}: couldnt note the days only one server has: {e:#}"),
    }
}

/// `chunk` is done with `dated` articles dated in its day. Only when this
/// claim is what made it done does what the day showed count: a claim taken
/// over (gone stale while it ran) leaves the speculative window and the older
/// days to the worker that has it now. `alone`, `floor` and `oldest` are as
/// `index_chunk` found them.
async fn finish_chunk(
    db: &Db,
    chunk: &crate::chunks::Claim,
    (alone, floor, oldest): (bool, Option<i64>, Option<i64>),
    dated: i64,
) -> Result<()> {
    let (group, day) = (chunk.group.as_str(), chunk.day);
    let c = chunk.clone();
    let now = chrono::Utc::now().timestamp();
    if !on_db(db, move |conn| Ok(crate::chunks::finish(conn, &c, now)?)).await? {
        println!("[CHUNK] {group} day {day}: taken over by another worker, leaving it to that one");
        return Ok(());
    }
    if alone && dated == 0 {
        drop_older_days(db, group, day).await;
    } else if let Some(floor) = floor {
        reach_further(db, group, (day, floor, oldest), dated).await;
    }
    Ok(())
}

async fn index_chunk<P>(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    chunk: &crate::chunks::Claim,
    server: usize,
    progress: &mut P,
) -> Result<Progress>
where
    P: FnMut(&Progress) + ?Sized,
{
    let (group, day) = (chunk.group.as_str(), chunk.day);
    // give the chunk back after `e`, and return `e`: failing to give it back
    // is only logged (the claim goes stale and gets taken over). A day this
    // server doesnt have is the others' at once; any other failure rests the
    // chunk (see `chunks::FAIL_REST`), so a day that fails every time doesnt
    // hold every older one back
    let failed = async |e: anyhow::Error| {
        let c = chunk.clone();
        let rests = chunk_failure_rests(&e);
        let gave_back = on_db(db, move |conn| {
            Ok(if rests {
                crate::chunks::fail(conn, &c, chrono::Utc::now().timestamp())?
            } else {
                crate::chunks::release(conn, &c)?
            })
        });
        if let Err(r) = gave_back.await {
            println!("[CHUNK] {group} day {day}: couldnt give the chunk back: {r:#}");
        }
        Err(e)
    };

    // GROUP on this server alone: falling over would move where the group
    // lives, and only a 411 means the server doesnt carry it
    let found = match ctx.pool.group_on(server, group).await {
        Ok(info) => info,
        Err(e) if crate::nntp::carries_not(&e) => {
            // it has no post of it (see `drop_unkept_days`)
            ctx.states.not_carrying(group, &ctx.pool.host(server));
            // if it was noted as going back furthest, the oldest day goes to the next one
            let (g, host) = (group.to_string(), ctx.pool.host(server));
            if let Err(e) = on_db(db, move |conn| Ok(crate::chunks::forget_deepest(conn, &g, &host)?)).await {
                println!("[CHUNK] {group}: couldnt forget the server going back furthest: {e:#}");
            }
            return failed(NotCarried { group: group.to_string(), host: ctx.pool.host(server) }.into()).await;
        }
        Err(e) => return failed(e.into()).await,
    };
    let (_count, first, last, _name) = found;

    // a day this server doesnt keep from its start would come out empty or
    // cut short, though another server may have all of it: give it back, and
    // the server takes no day that old for a while. the split's oldest day is
    // the exception: no server keeps it whole, the one that goes back
    // furthest on it does what there is
    // (whether its first dates agree: see `goes_back_furthest`)
    let (oldest, sure) = match ctx.pool.retention(server, group, first, last).await {
        Ok(Retention::Since(t)) => (Some(t), true),
        Ok(Retention::Unsure(t)) => (Some(t), false),
        Ok(Retention::Empty) => (None, true),
        Err(e) => return failed(e.into()).await,
    };
    // from when the server is taken to keep days whole
    let mut keeps_from = oldest;
    // every carrier's first post is noted, a day it keeps or not: a forged
    // run in the dates must not leave `drop_unkept_days` without evidence
    ctx.states.first_post_on(group, &ctx.pool.host(server), oldest.map_or(i64::MAX, crate::chunks::unix_day));
    // this day is older than any other server goes back: only this server
    // may have it, on the word of its own (forgeable) dates
    let mut alone = false;
    // and then the first day the next deepest server keeps
    let mut runner_up_day = None;
    let keeps_day = match oldest {
        // a day older than the second furthest server goes back is the
        // furthest one's alone: a server only looks deep enough by the dates
        // its posters set
        Some(t) if t <= day * 86_400 => match goes_back_furthest(ctx, db, group, server, (t, sure)).await {
            Ok((true, runner_up)) => {
                alone = runner_up.is_none_or(|r| r >= (day + 1) * 86_400);
                runner_up_day = runner_up.filter(|_| alone).map(crate::chunks::unix_day);
                true
            }
            Ok((false, runner_up)) => {
                keeps_from = keeps_from.max(runner_up);
                runner_up.is_none_or(|r| r <= day * 86_400)
            }
            Err(e) => return failed(e).await,
        },
        Some(t) if crate::chunks::unix_day(t) == day => {
            let g = group.to_string();
            let split_oldest = match on_db(db, move |conn| Ok(crate::chunks::oldest_day(conn, &g)?)).await {
                Ok(d) => d,
                Err(e) => return failed(e).await,
            };
            if split_oldest != Some(day) {
                false
            } else {
                match goes_back_furthest(ctx, db, group, server, (t, sure)).await {
                    Ok((furthest, _)) => furthest,
                    Err(e) => return failed(e).await,
                }
            }
        }
        _ => false,
    };
    if !keeps_day {
        // the first day it keeps whole
        let oldest_day = keeps_from.map_or(i64::MAX, |t| crate::chunks::unix_day(t - 1) + 1);
        let host = ctx.pool.host(server);
        ctx.states.keep_from(group, &host, oldest_day);
        ctx.states.first_post_on(group, &host, oldest.map_or(i64::MAX, crate::chunks::unix_day));
        println!("[CHUNK] {group} day {day} is older than {host} keeps, leaving it to the other servers");
        // a server that doesnt keep this day keeps none of the split's oldest
        // either: if it was noted as going back furthest, its retention moved
        // on, and the oldest day goes to the next one
        let (g, h) = (group.to_string(), host.clone());
        if let Err(e) = on_db(db, move |conn| Ok(crate::chunks::forget_deepest(conn, &g, &h)?)).await {
            println!("[CHUNK] {group}: couldnt forget the server going back furthest: {e:#}");
        }
        drop_unkept_days(ctx, db, group).await;
        return failed(TooOld { group: group.to_string(), host, oldest_day }.into()).await;
    }
    // the split's days and, for its newest, how far down this server's
    // backfill got before the split (its high mark when it has no cursor)
    let (g, key) = (group.to_string(), cursor_key(&ctx.pool, server, group));
    let bounds = on_db(db, move |conn| {
        let days = crate::chunks::oldest_day(conn, &g)?.zip(crate::chunks::newest_day(conn, &g)?);
        Ok((days, db::get_group_state(conn, &key)?))
    })
    .await;
    let (days, cursor) = match bounds {
        Ok((Some(days), state)) => (days, state.map_or(last, |s| (s.backfill_cursor.max(0) as u64).min(last))),
        Ok((None, _)) => return failed(anyhow!("{group} has no day chunks")).await,
        Err(e) => return failed(e).await,
    };
    let (start, end) = match chunk_range(
        &ctx.pool,
        server,
        group,
        (first, last),
        day,
        days,
        (cursor, settings.chunk_safety),
        chunk.reread,
    )
    .await
    {
        Ok(range) => range,
        Err(e) => return failed(e.into()).await,
    };
    // days only this server has are bounded all together, see `chunks::Speculative`
    let floor = match runner_up_day {
        Some(r) => {
            let g = group.to_string();
            let dropped = on_db(db, move |db| {
                // under the window's lock, as `reach_further` changes it: else its count or extension could be
                // written over, and the days it just added dropped
                Ok(db.tx(|conn| {
                    conn.lock(&crate::chunks::speculative_lock(&g))?;
                    let s = match crate::chunks::speculative(conn, &g)? {
                        Some(s) => crate::chunks::Speculative { runner_up_day: r, ..s },
                        None => crate::chunks::Speculative {
                            runner_up_day: r,
                            reach: SPECULATIVE_WINDOW,
                            days: 0,
                            articles: 0,
                        },
                    };
                    crate::chunks::set_speculative(conn, &g, &s)?;
                    let floor = r - s.reach;
                    Ok((floor, crate::chunks::drop_before(conn, &g, floor)?))
                })?)
            })
            .await;
            let floor = match dropped {
                Ok((floor, 0)) => floor,
                Ok((floor, n)) => {
                    println!(
                        "[CHUNK] {group}: {n} days further back than {} days before the other servers go back are left to the sweeps",
                        r - floor
                    );
                    floor
                }
                Err(e) => return failed(e).await,
            };
            // this one was among them (claimed, not done): nothing to do
            if day < floor {
                return Ok(Progress::default());
            }
            Some(floor)
        }
        None => None,
    };
    if start > end {
        finish_chunk(db, chunk, (alone, floor, oldest), 0).await?;
        return Ok(Progress::default());
    }

    let pass = Pass { ctx, settings, db, group, server, key: cursor_key(&ctx.pool, server, group) };
    let window = alone.then_some((day * 86_400, (day + 1) * 86_400));
    let ranged = pass.process_range_dated(start as i64, end as i64, "CHUNK", window, progress).await;
    match ranged {
        Ranged { error: Some(e), .. } => failed(e).await,
        Ranged { saved, complete: true, in_window, .. } => {
            finish_chunk(db, chunk, (alone, floor, oldest), in_window).await?;
            Ok(saved)
        }
        Ranged { saved, .. } => {
            // stopped part way, not failed: given back as it is
            let c = chunk.clone();
            on_db(db, move |conn| Ok(crate::chunks::release(conn, &c)?)).await?;
            Ok(saved)
        }
    }
}

/// The article numbers day chunk `day` of a split over `days` (oldest,
/// newest) covers on `server`, whose low and high marks are `first` and
/// `last`: from where the day starts to where the next one does, each found
/// by the same date search an hour early (`CHUNK_OVERLAP`), and to an hour
/// past the day when that is further. The oldest day starts at `first`, the
/// newest ends at `cursor`. Neighbouring days share where one ends and the
/// next starts, soo every number from `first` to `cursor` is some chunk's
/// however forged Dates move the searches: a forged Date only moves an
/// article into another chunk. Empty (start past end) when the day's posts
/// are all in its neighbours.
///
/// That holds on one server; neighbouring days done on different servers
/// search their own numbering, and around a hole a run of forged Dates can
/// move the two searches differently, leaving articles in neither. Each end
/// also reaches `safety` (`CHUNK_SAFETY`) numbers past where its search put it (the
/// articles a neighbour has too are dropped as duplicates when saved): a
/// shift up to that many articles is covered, a forged run moving a search
/// further can still leave a gap, for the carriers' sweeps once the
/// chunks are done (see `run_pass` and `run_sweep`).
///
/// A day read again (`reread`, see `chunks::reopen_done`) is bounded by its dates alone, the newest day too: this
/// server's cursor has moved down since the first read (its sweep), and would cut the day short or leave it empty.
#[allow(clippy::too_many_arguments)]
pub async fn chunk_range(
    pool: &Pool,
    server: usize,
    group: &str,
    (first, last): (u64, u64),
    day: i64,
    (oldest_day, newest_day): (i64, i64),
    (cursor, safety): (u64, u64),
    reread: bool,
) -> crate::nntp::Result<(u64, u64)> {
    let starts = |day: i64| pool.article_at(server, group, first, last, day * 86_400 - CHUNK_OVERLAP);
    if !reread && day <= oldest_day && day >= newest_day {
        return Ok((first, cursor));
    }
    let next = starts(day + 1).await?;
    // a start past the next day's (forged Dates) is moved down to it
    let start = if day <= oldest_day { first } else { starts(day).await?.min(next).saturating_sub(safety).max(first) };
    let end = if day >= newest_day && !reread {
        cursor
    } else {
        let past = pool.article_at(server, group, first, last, (day + 1) * 86_400 + CHUNK_OVERLAP).await?;
        let end = next.max(past).saturating_sub(1).saturating_add(safety);
        if reread { end.min(last) } else { end.min(cursor) }
    };
    Ok((start, end))
}

/// Whether `server`, which keeps split `group` from `oldest` (see
/// `Pool::retention`; with whether its first dates agree), goes back furthest of the indexing servers: the one to
/// index the split's oldest day, and the days before the second furthest
/// goes back to (returned with it, when there is one). Found by asking each
/// server how far back it keeps the group the first time, then kept, soo it
/// doesnt move as retention rolls on (asked again once that server is no
/// longer an indexing one, no longer carries the group, or no longer keeps
/// the day: `index_chunk` forgets it on a 411 or a day older than it keeps).
/// A server whose first dates disagree goes back furthest only when every
/// one's do: a forged run could make it look deeper than it is. A server
/// that cant be asked fails it: the chunk goes back and is tried again later.
async fn goes_back_furthest(
    ctx: &PassContext,
    db: &Db,
    group: &str,
    server: usize,
    oldest: (i64, bool),
) -> Result<(bool, Option<i64>)> {
    let host = ctx.pool.host(server);
    let servers = ctx.pool.indexing_servers();
    let g = group.to_string();
    let known =
        on_db(db, move |conn| Ok((crate::chunks::deepest(conn, &g)?, crate::chunks::runner_up(conn, &g)?))).await?;
    if let (Some(deepest), runner_up) = known
        && servers.iter().any(|&i| ctx.pool.host(i) == deepest)
    {
        return Ok((deepest == host, runner_up));
    }

    let total_conns = ctx.pool.indexing_connections();
    // every carrier's (whether unsure, since, host)
    let mut found = vec![(!oldest.1, oldest.0, host.clone())];
    for other in servers.into_iter().filter(|&i| i != server) {
        if total_conns >= 50 && ctx.pool.connections(other) * 20 < total_conns {
            continue;
        }
        let (_, low, high, _) = match ctx.pool.group_on(other, group).await {
            Ok(info) => info,
            Err(e) if matches!(e.code(), Some(403 | 411 | 430 | 500 | 501)) => continue,
            Err(e) => return Err(e.into()),
        };
        match ctx.pool.retention(other, group, low, high).await? {
            Retention::Since(t) => found.push((false, t, ctx.pool.host(other))),
            Retention::Unsure(t) => found.push((true, t, ctx.pool.host(other))),
            Retention::Empty => {}
        }
    }
    // the deepest of those whose dates agree, a tie to `server` (asked
    // first); the days before the next deepest (by the same order) are its
    // alone
    found.sort_by_key(|(unsure, t, h)| (*unsure, *t, *h != host));
    let (_, since, deepest) = found.remove(0);
    let runner_up = found.first().map(|(_, t, _)| *t);
    let (g, d) = (group.to_string(), deepest.clone());
    on_db(db, move |conn| {
        crate::chunks::set_deepest(conn, &g, &d, since)?;
        if let Some(r) = runner_up {
            crate::chunks::set_runner_up(conn, &g, r)?;
        }
        Ok(())
    })
    .await?;
    println!("[CHUNK] {group}: {deepest} goes back furthest, it does the oldest day");
    Ok((deepest == host, runner_up))
}

struct Pass<'a> {
    ctx: &'a PassContext,
    settings: &'a PassSettings,
    db: &'a Db,
    group: &'a str,
    server: usize,
    key: String,
}

impl Pass<'_> {
    async fn live<P>(&self, state: db::GroupState, last: i64, progress: &mut P) -> Result<Progress>
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let group = self.group;
        self.ctx.states.with(group, |st| st.backfilling = false);
        let start = state.live_cursor + 1;

        // nothing new since last check
        if start > last {
            if self.ctx.verbose {
                println!("no new articles");
            }

            let newly_idle = self.ctx.states.with(group, |st| !std::mem::replace(&mut st.idle, true));
            if newly_idle {
                println!("[LIVE] {group} no new articles, idle");
            }

            return Ok(Progress::default());
        }

        let end = last.min(start + self.settings.batch_size.max(1) - 1);
        let ranged = self.process_range(start, end, "LIVE", progress).await;

        // done up from the cursor: the cursor moves past it, failed or not
        if let Some(low) = ranged.low_done {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_live_cursor(conn, &key, low as i64)?)).await?;
        }
        ranged.into_result()
    }

    async fn backfill<P>(&self, state: db::GroupState, first: i64, last: i64, progress: &mut P) -> Result<Progress>
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let group = self.group;
        let end = state.backfill_cursor.min(last);

        if end < first {
            // only idle when the live side is caught up too, otherwise dynamic
            // mode would drain a live backlog at one batch per idle sleep
            let live_caught_up = state.live_cursor >= last || self.settings.mode == "backfill";

            let newly_idle = self.ctx.states.with(group, |st| {
                st.backfilling = false;
                st.phase = Phase::Live;
                let newly = !st.idle && live_caught_up;
                if newly {
                    st.idle = true;
                }
                newly
            });

            if newly_idle {
                println!("[BACKFILL] {group} {end} < first {first}, nothing to backfill, idle");
            }

            return Ok(Progress::default());
        }

        // grab a chunk going backwards from the cursor
        let start = first.max(end - self.settings.batch_size.max(1) + 1);
        let ranged = self.process_range(start, end, "BACKFILL", progress).await;

        // done down from the cursor: the cursor moves past it, failed or not
        if let Some(high) = ranged.high_done {
            let key = self.key.clone();
            on_db(self.db, move |conn| Ok(db::update_backfill_cursor(conn, &key, high as i64 - 1)?)).await?;
        }
        let saved = ranged.into_result()?;
        self.ctx.states.with(group, |st| st.backfilling = true);
        Ok(saved)
    }

    /// Index `start..=end`. The range is cut into `request_size` slices that
    /// stream in over the server's connections; each slice is parsed, named and
    /// saved as soon as it lands while the rest keep downloading. Backfill takes
    /// the newest slices first.
    ///
    /// A slice failing stops new ones from starting, but the ones that made it
    /// are still saved, and `Ranged` says how far the range is done from
    /// either end, soo a cursor can move past them and they arent fetched again.
    async fn process_range<P>(&self, start: i64, end: i64, kind: &str, progress: &mut P) -> Ranged
    where
        P: FnMut(&Progress) + ?Sized,
    {
        self.process_range_dated(start, end, kind, None, progress).await
    }

    /// Moves the cursor of `side` past what `finished` holds done of `start..=end` next to it (see `Finished::ends`),
    /// when that is further than `moved`, the last place this pass moved it to. One that can't be written now is
    /// moved by the next slice or once the pass ends.
    async fn advance(&self, side: Side, finished: &Finished, (start, end): (i64, i64), moved: &mut Option<u64>) {
        let (low, high) = finished.ends(start.max(0) as u64, end.max(0) as u64);
        let Some(to) = (match side {
            Side::Up => low,
            Side::Down => high,
        }) else {
            return;
        };
        if *moved == Some(to) {
            return;
        }
        let key = self.key.clone();
        let saved = on_db(self.db, move |conn| {
            match side {
                Side::Up => db::update_live_cursor(conn, &key, to as i64)?,
                Side::Down => db::update_backfill_cursor(conn, &key, to as i64 - 1)?,
            }
            Ok(())
        })
        .await;
        if saved.is_ok() {
            *moved = Some(to);
        }
    }

    /// `process_range`, also how many articles retrieved are dated within
    /// `window` (unix seconds, from up to before), 0 when there is none.
    async fn process_range_dated<P>(
        &self,
        start: i64,
        end: i64,
        kind: &str,
        window: Option<(i64, i64)>,
        progress: &mut P,
    ) -> Ranged
    where
        P: FnMut(&Progress) + ?Sized,
    {
        let mut in_window = 0i64;
        let (pool, group) = (&self.ctx.pool, self.group);
        if crate::filters::group_blocked(group) {
            let (start, end) = (start.max(0) as u64, end.max(0) as u64);
            let saved = Progress::default();
            return Ranged {
                saved,
                complete: true,
                in_window,
                low_done: Some(end),
                high_done: Some(start),
                error: None,
            };
        }
        // no slice bigger than the whole unsaved budget: it would hold more
        // headers than the cap allows
        let size = self.settings.request_size.min(pool.max_unsaved() as u64);
        let slices = make_slices(start.max(0) as u64, end.max(0) as u64, size, kind == "BACKFILL");
        let total = slices.len();

        let width = stream_width(kind);
        // everything but live takes from the bulk share of the unsaved budget: new posts never wait behind old days
        let mut rx = pool.stream_headers(group, self.server, slices, width, kind != "LIVE", self.ctx.stop.clone());
        let mut saved = Progress::default();
        let mut done = 0;
        let mut finished = Finished::default();
        let mut error: Option<anyhow::Error> = None;
        // a failed save stops the saving, a failed fetch only stops the fetching
        let mut save_failed = false;
        // dated ends of what this pass saved, for the stats dashboard's history numbers
        let (mut low, mut high): (Option<db::Dated>, Option<db::Dated>) = (None, None);

        // the cursor this pass moves goes on past what's done as the slices come in, not only once the pass ends: with
        // the writers busy a pass can take longer than prod runs between restarts, and one cut short kept nothing
        let side = match kind {
            "LIVE" => Some(Side::Up),
            "BACKFILL" | "SWEEP" => Some(Side::Down),
            _ => None,
        };
        let mut moved = None;
        loop {
            if let Some(side) = side {
                self.advance(side, &finished, (start, end), &mut moved).await;
            }
            let Some(slice) = rx.recv().await else { break };
            // the slice's room in the unsaved budget goes back once it's parsed
            // (or skipped)
            let unsaved = slice.unsaved;
            let headers: Vec<Overview> = match slice.result {
                Ok(h) => {
                    self.ctx.states.read(group, &self.key, (slice.start, slice.end));
                    h
                }
                // 423 (or 420) = no articles in that slice
                Err(e) if e.is_empty_range() => {
                    self.ctx.states.read(group, &self.key, (slice.start, slice.end));
                    done += 1;
                    finished.add(slice.start, slice.end);
                    continue;
                }
                // a 5xx: passed over only once refused for a while (see `REFUSED_FOR`), until then
                // the slice fails the pass like any other failure and is asked again later
                Err(e) if e.is_permanent() => {
                    let range = (slice.start, slice.end);
                    if self.ctx.states.refused(group, &self.key, range, Instant::now()) {
                        println!(
                            "[{kind}] {group} {}-{} refused for over {}m ({e}), skipping",
                            slice.start,
                            slice.end,
                            REFUSED_FOR.as_secs() / 60
                        );
                        done += 1;
                        finished.add(slice.start, slice.end);
                    } else {
                        println!("[{kind}] {group} {}-{} refused ({e}), trying it again later", slice.start, slice.end);
                        error.get_or_insert_with(|| anyhow::Error::new(e));
                    }
                    continue;
                }
                Err(e) => {
                    error.get_or_insert_with(|| anyhow::Error::new(e));
                    continue;
                }
            };

            if save_failed {
                continue;
            }

            if let Some((from, to)) = window {
                in_window += headers
                    .iter()
                    .filter(|h| crate::dates::posted_timestamp(&h.date).is_some_and(|t| (from..to).contains(&t)))
                    .count() as i64;
            }
            let dated = slice_date(&headers);
            match save_slice(pool, self.db, group, headers, unsaved).await {
                Ok(p) => {
                    if let Some(d) = dated {
                        low = low.filter(|l| l.0 <= d.0).or(Some(d));
                        high = high.filter(|h| h.0 >= d.0).or(Some(d));
                    }
                    saved.add(&p);
                    done += 1;
                    finished.add(slice.start, slice.end);
                    progress(&p);
                }
                Err(e) => {
                    save_failed = true;
                    error.get_or_insert(e);
                }
            }
        }

        if let (Some(low), Some(high)) = (low, high) {
            let key = self.key.clone();
            if let Err(e) = on_db(self.db, move |conn| Ok(db::save_group_dates(conn, &key, low, high)?)).await {
                error.get_or_insert(e);
            }
        }

        if error.is_none() && saved.articles == 0 && finished.ends(start.max(0) as u64,end.max(0) as u64).0==Some(end.max(0) as u64) {
            println!("[{kind}] {group} {start}-{end} empty, skipping");
        }

        if saved.skipped > 0 {
            println!("[{kind}] {group} {start}-{end} gathered {} articles obfuscated per article", saved.skipped);
        }

        if saved.articles > 0 {
            self.ctx.states.with(group, |st| st.idle = false);
        }

        if self.ctx.verbose {
            println!("[{kind}] {group} {} headers in {done}/{total} slices", saved.articles);
        }

        // failed or stopped early: what got saved stays, the cursor moves past
        // the done run next to it and waits for the rest
        let (low_done, high_done) = finished.ends(start.max(0) as u64, end.max(0) as u64);
        Ranged { saved, complete: low_done==Some(end.max(0) as u64), in_window, low_done, high_done, error }
    }
}

/// `group`'s articles posted from `from` to `to` (unix seconds) on `server`, read and saved like any pass: the
/// headers of a time window another server's listing left out (see `repair`). No cursor moves. A window of more than
/// `max_articles` isn't read.
pub async fn reread_window(
    ctx: &PassContext,
    settings: &PassSettings,
    db: &Db,
    group: &str,
    server: usize,
    (from, to): (i64, i64),
    max_articles: u64,
) -> Result<Progress> {
    let (_, first, last, _) = ctx.pool.group_on(server, group).await?;
    if last < first {
        return Ok(Progress::default());
    }
    let start = ctx.pool.article_at(server, group, first, last, from).await?;
    let end = ctx.pool.article_at(server, group, first, last, to).await?.saturating_sub(1).min(last);
    reread_range(ctx,settings,db,group,server,start,end,max_articles).await
}

#[derive(Debug)]
pub(crate) struct RereadWindowTooWide {group:String,count:u64,start:u64,end:u64}
impl std::fmt::Display for RereadWindowTooWide {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"{}: {} articles from {} to {}, more than a window takes",self.group,self.count,self.start,self.end)}
}
impl std::error::Error for RereadWindowTooWide {}

/// The exact ordinary repair range pipeline, after provider-specific bounds
/// have been observed. Native callers retain/join their cancellable probes,
/// then await this whole parse/admission/commit/NXV operation without cancel.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reread_range(ctx:&PassContext,settings:&PassSettings,db:&Db,group:&str,server:usize,start:u64,end:u64,max_articles:u64)->Result<Progress> {
    if end < start {
        return Ok(Progress::default());
    }
    if end - start + 1 > max_articles {
        return Err(RereadWindowTooWide {group:group.into(),count:end-start+1,start,end}.into());
    }
    // a key no cursor has: the dates this pass finds are noted nowhere
    let key = format!("repair:{group}@{}", ctx.pool.host(server));
    let pass = Pass { ctx, settings, db, group, server, key };
    let mut quiet = no_progress;
    pass.process_range(start as i64, end as i64, "repair", &mut quiet).await.into_result()
}

/// Which way a pass's cursor goes: up (live, from the cursor to the newest) or down (backfill and sweeps).
#[derive(Clone, Copy)]
enum Side {
    Up,
    Down,
}

/// How a range pass went. `low_done` / `high_done`: the range is done from
/// its start up to `low_done`, and from `high_done` up to its end (None when
/// its first, or its last, slice isnt done): what a cursor going up, or going
/// down, can move past even when the pass failed or was stopped.
struct Ranged {
    saved: Progress,
    /// every slice done: the whole range, and its cursor can move
    complete: bool,
    in_window: i64,
    low_done: Option<u64>,
    high_done: Option<u64>,
    /// the first failure; what got saved before and after it stays saved
    error: Option<anyhow::Error>,
}

impl Ranged {
    /// the progress, or the failure (once the caller moved its cursor)
    fn into_result(self) -> Result<Progress> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.saved),
        }
    }
}

/// The slices of a range that are done (saved, empty or not available).
#[derive(Default)]
struct Finished(Vec<(u64, u64)>);

impl Finished {
    fn add(&mut self, start: u64, end: u64) {
        self.0.push((start, end));
        self.0.sort_unstable();
        let mut kept=0;
        for i in 0..self.0.len() {
            let (a,b)=self.0[i];
            if kept>0 && a<=self.0[kept-1].1.saturating_add(1) {
                self.0[kept-1].1=self.0[kept-1].1.max(b);
            }else{self.0[kept]=(a,b);kept+=1;}
        }
        self.0.truncate(kept);
    }

    /// How far `start..=end` is done from each end: the last number of the
    /// done run starting at `start`, the first of the one ending at `end`.
    fn ends(&self, start: u64, end: u64) -> (Option<u64>, Option<u64>) {
        let done=&self.0;

        let mut low = None;
        let mut next = start;
        for &(a, b) in done {
            if a != next {
                break;
            }
            low = Some(b);
            match b.checked_add(1) {
                Some(n) => next = n,
                None => break,
            }
        }

        let mut high = None;
        let mut prev = end;
        for &(a, b) in done.iter().rev() {
            if b != prev {
                break;
            }
            high = Some(a);
            match a.checked_sub(1) {
                Some(n) => prev = n,
                None => break,
            }
        }
        (low, high)
    }
}

/// The middle article number of a slice and when its articles were posted: the
/// median of a sample of their dates, soo a few forged or odd dates dont count.
fn slice_date(headers: &[Overview]) -> Option<db::Dated> {
    let first = headers.iter().map(|h| h.number).min()?;
    let last = headers.iter().map(|h| h.number).max()?;
    let step = (headers.len() / 64).max(1);
    let mut posted: Vec<i64> =
        headers.iter().step_by(step).filter_map(|h| crate::dates::posted_timestamp(&h.date)).collect();
    if posted.is_empty() {
        return None;
    }
    let mid = posted.len() / 2;
    let (_, median, _) = posted.select_nth_unstable(mid);
    Some((((first + last) / 2) as i64, *median))
}

/// Parse one slice into releases, look up real names, save.
/// `unsaved` is the slice's room in the unsaved budget: the parsed releases
/// stand for its headers until they are committed, soo it goes back only then.
pub(crate) async fn save_slice(
    pool: &Arc<Pool>,
    db: &Db,
    group: &str,
    headers: Vec<Overview>,
    unsaved: crate::nntp::Unsaved,
) -> Result<Progress> {
    let articles = headers.len() as i64;
    if crate::filters::group_blocked(group) {
        return Ok(Progress { articles, ..Progress::default() });
    }
    let span = crate::metrics::span("indexer.save_slice");
    let saved = parse_and_save(pool, db, group, headers, unsaved).await;
    if saved.is_err() {
        span.failed();
    }
    saved
}

async fn parse_and_save(
    pool: &Arc<Pool>,
    db: &Db,
    group: &str,
    headers: Vec<Overview>,
    unsaved: crate::nntp::Unsaved,
) -> Result<Progress> {
    let articles = headers.len() as i64;
    crate::profile::SLICES.add(1);
    crate::profile::HEADERS.add(articles as u64);

    // parsing is cpu work, keep it off the async threads
    let t = std::time::Instant::now();
    let parsing = {
        let group = group.to_string();
        move || {
            let _span = crate::metrics::span("indexer.parse_slice");
            // articles whose every subject is random are left to the header store's linker (`xref::hold`)
            let articles = crate::xref::hold(&group, headers_to_articles(headers));
            let (articles, uploads, skipped) = crate::filters::gather_per_article(&group, articles);
            let mut releases: Vec<Release> =
                crate::collections::group_articles(&group, articles).into_values().collect();
            releases.extend(uploads);
            anyhow::Ok((releases, skipped))
        }
    };
    let (mut releases, skipped) =
        tokio::task::spawn_blocking(parsing).await.map_err(|e| anyhow!("parse task failed: {e}"))??;
    crate::profile::PARSE.add_since(t);
    crate::profile::Load::add_since(&crate::profile::LOAD.parse_ns, t);

    // real names from par2/nfo bodies, every release looked up concurrently,
    // a few first segments each (see `name_sources`)
    let t = std::time::Instant::now();
    // once the stage has a body budget it reads them, not the save path
    let names = if crate::archive::inline_lookups() {
        let _span = crate::metrics::span("indexer.first_names");
        first_names(pool.clone(), releases.iter().map(name_sources).collect()).await?
    } else {
        Vec::new()
    };
    crate::profile::NAMES.add_since(t);

    let mut bytes = 0;
    for (i, release) in releases.iter_mut().enumerate() {
        release.display_name = names.get(i).cloned().flatten();
        release.complete = is_complete(&release.articles);
        release.group = group.to_string();
        release.poster = release.articles[0].author.clone();
        release.date = release.articles[0].date.clone();

        bytes += release.articles.iter().map(|a| a.bytes).filter(|b| *b > 0).sum::<i64>();
    }

    let count = releases.len() as i64;
    {
        let span = crate::metrics::span("indexer.wait_for_save");
        let saved = db.save(releases).await;
        if saved.is_err() {
            span.failed();
        }
        saved?;
    }
    // with the header store on, the slice's rows durable in its logs too before the slice counts as read
    if let Some(ticket) = crate::nxv_store::live::sync_point(group)? {
        tokio::task::spawn_blocking(move || ticket.wait(Duration::from_secs(600)))
            .await
            .map_err(|e| anyhow!("waiting for the header store failed: {e}"))??;
    }
    // the parsed releases stood for these headers until they were committed: the room goes back only now, soo
    // slices waiting on a slow writer stay inside `max_unsaved_headers`
    drop(unsaved);

    Ok(Progress { articles, bytes, releases: count, skipped })
}

/// bodies a release's name lookup tries at most
const MAX_NAME_BODIES: usize = 3;

/// Bodies worth fetching for a real name: base par2 files first, then nfos,
/// only their first segment (where par2 and nfo files keep what names a
/// release, and it comes once per file, so a release is looked up once
/// however many slices it spans) and `MAX_NAME_BODIES` at most. A big
/// obfuscated par2 comes in thousands of segments of most of a MB each;
/// trying them one after another held a slice for minutes.
fn name_sources(release: &Release) -> Vec<(String, Extract)> {
    type Matches = fn(&str) -> bool;
    let sources: [(Matches, Extract); 2] = [(par2::is_base_par2, par2::display_name), (nfo::is_nfo, nfo::display_name)];

    sources
        .iter()
        .flat_map(|(matches, extract)| {
            release
                .articles
                .iter()
                .filter(|a| a.part.unwrap_or(1) == 1 && matches(&a.subject))
                .map(|a| (a.message_id.clone(), *extract))
        })
        .take(MAX_NAME_BODIES)
        .collect()
}

/// `start..=end` cut into `size` long slices, newest first when `descending`.
pub fn make_slices(start: u64, end: u64, size: u64, descending: bool) -> Vec<(u64, u64)> {
    let size = size.max(1);
    let mut slices = Vec::new();
    let mut a = start;

    while a <= end {
        let b = end.min(a.saturating_add(size - 1));
        slices.push((a, b));
        if b == u64::MAX {
            break;
        }
        a = b + 1;
    }

    if descending {
        slices.reverse();
    }
    slices
}

/// One group at a time from sync code, on the pool's active server.
/// The background indexer runs many passes at once instead (see `bg_indexer`).
pub struct Indexer {
    pub client: BlockingPool,
    pub mode: String,
    pub verbose: bool,
    pub state: RunStates,
    pub last_batch_articles: i64,
    pub last_batch_bytes: i64,
    pub last_batch_releases: i64,
    /// article numbers per pass over a group
    pub batch_size: i64,
    /// article numbers per XOVER request
    pub request_size: u64,
    /// set to stop handing out new slices, the current pass ends without moving cursors
    pub stop: Arc<AtomicBool>,
    db: Db,
}

impl Indexer {
    pub fn new(client: BlockingPool, mode: &str, db: Db) -> Self {
        let defaults = PassSettings::default();
        Indexer {
            client,
            mode: mode.to_string(),
            verbose: false,
            state: RunStates::default(),
            last_batch_articles: 0,
            last_batch_bytes: 0,
            last_batch_releases: 0,
            batch_size: defaults.batch_size,
            request_size: defaults.request_size,
            stop: Arc::new(AtomicBool::new(false)),
            db,
        }
    }

    pub fn is_idle(&self, group: &str) -> bool {
        self.state.is_idle(group)
    }

    pub fn is_backfilling(&self, group: &str) -> bool {
        self.state.is_backfilling(group)
    }

    pub fn all_idle(&self, groups: &[String]) -> bool {
        self.state.all_idle(groups)
    }

    /// switching modes starts every group over in the backfill phase
    pub fn set_mode(&mut self, mode: &str) {
        self.mode = mode.to_string();
        self.state.reset();
    }

    /// One pass over `group` with no progress reporting.
    pub fn index_group(&mut self, group: &str) -> Result<()> {
        self.index_group_with(group, &mut no_progress)
    }

    /// One pass over `group`: a batch of live or backfill articles depending
    /// on the mode. `progress` hears about every slice as it is saved.
    pub fn index_group_with(&mut self, group: &str, progress: &mut dyn FnMut(&Progress)) -> Result<()> {
        self.last_batch_articles = 0;
        self.last_batch_bytes = 0;
        self.last_batch_releases = 0;
        if crate::filters::group_blocked(group) {
            return Ok(());
        }

        let ctx = PassContext {
            pool: self.client.pool.clone(),
            states: self.state.clone(),
            stop: self.stop.clone(),
            verbose: self.verbose,
        };
        let settings = PassSettings {
            mode: self.mode.clone(),
            batch_size: self.batch_size,
            request_size: self.request_size,
            ..PassSettings::default()
        };
        let db = self.db.clone();

        let saved = self.client.block_on(async {
            // the active server, which moves when another server has to carry the group
            ctx.pool.select_group(group).await?;
            let server = ctx.pool.active_index();
            run_pass(&ctx, &settings, &db, group, server, progress).await
        })?;

        self.last_batch_articles = saved.articles;
        self.last_batch_bytes = saved.bytes;
        self.last_batch_releases = saved.releases;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Article;

    fn native_fixture() -> (crate::pg::testing::Schema, Db) {
        let schema = crate::pg::testing::Schema::native_writer().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let database: String = schema.pool.get().unwrap().query_one("select current_database()", &[]).unwrap().get(0);
        let db = shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            PgWriteLimits::default(),
        )
        .unwrap();
        (schema, db)
    }

    #[test]
    fn a_refused_slice_is_passed_over_only_after_an_hour_and_three_refusals() {
        let states = RunStates::default();
        let t0 = Instant::now();
        let refused = |range, at| states.refused("g", "g@a", range, t0 + at);
        assert!(!refused((100, 199), Duration::ZERO), "the first refusal is tried again");
        assert!(!refused((100, 199), Duration::from_secs(10)));
        assert!(!refused((150, 249), Duration::from_secs(20)), "three refusals, but not an hour yet");
        assert!(!states.refused("g", "g@b", (100, 199), t0 + REFUSED_FOR), "another server's numbers count apart");
        assert!(!refused((300, 399), REFUSED_FOR), "a range not refused before starts over");
        assert!(refused((120, 130), REFUSED_FOR), "an hour after the first, overlapping ranges are passed over");
        assert!(refused((100, 199), REFUSED_FOR + REFUSED_FOR / 2), "and stay passed over while asked again");
        assert!(!refused((90, 199), REFUSED_FOR * 2), "but not past what was passed over");
        assert!(!refused((100, 199), REFUSED_FOR * 3), "and forgotten once not asked for the hour");

        // a read in between ends it
        assert!(!refused((300, 399), REFUSED_FOR + Duration::from_secs(1)));
        states.read("g", "g@a", (250, 449));
        assert!(!refused((300, 399), REFUSED_FOR * 3), "read since: counted from the start again");

        // a read of part of a refused range ends only that part
        assert!(!refused((1000, 1099), Duration::ZERO));
        assert!(!refused((1000, 1099), Duration::from_secs(10)));
        states.read("g", "g@a", (950, 1049));
        assert!(refused((1050, 1149), REFUSED_FOR), "the rest kept its hour");
        assert!(!refused((1000, 1049), REFUSED_FOR), "the part read starts over");

        // one never asked again (past retention) doesnt keep its room for good
        assert!(!refused((5000, 5099), REFUSED_FOR));
        assert!(!refused((5000, 5099), REFUSED_FOR * 2));
        assert!(!refused((5000, 5099), REFUSED_FOR * 2 + REFUSED_IDLE), "forgotten: starts over");
    }

    #[test]
    fn more_refused_slices_than_are_kept_still_reach_the_hour() {
        let states = RunStates::default();
        let t0 = Instant::now();
        let slices = REFUSED_KEPT as u64 * 2;
        let pass =
            |at: Duration| (0..slices).filter(|i| states.refused("g", "g@a", (i * 100, i * 100 + 99), t0 + at)).count();
        // every pass asks them all again, as a day chunk read again whole does:
        // the first ones keep their history
        assert_eq!(pass(Duration::ZERO), 0);
        assert_eq!(pass(Duration::from_secs(60)), 0);
        assert_eq!(pass(REFUSED_FOR), REFUSED_KEPT, "the ones kept are passed over after the hour");
        // the rest were noted as room freed: their hour runs from then, and
        // the ones passed over stay passed over meanwhile
        assert_eq!(pass(REFUSED_FOR + Duration::from_secs(60)), REFUSED_KEPT);
        assert_eq!(pass(REFUSED_FOR * 2), 2 * REFUSED_KEPT, "an hour later every slice is passed over");
    }

    #[test]
    fn a_name_lookup_tries_first_segments_only_and_a_few_bodies_at_most() {
        let article = |n: u64, file: &str, part: i64| Article {
            number: n,
            subject: format!(r#""{file}" yEnc ({part}/900)"#),
            message_id: format!("<{n}>"),
            filename: Some(file.into()),
            part: Some(part),
            ..Default::default()
        };
        let ids = |r: &Release| name_sources(r).into_iter().map(|(id, _)| id).collect::<Vec<_>>();

        // a big par2 with its first segment, and an nfo
        let mut big = Release {
            articles: (0..900).map(|i| article(i, "x.par2", 900 - i as i64)).collect(),
            ..Default::default()
        };
        big.articles.push(article(1000, "x.nfo", 1));
        assert_eq!(ids(&big), ["<899>", "<1000>"]);
        // seen part way, without its first segment: nothing worth a body
        big.articles.retain(|a| a.part != Some(1));
        assert!(ids(&big).is_empty());

        // many par2 files: no more than MAX_NAME_BODIES of them
        let many =
            Release { articles: (0..10).map(|i| article(i, &format!("f{i}.par2"), 1)).collect(), ..Default::default() };
        assert_eq!(ids(&many).len(), MAX_NAME_BODIES);
    }

    #[test]
    fn finished_slices_count_from_either_end_up_to_the_first_gap() {
        let mut f = Finished::default();
        assert_eq!(f.ends(1, 40), (None, None));
        f.add(31, 40);
        f.add(1, 10);
        f.add(21, 30);
        assert_eq!(f.ends(1, 40), (Some(10), Some(21)), "11-20 missing");
        f.add(11, 20);
        assert_eq!(f.ends(1, 40), (Some(40), Some(1)));
        let mut top = Finished::default();
        top.add(u64::MAX - 9, u64::MAX);
        assert_eq!(top.ends(0, u64::MAX), (None, Some(u64::MAX - 9)));
        let mut bottom = Finished::default();
        bottom.add(0, 9);
        assert_eq!(bottom.ends(0, u64::MAX), (Some(9), None));
    }

    #[test]
    fn adaptively_split_finished_ranges_coalesce_and_never_cover_a_missing_leaf() {
        let mut finished=Finished::default();
        for (a,b) in [(1,4),(9,12),(5,6)] {finished.add(a,b);}
        assert_eq!(finished.ends(1,12),(Some(6),Some(9)),"the unreceived7-8 leaf prevents completion");
        finished.add(7,8);assert_eq!(finished.ends(1,12),(Some(12),Some(1)));
        assert_eq!(finished.0,[(1,12)],"one original slice split into many leaves retains one covered interval");
        finished.add(5,6);assert_eq!(finished.0,[(1,12)],"replayed leaves cannot grow the retained interval set");
    }

    /// `#key`s that differ by case are two servers, and the host in a key is
    /// lowercased as before
    #[test]
    fn cursor_keys_keep_the_case_of_a_servers_key() {
        let mut upper = crate::config::UsenetServer::new("A.Example", "u", "p", 563);
        upper.key = Some("Block".into());
        let mut lower = upper.clone();
        lower.key = Some("block".into());
        let plain = crate::config::UsenetServer::new("B.Example", "u", "p", 563);
        let pool = Pool::new(&[upper, lower, plain]);
        assert_eq!(cursor_key(&pool, 0, "g"), "g@a.example#Block");
        assert_eq!(cursor_key(&pool, 1, "g"), "g@a.example#block");
        assert_eq!(cursor_key(&pool, 2, "g"), "g@b.example");
    }

    /// a lone server keeps the plain group name, unless it has a `#key`
    #[test]
    fn a_lone_keyed_server_keeps_its_keyed_cursor() {
        let plain = crate::config::UsenetServer::new("A.Example", "u", "p", 563);
        let mut keyed = crate::config::UsenetServer::new("B.Example", "u", "p", 563);
        keyed.key = Some("Block".into());
        assert_eq!(cursor_key(&Pool::new(&[plain]), 0, "g"), "g");
        assert_eq!(cursor_key(&Pool::new(&[keyed]), 0, "g"), "g@b.example#Block");
    }

    #[test]
    #[ignore = "requires an explicit isolated ATLAS_PG_TEST_URL"]
    fn each_speculative_window_has_to_hold_enough_on_its_own() {
        let (schema, db) = native_fixture();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(each_speculative_window(&db));
        drop(db);
        drop(schema);
    }

    async fn each_speculative_window(db: &Db) {
        let (r, g) = (20_000, "g");
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        on_db(&db, move |c| Ok(crate::chunks::set_speculative(c, g, &start)?)).await.unwrap();
        let floor = r - SPECULATIVE_WINDOW;
        // a dense window: its oldest day is done with plenty of articles
        reach_further(&db, g, (floor, floor, Some(0)), 100_000).await;
        let s = on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?)).await.unwrap().unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0));
        // the next, sparse one doesnt ride on that: no further chunks
        let floor = r - s.reach;
        reach_further(&db, g, (floor, floor, Some(0)), 1).await;
        let s = on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?)).await.unwrap().unwrap();
        assert_eq!(s.reach, 2 * SPECULATIVE_WINDOW);
    }

    #[test]
    #[ignore = "requires an explicit isolated ATLAS_PG_TEST_URL"]
    fn a_speculative_window_is_judged_once_every_day_of_it_is_done() {
        let (schema, db) = native_fixture();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(a_window_judged_when_done(&db));
        drop(db);
        drop(schema);
    }

    async fn a_window_judged_when_done(db: &Db) {
        let (r, g) = (20_000, "g");
        let floor = r - SPECULATIVE_WINDOW;
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        on_db(&db, move |c| {
            // three days of the window, one older
            crate::chunks::add(c, g, floor + 2, floor)?;
            Ok(crate::chunks::set_speculative(c, g, &start)?)
        })
        .await
        .unwrap();
        let window = || on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?.unwrap()));
        let done = |day: i64| {
            on_db(&db, move |c| Ok(c.execute("update backfill_chunks set state = 2 where day = $1", vals![day])?))
        };

        // the floor day is done and dense, the rest of the window is still running
        done(floor).await.unwrap();
        reach_further(&db, g, (floor, floor, Some(0)), 100_000).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (SPECULATIVE_WINDOW, 1, 100_000), "counted, not judged yet");

        // the last one in judges the window on all of them
        done(floor + 1).await.unwrap();
        reach_further(&db, g, (floor + 1, floor, Some(0)), 1).await;
        done(floor + 2).await.unwrap();
        reach_further(&db, g, (floor + 2, floor, Some(0)), 1).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0));

        // a day of the window before it, finishing late, is not the new window's
        reach_further(&db, g, (floor + 2, floor, Some(0)), 100_000).await;
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (2 * SPECULATIVE_WINDOW, 0, 0), "ignored");
    }

    #[test]
    #[ignore = "requires an explicit isolated ATLAS_PG_TEST_URL"]
    fn a_chunk_taken_over_leaves_the_older_days_and_the_window_alone() {
        let (schema, db) = native_fixture();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(a_chunk_taken_over(&db));
        drop(db);
        drop(schema);
    }

    async fn a_chunk_taken_over(db: &Db) {
        let (r, g) = (20_000, "g");
        let floor = r - SPECULATIVE_WINDOW;
        let start = crate::chunks::Speculative { runner_up_day: r, reach: SPECULATIVE_WINDOW, days: 0, articles: 0 };
        let claim = on_db(&db, move |c| {
            crate::chunks::add(c, g, floor, floor - 5)?;
            crate::chunks::set_speculative(c, g, &start)?;
            Ok(crate::chunks::claim(c, &[(g.to_string(), i64::MIN)], "a", 0)?.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(claim.day, floor);
        let days = || on_db(&db, move |c| Ok(c.one::<i64>("select count(*) from backfill_chunks", &[])?));
        let window = || on_db(&db, move |c| Ok(crate::chunks::speculative(c, g)?.unwrap()));

        // its claim went stale and another worker has it now
        on_db(&db, move |c| Ok(c.execute("update backfill_chunks set claimed_at = 99 where day = $1", vals![floor])?))
            .await
            .unwrap();
        // no dated article in the day: the older days stay
        finish_chunk(&db, &claim, (true, None, Some(0)), 0).await.unwrap();
        assert_eq!(days().await.unwrap(), 6);
        // a dense day: the window doesnt count it, nor reach further
        finish_chunk(&db, &claim, (true, Some(floor), Some(0)), 100_000).await.unwrap();
        let s = window().await.unwrap();
        assert_eq!((s.reach, s.days, s.articles), (SPECULATIVE_WINDOW, 0, 0));
        assert_eq!(days().await.unwrap(), 6);

        // the claim that has it does both
        let mut now = claim.clone();
        now.claimed_at = 99;
        finish_chunk(&db, &now, (true, None, Some(0)), 0).await.unwrap();
        assert_eq!(days().await.unwrap(), 1, "the older days went to the sweeps");
    }

    /// a group taken out of config.toml keeps its run state, but stops counting among the tracked groups
    #[test]
    fn the_tracked_groups_are_the_ones_config_tracks_now() {
        let states = RunStates::default();
        states.with("gone", |st| st.idle = true);
        assert_eq!(states.groups(), HashSet::from(["gone".to_string()]), "until noted: the groups passes ran for");
        states.set_tracked(&["kept".to_string()]);
        assert_eq!(states.groups(), HashSet::from(["kept".to_string()]));
        assert!(states.is_idle("gone"));
    }

    #[test]
    fn a_split_group_behind_on_its_live_side_goes_live_first() {
        // behind by more than a slice: live, whatever the turn
        assert!(live_first("dynamic", Phase::Backfill, true, 100_001, 100_000, false));
        // a slice or less behind (a busy group always is a little): its turn
        assert!(!live_first("dynamic", Phase::Backfill, true, 100_000, 100_000, false));
        // not split: its backfill has no other takers, turns as before
        assert!(!live_first("dynamic", Phase::Backfill, false, 10_000_000, 100_000, false));
        // the modes that do one side only, and a live turn anyway
        assert!(!live_first("backfill", Phase::Backfill, true, 10_000_000, 100_000, false));
        assert!(!live_first("live", Phase::Backfill, true, 10_000_000, 100_000, false));
        assert!(!live_first("dynamic", Phase::Live, true, 10_000_000, 100_000, false));
    }

    #[test]
    fn a_day_chunk_streams_wider_than_other_passes() {
        assert_eq!(stream_width("CHUNK"), crate::nntp::CHUNK_STREAM_SLICES);
        assert!(crate::nntp::CHUNK_STREAM_SLICES >= 4 * crate::nntp::STREAM_SLICES);
        for kind in ["LIVE", "BACKFILL", "SWEEP"] {
            assert_eq!(stream_width(kind), crate::nntp::STREAM_SLICES);
        }
    }

    /// A big split group far behind on its live side reads live on every turn: its frontier days are the chunk
    /// workers', and a day chunk holding its worker for hours left its new posts a day behind on prod.
    #[test]
    fn a_split_group_behind_on_live_always_reads_live_first() {
        for deferred in [false, true] {
            assert!(live_first("dynamic", Phase::Backfill, true, 68_000_000, 100_000, deferred));
        }
        assert!(!live_first("dynamic", Phase::Backfill, true, 50_000, 100_000, false), "caught up: its own turn");
    }

    #[test]
    #[ignore = "requires an explicit isolated ATLAS_PG_TEST_URL"]
    fn cursors_saved_under_a_servers_old_key_are_adopted_once() {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let conn = crate::pg::sql::Pg::new(schema.pool.get().unwrap());
        let state = db::GroupState { live_cursor: 900, backfill_cursor: 500 };
        db::save_group_state(&conn, "g@x@a.example:563", state).unwrap();
        let legacy = ["a.example:563".to_string(), "x@a.example:563".to_string()];
        assert_eq!(cursors_or_adopted(&conn, "g@a.example", "g", false, &legacy).unwrap(), Some(state));
        assert_eq!(db::get_group_state(&conn, "g@x@a.example:563").unwrap(), None, "moved, not copied");
        assert_eq!(db::get_group_state(&conn, "g@a.example").unwrap(), Some(state));
        // nothing to adopt: nothing made up
        assert_eq!(cursors_or_adopted(&conn, "g@b.example", "g", false, &[]).unwrap(), None);
        // the plain group name goes to the server that kept it
        db::save_group_state(&conn, "h", state).unwrap();
        assert_eq!(cursors_or_adopted(&conn, "h@a.example", "h", true, &legacy).unwrap(), Some(state));
    }

    /// a lone server's plain cursors are its own: adding a server that sorts
    /// before it doesnt hand them to the new one
    #[test]
    #[ignore = "requires an explicit isolated ATLAS_PG_TEST_URL"]
    fn plain_cursors_stay_with_the_server_that_kept_them() {
        let schema = crate::pg::testing::Schema::new().expect("explicit isolated ATLAS_PG_TEST_URL required");
        let conn = crate::pg::sql::Pg::new(schema.pool.get().unwrap());
        let state = db::GroupState { live_cursor: 900, backfill_cursor: 500 };
        db::save_group_state(&conn, "g", state).unwrap();

        let old = crate::config::UsenetServer::new("old.example", "u", "p", 563);
        let lone = Pool::new(std::slice::from_ref(&old));
        claim_plain_cursors(&conn, &lone).unwrap();
        assert_eq!(cursor_key(&lone, 0, "g"), "g", "still the plain name on its own");

        // a new server, ahead of it by priority
        let mut new = crate::config::UsenetServer::new("new.example", "u", "p", 563);
        new.priority = 0;
        let both = Pool::new(&[new, old]);
        claim_plain_cursors(&conn, &both).unwrap();
        assert_eq!(both.host(0), "new.example", "the new one sorts first");
        let adopted = |i: usize| {
            let key = cursor_key(&both, i, "g");
            cursors_or_adopted(&conn, &key, "g", owns_plain_cursors(&conn, &both.host(i)).unwrap(), &[]).unwrap()
        };
        assert_eq!(adopted(0), None, "the new server starts fresh");
        assert_eq!(adopted(1), Some(state), "the old one keeps its place");

        // plain cursors from before anyone was recorded, with two servers: no one's
        let schema2 = crate::pg::testing::Schema::new().unwrap();
        let conn2 = crate::pg::sql::Pg::new(schema2.pool.get().unwrap());
        db::save_group_state(&conn2, "g", state).unwrap();
        claim_plain_cursors(&conn2, &both).unwrap();
        for i in 0..2 {
            assert!(!owns_plain_cursors(&conn2, &both.host(i)).unwrap(), "ambiguous, left alone");
        }
    }

    #[test]
    fn what_a_server_keeps_is_remembered_by_its_host_not_its_place_in_the_pool() {
        let states = RunStates::default();
        // index 0 was a.example when this was learned; a config reload put b.example there
        states.keep_from("alt.binaries.g", "a.example", 20_000);
        assert_eq!(states.keeps_from("alt.binaries.g", "a.example"), 20_000);
        assert_eq!(states.kept_days("a.example"), HashMap::from([("alt.binaries.g".to_string(), 20_000)]));
        assert_eq!(states.keeps_from("alt.binaries.g", "b.example"), i64::MIN, "b is not restricted");
        assert!(states.kept_days("b.example").is_empty());
    }

    #[test]
    fn a_server_not_carrying_a_group_stays_noted_but_other_first_posts_expire() {
        let states = RunStates::default();
        states.not_carrying("g", "a.example");
        states.first_post_on("g", "b.example", 20_000);
        assert_eq!(states.first_post_day("g", "b.example"), 20_000);
        let past = Instant::now() - Duration::from_secs(1);
        states.with("g", |st| {
            for (_, until) in st.first_post_days.values_mut() {
                if until.is_some() {
                    *until = Some(past);
                }
            }
        });
        assert_eq!(states.first_post_day("g", "a.example"), i64::MAX, "no expiry for a non-carrier");
        assert_eq!(states.first_post_day("g", "b.example"), i64::MIN, "an ordinary first post expires");
    }

    #[test]
    fn slices_cover_the_range() {
        assert_eq!(make_slices(1, 10, 4, false), vec![(1, 4), (5, 8), (9, 10)]);
        assert_eq!(make_slices(1, 10, 4, true), vec![(9, 10), (5, 8), (1, 4)]);
        assert_eq!(make_slices(5, 5, 100, false), vec![(5, 5)]);
        assert!(make_slices(6, 5, 100, false).is_empty());
        assert_eq!(make_slices(u64::MAX - 1, u64::MAX, 10, false), vec![(u64::MAX - 1, u64::MAX)]);
    }

    #[test]
    fn a_split_is_made_only_from_dates_that_make_sense() {
        let (today, day) = (20_400, 86_400);
        let home = 19_500 * day;
        let at = |times: &[i64]| Newest::AtCursor(times.to_vec());
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 19_000 * day, today), Some((20_000, 19_000)), "sane");
        assert_eq!(split_bounds(&at(&[0]), home, 19_000 * day, today), None, "a date from 1970 at the cursor");
        assert_eq!(split_bounds(&at(&[30_000 * day]), home, 19_000 * day, today), None, "one in the future");
        assert_eq!(split_bounds(&at(&[19_000 * day]), home, 19_000 * day, today), None, "before home's first");
        assert_eq!(split_bounds(&at(&[home - 3600]), home, home, today), None, "newest day before the oldest");
        assert_eq!(split_bounds(&at(&[home - 3600]), home, home - 7200, today), Some((19_499, 19_499)), "same day");
        assert_eq!(
            split_bounds(&at(&[19_499 * day]), home - day, 19_500 * day, today),
            None,
            "a day before the oldest"
        );
        assert_eq!(split_bounds(&Newest::Day(19_000), home, 19_100 * day, today), None, "a split's newest day too");
        assert_eq!(
            split_bounds(&at(&[0, 20_000 * day]), home, 19_000 * day, today),
            Some((20_000, 19_000)),
            "one forged date of two is left out"
        );
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 5 * day, today), None, "a carrier's first from 1970");
        assert_eq!(split_bounds(&Newest::Day(20_000), home, 19_000 * day, today), Some((20_000, 19_000)));
        assert_eq!(split_bounds(&Newest::Day(20_000), home, 5 * day, today), None);
        assert_eq!(split_bounds(&at(&[20_000 * day]), home, 19_000 * day, 0), None, "a clock from 1970");
    }

    #[test]
    fn a_release_looks_at_a_few_bodies_for_its_name() {
        let article = |i: usize, name: &str| crate::parser::Article {
            subject: format!(r#""{name}" yEnc (1/1)"#),
            message_id: format!("<{i}@x>"),
            ..Default::default()
        };
        let mut articles: Vec<_> = (0..40).map(|i| article(i, &format!("n{i}.nfo"))).collect();
        articles.push(article(99, "base.par2"));
        let sources = name_sources(&Release { articles, ..Default::default() });
        assert_eq!(sources.len(), MAX_NAME_BODIES);
        assert_eq!(sources[0].0, "<99@x>", "the par2 first");
    }
}

#[cfg(test)]
#[path = "../tests/common/native_provider_contract.rs"]
mod native_provider_contract;

#[cfg(test)]
#[path = "../tests/common/native_per_article_contract.rs"]
mod native_per_article_contract;

#[cfg(test)]
#[path = "../tests/common/native_collection_paths.rs"]
mod native_collection_paths;

#[cfg(test)]
#[path = "../tests/common/native_multi_server_contract.rs"]
mod native_multi_server_contract;
