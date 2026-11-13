//! Attempt-owned inferred repair. Source proofs are minted only by actual PG
//! snapshots; a derived MID is never itself an accepted article/header.
use anyhow::{Context,Result,ensure};
use postgres::{GenericClient,IsolationLevel};
use std::{collections::{HashMap,HashSet},sync::{Arc,atomic::Ordering},time::{Duration,Instant}};
use crate::{pg::indexer_runtime::Owner,nxv_store::store::Store};
use super::{Wanted,Outcome,template::Proof,head_journal::{Journal,Record}};
const PLAN_BYTES:usize=16*1024*1024;
#[derive(Clone)]
struct Source {id:i64,revision:i64,group:String,collection:bool,identity:[u8;32],epoch:[u8;32],catalog:[u8;32]}
struct Request {source:Arc<Source>,wanted:Wanted,prior:Option<(i64,i64)>}
struct PostMark {source:Arc<Source>,next:i64}
struct Plan {requests:Vec<Request>,marks:Vec<PostMark>,deferred:usize}
#[derive(Debug)]
struct SourceChanged;
impl std::fmt::Display for SourceChanged {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("inferred source changed during bounded planning")}}
impl std::error::Error for SourceChanged {}

pub(crate) struct Service {
    owner:Arc<Owner>,db:crate::indexer::Db,pool:Arc<crate::nntp::Pool>,store:Arc<Store>,
    epoch:[u8;32],catalog:[u8;32],journal:Journal,next:Instant,
}
impl Service {
    pub(crate) fn new(owner:&Arc<Owner>,db:&crate::indexer::Db,pool:Arc<crate::nntp::Pool>,store:Arc<Store>,headers:&crate::config::HeaderStoreConfig)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native inferred repair requires a standard thread");
        db.check_native_owner(owner)?;
        let epoch=owner.epoch().context("native inferred owner epoch missing")?;
        let expected=epoch.catalog().header_config(headers)?;
        ensure!(store.root()==expected.root(),"native inferred header store belongs to another catalog");
        owner.with(|_|Ok(()))?;let journal=Journal::open(store.root(),*epoch.catalog().nonce())?;
        Ok(Self {owner:owner.clone(),db:db.clone(),pool,store,epoch:*epoch.id(),catalog:*epoch.catalog().nonce(),journal,next:Instant::now()+super::EVERY})
    }
    #[cfg(test)]
    pub(crate) fn start_without_delay_for_test(&mut self) {self.next=Instant::now();}
    fn binding(&self,source:&Source)->Result<()> {
        ensure!(source.epoch==self.epoch&&source.catalog==self.catalog,"inferred source belongs to another original attempt");
        ensure!(self.owner.epoch().is_some_and(|epoch|epoch.id()==&self.epoch&&epoch.catalog().nonce()==&self.catalog),"inferred owner binding changed");Ok(())
    }
    fn source_matches(tx:&mut impl GenericClient,source:&Source,revision:i64)->Result<bool> {
        Ok(tx.query_opt("select native_work_revision=$2 and group_name=$3 and is_collection=$4 and identity_key=$5 from releases where id=$1",
            &[&source.id,&revision,&source.group,&source.collection,&&source.identity[..]])?.map(|row|row.try_get::<_,bool>(0)).transpose()?.unwrap_or(false))
    }
    fn source_current(&self,source:&Source,revision:i64)->Result<bool> {
        self.binding(source)?;self.owner.with(|client|Self::source_matches(client,source,revision))
    }
    fn stopped(&self)->Result<()> {
        if self.owner.stop.load(Ordering::Acquire){return Err(crate::pg::outage::AttemptEnded.into())}
        self.journal.check_root(self.store.root())?;ensure!(self.store.failure().is_none(),"native inferred header store is unavailable");Ok(())
    }
    fn read<T>(&self,work:impl FnOnce(&mut postgres::Transaction<'_>)->Result<T>)->Result<T> {
        self.stopped()?;self.owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
            let value=work(&mut tx)?;tx.commit()?;Ok(value)
        })
    }
    fn read_source<T>(&self,source:&Source,work:impl FnOnce(&mut postgres::Transaction<'_>)->Result<T>)->Result<T> {
        self.binding(source)?;
        let value=self.read(|tx| {
            if !Self::source_matches(tx,source,source.revision)? {return Ok(None)}
            work(tx).map(Some)
        })?;
        // Supported concurrent growth is a normal stale selection, never a
        // storage failure passed into Owner's Fatal/Unavailable classifier.
        value.ok_or_else(||anyhow::Error::new(SourceChanged))
    }
    fn plan(&self,now:i64)->Result<Plan> {self.plan_mode(now,None)}
    fn plan_mode(&self,now:i64,recovery:Option<&Record>)->Result<Plan> {
            // Only a bounded page occupies the original session at once.
            // Fixed metadata and monotonic source revisions let the private
            // proof span pages without pinning a long owner transaction.
            let candidates=self.read(|tx|Ok(tx.query("select r.id,r.native_work_revision,
                case when octet_length(r.group_name)<=4096 then r.group_name end,octet_length(r.group_name),r.is_collection,
                substring(r.identity_key from 1 for 33),octet_length(r.identity_key),r.size,
                trunc(coalesce((select sum(case when f.expected>0 then least(f.distinct_parts,f.expected) else 1 end)*1000/
                    nullif(sum(case when f.expected>0 then f.expected else 1 end),0) from files f where f.release_id=r.id),0))::bigint
                from releases r where ($5::bigint is not null and r.id=$5) or ($5 is null and not r.complete and r.size>=$1 and r.posted_clock>=$2 and r.posted_clock<$3
                  and not exists(select 1 from inferred_asked a where a.release_id=r.id and a.next_at>$4))
                order by r.posted_clock desc,r.id desc limit 500",&[&super::MIN_BYTES,&(now-super::DAYS*86400),&(now-super::SETTLED_SECS),&now,&recovery.map(|record|record.release)])?))?;
            let mut sources:Vec<(i64,i64,Arc<Source>)>=Vec::new();let mut marks:Vec<PostMark>=Vec::new();let mut deferred=0;
            for row in candidates {
                ensure!(row.try_get::<_,i32>(3)?<=4096,"inferred group exceeds source admission");
                let key:Vec<u8>=row.try_get(5)?;ensure!(row.try_get::<_,i32>(6)?==32&&key.len()==32,"inferred identity extent invalid");
                let source=Arc::new(Source {id:row.try_get(0)?,revision:row.try_get(1)?,group:row.try_get(2)?,collection:row.try_get(4)?,identity:key.try_into().map_err(|_|anyhow::anyhow!("inferred identity changed"))?,epoch:self.epoch,catalog:self.catalog});
                let permille:i64=row.try_get(8)?;
                if crate::filters::group_blocked(&source.group)||!(super::MIN_PERMILLE..1000).contains(&permille) {marks.push(PostMark {source,next:now+super::NOTHING_AGAIN});}
                else {sources.push((permille,row.try_get::<_,i64>(7)?,source));}
            }
            sources.sort_by_key(|(permille,size,_)|(std::cmp::Reverse(*permille),std::cmp::Reverse(*size)));
            // Reserve fixed worst-case metadata/decoded-row scratch first:
            // 16 admitted MID/name rows plus one page of retry keys and the
            // 500 scalar/source candidates. Retained requests use the balance.
            let mut requests:Vec<Request>=Vec::new();let mut seen:HashSet<String>=HashSet::new();let mut charged=10*1024*1024usize;
            for (_,_,source) in sources {
                self.stopped()?;let before=requests.len();let before_charge=charged;let before_deferred=deferred;
                let planned=(||->Result<bool> {
                let mut potential=false;let mut first_due:Option<i64>=None;let mut after_file:Option<i64>=None;
                loop {
                    let files=self.read_source(&source,|tx|Ok(tx.query("select id,expected,case when octet_length(original_filename)<=65536 then original_filename end,octet_length(original_filename)
                        from files where release_id=$1 and($2::bigint is null or id>$2) order by id limit 16",&[&source.id,&after_file])?))?;
                    if files.is_empty(){break;}
                    for file in files {
                        self.stopped()?;let id:i64=file.try_get(0)?;after_file=Some(id);
                        let expected:Option<i64>=file.try_get(1)?;let Some(mut proof)=expected.and_then(Proof::new) else {continue;};
                        let name_bytes=file.try_get::<_,Option<i32>>(3)?.context("inferred file lacks its original filename")?;
                        if name_bytes>65536 {deferred+=1;continue;}
                        let filename=String::from_utf8(file.try_get(2)?)?;if filename.is_empty(){continue;}
                        let mut after:Option<Vec<u8>>=None;
                        loop {
                            self.stopped()?;
                            let rows=self.read_source(&source,|tx|Ok(tx.query("select substring(message_key from 1 for 33),octet_length(message_key),part,
                                case when octet_length(message_id)<=65500 then message_id end,octet_length(message_id)
                                from segments where file_id=$1 and($2::bytea is null or message_key>$2) order by message_key limit 16",&[&id,&after])?))?;
                            if rows.is_empty(){break;}
                            for row in rows {
                                let key:Vec<u8>=row.try_get(0)?;ensure!(row.try_get::<_,i32>(1)?==32&&key.len()==32,"inferred segment key extent invalid");after=Some(key);
                                if row.try_get::<_,i32>(4)?>65500 {proof.push(None,&[]);continue;}
                                let mid:Vec<u8>=row.try_get(3)?;proof.push(row.try_get(2)?,&mid);
                            }
                        }
                        // Same mode/tie rule as legacy common_size: prefer
                        // non-last parts, falling back to all only if absent.
                        let size=self.read_source(&source,|tx|Ok(tx.query_opt("select bytes from segments where file_id=$1 and part is distinct from $2
                            group by bytes order by count(*) desc,bytes desc limit 1",&[&id,&expected])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?;
                        let size=match size {Some(size)=>Some(size),None=>self.read_source(&source,|tx|Ok(tx.query_opt("select bytes from segments where file_id=$1
                            group by bytes order by count(*) desc,bytes desc limit 1",&[&id])?.map(|row|row.try_get::<_,i64>(0)).transpose()?))?};
                        let mut missing=proof.missing();
                        loop {
                            let parts=missing.by_ref().take(16).collect::<Vec<_>>();if parts.is_empty(){break;}potential=true;
                            let ids=parts.iter().map(|part|proof.message_id(*part).trim_matches(['<','>']).to_string()).collect::<Vec<_>>();
                            let previous=self.read_source(&source,|tx|Ok(tx.query("select message_id,next_at,tries from inferred_ids where message_id=any($1)",&[&ids])?.into_iter()
                                .map(|row|Ok((row.try_get::<_,String>(0)?,(row.try_get::<_,i64>(1)?,row.try_get::<_,i64>(2)?)))).collect::<Result<HashMap<_,_>>>()?))?;
                            for (part,mid) in parts.into_iter().zip(ids) {
                                if recovery.is_some_and(|record|record.mid!=mid){continue;}
                                let prior=previous.get(&mid).copied();
                                if let Some((at,_))=prior.filter(|(at,_)|recovery.is_none()&&*at>now) {first_due=Some(first_due.map_or(at,|old|old.min(at)));continue;}
                                if seen.contains(&mid){continue;}
                                let charge=mid.len().checked_mul(3).and_then(|n|n.checked_add(filename.len()+2*source.group.len()+1024)).context("inferred plan allocation overflow")?;
                                if requests.len()==super::PER_ROUND||charged.checked_add(charge).is_none_or(|n|n>PLAN_BYTES) {return Ok(false);}
                                charged+=charge;seen.insert(mid.clone());
                                requests.push(Request {source:source.clone(),prior,wanted:Wanted {release_id:source.id,group:source.group.clone(),groups:vec![source.group.clone()].into(),row:crate::search::ArticleRow {
                                    message_id:format!("<{mid}>"),filename:Some(filename.clone()),part:Some(part),total_parts:expected,bytes:size,subject:None,poster:None,posted_date:None,
                                }}});
                            }
                        }
                    }
                }
                let next=if requests.len()>before||deferred>before_deferred {now+super::FAILED_AGAIN}else if potential {first_due.unwrap_or(now+super::FAILED_AGAIN)}else{now+super::NOTHING_AGAIN};
                marks.push(PostMark {source:source.clone(),next});Ok(true)
                })();
                match planned {
                    Ok(true)=>{},Ok(false)=>break,
                    Err(error) if error.downcast_ref::<SourceChanged>().is_some()=>{
                        for request in requests.drain(before..) {seen.remove(&request.wanted.key());}
                        charged=before_charge;deferred=before_deferred;
                    },Err(error)=>return Err(error),
                }
            }
            Ok(Plan {requests,marks,deferred})
    }
}

#[derive(Debug,Default)]
pub(crate) struct Progress {pub requested:usize,pub deferred:usize,pub found:usize,pub absent:usize,pub unusable:usize,pub failed:usize,pub superseded:usize,pub replayed:usize,pub waiting:Option<Duration>}
enum Acceptance {Applied(i64,Outcome),Replayed(i64),Superseded}
impl Service {
    fn previous(tx:&mut impl GenericClient,mid:&str,lock:bool)->Result<Option<(i64,i64)>> {
        let sql=if lock {"select next_at,tries from inferred_ids where message_id=$1 for update"}else{"select next_at,tries from inferred_ids where message_id=$1"};
        tx.query_opt(sql,&[&mid])?.map(|row|Ok((row.try_get(0)?,row.try_get(1)?))).transpose()
    }
    fn mark_posts(&self,plan:&Plan,_now:i64)->Result<()> {
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            let mut marks=plan.marks.iter().collect::<Vec<_>>();marks.sort_by_key(|mark|mark.source.id);
            for mark in marks {
                self.stopped()?;self.binding(&mark.source)?;
                tx.query_opt("select id from releases where id=$1 for update",&[&mark.source.id])?;
                if Self::source_matches(&mut tx,&mark.source,mark.source.revision)? {
                    tx.execute("insert into inferred_asked(release_id,next_at) values($1,$2) on conflict(release_id) do update set next_at=excluded.next_at",&[&mark.source.id,&mark.next])?;
                }
            }
            self.stopped()?;tx.commit()?;Ok(())
        })
    }
    fn prune(&self,now:i64)->Result<()> {
        self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;let stale=now-super::DAYS*86400;
            tx.execute("delete from inferred_ids where message_id in(select message_id from inferred_ids where next_at<$1 order by next_at,message_id limit 256)",&[&stale])?;
            tx.execute("delete from inferred_asked where release_id in(select release_id from inferred_asked where next_at<$1 order by next_at,release_id limit 256)",&[&stale])?;
            self.stopped()?;tx.commit()?;Ok(())
        })
    }
    fn parsed(request:&Request,head:&[u8])->Option<Vec<crate::parser::Release>> {
        let(group,overview)=super::overview_of(head,&request.wanted)?;
        if group!=request.source.group {return None;}
        let articles=crate::xref::hold(&group,crate::nntp::headers_to_articles(vec![overview]));
        let (articles,uploads,_)=crate::filters::gather_per_article(&group,articles);
        let mut releases=crate::collections::group_articles(&group,articles).into_values().collect::<Vec<_>>();releases.extend(uploads);
        if releases.len()!=1 {return None;}
        for release in &mut releases {
            let (collection,key)=crate::pg::release_writer::identity(release);
            if collection!=request.source.collection||key!=request.source.identity||release.articles.len()!=1 {return None;}
            let article=&release.articles[0];
            if article.filename!=request.wanted.row.filename||article.part!=request.wanted.row.part||article.total_parts!=request.wanted.row.total_parts
                ||article.message_id.trim_matches(['<','>'])!=request.wanted.key() {return None;}
            release.group=group.clone();release.poster=article.author.clone();release.date=article.date.clone();release.complete=crate::parser::is_complete(&release.articles);
        }Some(releases)
    }
    fn parsed_admitted(request:&Request,head:&[u8],permit:&crate::pg::ingest::NativeSavePermit)->Result<Option<Vec<crate::parser::Release>>> {
        // One <=64KiB HEAD makes at most one Article/Release. Reserve a
        // conservative allowance for UTF-8 repair, field/name clones and Vec
        // growth BEFORE constructing that charged batch; check actual retained
        // capacities again afterwards. Tiny custom job limits cannot overrun
        // their reservation merely because the final check would reject it.
        let bound=head.len().checked_mul(64).and_then(|n|n.checked_add(65536+8*request.source.group.len())).context("inferred parser admission overflow")?;
        ensure!(head.len()<=65536&&bound<=permit.max_job_bytes(),"inferred HEAD expansion exceeds native job admission");
        let releases=Self::parsed(request,head);
        if let Some(releases)=&releases {permit.check_batch(releases)?;}
        Ok(releases)
    }
    fn accept(&self,request:&Request,revision:i64,outcome:Outcome,releases:Option<&[crate::parser::Release]>,now:i64)->Result<Acceptance> {
        self.binding(&request.source)?;self.owner.with(|client| {
            let mut tx=client.transaction()?;crate::pg::history_gate::native(&mut tx)?;
            // Same order as the ordinary native writer: group before release.
            tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1,7936))",&[&request.source.group])?;
            tx.query_opt("select id from releases where id=$1 for update",&[&request.source.id])?;
            if !Self::source_matches(&mut tx,&request.source,revision)? {tx.commit()?;return Ok(Acceptance::Superseded)}
            self.stopped()?;let mid=request.wanted.key();let previous=Self::previous(&mut tx,&mid,true)?;
            if previous!=request.prior {tx.commit()?;return Ok(Acceptance::Replayed(revision))}
            self.stopped()?;let mut actual=outcome;
            if let Some(releases)=releases {
                crate::pg::release_writer::save_in(&mut tx,releases,now)?;
                // A MID already owned by another file may lose exact segment
                // acceptance. Never label that a repaired part merely because
                // the COPY/writer transaction itself succeeded.
                // The retry key is deliberately bare. overview_of normalizes
                // actual HEADs to bracketed MIDs, copied unchanged by writer.
                let catalog_mid=format!("<{mid}>");
                let present:bool=tx.query_one("select exists(select 1 from segments s join files f on(f.release_id,f.id)=(s.release_id,s.file_id)
                    where s.release_id=$1 and s.message_id=$2 and f.original_filename=$3 and s.part=$4)",
                    &[&request.source.id,&catalog_mid.as_bytes(),&request.wanted.row.filename.as_ref().map(|name|name.as_bytes()),&request.wanted.row.part])?.try_get(0)?;
                if !present {actual=Outcome::Unusable;}
            }
            self.stopped()?;let old_tries=previous.map_or(0,|(_,tries)|tries);ensure!(old_tries>=0,"inferred retry state is invalid");
            let tries=if actual==Outcome::Failed {old_tries}else{old_tries.checked_add(1).context("inferred retry count overflow")?};
            let at=if actual==Outcome::Failed {now+super::FAILED_AGAIN}else{super::backoff(now,tries)};
            tx.execute("insert into inferred_ids(message_id,next_at,tries) values($1,$2,$3) on conflict(message_id) do update set next_at=excluded.next_at,tries=excluded.tries",&[&mid,&at,&tries])?;
            let revision=tx.query_one("select native_work_revision from releases where id=$1",&[&request.source.id])?.try_get(0)?;
            self.stopped()?;tx.commit()?;Ok(Acceptance::Applied(revision,actual))
        })
    }
    fn note(progress:&mut Progress,accepted:Acceptance)->Option<i64> {
        match accepted {
            Acceptance::Applied(revision,outcome)=>{crate::metrics::names::INFERRED_PARTS.inc(&[("outcome",outcome.label())],1);match outcome {Outcome::Found=>progress.found+=1,Outcome::Absent=>progress.absent+=1,Outcome::Unusable=>progress.unusable+=1,Outcome::Failed=>progress.failed+=1};Some(revision)},
            Acceptance::Replayed(revision)=>{progress.replayed+=1;Some(revision)},Acceptance::Superseded=>{progress.superseded+=1;None},
        }
    }
    fn record(request:&Request,revision:i64,reply:&crate::nntp::HeadReply,now:i64)->Record {
        Record {release:request.source.id,revision,identity:request.source.identity,group:request.source.group.clone(),
            server:reply.provider_binding().iter().map(|byte|format!("{byte:02x}")).collect(),mid:request.wanted.key(),head:reply.bytes().to_vec(),prior:request.prior,asked_at:now}
    }
    fn reserve(&self,handle:&tokio::runtime::Handle)->Result<crate::pg::ingest::NativeSavePermit> {
        handle.block_on(crate::nntp::unless_stopped(&self.owner.stop,self.db.reserve_native_for_owner(&self.owner)))
            .ok_or_else(||anyhow::Error::new(crate::pg::outage::AttemptEnded))?
    }
    fn recover(&mut self,handle:&tokio::runtime::Handle,progress:&mut Progress,now:i64)->Result<()> {
        let Some(durable)=self.journal.pending()? else{return Ok(())};self.stopped()?;
        let record=durable.record();
        // The journal retains the bare continuation key, while this query
        // checks the exact normalized catalog representation of the HEAD.
        let catalog_mid=format!("<{}>",record.mid);
        enum Recovery {Retry,Consumed}
        let status=self.owner.with(|client| {
            let mut tx=client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
            let same=tx.query_opt("select group_name=$2 and identity_key=$3,native_work_revision,
                exists(select 1 from segments s where s.release_id=r.id and s.message_id=$4) from releases r where id=$1",
                &[&record.release,&record.group,&&record.identity[..],&catalog_mid.as_bytes()])?;
            let retry=if let Some(row)=same {
                let previous=Self::previous(&mut tx,&record.mid,false)?;
                row.try_get::<_,bool>(0)?&&!row.try_get::<_,bool>(2)?&&row.try_get::<_,i64>(1)?==record.revision&&previous==record.prior
            }else{false};
            tx.commit()?;Ok(if retry {Recovery::Retry}else{Recovery::Consumed})
        })?;
        if matches!(status,Recovery::Consumed) {self.journal.consumed(durable)?;progress.replayed+=1;return Ok(())}
        let mut plan=self.plan_mode(now,Some(record))?;
        let Some(request)=plan.requests.pop() else {self.journal.consumed(durable)?;progress.superseded+=1;return Ok(())};
        if request.prior!=record.prior {self.journal.consumed(durable)?;progress.replayed+=1;return Ok(())}
        let permit=self.reserve(handle)?;
        let Some(releases)=Self::parsed_admitted(&request,&record.head,&permit)? else {self.journal.consumed(durable)?;progress.superseded+=1;return Ok(())};
        permit.check_batch(&releases)?;
        let accepted=self.accept(&request,record.revision,Outcome::Found,Some(&releases),record.asked_at)?;
        Self::note(progress,accepted);self.journal.consumed(durable)?;Ok(())
    }
    pub(crate) fn tick(&mut self,handle:&tokio::runtime::Handle,now:i64)->Result<Progress> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native inferred repair requires a standard thread");self.stopped()?;
        let at=Instant::now();if at<self.next {return Ok(Progress {waiting:Some(self.next-at),..Default::default()})}
        let mut progress=Progress::default();self.recover(handle,&mut progress,now)?;
        // Prune before capturing MID continuations, never invalidate our own
        // observed CAS state between the source snapshot and its acceptance.
        self.prune(now)?;let plan=self.plan(now)?;progress.deferred+=plan.deferred;self.mark_posts(&plan,now)?;
        let mut requests=plan.requests.into_iter();let mut revisions:HashMap<i64,i64>=HashMap::new();let mut superseded:HashSet<i64>=HashSet::new();
        loop {
            self.stopped()?;
            let mut batch=Vec::new();
            while batch.len()<super::AT_ONCE {
                let Some(request)=requests.next() else {break};if superseded.contains(&request.source.id){continue;}
                let revision=*revisions.get(&request.source.id).unwrap_or(&request.source.revision);
                if !self.source_current(&request.source,revision)? {progress.superseded+=1;superseded.insert(request.source.id);continue;}
                batch.push(request);
            }
            if batch.is_empty(){break;}
            let replies=handle.block_on(async {
                let mut set=tokio::task::JoinSet::new();
                for(index,request)in batch.iter().enumerate(){let pool=self.pool.clone();let mid=request.wanted.row.message_id.clone();set.spawn(async move{(index,pool.background_head(&mid).await)});}
                let mut replies=Vec::new();
                let failure=loop {
                    match crate::nntp::unless_stopped(&self.owner.stop,set.join_next()).await {
                        Some(Some(Ok(reply)))=>replies.push(reply),
                        Some(Some(Err(error)))=>break Some(anyhow::Error::new(error).context("native inferred HEAD task ended")),
                        Some(None)=>break None,None=>break Some(anyhow::Error::new(crate::pg::outage::AttemptEnded)),
                    }
                };
                if let Some(error)=failure {
                    set.abort_all();while set.join_next().await.is_some(){}
                    return Err(error);
                }
                self.stopped()?;replies.sort_by_key(|(index,_)|*index);Ok::<_,anyhow::Error>(replies)
            })?;
            ensure!(replies.len()==batch.len(),"native inferred HEAD batch did not finish");
            for(index,reply)in replies {
                self.stopped()?;let request=&batch[index];if superseded.contains(&request.source.id){continue;}
                progress.requested+=1;let revision=*revisions.get(&request.source.id).unwrap_or(&request.source.revision);
                let accepted=match reply {
                    Ok(crate::nntp::HeadAnswer::Found(reply))=>{
                        // Reserve the ORIGINAL shared native writer BEFORE
                        // parser expansion, and retain its admission through
                        // the actual guarded commit/rollback below.
                        let permit=self.reserve(handle)?;
                        if let Some(releases)=Self::parsed_admitted(request,reply.bytes(),&permit)? {
                            permit.check_batch(&releases)?;let record=Self::record(request,revision,&reply,now);let durable=self.journal.append(record)?;
                            let accepted=self.accept(request,revision,Outcome::Found,Some(&releases),now)?;
                            self.journal.consumed(durable)?;accepted
                        }else{self.accept(request,revision,Outcome::Unusable,None,now)?}
                    },
                    Ok(crate::nntp::HeadAnswer::Missing)=>self.accept(request,revision,Outcome::Absent,None,now)?,
                    Ok(crate::nntp::HeadAnswer::TooBig)=>self.accept(request,revision,Outcome::Unusable,None,now)?,
                    Err(_)=>self.accept(request,revision,Outcome::Failed,None,now)?,
                };
                if let Some(revision)=Self::note(&mut progress,accepted){revisions.insert(request.source.id,revision);}else{superseded.insert(request.source.id);}
            }
        }
        // Legacy run rests for EVERY after the actual completed round, not
        // merely after its start when provider reads take longer than that.
        self.next=Instant::now()+super::EVERY;Ok(progress)
    }
}

#[cfg(test)]
pub(crate) mod tests;
