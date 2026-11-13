//! Native fresh-catalog processing on the canonical PostgreSQL owner session.

use anyhow::{Context,Result,ensure};
use postgres::GenericClient;
use sha2::{Digest,Sha256};

#[derive(Clone)]
pub(crate) struct CatalogIdentity { nonce:[u8;32],header_root:String }
impl CatalogIdentity {
    pub(crate) fn read_in(client:&mut impl GenericClient)->Result<Self> {
        let row=client.query_opt("select c.catalog_nonce,c.header_root,c.codec_version from native_catalog_identity c
            where c.singleton and c.database_oid=(select oid from pg_database where datname=current_database())
            and c.schema_oid=(select oid from pg_namespace where nspname=current_schema())",&[])?
            .context("native catalog identity is not initialized")?;
        ensure!(row.try_get::<_,i32>(2)?==1,"native catalog codec changed");
        Ok(Self {nonce:bytes32(&row,0)?,header_root:row.try_get(1)?})
    }
    pub(crate) fn nonce(&self)->&[u8;32] {&self.nonce}
    pub(crate) fn codec_version(&self)->i32 {1}
    pub(crate) fn header_config(&self,configured:&crate::config::HeaderStoreConfig)->Result<crate::config::HeaderStoreConfig> {
        let expected=catalog_root(configured,&self.nonce)?;
        ensure!(expected==self.header_root,"native catalog header root changed");
        Ok(crate::config::HeaderStoreConfig {enabled:configured.enabled,dir:expected})
    }
}

fn catalog_root(configured:&crate::config::HeaderStoreConfig,nonce:&[u8;32])->Result<String> {
    ensure!(configured.enabled,"native catalog requires the durable header store");
    let hex=nonce.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    let root=resolved_header_base(&configured.root())?.join("catalogs").join(hex);
    root.into_os_string().into_string().map_err(|_|anyhow::anyhow!("native header root is not UTF-8"))
}

/// Resolve only existing ancestor metadata; the new catalog directory need not
/// exist yet. No payload walk or directory creation participates in identity.
fn resolved_header_base(path:&std::path::Path)->Result<std::path::PathBuf> {
    use std::path::Component;
    let absolute=std::path::absolute(path)?;
    ensure!(absolute.components().count()<=256 && !absolute.components().any(|part|matches!(part,Component::ParentDir)),
            "native header root requires a bounded path without parent traversal");
    let mut ancestor=absolute.as_path();let mut missing=Vec::new();
    loop {
        match std::fs::metadata(ancestor) {
            Ok(metadata)=>{
                ensure!(metadata.is_dir(),"native header root ancestor is not a directory");
                let mut resolved=std::fs::canonicalize(ancestor)?;
                for component in missing.iter().rev() {resolved.push(component);}
                return Ok(resolved);
            }
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{
                missing.push(ancestor.file_name().context("native header root has no existing ancestor")?.to_os_string());
                ancestor=ancestor.parent().context("native header root has no existing ancestor")?;
            }
            Err(error)=>return Err(error.into()),
        }
    }
}

fn require_pristine_catalog(client:&mut impl GenericClient)->Result<()> {
    // Closed canonical table names, never request-supplied identifiers. Cache
    // ext_* tables deliberately do not participate in this fresh-data test.
    const EMPTY:&[&str]=&["releases","files","segments","file_parts","header_blocks","pp_queue","pp_state","pp_walks",
        "groups","backfill_chunks","backfill_deepest","backfill_media","backfill_speculative","backfill_sweep_gen",
        "backfill_sweeps","backfill_wanted","spot_comments","spot_disposals","spot_reports","spots_seen",
        "inferred_asked","inferred_ids","repair_tried","repair_older","archive_inner_files","sidecars","archive_release",
        "par2_files","segment_crc","rev_volumes","release_fingerprint","upload_link","upload_link_tried","beside_files",
        "release_probe","release_nfo","release_cover","release_opensubtitles","release_stills","release_images","picture_requests",
        "release_audio_print","release_audio_anchor","predb_match","predb_inner_key","release_category","media_release",
        "file_timing","release_indexed","upload_routes","release_keys","junk_releases","junk_collections","freeze_pending",
        "freeze_exempt","spots","nzb_pointed","release_nfo_password","word_queue","native_operations","native_runtime_epochs"];
    let mut checks=EMPTY.iter().map(|name|format!("exists(select 1 from \"{name}\")")).collect::<Vec<_>>();
    checks.push("exists(select 1 from meta where key not in ('archive_name_bytes','archive_names_over_quota','sidecar_text_bytes') or value is distinct from 0 or text_value is not null)".into());
    let occupied:bool=client.query_one(&format!("select {}",checks.join(" or ")),&[])?.try_get(0)?;
    ensure!(!occupied,"native catalog bootstrap refuses preexisting releases, headers, cursors or worker state");Ok(())
}

#[derive(Clone)]
pub(crate) struct OwnerEpoch {
    id:[u8;32],catalog:CatalogIdentity,pid:i32,started:String,schema:String,version:i32,code:[u8;32],
}
impl OwnerEpoch {
    pub(crate) fn id(&self)->&[u8;32] {&self.id}
    pub(crate) fn catalog(&self)->&CatalogIdentity {&self.catalog}
    /// Mutations require the original backend, not just a live row observed
    /// through a different pooled session. A replacement always has a new epoch.
    pub(crate) fn verify(&self,client:&mut impl GenericClient)->Result<()> {
        client.execute("select pg_stat_clear_snapshot()",&[])?;
        let valid:bool=client.query_one("select exists(select 1 from native_runtime_current c join native_runtime_epochs e on e.epoch=c.epoch
            join pg_stat_activity a on a.pid=e.backend_pid and a.backend_start=e.backend_start
            where c.singleton and e.epoch=$1 and e.catalog_nonce=$2 and e.state='active'
            and e.backend_pid=pg_backend_pid() and e.backend_pid=$3 and e.backend_start=$4::text::timestamptz
            and e.schema_version=$5 and e.code_digest=$6
            and e.schema_version=(select max(version) from schema_migrations)
            and e.database_oid=(select oid from pg_database where datname=current_database())
            and e.schema_oid=(select oid from pg_namespace where nspname=$7)
            and e.role_oid=(select oid from pg_roles where rolname=current_user)
            and e.schema_owner_oid=(select nspowner from pg_namespace where nspname=$7)
            and e.migrations_oid=(select c.oid from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname=$7 and c.relname='schema_migrations')
            and exists(select 1 from pg_locks l where l.pid=a.pid and l.locktype='advisory' and l.mode='ExclusiveLock' and l.granted
              and l.database=a.datid and l.classid=(hashtext('atlas.indexer')::bigint & 4294967295)::oid
              and l.objid=(hashtext($7)::bigint & 4294967295)::oid and l.objsubid=2))",
            &[&&self.id[..],&&self.catalog.nonce[..],&self.pid,&self.started,&self.version,&&self.code[..],&self.schema])?.try_get(0)?;
        ensure!(valid,"native owner epoch or original session ended");
        let actual=CatalogIdentity::read_in(client)?;
        ensure!(actual.nonce==self.catalog.nonce && actual.header_root==self.catalog.header_root,"native catalog binding changed");
        Ok(())
    }
}

