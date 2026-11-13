//! Native queue/rank/walk waits use the existing adaptive count oracle. SQL
//! predicates use actual fresh IDs; no shard ID arithmetic or SQLite hint.
use super::fresh_reader::ReadView;
use crate::postprocess::engine::{claim_shares, queue_shares};
use crate::web_pending::{NativePace, State, Wait, Waits, wait_in};
use anyhow::{Result, ensure};
use postgres::types::ToSql;

impl ReadView<'_> {
    fn ahead(&self, from_where: &str, args: &[&(dyn ToSql + Sync)], cap: i64) -> Result<(i64, bool)> {
        ensure!((0..=500000).contains(&cap), "native wait count exceeds original horizon");
        self.before_query()?;
        let n: i64 = self
            .tx
            .borrow_mut()
            .query_one(&format!("select count(*) from(select 1 from {from_where} limit {}) horizon", cap + 1), args)?
            .try_get(0)?;
        ensure!((0..=cap + 1).contains(&n), "native wait count is invalid");
        Ok((n.min(cap), n > cap))
    }
    pub(crate) fn processing_waits(&self, state: &State, pace: &NativePace, now: i64) -> Result<Waits> {
        let Some(release) = &state.release else {
            return Ok(Waits::default());
        };
        let id = release.id;
        let rate = pace.measured();
        let mut waits = Waits::default();
        if state.net_wanted {
            let (by_rank, newest, oldest) = claim_shares();
            let ranked = |cap: i64| -> Result<(i64, bool)> {
                let (higher, more) = match state.net_rank {
                    Some(rank) => self.ahead("pp_state where net_wanted and net_rank>$1", &[&rank], cap)?,
                    None => self.ahead("pp_state where net_wanted and net_rank is not null", &[], cap)?,
                };
                if more {
                    return Ok((higher, true));
                }
                let (same, more) = match state.net_rank {
                    Some(rank) => self.ahead(
                        "pp_state where net_wanted and net_rank=$1 and release_id>$2",
                        &[&rank, &id],
                        cap - higher,
                    )?,
                    None => self.ahead(
                        "pp_state where net_wanted and net_rank is null and release_id>$1",
                        &[&id],
                        cap - higher,
                    )?,
                };
                Ok((higher + same, more))
            };
            let newer =
                |cap| self.ahead("pp_state where net_wanted and release_id>$1 and net_next_at<=$2", &[&id, &now], cap);
            let older =
                |cap| self.ahead("pp_state where net_wanted and release_id<$1 and net_next_at<=$2", &[&id, &now], cap);
            waits.net = Some(wait_in(
                rate.map(|pace| pace.net_visits),
                &[(by_rank, &ranked), (newest, &newer), (oldest, &older)],
            )?);
        }
        if let Some(due) = state.due_at.filter(|due| *due <= now) {
            let (latest, longest) = queue_shares();
            let newer = |cap: i64| -> Result<(i64, bool)> {
                let (n, more) = self.ahead("pp_queue where due_at>$1 and due_at<=$2", &[&due, &now], cap)?;
                if more {
                    return Ok((n, true));
                }
                let (tie, more) = self.ahead("pp_queue where due_at=$1 and release_id>$2", &[&due, &id], cap - n)?;
                Ok((n + tie, more))
            };
            let older = |cap: i64| -> Result<(i64, bool)> {
                let (n, more) = self.ahead("pp_queue where due_at<$1", &[&due], cap)?;
                if more {
                    return Ok((n, true));
                }
                let (tie, more) = self.ahead("pp_queue where due_at=$1 and release_id<$2", &[&due, &id], cap - n)?;
                Ok((n + tie, more))
            };
            waits.queue = Some(wait_in(rate.map(|pace| pace.queue_taken), &[(latest, &newer), (longest, &older)])?);
        }
        if let Some(walk) = &state.walk {
            waits.walk = Some(if !walk.active {
                Wait::Later
            } else {
                let count = |cap| match &walk.group {
                    Some(group) => {
                        self.ahead("releases where group_name=$3 and id>$1 and id<$2", &[&id, &walk.before, group], cap)
                    }
                    None => self.ahead("releases where id>$1 and id<$2", &[&id, &walk.before], cap),
                };
                wait_in(rate.map(|pace| pace.walk_rows), &[(1.0, &count)])?
            });
        }
        Ok(waits)
    }
}

