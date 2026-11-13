//! The actual NNTP pass saves its catalog and cursor without creating SQLite files.
use atlas as atlas_api;
#[path = "common/nntp.rs"]
mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use atlas::indexer::{PassContext, PassSettings, PgWriteLimits, RunStates, run_pass, shared_postgres_db};
use atlas::pg::{Backend, StorageConfig};
use common::{GROUP, Server, mock, post, spawn_server};

struct Schema {
    client: postgres::Client,
    name: String,
}

impl Drop for Schema {
    fn drop(&mut self) {
        let _ = self.client.batch_execute(&format!("drop schema {} cascade", self.name));
    }
}

#[test]
fn nntp_pass_commits_native_releases_and_cursor_without_sqlite() {
    let Ok(url) = std::env::var("ATLAS_PG_TEST_URL") else { return };
    if url.is_empty() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    // One test, before starting threads; the provider and state are isolated fixtures.
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
    }
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let database: String = client.query_one("select current_database()", &[]).unwrap().get(0);
    let name = format!("t_native_pass_{}", std::process::id());
    atlas::pg::migrate::run(&mut client, &name).unwrap();
    let mut schema = Schema { client, name };
    let config = StorageConfig {
        backend: Backend::Postgres,
        url,
        schema: schema.name.clone(),
        allow_plaintext: true,
        ..Default::default()
    };
    let db = shared_postgres_db(&config, &database, PgWriteLimits::default()).unwrap();
    let posts = (1..=50)
        .map(|n| {
            post(n, &format!(r#""Example.Release.{}.part{}.rar" yEnc (1/1)"#, (n - 1) / 10, (n - 1) % 10), 100, vec![])
        })
        .collect();
    let mut server = Server::new(posts);
    Arc::get_mut(&mut server).unwrap().any_group = true;
    let port = spawn_server(server);
    let pool = Arc::new(atlas::nntp::Pool::new(&[mock(port, "secret", 1, 1)]));
    let ctx = PassContext {
        pool: pool.clone(),
        states: RunStates::default(),
        stop: Arc::new(AtomicBool::new(false)),
        verbose: false,
    };
    let settings = PassSettings {
        mode: "backfill".into(),
        batch_size: 50,
        request_size: 10,
        split_min_backlog: i64::MAX,
        ..Default::default()
    };
    // Name/provider enrichment belongs to background processing, not this ingestion-only pass.
    atlas::archive::sync_inline_lookups(&atlas::config::Config::default());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        pool.connect().await.unwrap();
        run_pass(&ctx, &settings, &db, GROUP, 0, &mut |_| {}).await.unwrap();
    });
    for (table, expected) in [("releases", 5i64), ("files", 50), ("segments", 50)] {
        let count: i64 =
            schema.client.query_one(&format!("select count(*) from {}.{table}", schema.name), &[]).unwrap().get(0);
        assert_eq!(count, expected, "{table}");
    }
    let cursor: i64 = schema
        .client
        .query_one(&format!("select backfill_cursor from {}.groups where name=$1", schema.name), &[&GROUP])
        .unwrap()
        .get(0);
    assert_eq!(cursor, 0);
    assert!(
        !home.path().read_dir().unwrap().any(|p| p.unwrap().path().extension().is_some_and(|s| s == "db")),
        "native ingestion must not create SQLite stores"
    );
}
