//! A Postgres schema of a test's own, dropped when the test ends. The server comes from `ATLAS_PG_TEST_URL`
//! (`host=127.0.0.1 port=5432 user=atlas dbname=atlas_test`); without it the tests that need one return early,
//! as the shared catalog's do.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Pool, StorageConfig};

pub struct Schema {
    pub name: String,
    pub url: String,
    pub pool: Pool,
}

impl Schema {
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
