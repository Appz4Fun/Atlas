//! The configured alternate-provider overview sweep on the original PG attempt.
//! No SQLite reader, reconnect, new provider or alternate header writer exists.
use std::{collections::{BTreeMap,BTreeSet,HashMap,HashSet},sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};
use anyhow::{Context,Result,ensure};
use postgres::{GenericClient,IsolationLevel};
use crate::{indexer::{Db,PassContext,PassSettings},pg::indexer_runtime::Owner,nxv_store::store::Store};
use super::{Window,Looked,Found,Older,EVERY,MIN_BYTES,MIN_ROW_BYTES,MIN_PERMILLE,DAYS,SETTLED_SECS,CANDIDATES,OLDER_SCAN,OLDER_CANDIDATES,PER_ROUND,MARGIN,MAX_ARTICLES};
#[path="native_store.rs"]
mod storage;
#[path="native_posts.rs"]
mod posts;
use storage::{Source,Budget,Read,Deferred};
#[derive(Debug)]
struct SourceChanged;
impl std::fmt::Display for SourceChanged {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("native overview source changed during observation")}}
impl std::error::Error for SourceChanged {}
#[derive(Debug,Default)]
pub(crate) struct Progress {pub waiting:Option<Duration>,pub posts:usize,pub headers:i64,pub deferred:usize,pub provider_failures:usize}
struct Plan {found:Found,sources:HashMap<i64,Source>,deferred:usize}
pub(crate) struct Service {
    owner:Arc<Owner>,db:Db,ctx:PassContext,handle:tokio::runtime::Handle,store:Arc<Store>,settings:PassSettings,
    epoch:[u8;32],catalog:[u8;32],next:Instant,
}
impl Service {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(owner:&Arc<Owner>,db:&Db,ctx:PassContext,handle:tokio::runtime::Handle,store:Arc<Store>,headers:&crate::config::HeaderStoreConfig,settings:PassSettings)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native overview repair requires a standard worker thread");
        db.check_native_owner(owner)?;let epoch=owner.epoch().context("native overview owner epoch missing")?;
        let actual=epoch.catalog().header_config(headers)?;ensure!(store.root()==actual.root(),"native overview header store belongs to another catalog");
        let live=crate::nxv_store::live::handle().context("native overview requires the existing live header store")?;
        ensure!(Arc::ptr_eq(&live,&store),"native overview was not given the original live header store");
        ensure!(settings.request_size>0,"native overview request size is zero");owner.with(|_|Ok(()))?;
        Ok(Self {owner:owner.clone(),db:db.clone(),ctx,handle,store,settings,epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),next:Instant::now()+EVERY})
    }
    fn stopped(&self)->Result<()> {
        // A recorded local storage failure wins a simultaneous intentional
        // stop or recoverable PG failure; it is never recast as cancellation.
        if let Some(error)=self.store.failure(){anyhow::bail!("native overview header store is unavailable: {error}")}
        if self.owner.stop.load(Ordering::Acquire)||self.ctx.stop.load(Ordering::Acquire){return Err(crate::pg::outage::AttemptEnded.into())}
        self.db.check_native_owner(&self.owner)?;
        ensure!(self.owner.epoch().is_some_and(|e|e.id()==&self.epoch&&e.catalog().nonce()==&self.catalog),"native overview original epoch changed");
        let live=crate::nxv_store::live::handle().context("native overview live header store ended")?;ensure!(Arc::ptr_eq(&live,&self.store),"native overview live header store was rebound");Ok(())
    }
    fn budget()->Budget {Budget {bytes:2*1024*1024,until:Instant::now()+Duration::from_secs(10)}}
    fn read<T>(&self,budget:&mut Budget,work:impl FnOnce(&mut Read<'_,'_>)->Result<T>)->Result<T> {
        self.stopped()?;
        // An explicit bounded/deferred observation is returned outside with(),
        // so it cannot poison the owner as an invariant/storage failure. Actual
        // PG/protocol/local-store errors still cross the original classifier.
        self.owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
            let result=work(&mut Read {tx:&mut tx,budget,stop:&self.owner.stop});
            match result {
                Err(e) if e.is::<Deferred>()||e.is::<SourceChanged>()=>{tx.rollback()?;Ok(Err(e))},
                Err(e)=>Err(e),Ok(value)=>{tx.commit()?;Ok(Ok(value))},
            }
        })?
    }
    fn observe(&self,id:i64,recent:bool,now:i64,budget:&mut Budget)->Result<(Looked,Vec<Source>)> {
        self.read(budget,|r| {
            let post=posts::post(r,id)?;let permille=r.union(&post.ids)?.permille();let all_windows=r.windows(&post.ids)?;
            let worth=post.size>=MIN_BYTES&&(MIN_PERMILLE..1000).contains(&permille);
            let days=if recent&&post.size>=MIN_BYTES&&permille<MIN_PERMILLE {super::days_of(&all_windows)}else{Vec::new()};
            let name=r.head(id)?.ok_or(SourceChanged)?.name;let sources=r.sources(&post.ids)?;
            let looked=Looked {ids:post.ids,permille,windows:if worth{all_windows}else{Vec::new()},days,media:super::is_media(&name),size:post.size};
            r.budget.charge(512+looked.ids.len()*128+looked.windows.iter().map(|w|w.group.capacity()+128).sum::<usize>())?;
            let _=now;Ok((looked,sources))
        })
    }
    fn plan(&self,now:i64)->Result<Plan> {
        let mut budget=Self::budget();let (recent,older,end)=self.read(&mut budget,|r|candidate_ids(r,now))?;
        let mut plan=Plan {found:Found::default(),sources:HashMap::new(),deferred:0};let mut seen=HashSet::new();
        for id in recent {if seen.contains(&id){continue;}match self.observe(id,true,now,&mut budget) {
            Ok((post,sources))=>{seen.extend(post.ids.iter().copied());for s in sources{plan.sources.insert(s.id,s);}plan.found.posts.push(post);},
            Err(e) if e.is::<Deferred>()||e.is::<SourceChanged>()=>plan.deferred+=1,Err(e)=>return Err(e),
        }}
        super::by_worth(&mut plan.found.posts);let first=plan.found.posts.len();let mut keys=Vec::new();let mut end=end;
        for id in older {if seen.contains(&id){continue;}match self.observe(id,false,now,&mut budget) {
            Ok((post,sources))=>{seen.extend(post.ids.iter().copied());for s in sources{plan.sources.insert(s.id,s);}keys.push(id);plan.found.posts.push(post);},
            Err(e) if e.is::<Deferred>()||e.is::<SourceChanged>()=>{plan.deferred+=1;end=Some(id.saturating_add(1));break;},Err(e)=>return Err(e),
        }}
        plan.found.older.push(Older {shard:0,first,keys,end});Ok(plan)
    }
    fn still_current(&self,sources:&[Source])->Result<bool> {
        self.read(&mut Self::budget(),|r| {for s in sources {if !r.source_matches(s)?{return Ok(false)}}Ok(true)})
    }
    fn current_sources(&self,ids:&[i64],expected:&HashMap<i64,Source>)->Result<Vec<Source>> {
        self.read(&mut Self::budget(),|r| {let sources=r.sources(ids)?;for s in &sources {let prior=expected.get(&s.id).context("native overview unobserved source")?;
            if s.group!=prior.group||s.identity!=prior.identity||s.collection!=prior.collection{return Err(SourceChanged.into())}}
            Ok(sources)})
    }
    fn mark(&self,sources:&[Source],now:i64,days:&[(String,i64)],tried:&[(i64,i64)],older:&[(usize,Option<i64>)])->Result<()> {
        self.stopped()?;
        let result=self.owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(IsolationLevel::ReadCommitted).start()?;
            crate::pg::guard_native_transaction(&mut tx)?;self.owner.epoch().context("native overview lost epoch")?.verify(&mut tx)?;
            let groups=sources.iter().map(|s|s.group.as_str()).collect::<BTreeSet<_>>();
            for group in groups {tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1,7936))",&[&group])?;}
            let mut sorted=sources.iter().collect::<Vec<_>>();sorted.sort_by_key(|s|s.id);
            for s in sorted {if !storage::source_matches(&mut tx,s,true)? {tx.rollback()?;return Ok(false)}}
            if self.owner.stop.load(Ordering::Acquire)||self.ctx.stop.load(Ordering::Acquire){return Err(crate::pg::outage::AttemptEnded.into())}
            super::mark_in(&crate::pg::sql::Pg::new(&mut tx),now,days,tried,older)?;
            tx.commit()?;Ok(true)
        })?;
        if !result{return Err(SourceChanged.into())}Ok(())
    }
    fn provider_error(&self,error:&anyhow::Error)->bool {
        error.chain().any(|e|matches!(e.downcast_ref::<crate::nntp::NntpError>(),Some(crate::nntp::NntpError::Io(_)|crate::nntp::NntpError::Reply{..}|crate::nntp::NntpError::Protocol(_)|crate::nntp::NntpError::Decompress(_))))
    }
    fn group(&self,server:usize,group:&str)->Result<(u64,u64)> {
        self.stopped()?;let result=self.handle.block_on(crate::nntp::unless_stopped(&self.owner.stop,self.ctx.pool.group_on(server,group)));
        let result=result.ok_or(crate::pg::outage::AttemptEnded)??;
        if result.1>i64::MAX as u64||result.2>=i64::MAX as u64 {return Err(crate::nntp::NntpError::Protocol("native overview GROUP numbers exceed the admitted native range".into()).into())}
        Ok((result.1,result.2))
    }
    fn same_ids(&self,a:usize,b:usize,group:&str,last:u64)->Result<bool> {
        let start=last.saturating_sub(super::SAME_IDS_LOOK);
        // Unlike the legacy bool helper, storage failures retain their typed
        // provenance instead of becoming a false "different backend" fact.
        let read=|server|->Result<Option<Vec<(u64,String)>>> {self.stopped()?;match self.request_ids(server,group,start,last) {
            Ok(ids)=>Ok(Some(ids)),Err(e)=>{if self.provider_error(&e){self.stopped()?;Ok(None)}else{Err(e)}}
        }};
        let (Some(ours),Some(theirs))=(read(a)?,read(b)?)else{return Ok(false)};Ok(super::ids_agree(&ours,&theirs))
    }
    fn request_ids(&self,server:usize,group:&str,start:u64,end:u64)->Result<Vec<(u64,String)>> {
        self.stopped()?;self.handle.block_on(async {
            let fetch=crate::nntp::unless_stopped(&self.owner.stop,crate::nxv_store::live::Fetch::begin_on(self.store.clone())).await.ok_or(crate::pg::outage::AttemptEnded)??;
            let result=crate::nntp::unless_stopped(&self.owner.stop,self.ctx.pool.ids_on_with_fetch(server,group,start,end,&fetch)).await;
            fetch.finish().await?;
            Ok(result.ok_or(crate::pg::outage::AttemptEnded)??)
        })
    }
    fn date(&self,server:usize,group:&str,first:u64,last:u64,when:i64)->Result<u64> {
        self.stopped()?;self.handle.block_on(async {
            let fetch=crate::nntp::unless_stopped(&self.owner.stop,crate::nxv_store::live::Fetch::begin_on(self.store.clone())).await.ok_or(crate::pg::outage::AttemptEnded)??;
            let result=crate::nntp::unless_stopped(&self.owner.stop,self.ctx.pool.article_at_with_fetch(server,group,first,last,when,&fetch)).await;
            // This consuming close is outside unless_stopped. Any already
            // admitted recording and its exact durability answer are joined.
            fetch.finish().await?;
            Ok(result.ok_or(crate::pg::outage::AttemptEnded)??)
        })
    }
    fn window(&self,window:&Window,server:usize)->Result<crate::indexer::Progress> {
        let (first,last)=self.group(server,&window.group)?;if last<first{return Ok(crate::indexer::Progress::default())}
        let start=self.date(server,&window.group,first,last,window.from)?;
        let end=self.date(server,&window.group,first,last,window.to)?.saturating_sub(1).min(last);
        self.stopped()?;
        self.handle.block_on(crate::indexer::reread_range(&self.ctx,&self.settings,&self.db,&window.group,server,start,end,MAX_ARTICLES))
    }
    fn whole(&self,id:i64)->Result<bool> {self.read(&mut Self::budget(),|r|{let post=posts::post(r,id)?;Ok(r.union(&post.ids)?.complete())})}
    pub(crate) fn tick(&mut self,now:i64)->Result<Progress> {
        match self.tick_work(now) {
            Err(e)if e.is::<Deferred>()||e.is::<SourceChanged>()=>Ok(Progress {deferred:1,..Default::default()}),
            result=>result,
        }
    }
    fn tick_work(&mut self,now:i64)->Result<Progress> {
        self.stopped()?;if Instant::now()<self.next{return Ok(Progress {waiting:Some(self.next.saturating_duration_since(Instant::now())),..Default::default()})}
        self.next=Instant::now()+EVERY;
        let plan=match self.plan(now){Ok(plan)=>plan,Err(e)if e.is::<Deferred>()||e.is::<SourceChanged>()=>return Ok(Progress {deferred:1,..Default::default()}),Err(e)=>return Err(e)};
        let mut progress=Progress {deferred:plan.deferred,..Default::default()};
        let (to_read,looked)=super::plan(&plan.found.posts,PER_ROUND);let mut done=looked.iter().copied().collect::<HashSet<_>>();
        let sources=plan.sources.values().cloned().collect::<Vec<_>>();let days=plan.found.posts.iter().flat_map(|p|p.days.iter().cloned()).collect::<Vec<_>>();
        let tried=looked.iter().flat_map(|&i| {
            let post=&plan.found.posts[i];let permille=post.permille;
            post.ids.iter().map(move|&id|(id,permille))
        }).collect::<Vec<_>>();
        let sweeps=|done:&HashSet<usize>|plan.found.older.iter().map(|o|(o.shard,o.next(done))).collect::<Vec<_>>();
        if let Err(e)=self.mark(&sources,now,&days,&tried,&sweeps(&done)){if e.is::<SourceChanged>(){progress.deferred+=1;return Ok(progress)}return Err(e)}
        for i in to_read {
            self.stopped()?;let post=&plan.found.posts[i];let observed=post.ids.iter().filter_map(|id|plan.sources.get(id).cloned()).collect::<Vec<_>>();
            if !self.still_current(&observed)?{progress.deferred+=1;continue;}
            let mut deferred=false;let mut whole=false;
            for (_,day) in super::days_of(&post.windows){crate::nxv_store::live::note_claimed_day(day);}
            'windows:for window in &post.windows {
                self.stopped()?;
                let indexing=self.read(&mut Self::budget(),|r| {
                    let prefix=format!("{}@",window.group);let end=format!("{}A",window.group);
                    let extents=r.query("select octet_length(name) from groups where name>=$1 and name<$2 order by name limit 1025",&[&prefix,&end])?;
                    if extents.len()>1024{return Err(Deferred.into())}let mut bytes=0usize;
                    for row in &extents {let n:i32=row.try_get(0)?;if !(0..=65536).contains(&n){return Err(Deferred.into())}bytes=bytes.checked_add(n as usize*6+256).context("native overview provider extent overflow")?;}
                    r.budget.charge(bytes)?;
                    let rows=r.query("select name from groups where name>=$1 and name<$2 order by name limit 1025",&[&prefix,&end])?;
                    let mut found=HashSet::new();for row in rows {let name:String=row.try_get(0)?;if let Some(host)=name.strip_prefix(&prefix){found.insert(host.to_string());}}Ok(found)
                })?;
                let mut seen=indexing.iter().map(|h|super::machine(h)).collect::<HashSet<_>>();let mut spools=Vec::new();
                for server in self.ctx.pool.indexing_servers() {if indexing.iter().any(|h|super::machine(h)==super::machine(&self.ctx.pool.host(server))) {match self.group(server,&window.group){Ok(range)=>spools.push((server,range)),Err(e)if self.provider_error(&e)=>{self.stopped()?;progress.provider_failures+=1},Err(e)=>return Err(e)}}}
                for server in self.ctx.pool.indexing_servers() {
                    self.stopped()?;let host=self.ctx.pool.host(server);if !seen.insert(super::machine(&host)){continue;}
                    let spool=match self.group(server,&window.group){Ok(range)=>range,Err(e)if self.provider_error(&e)=>{self.stopped()?;progress.provider_failures+=1;continue},Err(e)=>return Err(e)};
                    let mut shared=false;for &(other,numbers) in &spools {if super::same_spool(numbers,spool)&&self.same_ids(other,server,&window.group,numbers.1.min(spool.1))?{shared=true;break;}}
                    if shared{continue;}
                    // Do not cancel this whole future: it owns parsing, native
                    // admission, the blocking PG commit and actual NXV tickets.
                    let read=self.window(window,server);
                    match read {
                        Ok(p)=>{self.stopped()?;progress.headers+=p.articles;spools.push((server,spool));if p.articles>0&&self.whole(post.ids[0])?{whole=true;break 'windows;}},
                        Err(e)if e.is::<crate::indexer::RereadWindowTooWide>() || e.chain().any(|e|e.downcast_ref::<crate::nntp::NntpError>().is_some_and(crate::nntp::NntpError::is_overview_budget))=>{deferred=true;progress.deferred+=1;},
                        Err(e)if self.provider_error(&e)=>{self.stopped()?;progress.provider_failures+=1;},Err(e)=>return Err(e),
                    }
                }
            }
            if deferred&&!whole {continue;}
            self.stopped()?;let sources=match self.current_sources(&post.ids,&plan.sources){Ok(s)=>s,Err(e)if e.is::<SourceChanged>()=>{progress.deferred+=1;continue},Err(e)=>return Err(e)};
            match self.mark(&sources,now,&[],&post.ids.iter().map(|&id|(id,post.permille)).collect::<Vec<_>>(),&[]) {
                Ok(())=>{done.insert(i);progress.posts+=1;},Err(e)if e.is::<SourceChanged>()=>progress.deferred+=1,Err(e)=>return Err(e),
            }
        }
        self.stopped()?;let ids=plan.sources.keys().copied().collect::<Vec<_>>();let current=match self.current_sources(&ids,&plan.sources){Ok(s)=>s,Err(e)if e.is::<SourceChanged>()=>{progress.deferred+=1;return Ok(progress)},Err(e)=>return Err(e)};
        if let Err(e)=self.mark(&current,now,&[],&[],&sweeps(&done)){if e.is::<SourceChanged>(){progress.deferred+=1}else{return Err(e)}}Ok(progress)
    }
}
// Exactly one production selection query implementation; test fixtures call
// it under the same original-owner RR/budget instead of reproducing its SQL.
fn candidate_ids(r:&mut Read<'_,'_>,now:i64)->Result<(Vec<i64>,Vec<i64>,Option<i64>)> {
            let recent=r.query("select id from releases where not complete and size>=$1 and posted_clock>=$2 and posted_clock<$3 and not exists(select 1 from repair_tried t where t.release_id=releases.id) order by id desc limit $4",&[&MIN_ROW_BYTES,&(now-DAYS*86400),&(now-SETTLED_SECS),&CANDIDATES])?.into_iter().map(|r|r.try_get::<_,i64>(0)).collect::<std::result::Result<Vec<_>,_>>()?;
            let before=r.query("select before from repair_older where shard=0",&[])?.first().map(|r|r.try_get::<_,Option<i64>>(0)).transpose()?.flatten().unwrap_or(i64::MAX);
            let low=r.query("select min(id) from(select id from releases where id<$1 order by id desc limit $2) scanned",&[&before,&OLDER_SCAN])?[0].try_get::<_,Option<i64>>(0)?;
            let older=if let Some(low)=low {r.query("select id from releases where id>=$1 and id<$2 and not complete and size>=$3 and posted_clock<$4 and not exists(select 1 from repair_tried t where t.release_id=releases.id) order by id desc limit $5",&[&low,&before,&MIN_ROW_BYTES,&(now-DAYS*86400),&OLDER_CANDIDATES])?.into_iter().map(|r|r.try_get::<_,i64>(0)).collect::<std::result::Result<Vec<_>,_>>()?}else{Vec::new()};
            let end=if older.len()==OLDER_CANDIDATES as usize {older.last().copied()}else{low};Ok((recent,older,end))
}
#[cfg(test)]
pub(super) fn selected_older_for_test(owner:&Arc<Owner>,now:i64)->Result<(Vec<i64>,Option<i64>)> {
    test_snapshot(owner,|r|{let (_,older,end)=candidate_ids(r,now)?;Ok((older,end))})
}
// Closed fixture reads of the actual native resolver. No generic callback or
// production service capability is exposed, and no live singleton is needed
// merely to inspect PG facts in an actual canonical owner's RR snapshot.
#[cfg(test)]
fn test_snapshot<T>(owner:&Arc<Owner>,work:impl FnOnce(&mut Read<'_,'_>)->Result<T>)->Result<T> {
    ensure!(owner.epoch().is_some(),"native post fixture requires an actual Runtime owner");
    let mut budget=Service::budget();owner.with(|client| {
        let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
        let value=work(&mut Read {tx:&mut tx,budget:&mut budget,stop:&owner.stop})?;tx.commit()?;Ok(value)
    })
}
#[cfg(test)]
pub(crate) fn test_whole_post(owner:&Arc<Owner>,id:i64)->Result<Option<crate::release_info::SplitPost>> {test_snapshot(owner,|r|posts::whole_post(r,id))}
#[cfg(test)]
pub(crate) fn test_linked_post(owner:&Arc<Owner>,id:i64)->Result<Option<crate::release_info::SplitPost>> {test_snapshot(owner,|r|{let Some(head)=r.head(id)?else{return Ok(None)};posts::linked(r,&head)})}
#[cfg(test)]
pub(crate) fn test_permille(owner:&Arc<Owner>,ids:&[i64])->Result<i64> {test_snapshot(owner,|r|Ok(r.union(ids)?.permille()))}
#[cfg(test)]
pub(crate) fn test_whole(owner:&Arc<Owner>,ids:&[i64])->Result<bool> {test_snapshot(owner,|r|Ok(r.union(ids)?.complete()))}
#[cfg(all(test,unix))]
#[path="native_tests.rs"]
mod tests;
