//! Startup fencing for the native ingestion profile while auxiliary services are ported.

use super::outage::{AttemptEnded, Failure, SessionUnavailable, classify};
use anyhow::{Context, Result, ensure};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use crate::config::{Config, HeaderStoreConfig};
use crate::indexer::{Db, PgWriteLimits, shared_postgres_db};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    storage: super::StorageConfig,
    headers: HeaderStoreConfig,
}

impl Profile {
    pub(crate) fn check(config: &Config, headers: &HeaderStoreConfig) -> Result<()> {
        ensure!(headers.enabled, "native ingestion requires the NXV header store");
        ensure!(!config.postprocess.enabled, "native ingestion requires postprocessing to be disabled");
        ensure!(!config.predb.enabled, "native ingestion requires the PreDB worker to be disabled");
        ensure!(!config.spots.enabled, "native ingestion requires Spotnet workers to be disabled");
        ensure!(!config.inferred_parts, "native ingestion requires inferred-part repair to be disabled");
        ensure!(
            !config.media.any_provider()
                && !config.media.allocine.enabled
                && config.media.omdb.api_key.is_empty()
                && config.media.opensubtitles.api_key.is_empty()
                && config.media.details_per_minute == 0,
            "native ingestion requires metadata providers and cache workers to be disabled"
        );
        ensure!(!config.auto_run_compact, "native ingestion requires SQLite compaction to be disabled");
        ensure!(!config.shared_catalog.enabled, "native ingestion cannot publish the SQLite catalog mirror");
        ensure!(
            !config.overview_archive.enabled,
            "native ingestion requires the SQLite overview archive to be disabled"
        );
        ensure!(
            config.sealed_log.addr.is_empty(),
            "native ingestion requires SQLite sealed-log offload to be disabled"
        );
        Ok(())
    }

    pub(crate) fn check_reload(&self, config: &Config) -> Result<()> {
        ensure!(
            crate::config::storage_config() == self.storage,
            "native ingestion storage destination changed; restart with the intended destination"
        );
        let headers = crate::config::header_store_config();
        ensure!(headers == self.headers, "native ingestion header-store configuration changed");
        Self::check(config, &headers)
    }
}

pub(crate) struct Runtime {
    pub(crate) db: Db,
    pub(crate) profile: Profile,
    pub(crate) owner: Arc<Owner>,
    pub(crate) identity: DestinationIdentity,
    // Held outside Tokio for the full indexing run. Claims are reclaimed only
    // after this connection has acquired sole indexer ownership of the schema.
}

/// The ownership lock and every cursor/catalog write use this same session.
/// A lost session cannot continue writing through a different pooled client.
pub(crate) struct Owner {
    client: Mutex<Option<postgres::Client>>,
    pub(crate) stop: Arc<AtomicBool>,
    accepting: AtomicBool,
    failure: AtomicU8,
    busy_since: Mutex<Option<std::time::Instant>>,
    cache_destination: Option<CacheDestination>,
}

impl Owner {
    pub(crate) fn new(client: postgres::Client) -> Self {
        Self {
            client: Mutex::new(Some(client)),
            stop: Arc::new(AtomicBool::new(false)),
            accepting: AtomicBool::new(true),
            failure: AtomicU8::new(0),
            busy_since: Mutex::new(None),
            cache_destination: None,
        }
    }

    fn with_cache_destination(client: postgres::Client, storage: super::StorageConfig,
        identity: DestinationIdentity, version: i32) -> Self {
        let mut owner = Self::new(client);
        owner.cache_destination = Some(CacheDestination { storage, identity, version });
        owner
    }

    pub(crate) fn cache_destination(&self) -> Option<&CacheDestination> {
        self.cache_destination.as_ref()
    }

