//! Explicit, fenced cold-cache archival. No startup path calls this operation.
use anyhow::{Context,Result,ensure,bail};
use postgres::GenericClient;
use serde::{Serialize,Deserialize};
use super::{indexer_runtime::Owner,natural_cache::par2_quota};

const FACTS:&[(&str,&str)]=&[("ext_raw_par2","atlas_par2_cold_raw"),("ext_raw_par2_files","atlas_par2_cold_files"),("ext_preserved_cache_rows","atlas_par2_cold_preserved")];
const LEDGER:&[(&str,&str)]=&[("atlas_par2_cache_usage","atlas_par2_cold_usage"),("atlas_par2_cache_inventories","atlas_par2_cold_inventories"),("atlas_par2_cache_members","atlas_par2_cold_members")];
const TOUCH:(&str,&str)=("atlas_par2_cache_touch","atlas_par2_cold_touch");
const SCOPE:&str="atlas_par2_live_scope";
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
struct Object {name:String,cold:String,old_oid:u32,old_node:u32,new_oid:u32,new_node:u32}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub(crate) struct Receipt {
    scope_id:String,catalog_nonce:String,owner_epoch:String,
    baseline_kind:String,source_baseline_sha256:String,
    old_partial_usage:par2_quota::Usage,objects:Vec<Object>,
}
fn q(name:&str)->String {format!("\"{}\"",name.replace('"',"\"\""))}
fn object(c:&mut impl GenericClient,name:&str)->Result<(u32,u32)> {
    let r=c.query_one("select c.oid,pg_relation_filenode(c.oid) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname=current_schema() and c.relname=$1 and c.relkind in('r','S') and c.relpersistence='p'",&[&name])?;
    Ok((r.try_get(0)?,r.try_get::<_,Option<u32>>(1)?.context("cache scope object has no physical identity")?))
}
fn exists(c:&mut impl GenericClient,name:&str)->Result<bool> {
    Ok(c.query_one("select to_regclass(format('%I.%I',current_schema(),$1)) is not null",&[&name])?.try_get(0)?)
}
fn others(c:&mut impl GenericClient)->Result<Vec<(String,u32)>> {
    Ok(c.query("select relname::text,c.oid from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname=current_schema() and relkind='r' and relname like 'ext\\_%' and relname not in('ext_raw_par2','ext_raw_par2_files','ext_preserved_cache_rows') order by relname",&[])?.into_iter().map(|r|(r.get(0),r.get(1))).collect())
}
fn validate(c:&mut impl GenericClient,r:&Receipt,catalog:&str,baseline:&str)->Result<()> {
    ensure!(r.catalog_nonce==catalog&&r.source_baseline_sha256==baseline&&r.baseline_kind=="operator_reference_to_old_scope","cache scope reference/catalog differs");
    let pairs=FACTS.iter().chain(LEDGER).chain(std::iter::once(&TOUCH)).collect::<Vec<_>>();
    ensure!(r.objects.len()==pairs.len(),"cache scope object receipt is incomplete");
    for (o,(hot,cold)) in r.objects.iter().zip(pairs) {
        ensure!(o.name==*hot&&o.cold==*cold,"cache scope object names differ");
        ensure!(object(c,hot)?==(o.new_oid,o.new_node)&&object(c,cold)?==(o.old_oid,o.old_node),"cache scope physical identity changed");
    }
    Ok(())
}
/// A source-scope stamp only. It grants neither target adoption nor COPY parity.
pub(crate) fn source_scope(c:&mut impl GenericClient)->Result<Option<String>> {
    if !exists(c,SCOPE)? {return Ok(None)}
    Ok(Some(c.query_one("select encode(scope_id,'hex') from atlas_par2_live_scope where singleton",&[])?.try_get(0)?))
}
/// The only production caller is the explicit CLI after genuine Runtime::open.
/// All DDL, empty EOFs and the receipt commit atomically on its original Owner.
pub(crate) fn archive(owner:&Owner,baseline:[u8;32])->Result<Receipt> {
    ensure!(tokio::runtime::Handle::try_current().is_err(),"cache archival requires a standard native lane");
    let epoch=owner.epoch().context("cache archival requires canonical ownership")?;
    let catalog=super::fresh_assets::hex(epoch.catalog().nonce());
    let baseline=super::fresh_assets::hex(&baseline);
    owner.with(|client| {
        let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;epoch.verify(&mut tx)?;
        // The defensive no-claims proof can scan PP tables; bound server
        // execution as well as lock acquisition. Any timeout rolls back DDL.
        tx.batch_execute("set local synchronous_commit=on;set local lock_timeout='5s';set local statement_timeout='5s'")?;
        let claimed:bool=tx.query_one("select exists(select 1 from pp_queue where lease_epoch=$1) or exists(select 1 from pp_state where native_net_epoch=$1)",&[&epoch.id().as_slice()])?.try_get(0)?;
        ensure!(!claimed,"cache archival requires pre-claim or fully drained ownership");
        if exists(&mut tx,SCOPE)? {
            let r:Receipt=serde_json::from_str(&tx.query_one("select receipt from atlas_par2_live_scope where singleton",&[])?.try_get::<_,String>(0)?)?;
            validate(&mut tx,&r,&catalog,&baseline)?;tx.commit()?;return Ok(r)
        }
        let pairs=FACTS.iter().chain(LEDGER).chain(std::iter::once(&TOUCH)).copied().collect::<Vec<_>>();
        for (_,cold) in &pairs {ensure!(!exists(&mut tx,cold)?,"cold cache object exists without a committed scope receipt");}
        let untouched=others(&mut tx)?;
        // Fixed closed relation names; no operator argument becomes an identifier.
        tx.batch_execute(&format!("lock table {} in access exclusive mode",FACTS.iter().chain(LEDGER).map(|(n,_)|q(n)).collect::<Vec<_>>().join(",")))?;
        let old_partial_usage=par2_quota::usage(&mut tx)?;
        let mut objects=Vec::new();
        for (hot,cold) in &pairs {
            let (old_oid,old_node)=object(&mut tx,hot)?;
            tx.batch_execute(&format!("alter {} {} rename to {}",if *hot==TOUCH.0 {"sequence"}else{"table"},q(hot),q(cold)))?;
            objects.push(Object {name:(*hot).into(),cold:(*cold).into(),old_oid,old_node,new_oid:0,new_node:0});
        }
        for (hot,cold) in FACTS.iter().chain(LEDGER) {
            tx.batch_execute(&format!("create table {} (like {} including all);revoke all on {} from public",q(hot),q(cold),q(hot)))?;
        }
        tx.batch_execute("create sequence atlas_par2_cache_touch;revoke all on atlas_par2_cache_touch from public;
            alter table atlas_par2_cache_members add foreign key(inventory_key) references atlas_par2_cache_inventories(inventory_key)")?;
        // Renaming preserves the OLD composite type OIDs. New overloads bind
        // the same unchanged quota logic to the freshly created row types.
        tx.batch_execute(include_str!("par2_scope.sql"))?;
        for (hot,_) in FACTS.iter().chain(LEDGER) {
            tx.batch_execute(&format!("create trigger atlas_history_write_gate before insert or update or delete or truncate on {} for each statement execute function atlas_native_write_trigger()",q(hot)))?;
        }
        for (hot,_) in FACTS {
            tx.batch_execute(&format!("create trigger cache_natural_keys before insert or update on {t} for each row execute function atlas_cache_natural_keys();
                create trigger par2_cache_lock before insert or update or delete or truncate on {t} for each statement execute function atlas_par2_cache_lock();
                create trigger par2_cache_account after insert or update or delete on {t} for each row execute function atlas_par2_cache_account();
                create trigger par2_cache_truncate before truncate on {t} for each statement execute function atlas_par2_cache_truncate()",t=q(hot)))?;
        }
        tx.batch_execute("create constraint trigger atlas_par2_cache_at_commit after update on atlas_par2_cache_usage deferrable initially deferred for each row when (OLD.pending_xid is distinct from NEW.pending_xid) execute function atlas_par2_cache_commit();
            insert into atlas_par2_cache_usage(singleton) values(true)")?;
        for (_,cold) in FACTS.iter().chain(LEDGER) {
            tx.batch_execute(&format!("create trigger atlas_par2_cold_refuse_gate before insert or update or delete or truncate on {} for each statement execute function atlas_par2_cold_refuse()",q(cold)))?;
        }
        // Rebind any backend-local PL/pgSQL plans after the relation-name swap.
        tx.batch_execute("discard plans")?;
        for _ in 0..5 {par2_quota::initialize_page(&mut tx)?;}
        let ready=par2_quota::usage(&mut tx)?;
        ensure!(ready.phase==par2_quota::Phase::Ready&&ready.retained_rows==0&&ready.retained_bytes==0&&ready.evicted_rows==0,"new live cache did not reach genuine empty EOF readiness");
        for o in &mut objects {
            ensure!(object(&mut tx,&o.cold)?==(o.old_oid,o.old_node),"archival changed the inherited physical heap");
            (o.new_oid,o.new_node)=object(&mut tx,&o.name)?;
            ensure!(o.new_oid!=o.old_oid,"new live cache reused an inherited relation");
        }
        ensure!(others(&mut tx)?==untouched,"archival changed an unrelated active cache relation");
        let mut id=[0;32];getrandom::fill(&mut id).map_err(|_|anyhow::anyhow!("cache scope identity unavailable"))?;
        let r=Receipt {scope_id:super::fresh_assets::hex(&id),catalog_nonce:catalog.clone(),owner_epoch:super::fresh_assets::hex(epoch.id()),baseline_kind:"operator_reference_to_old_scope".into(),source_baseline_sha256:baseline.clone(),old_partial_usage,objects};
        tx.batch_execute("create table atlas_par2_live_scope(singleton boolean primary key check(singleton),scope_id bytea not null check(octet_length(scope_id)=32),receipt text not null);
            revoke all on atlas_par2_live_scope from public;
            create trigger atlas_history_write_gate before insert or update or delete or truncate on atlas_par2_live_scope for each statement execute function atlas_native_write_trigger();
            create trigger atlas_par2_scope_immutable before update or delete or truncate on atlas_par2_live_scope for each statement execute function atlas_par2_cold_refuse()")?;
        tx.execute("insert into atlas_par2_live_scope values(true,$1,$2)",&[&id.as_slice(),&serde_json::to_string(&r)?])?;
        validate(&mut tx,&r,&catalog,&baseline)?;
        if owner.stop.load(std::sync::atomic::Ordering::Acquire) {tx.rollback()?;return Err(super::outage::AttemptEnded.into())}
        tx.commit()?;Ok(r)
    })
}
fn parse(args:&[String])->Result<[u8;32]> {
    let mut apply=false;let mut command=false;let mut baseline=None;let mut i=0;
    while i<args.len() {match args[i].as_str() {
        "--archive-par2-cache" if !command=>command=true,
        "--apply" if !apply=>apply=true,
        "--source-baseline-sha256" if baseline.is_none()=>{i+=1;baseline=args.get(i).map(String::as_str);ensure!(baseline.is_some(),"source baseline digest is required");},
        _=>bail!("use --archive-par2-cache --source-baseline-sha256 SHA256 --apply without other commands"),
    }i+=1;}
    ensure!(command&&apply,"explicit --archive-par2-cache and --apply are required");
    let text=baseline.context("source baseline digest is required")?;
    ensure!(text.len()==64&&text.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)),"source baseline must be a lowercase SHA256");
    let mut out=[0;32];for (i,byte) in out.iter_mut().enumerate(){*byte=u8::from_str_radix(&text[2*i..2*i+2],16)?;}Ok(out)
}
pub fn run_cli(args:&[String])->i32 {
    let go=||->Result<()> {
        let baseline=parse(args)?;
        let storage=crate::config::checked_storage_config().map_err(|_|anyhow::anyhow!("checked PostgreSQL selection is required"))?;
        let config=crate::config::load_config().context("checked native configuration is required")?;
        let servers=config.servers.iter().filter(|s|!s.host.is_empty()&&!s.password.is_empty()).cloned().collect::<Vec<_>>();
        let runtime=super::indexer_runtime::Runtime::open(&config,storage,crate::config::header_store_config(),&crate::nntp::server_keys(&servers),&mut None)?;
        let result=archive(&runtime.owner,baseline);runtime.owner.close();
        println!("{}",serde_json::to_string(&result?)?);
        eprintln!("Old baseline remains the old cold scope. A new live dump baseline requires explicit --allow-shrink after receipt review.");Ok(())
    };
    match go(){Ok(())=>0,Err(error)=>{eprintln!("PAR2 archival maintenance refused: {}",super::linker_runtime::failure_diagnostic(&error));1}}
}
#[cfg(test)]
#[path="par2_scope_tests.rs"]
pub(crate) mod tests;
