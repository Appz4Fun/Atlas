//! Bounded source capture and staging steps. The caller holds the store's
//! retention pin until this phase finishes; source receipts are not publication
//! acknowledgements. Every inventory page and every block mapping is durable.

use super::linker_stage::Budget;
use crate::indexer::{Db, on_native_db};
use crate::nxv_store::Store;
use anyhow::{Context, Result, ensure};
use std::sync::Arc;
use std::time::Duration;

const INVENTORY_PAGE: usize = 32;

#[derive(Default)]
pub(crate) struct Cache {
    block: Option<super::linker_stage::VerifiedBlock>,
    pub(crate) verifications: u64,
}

/// Advance one barrier, inventory or NXV row page. True means staging is
/// complete, including exact block source identities. Safe to retry after any
/// ambiguous commit; the persisted high-watermarks never widen on replay.
pub(crate) async fn step(db: &Db, store: &Arc<Store>, epoch: i64, budget: Budget, cache: &mut Cache) -> Result<bool> {
    let state = on_native_db(db, move |c| {
        let r = c.query_one(
            "select phase,logical_now,flush_complete,flush_after_day,inventory_sealed,
            inventory_after_group,inventory_after_day from linker_epochs where id=$1",
            &[&epoch],
        )?;
        Ok((
            r.get::<_, String>(0),
            r.get::<_, i64>(1),
            r.get::<_, bool>(2),
            r.get::<_, Option<i64>>(3),
            r.get::<_, bool>(4),
            r.get::<_, Option<String>>(5),
            r.get::<_, Option<i64>>(6),
        ))
    })
    .await?;
    ensure!(state.0 == "stage", "native source step requires staging phase");
    if !state.2 {
        let days = store.open_days_page(state.3, INVENTORY_PAGE);
        if let Some(&last) = days.last() {
            let source = Arc::clone(store);
            tokio::task::spawn_blocking(move || source.flush_days(&days, Duration::from_secs(300))).await??;
            on_native_db(db, move |c| {
                super::linker::durable_execute(
                    c,
                    "update linker_epochs set flush_after_day=$2 where id=$1",
                    &[&epoch, &last],
                )?;
                Ok(())
            })
            .await?;
        } else {
            on_native_db(db, move |c| {
                super::linker::durable_execute(
                    c,
                    "update linker_epochs set flush_complete=true where id=$1",
                    &[&epoch],
                )?;
                Ok(())
            })
            .await?;
        }
        return Ok(false);
    }
    if !state.4 {
        let after = state.5.as_deref().zip(state.6);
        let page = store.segment_inventory_page(after, INVENTORY_PAGE);
        let mut captured = Vec::with_capacity(page.entries.len());
        for (group, day, _) in page.entries {
            let snapshot =
                store.segment_snapshot(&group, day).context("native source disappeared while retention was pinned")?;
            captured.push((group, day, snapshot.identity, i64::try_from(snapshot.count)?));
        }
        let now = state.1;
        on_native_db(db, move |c| {
            let mut tx = super::linker::durable_transaction(c)?;
            let reconcile: bool = tx
                .query_one(
                    "select coalesce((select not counts_reconciled from linker_bootstrap where singleton),false)",
                    &[],
                )?
                .get(0);
            for (group, day, generation, blocks) in &captured {
                let legacy: i64 = if reconcile {
                    tx.query_opt(
                        "select blocks from linker_legacy_counts where group_name=$1 and day=$2 limit 1",
                        &[group, day],
                    )?
                    .map_or(0, |r| r.get(0))
                } else {
                    0
                };
                let legacy = if legacy <= *blocks { legacy } else { 0 };
                tx.execute(
                    "insert into linker_epoch_inventory(epoch,group_name,day,generation,blocks,legacy_prefix)
                    values($1,$2,$3,$4,$5,$6) on conflict do nothing",
                    &[&epoch, group, day, &&generation[..], blocks, &legacy],
                )?;
                if *blocks > 0 {
                    tx.execute("insert into linker_day_seen values($1,$2) on conflict do nothing", &[day, &now])?;
                }
            }
            if let Some((group, day)) = &page.after {
                tx.execute(
                    "update linker_epochs set inventory_after_group=$2,inventory_after_day=$3 where id=$1",
                    &[&epoch, group, day],
                )?;
            }
            if page.complete {
                tx.execute("update linker_epochs set inventory_sealed=true where id=$1", &[&epoch])?;
                tx.execute("update linker_bootstrap set counts_reconciled=true where singleton", &[])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        return Ok(false);
    }
    let next = on_native_db(db, move |c| {
        Ok(c.query_opt(
            "select group_name,day,generation,blocks,scanned_blocks,legacy_prefix from linker_epoch_inventory
            where epoch=$1 and scanned_blocks<blocks order by group_name,day limit 1",
            &[&epoch],
        )?
        .map(|r| {
            (
                r.get::<_, String>(0),
                r.get::<_, i64>(1),
                r.get::<_, Vec<u8>>(2),
                r.get::<_, i64>(3),
                r.get::<_, i64>(4),
                r.get::<_, i64>(5),
            )
        }))
    })
    .await?;
    let Some((group, day, generation, blocks, index, legacy)) = next else {
        cache.block = None;
        return Ok(true);
    };
    let snapshot =
        store.segment_snapshot(&group, day).context("native source disappeared before durable staging completed")?;
    ensure!(
        snapshot.identity.as_slice() == generation.as_slice(),
        "native source generation changed before staging completed"
    );
    ensure!(snapshot.count >= usize::try_from(blocks)?, "native source prefix shrank before staging completed");
    let entry = snapshot
        .entries(usize::try_from(index)?, 1)
        .into_iter()
        .next()
        .context("native source manifest entry missing")?;
    ensure!(entry.seq as i64 == index, "native source manifest sequence changed");
    let prior = cache.block.take();
    let (verified,fresh,result)=on_native_db(db,move|c| {
        let (verified,fresh)=match prior {
            Some(block) if block.identity==snapshot.identity && block.entry==entry => (block,false),
            _=>(super::linker_stage::VerifiedBlock::new(&snapshot,entry)?,true),
        };
        let result=(||->Result<()> {
            let progress=super::linker_stage::stage_verified(c,&verified,&group,day,budget)?;
            ensure!(progress.scanned<=i64::from(entry.rows),"native source cursor exceeds manifest rows");
            if progress.complete {
                ensure!(progress.scanned==i64::from(entry.rows),"native completed source cursor differs from manifest rows");
                let mut tx=super::linker::durable_transaction(c)?;
                tx.execute("insert into linker_inventory_blocks values($1,$2,$3,$4,$5) on conflict do nothing",&[&epoch,&group,&day,&index,&progress.source])?;
                if index<legacy {tx.execute("update linker_source_blocks set legacy_priority=true where id=$1",&[&progress.source])?;}
                tx.execute("update linker_epoch_inventory set scanned_blocks=$4+1 where epoch=$1 and group_name=$2 and day=$3 and scanned_blocks=$4",
                    &[&epoch,&group,&day,&index])?;
                tx.commit()?;
            }
            Ok(())
        })();
        Ok((verified,fresh,result))
    }).await?;
    cache.block = Some(verified);
    cache.verifications += u64::from(fresh);
    result?;
    Ok(false)
}