    pub(crate) fn with<T>(&self, work: impl FnOnce(&mut postgres::Client) -> Result<T>) -> Result<T> {
        let mut held = self.client.lock().map_err(|_| {
            self.fail(Failure::Fatal);
            anyhow::anyhow!("native owner connection is poisoned")
        })?;
        if !self.accepting.load(Ordering::Acquire) || self.stop.load(Ordering::Acquire) {
            return Err(AttemptEnded.into());
        }
        *self.busy_since.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
        let result = match held.as_mut() {
            Some(client) if !client.is_closed() => work(client),
            _ => Err(SessionUnavailable.into()),
        };
        *self.busy_since.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Err(error) = &result {
            self.fail(classify(error));
        }
        result
    }

    pub(crate) fn fail(&self, failure: Failure) {
        self.failure.fetch_max(if failure == Failure::Fatal { 2 } else { 1 }, Ordering::AcqRel);
        self.stop.store(true, Ordering::Release);
    }

    pub(crate) fn failure(&self) -> Option<Failure> {
        match self.failure.load(Ordering::Acquire) {
            0 => None,
            1 => Some(Failure::Unavailable),
            _ => Some(Failure::Fatal),
        }
    }

    /// A silent local forward can ACK TCP while withholding PG replies. Mark
    /// quarantine independently of the client mutex; never abandon that work
    /// or acquire a replacement owner until it actually drains.
    pub(crate) fn overdue(&self) -> bool {
        self.busy_since
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(30))
    }

    /// Run outside Tokio after worker drain. Queued blocking callbacks may
    /// retain this Owner, but can never access a replacement session.
    pub(crate) fn close(&self) {
        self.accepting.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        let mut held = self.client.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(held.take());
    }
}

#[derive(Clone)]
pub(crate) struct CacheDestination {
    pub(crate) storage: super::StorageConfig,
    pub(crate) identity: DestinationIdentity,
    pub(crate) version: i32,
}

/// Catalog pins supplement the fixed configured destination without requiring
/// cluster-control privileges. They are not cryptographic cluster identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DestinationIdentity {
    database: String,
    role: String,
    session_role: String,
    ids: Vec<u32>,
}

pub(crate) fn destination_identity(client: &mut postgres::Client, schema: &str) -> Result<DestinationIdentity> {
    let row = client.query_one(
        "select current_database(),current_user,session_user,d.oid,r.oid,s.oid,n.oid,n.nspowner,c.oid
         from pg_database d join pg_roles r on r.rolname=current_user
         join pg_roles s on s.rolname=session_user join pg_namespace n on n.nspname=$1
         join pg_class c on c.relnamespace=n.oid and c.relname='schema_migrations'
         where d.datname=current_database()",
        &[&schema],
    )?;
    Ok(DestinationIdentity {
        database: row.get(0),
        role: row.get(1),
        session_role: row.get(2),
        ids: (3..9).map(|i| row.get(i)).collect(),
    })
}

impl Drop for Owner {
    fn drop(&mut self) {
        let client = self.client.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::spawn(move || drop(client));
        } else {
            drop(client);
        }
    }
}

impl Runtime {
    pub(crate) fn open(
        config: &Config,
        storage: super::StorageConfig,
        headers: HeaderStoreConfig,
        hosts: &[String],
        expected_identity: &mut Option<DestinationIdentity>,
    ) -> Result<Self> {
        storage.check().context("invalid native storage configuration")?;
        ensure!(storage.is_postgres(), "native ingestion requires PostgreSQL storage");
        Profile::check(config, &headers)?;
        let connection = owner_connection_config(&storage)?;
        let expected = connection.get_dbname().context("native storage requires an explicit database name")?;
        // Validate schema/version and transport policy before taking ownership or changing rows.
        let db = shared_postgres_db(&storage, expected, PgWriteLimits::default())
            .context("validating the native catalog destination")?;
        let mut owner = connection.connect(postgres::NoTls).context("opening the native owner session")?;
        let identity = destination_identity(&mut owner, &storage.schema)?;
        ensure!(
            expected_identity.as_ref().is_none_or(|expected| *expected == identity),
            "native PostgreSQL destination identity changed"
        );
        let version = super::migrate::version(&mut owner, &storage.schema)?;
        ensure!(
            version == super::migrate::MIGRATIONS.last().expect("storage migrations").0,
            "native PostgreSQL schema version changed"
        );
        // Pin even when an initialization COMMIT later loses its response.
        *expected_identity = Some(identity.clone());
        let locked: bool = owner
            .query_one("select pg_try_advisory_lock(hashtext('atlas.indexer'), hashtext($1))", &[&storage.schema])?
            .get(0);
        ensure!(locked, "another native indexer owns this PostgreSQL schema");
        owner.batch_execute(&format!("set search_path to \"{}\"", storage.schema))?;
        {
            let mut tx = owner.transaction()?;
            super::history_gate::native(&mut tx).context("historical readiness before native cursor initialization")?;
            tx.commit()?;
            let pg = super::sql::Pg::new(&mut owner);
            let sql: &dyn super::sql::Sql = &pg;
            sql.tx(|connection| {
                crate::chunks::set_forward_start(
                    connection,
                    config.backfill_start_days_back,
                    chrono::Utc::now().timestamp(),
                )?;
                crate::chunks::release_left_over(connection, hosts)?;
                Ok(())
            })
            .context("initializing native indexer cursors")?;
        }
        let owner = Arc::new(Owner::with_cache_destination(owner, storage.clone(), identity.clone(), version));
        let db = crate::indexer::owned_postgres_db(db, owner.clone(), PgWriteLimits::default())?;
        Ok(Self { db, profile: Profile { storage, headers }, owner, identity })
    }
}

