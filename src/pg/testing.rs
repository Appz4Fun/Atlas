//! A Postgres schema of a test's own, dropped when the test ends. The server comes from `ATLAS_PG_TEST_URL`
//! (`host=127.0.0.1 port=5432 user=atlas dbname=atlas_test`); without it the tests that need one return early,
//! as the shared catalog's do.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Pool, StorageConfig};

// Test-only access to the real manifest producer, without exposing its
// private production module to API/media/worker fixture siblings.
pub(crate) use super::cache_assets::{capture_file as capture_asset_manifest,Limits as AssetCaptureLimits};

pub struct Schema {
    pub name: String,
    pub url: String,
    pub pool: Pool,
}

impl Schema {
    /// A direct-writer fixture without Runtime::open still needs a real
    /// catalog privacy key. Opt in here; ordinary empty/migration/read-only
    /// fixtures remain pristine and missing-key refusal remains testable.
    pub fn native_writer() -> Option<Schema> {
        let s = Self::new()?;
        let mut key = [0; 8];
        getrandom::fill(&mut key).expect("native writer fixture key entropy");
        s.pool.get().unwrap().execute("insert into meta(key,value) values('key_salt',$1)", &[&i64::from_le_bytes(key)]).unwrap();
        Some(s)
    }

    /// A fresh schema, migrated. None without a test server.
    pub fn new() -> Option<Schema> {
        let s = Schema::empty()?;
        let mut c = s.pool.get().expect("connecting to the test server");
        super::migrate::run(&mut c, &s.name).expect("migrating the test schema");
        drop(c);
        Some(s)
    }

    /// A fresh schema with nothing in it, not even `schema_migrations`. None without a test server.
    pub fn empty() -> Option<Schema> {
        let url = std::env::var("ATLAS_PG_TEST_URL").ok().filter(|u| !u.is_empty())?;
        static N: AtomicUsize = AtomicUsize::new(0);
        let name = format!("t_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let mut c = postgres::Client::connect(&url, postgres::NoTls).expect("connecting to the test server");
        c.batch_execute(&format!("drop schema if exists \"{name}\" cascade; create schema \"{name}\"")).unwrap();
        let pool = Pool::new(&StorageConfig {
            backend: super::Backend::Postgres,
            url: url.clone(),
            schema: name.clone(),
            max_connections: 4,
            allow_plaintext: true,
        })
        .unwrap();
        Some(Schema { name, url, pool })
    }
}

impl Drop for Schema {
    fn drop(&mut self) {
        if let Ok(mut c) = postgres::Client::connect(&self.url, postgres::NoTls) {
            let _ = c.batch_execute(&format!("drop schema if exists \"{}\" cascade", self.name));
        }
    }
}

/// Runs `body` on a fresh migrated PostgreSQL schema. Without explicit test
/// admission, reports the native requirement and skips; no backend substitutes.
pub fn each(body: impl FnOnce(&dyn super::sql::Sql)) {
    let Some(s) = Schema::new() else {
        eprintln!("skipping PostgreSQL test: ATLAS_PG_TEST_URL is required");
        return;
    };
    let conn = s.pool.get().expect("connecting to the test server");
    body(&super::sql::Pg::new(conn));
}