fn bytes32(row:&postgres::Row,column:usize)->Result<[u8;32]> {
    row.try_get::<_,Vec<u8>>(column)?.try_into().map_err(|_|anyhow::anyhow!("native identity extent"))
}

/// Called only by Runtime::open after its actual exclusive schema lock, before
/// any worker admission. Existing NXV/cursor initialization ordering is retained.
pub(crate) fn open_epoch(
    client:&mut postgres::Client,storage:&super::StorageConfig,headers:&crate::config::HeaderStoreConfig,version:i32,
)->Result<OwnerEpoch> {
    ensure!(version>=9 && super::valid_schema(&storage.schema),"native Stage schema is unavailable");
    let mut tx=client.transaction()?;
    super::history_gate::native(&mut tx)?;
    let held:bool=tx.query_one("select exists(select 1 from pg_locks where pid=pg_backend_pid() and locktype='advisory'
        and mode='ExclusiveLock' and granted and classid=(hashtext('atlas.indexer')::bigint & 4294967295)::oid
        and objid=(hashtext($1)::bigint & 4294967295)::oid and objsubid=2)",&[&storage.schema])?.try_get(0)?;
    ensure!(held,"native Stage initialization requires canonical owner acquisition");
    let slot=tx.query_one("select epoch from native_runtime_current where singleton for update",&[])?;
    if let Some(old)=slot.try_get::<_,Option<Vec<u8>>>(0)? {
        tx.execute("select pg_stat_clear_snapshot()",&[])?;
        let alive:bool=tx.query_one("select exists(select 1 from native_runtime_epochs e join pg_stat_activity a
            on a.pid=e.backend_pid and a.backend_start=e.backend_start where e.epoch=$1)",&[&old])?.try_get(0)?;
        if alive {return Err(super::outage::SessionUnavailable.into());}
        tx.execute("update native_runtime_epochs set state='drained' where epoch=$1",&[&old])?;
    }
    if tx.query_opt("select 1 from native_catalog_identity where singleton",&[])?.is_none() {
        require_pristine_catalog(&mut tx)?;
        let mut nonce=[0;32];getrandom::fill(&mut nonce).map_err(|_|anyhow::anyhow!("native catalog entropy unavailable"))?;
        let root=catalog_root(headers,&nonce)?;
        tx.execute("insert into native_catalog_identity(singleton,catalog_nonce,header_root,database_oid,schema_oid,codec_version)
            select true,$1,$2,d.oid,n.oid,1 from pg_database d join pg_namespace n on n.nspname=$3 where d.datname=current_database()",
            &[&&nonce[..],&root,&storage.schema])?;
    }
    let catalog=CatalogIdentity::read_in(&mut tx)?;catalog.header_config(headers)?;
    let mut epoch=[0;32];getrandom::fill(&mut epoch).map_err(|_|anyhow::anyhow!("native owner entropy unavailable"))?;
    let code:[u8;32]=Sha256::digest(env!("ATLAS_GIT_SHA").as_bytes()).into();
    let row=tx.query_one("insert into native_runtime_epochs
        (epoch,catalog_nonce,backend_pid,backend_start,database_oid,schema_oid,role_oid,schema_owner_oid,migrations_oid,schema_version,code_digest,state)
        select $1,$2,a.pid,a.backend_start,d.oid,n.oid,r.oid,n.nspowner,c.oid,$3,$4,'active'
        from pg_stat_activity a join pg_database d on d.datname=current_database()
        join pg_namespace n on n.nspname=$5 join pg_roles r on r.rolname=current_user
        join pg_class c on c.relnamespace=n.oid and c.relname='schema_migrations'
        where a.pid=pg_backend_pid() returning backend_pid,to_char(backend_start at time zone 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
        &[&&epoch[..],&&catalog.nonce[..],&version,&&code[..],&storage.schema])?;
    tx.execute("update native_runtime_current set epoch=$1 where singleton",&[&&epoch[..]])?;
    // The previous actual session has drained and the new exclusive fence is
    // held. Reclaim only now; logical operation identities survive lease loss.
    tx.execute("update pp_queue set lease_token=null,lease_until=null,lease_epoch=null where lease_token is not null",&[])?;
    let owner=OwnerEpoch {id:epoch,catalog,pid:row.try_get(0)?,started:row.try_get(1)?,schema:storage.schema.clone(),version,code};
    owner.verify(&mut tx)?;tx.commit()?;Ok(owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_header_namespace_is_stable_and_rejects_a_different_configured_root() {
        let base=std::env::temp_dir().canonicalize().unwrap().join("native-header-fixture");
        let configured=crate::config::HeaderStoreConfig {enabled:true,dir:base.to_str().unwrap().into()};
        let root=base.join("catalogs").join("0707070707070707070707070707070707070707070707070707070707070707");
        let catalog=CatalogIdentity {nonce:[7;32],header_root:root.to_str().unwrap().into()};
        assert_eq!(catalog.header_config(&configured).unwrap().root(),root);
        assert_eq!(catalog.header_config(&configured).unwrap().root(),root);
        let wrong=crate::config::HeaderStoreConfig {enabled:true,dir:base.join("other").to_str().unwrap().into()};
        assert!(catalog.header_config(&wrong).is_err());
        let another=CatalogIdentity {nonce:[8;32],header_root:catalog.header_root};
        assert!(another.header_config(&configured).is_err());
    }

    #[test]
    fn first_catalog_identity_refuses_existing_rows_but_allows_preserved_external_cache() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.execute("insert into ext_opensubtitles(moviehash,checked_at) values('0000000000000001',1)",&[]).unwrap();
        assert!(require_pristine_catalog(&mut *client).is_ok());
        client.execute("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            values('example.group',false,$1,'Example.Release','poster','2020-01-01',false)",&[&vec![1u8;32]]).unwrap();
        assert!(require_pristine_catalog(&mut *client).is_err());
        assert_eq!(client.query_one("select count(*) from native_catalog_identity",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from releases",&[]).unwrap().get::<_,i64>(0),1);
    }

    #[test]
    fn canonical_bootstrap_meta_allows_first_native_owner_without_relabeling_rows() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        assert_eq!(client.query_one("select value from meta where key='next_seq'",&[]).unwrap().get::<_,i64>(0),1);
        assert!(require_pristine_catalog(&mut *client).is_ok());
        for (value,text) in [(Some(0i64),None),(Some(2),None),(None,None),(Some(1),Some("stale"))] {
            client.execute("update meta set value=$1,text_value=$2 where key='next_seq'",&[&value,&text]).unwrap();
            assert!(require_pristine_catalog(&mut *client).is_err());
        }
        client.execute("update meta set value=1,text_value=null where key='next_seq'",&[]).unwrap();
        client.execute("insert into meta(key,value) values('worker_cursor',0)",&[]).unwrap();
        assert!(require_pristine_catalog(&mut *client).is_err());
        client.execute("delete from meta where key='worker_cursor'",&[]).unwrap();
        let dir=tempfile::tempdir().unwrap();
        let headers=crate::config::HeaderStoreConfig {enabled:true,dir:dir.path().join("headers").to_str().unwrap().into()};
        let mut config=crate::config::Config::default();
        config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        config.auto_run_compact=false;config.shared_catalog.enabled=false;config.overview_archive.enabled=false;config.sealed_log.addr.clear();
        let storage=super::super::StorageConfig {backend:super::super::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
        let runtime=super::super::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
        assert!(runtime.owner.epoch().is_some());
        runtime.owner.with(|client| {
            assert_eq!(client.query_one("select value from meta where key='next_seq'",&[])?.get::<_,i64>(0),1);
            assert_eq!(client.query_one("select count(*) from releases",&[])?.get::<_,i64>(0),0);Ok(())
        }).unwrap();runtime.owner.close();
    }

    #[test]
    fn canonical_fresh_stage_starts_empty_and_rejects_orphan_results() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        for table in ["releases","files","segments","pp_queue","pp_state","pp_walks",
                      "release_probe","release_category","release_opensubtitles","media_release"] {
            let count:i64=client.query_one(&format!("select count(*) from {table}"),&[]).unwrap().get(0);
            assert_eq!(count,0,"fresh canonical table {table} contains no retained old catalog rows");
        }
        assert!(client.execute("insert into pp_state(release_id) values(41)",&[]).is_err());
    }

    #[test]
    fn fresh_stage_delete_removes_local_facts_but_keeps_natural_cache_answers() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.execute("insert into ext_opensubtitles(moviehash,imdb_id,checked_at) values('0000000000000001',7,1)",&[]).unwrap();
        let id:i64=client.query_one("insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            values('example.group',false,$1,'Example.Release','poster','2020-01-01',false) returning id",&[&vec![1u8;32]]).unwrap().get(0);
        client.execute("insert into pp_state(release_id,done) values($1,5)",&[&id]).unwrap();
        client.execute("insert into release_category(release_id,category) values($1,2000)",&[&id]).unwrap();
        client.execute("delete from releases where id=$1",&[&id]).unwrap();
        assert_eq!(client.query_one("select count(*) from pp_state",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from release_category",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select imdb_id from ext_opensubtitles where moviehash='0000000000000001'",&[]).unwrap().get::<_,i64>(0),7);
    }
}