fn owner_connection_config(storage: &super::StorageConfig) -> Result<postgres::Config> {
    // Keep ownership sessions under the same transport policy as pooled sessions.
    Ok(super::Pool::new(storage)?.config.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn close_waits_for_current_work_and_rejects_queued_stale_callbacks() {
        let Some(schema) = super::super::testing::Schema::new() else { return };
        let owner = Arc::new(Owner::new(postgres::Client::connect(&schema.url, postgres::NoTls).unwrap()));
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let current = owner.clone();
        let work = std::thread::spawn(move || {
            current.with(|_| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let close_owner = owner.clone();
        let (closed, done) = std::sync::mpsc::channel();
        let close = std::thread::spawn(move || {
            close_owner.close();
            closed.send(()).unwrap();
        });
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !owner.stop.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(done.try_recv().is_err(), "close abandoned active work");
        let queued_owner = owner.clone();
        let queued = std::thread::spawn(move || queued_owner.with(|_| panic!("stale callback ran")) as Result<()>);
        release.send(()).unwrap();
        work.join().unwrap().unwrap();
        close.join().unwrap();
        done.recv().unwrap();
        assert!(queued.join().unwrap().is_err());
        assert!(owner.with(|_| Ok(())).is_err());
    }

    #[test]
    fn owner_connection_uses_bounded_transport_defaults_and_url_overrides() {
        let mut storage = super::super::StorageConfig {
            backend: super::super::Backend::Postgres,
            url: "host=127.0.0.1 dbname=example".into(),
            schema: "example".into(),
            ..Default::default()
        };
        let connection = owner_connection_config(&storage).unwrap();
        assert_eq!(connection.get_connect_timeout(), Some(&Duration::from_secs(5)));
        assert_eq!(connection.get_keepalives_idle(), Duration::from_secs(30));
        assert_eq!(connection.get_keepalives_interval(), Some(Duration::from_secs(10)));
        assert_eq!(connection.get_keepalives_retries(), Some(3));
        assert_eq!(connection.get_tcp_user_timeout(), Some(&Duration::from_secs(30)));
        storage.url.push_str(
            " connect_timeout=7 keepalives_idle=9 keepalives_interval=2 keepalives_retries=4 tcp_user_timeout=11",
        );
        let connection = owner_connection_config(&storage).unwrap();
        assert_eq!(connection.get_connect_timeout(), Some(&Duration::from_secs(7)));
        assert_eq!(connection.get_keepalives_idle(), Duration::from_secs(9));
        assert_eq!(connection.get_keepalives_interval(), Some(Duration::from_secs(2)));
        assert_eq!(connection.get_keepalives_retries(), Some(4));
        assert_eq!(connection.get_tcp_user_timeout(), Some(&Duration::from_secs(11)));
    }
}
