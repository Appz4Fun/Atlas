//! Closed scheduler read operations share selection admission, not service FIFO.
//! Callers cannot submit arbitrary SQL or mutation closures through this module.
use super::{Db, MainConnection, Sql};
use anyhow::{Result, anyhow};

// Private to this child module: its five fixed operations below are the only
// callers. Preserve on_db's wait accounting, original owner and error context.
async fn read<T: Send + 'static>(db: &Db, work: impl FnOnce(&dyn Sql) -> Result<T> + Send + 'static) -> Result<T> {
    let conn = db.conn.clone();
    tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        match &*conn {
            MainConnection::Postgres(pool) => {
                let client = pool.get()?;
                crate::profile::DB_WAIT.add_since(started);
                work(&crate::pg::sql::Pg::new(client))
            }
            MainConnection::OwnedPostgres(owner) => owner.with_selection(|client| {
                crate::profile::DB_WAIT.add_since(started);
                work(&crate::pg::sql::Pg::new(client))
            }),
        }
    })
    .await
    .map_err(|error| anyhow!("db task failed: {error}"))?
}

pub(super) async fn is_split(db: &Db, group: String) -> Result<bool> {
    read(db, move |sql| Ok(crate::chunks::is_split(sql, &group)?)).await
}

pub(super) async fn newest_day(db: &Db, group: String) -> Result<Option<i64>> {
    read(db, move |sql| Ok(crate::chunks::newest_day(sql, &group)?)).await
}

pub(super) async fn oldest_day(db: &Db, group: String) -> Result<Option<i64>> {
    read(db, move |sql| Ok(crate::chunks::oldest_day(sql, &group)?)).await
}

pub(super) async fn deepest_and_runner_up(db: &Db, group: String) -> Result<(Option<String>, Option<i64>)> {
    read(db, move |sql| Ok((crate::chunks::deepest(sql, &group)?, crate::chunks::runner_up(sql, &group)?))).await
}

pub(super) async fn sweep_due(db: &Db, wanted: [(String, i64); 1]) -> Result<bool> {
    read(db, move |sql| Ok(!crate::chunks::waiting(sql, &wanted)?)).await
}
