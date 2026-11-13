//! Natural-key cache commands on the current native attempt's owned session.

pub(crate) mod costly;

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use anyhow::{Result, bail, ensure};

use super::indexer_runtime::{Owner, destination_identity};
use super::outage::Failure;
use crate::probe::opensubtitles::Feature;

thread_local! {
    static CACHE_OWNER: RefCell<Option<Weak<Owner>>> = const { RefCell::new(None) };
    static NATIVE_SELECTED: Cell<bool> = const { Cell::new(false) };
}

/// A standard worker thread stays bound to one attempt; it cannot find a replacement session.
pub(crate) struct NativeCacheScope {
    previous: Option<Weak<Owner>>,
    bound: Weak<Owner>,
    _thread: PhantomData<Rc<()>>,
    _assets: super::worker_assets::Scope,
}

impl NativeCacheScope {
    pub(crate) fn bind(owner: &Arc<Owner>) -> Result<Self> {Self::bind_with_assets(owner,None)}
    pub(crate) fn bind_with_assets(owner: &Arc<Owner>,assets:Option<&Arc<super::worker_assets::Grant>>) -> Result<Self> {
        let assets=super::worker_assets::Scope::bind(owner,assets)?;
        ensure!(owner.cache_destination().is_some(), "cache requires a validated native owner");
        NATIVE_SELECTED.with(|selected| selected.set(true));
        let bound = Arc::downgrade(owner);
        let previous = CACHE_OWNER.with(|slot| slot.replace(Some(bound.clone())));
        Ok(Self { previous, bound, _thread: PhantomData, _assets: assets })
    }
}

impl Drop for NativeCacheScope {
    fn drop(&mut self) {
        CACHE_OWNER.with(|slot| {
            let mut current = slot.borrow_mut();
            if current.as_ref().is_some_and(|owner| Weak::ptr_eq(owner, &self.bound)) {
                *current = self.previous.take();
            }
        });
    }
}

pub(crate) enum NativeCacheCommand<'a> {
    Costly(costly::Command<'a>),
    LookupOpenSubtitles { hash: String },
    PutOpenSubtitles { hash: String, feature: Option<Feature>, checked_at: i64 },
}

pub(crate) enum NativeCacheReply {
    Costly(costly::Reply),
    Lookup(Option<Option<Feature>>),
    Stored,
}

/// None selects the legacy SQLite implementation only before a native scope exists.
pub(crate) fn execute_if_native(command: NativeCacheCommand<'_>) -> Result<Option<NativeCacheReply>> {
    let (registered, owner) = CACHE_OWNER.with(|slot| {
        let current = slot.borrow();
        (current.is_some(), current.as_ref().and_then(Weak::upgrade))
    });
    let storage = match crate::config::checked_storage_config() {
        Ok(storage) => storage,
        Err(_) => {
            if let Some(owner) = &owner { owner.fail(Failure::Fatal); }
            bail!("natural cache configuration is unavailable");
        }
    };
    let Some(owner) = owner else {
        let was_native = NATIVE_SELECTED.with(|selected| selected.get());
        if storage.is_postgres() { NATIVE_SELECTED.with(|selected| selected.set(true)); }
        ensure!(!registered && !was_native, "natural cache attempt has ended");
        storage.check().map_err(|_| anyhow::anyhow!("natural cache configuration is unavailable"))?;
        ensure!(!storage.is_postgres(), "natural cache requires the native owner scope");
        return Ok(None);
    };
    let destination = owner.cache_destination().ok_or_else(|| anyhow::anyhow!("cache owner is not validated"))?.clone();
    if storage != destination.storage {
        owner.fail(Failure::Fatal);
        bail!("natural cache destination changed");
    }
    // Stage lanes are existing standard threads. An entered Tokio context is not a sync-client lane.
    ensure!(tokio::runtime::Handle::try_current().is_err(), "natural cache requires a standard worker thread");
    owner.with(move |client| {
        ensure!(destination_identity(client, &destination.storage.schema)? == destination.identity,
            "natural cache destination identity changed");
        ensure!(super::migrate::version(client, &destination.storage.schema)? == destination.version,
            "natural cache schema version changed");
        let table = format!("\"{}\".ext_opensubtitles", destination.storage.schema);
        let mut tx = client.transaction()?;
        super::history_gate::native(&mut tx)?;
        let reply = match command {
            NativeCacheCommand::Costly(command) => NativeCacheReply::Costly(costly::execute(&mut tx,command)?),
            NativeCacheCommand::LookupOpenSubtitles { hash } => {
                let row = tx.query_opt(&format!(
                    "select imdb_id,tmdb_id,kind,title,year,season,episode,release_name from {table} where moviehash=$1"
                ), &[&hash])?;
                let found = row.map(|row| Feature {
                    imdb_id: row.get(0), tmdb_id: row.get(1), kind: row.get(2), title: row.get(3),
                    year: row.get(4), season: row.get(5), episode: row.get(6), release_name: row.get(7),
                }).map(|feature| if feature.imdb_id.is_some() { Some(feature) } else { None });
                NativeCacheReply::Lookup(found)
            }
            NativeCacheCommand::PutOpenSubtitles { hash, feature, checked_at } => {
                let f = feature.unwrap_or_default();
                tx.execute(&format!(
                    "insert into {table} as existing
                     (moviehash,imdb_id,tmdb_id,kind,title,year,season,episode,release_name,raw_json_zst,checked_at)
                     values($1,$2,$3,$4,$5,$6,$7,$8,$9,NULL,$10)
                     on conflict(moviehash) do update set
                     imdb_id=coalesce(excluded.imdb_id,existing.imdb_id),
                     tmdb_id=coalesce(excluded.tmdb_id,existing.tmdb_id),
                     kind=coalesce(excluded.kind,existing.kind), title=coalesce(excluded.title,existing.title),
                     year=coalesce(excluded.year,existing.year), season=coalesce(excluded.season,existing.season),
                     episode=coalesce(excluded.episode,existing.episode),
                     release_name=coalesce(excluded.release_name,existing.release_name),
                     raw_json_zst=NULL, checked_at=excluded.checked_at"
                ), &[&hash,&f.imdb_id,&f.tmdb_id,&f.kind,&f.title,&f.year,&f.season,&f.episode,&f.release_name,&checked_at])?;
                NativeCacheReply::Stored
            }
        };
        tx.commit()?;
        Ok(Some(reply))
    })
}
