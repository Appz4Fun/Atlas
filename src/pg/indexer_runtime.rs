//! Startup fencing for the native ingestion profile while auxiliary services are ported.

use anyhow::{Context, Result, ensure};
use std::sync::{Arc, Mutex};

use crate::config::{Config, HeaderStoreConfig};
use crate::indexer::{Db, PgWriteLimits, shared_postgres_db};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    storage: super::StorageConfig,
    headers: HeaderStoreConfig,
}

impl Profile {
    fn check(config: &Config, headers: &HeaderStoreConfig) -> Result<()> {
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
    // Held outside Tokio for the full indexing run. Claims are reclaimed only
    // after this connection has acquired sole indexer ownership of the schema.
}

/// The ownership lock and every cursor/catalog write use this same session.
/// A lost session cannot continue writing through a different pooled client.
pub(crate) struct Owner {
    client: Mutex<Option<postgres::Client>>,
}

impl Owner {
    pub(crate) fn with<T>(&self, work: impl FnOnce(&mut postgres::Client) -> Result<T>) -> Result<T> {
        let mut held = self.client.lock().map_err(|_| anyhow::anyhow!("native owner connection is poisoned"))?;
        let client = held.as_mut().context("native owner connection has closed")?;
        ensure!(!client.is_closed(), "native indexer lost its ownership session");
        work(client)
    }
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
    ) -> Result<Self> {
        storage.check().context("invalid native storage configuration")?;
        ensure!(storage.is_postgres(), "native ingestion requires PostgreSQL storage");
        Profile::check(config, &headers)?;
        let connection: postgres::Config = storage.url.parse().context("invalid native storage configuration")?;
        let expected = connection.get_dbname().context("native storage requires an explicit database name")?;
        // Validate schema/version and transport policy before taking ownership or changing rows.
        let db = shared_postgres_db(&storage, expected, PgWriteLimits::default())
            .context("validating the native catalog destination")?;
        let mut owner = connection.connect(postgres::NoTls).context("opening the native owner session")?;
        let locked: bool = owner
            .query_one("select pg_try_advisory_lock(hashtext('atlas.indexer'), hashtext($1))", &[&storage.schema])?
            .get(0);
        ensure!(locked, "another native indexer owns this PostgreSQL schema");
        owner.batch_execute(&format!("set search_path to \"{}\"", storage.schema))?;
        {
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
        let owner = Arc::new(Owner { client: Mutex::new(Some(owner)) });
        let db = crate::indexer::owned_postgres_db(db, owner.clone(), PgWriteLimits::default())?;
        Ok(Self { db, profile: Profile { storage, headers } })
    }
}