#[cfg(test)]
mod shipping_graph_tests {
    use super::*;
    use crate::{
        parser::{Article, Release},
        pg::{self, Backend, StorageConfig, fresh_reader::Budget},
        web_pending::Pace,
    };
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "requires explicit isolated PostgreSQL owner and native wait-reader admission"]
    fn native_wait_counts_use_real_pg_rank_queue_and_group_rows() {
        let schema = pg::testing::Schema::new().expect("explicit isolated PostgreSQL schema");
        let mut owner = schema.pool.get().unwrap();
        owner.query_one("select pg_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name]).unwrap();
        let root = tempfile::tempdir().unwrap();
        let headers = crate::config::HeaderStoreConfig { enabled: true, dir: root.path().to_str().unwrap().into() };
        let storage = StorageConfig {
            backend: Backend::Postgres,
            url: schema.url.clone(),
            schema: schema.name.clone(),
            max_connections: 4,
            allow_plaintext: true,
        };
        let version = pg::migrate::version(&mut owner, &schema.name).unwrap();
        let _epoch = pg::stage::open_epoch(&mut owner, &storage, &headers, version).unwrap();
        let mut ids = Vec::new();
        for n in 1..=9 {
            let release = Release {
                name: format!("Native.Wait.Fixture.{n}"),
                group: if n == 7 { "alt.binaries.wait-b" } else { "alt.binaries.wait-a" }.into(),
                poster: "fixture".into(),
                date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
                articles: vec![Article {
                    subject: format!("\"file{n}.mkv\" yEnc (1/1)"),
                    filename: Some(format!("file{n}.mkv")),
                    message_id: format!("wait-{n}@example.invalid"),
                    part: Some(1),
                    total_parts: Some(1),
                    bytes: 64,
                    ..Default::default()
                }],
                ..Default::default()
            };
            assert_eq!(
                pg::release_writer::save(&mut owner, &[release], 1).unwrap(),
                pg::release_writer::SaveStats { releases: 1, files: 1, parts: 1 }
            );
            ids.push(owner.query_one("select max(id) from releases", &[]).unwrap().get::<_, i64>(0));
        }
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        owner.execute("delete from pp_queue", &[]).unwrap();
        for (id, rank) in ids.iter().zip([2i64, 1, 1, 0, 1]) {
            owner
                .execute("insert into pp_state(release_id,net_wanted,net_rank) values($1,true,$2)", &[id, &rank])
                .unwrap();
        }
        for (id, due) in ids.iter().zip([50i64, 100, 100, 100, 200]) {
            owner.execute("insert into pp_queue(release_id,due_at) values($1,$2)", &[id, &due]).unwrap();
        }
        owner
            .execute(
                "insert into pp_walks(kind,top_id,before_id,group_name,created_at) values('group',$1,$1,$2,1)",
                &[&(ids[8] + 1), &"alt.binaries.wait-a"],
            )
            .unwrap();
        let mut connection = schema.pool.get().unwrap();
        let view =
            ReadView::open(&mut connection, &schema.name, Budget::default(), Instant::now() + Duration::from_secs(10))
                .unwrap();
        let pace = NativePace::Measured(Pace {
            running: true,
            enabled: Some(-1),
            window_secs: 600,
            net_visits: 8.0,
            queue_taken: 8.0,
            walk_rows: 60.0,
            ..Default::default()
        });
        let row = view.processing(ids[2]).unwrap().unwrap();
        let waits = view.processing_waits(&row.state, &pace, 1000).unwrap();
        assert_eq!(waits.net, Some(Wait::About(2.0 / 6.0, 2)));
        assert_eq!(waits.queue, Some(Wait::About(2.0 / 6.0, 2)));
        let row = view.processing(ids[5]).unwrap().unwrap();
        let waits = view.processing_waits(&row.state, &pace, 1000).unwrap();
        assert_eq!(waits.walk, Some(Wait::About(2.0 / 60.0, 2)));
        assert_eq!(view.ahead("pp_state where net_wanted", &[], 3).unwrap(), (3, true));
    }
}
