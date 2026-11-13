//! Finite native Spotnet service ticks. The Stage owner retains two standard
//! thread lanes; all per-pass children join before a tick returns. Network
//! work uses the supplied pool and runtime, outside the owner mutex.
use anyhow::{Result, anyhow, ensure};
use std::{future::Future, sync::{Arc,atomic::{AtomicBool,Ordering}}, time::{Duration,Instant}};
use crate::{config::SpotsConfig,indexer::Db,nntp::{Pool,says_absent},pg::{indexer_runtime::Owner,sql::Pg}};
use super::{MAX_XML_BYTES,SPOT_GROUP,binary,blacklist,comments,full,header,ingest,release,sign,store};
use super::native_plan::Plan;
use super::native_store::Store;

pub(crate) struct Service {
    storage:Store,db:Db,config:SpotsConfig,pool:Arc<Pool>,handle:tokio::runtime::Handle,
}
#[derive(Debug,Default)]
pub(crate) struct CommentPass {
    pub(crate) comments:usize,pub(crate) reports:usize,pub(crate) read:usize,
    pub(crate) gone:usize,pub(crate) retry:usize,pub(crate) more:bool,
}
enum Fetched { Pending, Settled(store::Settled), Skip(Option<String>) }

// Only actual NNTP operations mint this context. Header/NXV storage failures
// never carry it; text, SQL and other invariant errors cannot impersonate it.
#[derive(Debug)]
struct ProviderFailure;
impl std::fmt::Display for ProviderFailure {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("native Spotnet provider failure")}
}
fn provider_error(error:crate::nntp::NntpError)->anyhow::Error {
    match error {
        crate::nntp::NntpError::HeaderStore(_)=>error.into(),
        crate::nntp::NntpError::Io(_)|crate::nntp::NntpError::Reply {..}
        |crate::nntp::NntpError::Protocol(_)|crate::nntp::NntpError::Decompress(_)=>anyhow::Error::new(error).context(ProviderFailure),
    }
}
fn is_provider_failure(error:&anyhow::Error)->bool {error.downcast_ref::<ProviderFailure>().is_some()}

// All scoped children finish before this reducer chooses the pass error.
// A retry can never displace an independent storage/invariant failure, nor
// can a queued/session cancellation hide an independently fatal child.
pub(crate) fn prefer_failure(current:&mut Option<anyhow::Error>,incoming:anyhow::Error) {
    let replace=current.as_ref().is_none_or(|prior| {
        match (is_provider_failure(prior),is_provider_failure(&incoming)) {
            (true,false)=>true,
            (false,true)|(true,true)=>false,
            (false,false)=>(crate::pg::outage::is_attempt_ended(prior) && !crate::pg::outage::is_attempt_ended(&incoming))
                || (crate::pg::outage::classify(prior)==crate::pg::outage::Failure::Unavailable
                    && crate::pg::outage::classify(&incoming)==crate::pg::outage::Failure::Fatal),
        }
    });
    if replace {*current=Some(incoming);}
}

/// A typed provider failure is always a retry outcome, even if stop or a PG
/// failure arrived after a caller's earlier live check. No owner state is
/// cleared and no storage error is converted. The worker loop/backoff decides
/// whether a still-live attempt polls again; an ended one simply drains.
pub(crate) fn provider_pass<T>(result:Result<T>)->Result<Option<T>> {
    match result {
        Ok(value)=>Ok(Some(value)),
        Err(error) if is_provider_failure(&error)=>Ok(None),
        Err(error)=>Err(error),
    }
}
impl Service {
    pub(crate) fn new(owner:&Arc<Owner>,db:Db,config:SpotsConfig,pool:Arc<Pool>,handle:tokio::runtime::Handle)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native Spotnet starts on a standard thread");
        // These are the same configured/clamped shares as legacy Spotnet.
        ensure!((1..=64).contains(&config.concurrency) && (1..=10_000).contains(&config.per_pass)
            && (1..=1440).contains(&config.poll_minutes),"native Spotnet configuration was not clamped");
        db.check_native_owner(owner)?;
        Ok(Self {storage:Store::new(owner)?,db,config,pool,handle})
    }
    pub(crate) fn poll(&self)->Duration {Duration::from_secs(u64::from(self.config.poll_minutes)*60)}
    pub(crate) fn comments_enabled(&self)->bool {self.config.enabled && self.config.comments}
    fn stopped(&self)->bool {self.storage.owner.stop.load(Ordering::Acquire)}
    fn network<T>(&self,future:impl Future<Output=crate::nntp::Result<T>>)->Result<T> {
        // A lost PG session must be observed before launching more NNTP work.
        self.storage.check()?;
        self.handle.block_on(crate::nntp::unless_stopped(&self.storage.owner.stop,future))
            .ok_or_else(||anyhow::Error::new(crate::pg::outage::AttemptEnded))?.map_err(provider_error)
    }
    #[cfg(test)]
    pub(crate) fn network_for_test<T>(&self,future:impl Future<Output=crate::nntp::Result<T>>)->Result<T> {self.network(future)}
    fn binding(&self,server:usize)->Result<String> {Ok(hex::encode(self.pool.provider_binding_on(server)?))}

    pub(crate) fn spots_tick(&self,now:i64)->Result<ingest::Pass> {
        let mut done=ingest::Pass::default();if !self.config.enabled {return Ok(done)}
        self.storage.check()?;
        // An unavailable NNTP account is a later poll, not a PG outage or an
        // empty successful capture. Already committed stretches stay committed.
        match self.read_overview(now) {
            Ok((read,noted))=>{done.read=read;done.noted=noted;},
            Err(error) if is_provider_failure(&error) && self.storage.owner.failure().is_none() && !self.stopped()=>eprintln!("native spots overview: {error:#}"),
            Err(error)=>return Err(error),
        }
        self.storage.with(|tx| {
            let db=Pg::new(&mut *tx);store::prune(&db,now)?;
            if !self.config.comments {comments::prune(&db,now)?;}Ok(())
        })?;
        self.refresh_blacklist(now)?;
        self.backfill_xml()?;
        let todo=self.storage.with(|tx|Ok(store::todo(&Pg::new(tx),self.config.per_pass)?))?;
        let shared=Arc::new(ingest::Shared::new(ingest::MAX_PASS_POSTED));
        // Bounded waves use no detached blocking tasks and no Tokio PG calls.
        // Scope always joins every launched child, including on failure/stop.
        for wave in todo.chunks(self.config.concurrency) {
            if self.stopped() || shared.budget.load(Ordering::Relaxed)<ingest::MAX_NZB_ARTICLE+ingest::CAP_OVERSHOOT {break}
            let results=std::thread::scope(|scope| {
                let mut handles=Vec::with_capacity(wave.len());
                for id in wave {
                    let shared=shared.clone();handles.push(scope.spawn(move||self.fetch_and_settle(id,&shared)));
                }
                handles.into_iter().map(|handle|handle.join().unwrap_or_else(|_| {
                    self.storage.owner.fail(crate::pg::outage::Failure::Fatal);Err(anyhow!("native Spotnet child panicked"))
                })).collect::<Vec<_>>()
            });
            let mut failure=None;
            for result in results {
                match result {
                    Ok(None)=>done.pending+=1,
                    Ok(Some(Some(store::SAVED)))=>done.saved+=1,
                    Ok(Some(Some(_)))=>done.skipped+=1,
                    Ok(Some(None))=>done.retry+=1,
                    Err(error)=>{prefer_failure(&mut failure,error);},
                }
            }
            if let Some(error)=failure {return Err(error)}
        }
        Ok(done)
    }
    fn fetch_and_settle(&self,id:&str,shared:&Arc<ingest::Shared>)->Result<Option<Option<i64>>> {
        // Claims outlive the transaction, so a concurrent repost can only
        // observe the committed result or wait for the next pass.
        let mut claims=Vec::new();
        let fetched=match self.fetch(id,shared,&mut claims) {
            Ok(value)=>value,
            Err(error) if is_provider_failure(&error) && self.storage.owner.failure().is_none() && !self.stopped()=>{
                eprintln!("native spots {id}: {error:#}");
                self.storage.settle(id,None,chrono::Utc::now().timestamp())?;return Ok(Some(None))
            }
            Err(error)=>return Err(error),
        };
        match fetched {
            Fetched::Pending=>Ok(None),
            Fetched::Settled(settled)=>Ok(Some(Some(settled.state))), // already atomic with publication
            Fetched::Skip(nzb_ref)=> {
                let outcome=store::Settled {state:store::SKIPPED,nzb_ref,..Default::default()};
                self.storage.settle(id,Some(&outcome),chrono::Utc::now().timestamp())?;Ok(Some(Some(store::SKIPPED)))
            }
        }
    }
    fn fetch(&self,id:&str,shared:&Arc<ingest::Shared>,claims:&mut Vec<ingest::Claim>)->Result<Fetched> {
        if self.stopped() {return Ok(Fetched::Pending)}
        let head=match self.network(self.pool.fetch_head(&format!("<{id}>"),MAX_XML_BYTES*4)) {
            Ok(Some(head))=>head,Ok(None)=>return Ok(Fetched::Skip(None)),
            Err(error) if absent(&error)=>return Ok(Fetched::Skip(None)),Err(error)=>return Err(error),
        };
        let verdict=match (header::header_value(&head,"Subject"),header::header_value(&head,"From")) {
            (Some(subject),Some(from))=>header::parse(&subject,&from)
                .map(|h|sign::verify(&h.title,&from,id)).unwrap_or_default(),
            _=>sign::Verdict::default(),
        };
        if !verdict.verified {return Ok(Fetched::Skip(None))}
        let Some(xml)=full::xml_from_headers(&head) else {return Ok(Fetched::Skip(None))};
        let Some(spot)=full::parse(&xml) else {return Ok(Fetched::Skip(None))};
        let Some(kind)=spot.kind() else {return Ok(Fetched::Skip(None))};
        if spot.nzb.is_empty() || !ingest::nzb_ids_usable(&spot.nzb) {return Ok(Fetched::Skip(None))}
        let row=|key:String|store::SpotRow {message_id:id.to_owned(),kind,spot:spot.clone(),spotter_id:verdict.spotter_id.clone(),nzb_key:key,xml:xml.clone()};
        let nzb_ref=release::ids_key(&spot.nzb);
        let Some(claim)=shared.claim(&format!("ref:{nzb_ref}")) else {return Ok(Fetched::Pending)};claims.push(claim);
        match self.storage.with(|tx|Ok(store::known_ref(&Pg::new(tx),&nzb_ref)?))? {
            Some((store::SAVED,Some(release_id)))=>return Ok(Fetched::Settled(self.storage.copy_of(release_id,&row(String::new()),&nzb_ref,chrono::Utc::now().timestamp())?)),
            Some(_)=>return Ok(Fetched::Skip(Some(nzb_ref))),None=>{},
        }
        let mut bodies=Vec::<Vec<u8>>::with_capacity(spot.nzb.len());
        for segment in &spot.nzb {
            let left=ingest::MAX_NZB_POSTED.saturating_sub(bodies.iter().map(Vec::len).sum::<usize>());
            let cap=ingest::MAX_NZB_ARTICLE.min(left);let most=cap+ingest::CAP_OVERSHOOT;
            if self.stopped() || !shared.reserve(most) {return Ok(Fetched::Pending)}
            match self.network(self.pool.fetch_body_capped(&format!("<{segment}>"),cap)) {
                Ok(Some(body))=>{shared.give_back(most.saturating_sub(body.len()));bodies.push(body);},
                Ok(None)=>return Ok(Fetched::Skip(Some(nzb_ref))),
                Err(error)=>{shared.give_back(most);if absent(&error) {return Ok(Fetched::Skip(Some(nzb_ref)))}return Err(error)},
            }
        }
        // Original Spotnet decode/pass ownership remains separate from the
        // original Native writer admission. No full expanded Article vector.
        let _decode=self.handle.block_on(crate::nntp::unless_stopped(&self.storage.owner.stop,shared.decoding.acquire()))
            .ok_or_else(||anyhow::Error::new(crate::pg::outage::AttemptEnded))??;
        let parts=bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let Some(nzb)=binary::nzb(&parts) else {return Ok(Fetched::Skip(Some(nzb_ref)))};
        drop(parts);drop(bodies);
        let parsed=crate::archive::nzb::parse_keeping(&nzb,release::MAX_SEGMENTS);drop(nzb);
        let Some(mut plan)=Plan::new(&spot,&parsed)? else {return Ok(Fetched::Skip(Some(nzb_ref)))};
        let Some(claim)=shared.claim(plan.key()) else {return Ok(Fetched::Pending)};claims.push(claim);
        self.storage.check()?;
        // Resource-only token from the SAME Db/Native, not a replacement
        // semaphore, session, write authority or independent memory budget.
        let permit=self.handle.block_on(crate::nntp::unless_stopped(&self.storage.owner.stop,
            self.db.reserve_native_for_owner(&self.storage.owner)))
            .ok_or_else(||anyhow::Error::new(crate::pg::outage::AttemptEnded))??;
        let row=row(plan.key().to_owned());
        Ok(Fetched::Settled(self.storage.publish(&mut plan,&row,&nzb_ref,&permit,chrono::Utc::now().timestamp())?))
    }
    fn read_overview(&self,now:i64)->Result<(u64,usize)> {
        let prefer=self.pool.indexing_tier().first().copied().unwrap_or(0);
        let (server,(_,low,high,_))=self.network(self.pool.select_group_on(prefer,SPOT_GROUP))?;
        ensure!(high<i64::MAX as u64,"Spotnet cursor exceeds PostgreSQL integer representation");
        let binding=self.binding(server)?;
        let cursor=self.storage.with(|tx|Ok(store::cursor(&Pg::new(tx),&binding)?))?;
        let mut from=ingest::start_at(cursor,low,high,self.config.first_articles);let(mut read,mut noted)=(0,0);
        while from<=high && !self.stopped() {
            let to=high.min(from.saturating_add(ingest::XOVER_STEP-1));
            let rows=self.overview(server,SPOT_GROUP,from,to)?;read+=rows.len() as u64;
            let removals=rows.iter().filter_map(|r|ingest::removal(r,now)).collect::<Vec<_>>();
            let wanted=rows.iter().filter_map(|r| {
                let h=header::parse(&r.subject,&r.from).filter(header::SpotHeader::wanted)?;
                let verdict=sign::verify(&h.title,&r.from,&r.message_id);
                verdict.verified.then(||(r.message_id.trim_matches(['<','>']).to_owned(),r.number,verdict.spotter_id))
            }).collect::<Vec<_>>();
            noted+=self.storage.with(|tx| {
                let db=Pg::new(tx);store::note_disposals(&db,&removals)?;
                Ok(store::note_read(&db,&binding,to,&wanted,now)?)
            })?;
            let Some(next)=to.checked_add(1) else {break};from=next;
        }Ok((read,noted))
    }
    fn overview(&self,server:usize,group:&str,from:u64,to:u64)->Result<Vec<crate::nntp::Overview>> {
        match self.network(self.pool.xover_on(server,group,from,to)) {
            Ok(rows)=>Ok(rows),
            Err(error) if error.downcast_ref::<crate::nntp::NntpError>().is_some_and(|e|e.is_empty_range())=>Ok(Vec::new()),
            Err(error)=>Err(error),
        }
    }
    fn refresh_blacklist(&self,now:i64)->Result<()> {
        if self.config.blacklist_url.is_empty() {self.storage.with(|tx|Ok(blacklist::clear(&Pg::new(tx))?))?;return Ok(())}
        if !self.storage.with(|tx|Ok(blacklist::due(&Pg::new(tx),now)?))? {return Ok(())}
        self.storage.check()?;
        // Existing blocking, 60s/4MiB capped fetch stays in the owned lane;
        // stop/outage cannot detach it or start another fetch during drain.
        let fetched=blacklist::fetch(&self.config.blacklist_url).map(|xml|blacklist::parse(&xml));
        self.storage.check()?;
        match fetched {
            Ok(spotters) if !spotters.is_empty()=>{self.storage.with(|tx|blacklist::replace(&Pg::new(tx),&spotters,now))?;},
            result=> {
                if let Err(error)=result {eprintln!("native spots blacklist: {error:#}");}
                self.storage.with(|tx|Ok(blacklist::tried(&Pg::new(tx),now)?))?;
            }
        }Ok(())
    }
    fn backfill_xml(&self)->Result<()> {
        for (id,message_id) in self.storage.missing_xml(ingest::XML_BACKFILL)? {
            if self.stopped() {break}
            let xml=match self.network(self.pool.fetch_head(&format!("<{message_id}>"),MAX_XML_BYTES*4)) {
                Ok(Some(head))=>full::xml_from_headers(&head).filter(|xml|full::parse(xml).is_some()).unwrap_or_default(),
                Ok(None)=>String::new(),Err(error) if absent(&error)=>String::new(),
                Err(error) if is_provider_failure(&error) && self.storage.owner.failure().is_none() && !self.stopped()=>{eprintln!("native spots XML: {error:#}");break},
                Err(error)=>return Err(error),
            };self.storage.set_xml(id,&message_id,&xml)?;
        }Ok(())
    }

    pub(crate) fn comments_tick(&self,now:i64)->Result<CommentPass> {
        let mut done=CommentPass::default();if !self.comments_enabled() {return Ok(done)}
        self.storage.with(|tx|Ok(comments::prune(&Pg::new(tx),now)?))?;
        for (group,reports) in [(comments::COMMENT_GROUP,false),(comments::REPORT_GROUP,true)] {
            match self.read_comments(group,reports,now) {
                Ok(n) if reports=>done.reports=n,Ok(n)=>done.comments=n,
                Err(error) if is_provider_failure(&error) && self.storage.owner.failure().is_none() && !self.stopped()=>eprintln!("native spots {group}: {error:#}"),
                Err(error)=>return Err(error),
            }
        }
        let todo=self.storage.with(|tx|Ok(comments::todo(&Pg::new(tx),self.config.comments_per_pass)?))?;
        let failed=AtomicBool::new(false);let started=Instant::now();
        let mut outcomes=Vec::with_capacity(ingest::SETTLE_BATCH+self.config.concurrency);
        for wave in todo.chunks((self.config.concurrency/4).max(1)) {
            if self.stopped() || failed.load(Ordering::Acquire) {break}
            if started.elapsed()>=self.poll() {done.more=true;break}
            let results=std::thread::scope(|scope| {
                let mut handles=Vec::with_capacity(wave.len());
                for id in wave {
                    let failed=&failed;handles.push(scope.spawn(move|| {
                        let outcome=match self.network(self.pool.fetch_body_capped(&format!("<{id}>"),comments::MAX_BODY)) {
                            Ok(Some(body))=>comments::Outcome::Read(comments::body_text(&body)),Ok(None)=>comments::Outcome::Gone,
                            Err(error) if absent(&error)=>comments::Outcome::Gone,
                            Err(error) if is_provider_failure(&error) && self.storage.owner.failure().is_none() && !self.stopped()=> {
                                eprintln!("native spots comment {id}: {error:#}");failed.store(true,Ordering::Release);comments::Outcome::Failed
                            }
                            Err(error)=>return Err(error),
                        };Ok((id.clone(),outcome))
                    }));
                }
                handles.into_iter().map(|h|h.join().unwrap_or_else(|_| {
                    self.storage.owner.fail(crate::pg::outage::Failure::Fatal);Err(anyhow!("native Spotnet comment child panicked"))
                })).collect::<Vec<_>>()
            });
            let mut failure=None;
            for result in results {match result {Ok(outcome)=>outcomes.push(outcome),Err(error)=>{prefer_failure(&mut failure,error)}}}
            if let Some(error)=failure {return Err(error)}
            if outcomes.len()>=ingest::SETTLE_BATCH {self.commit_comments(&mut outcomes,&mut done)?;}
        }
        self.commit_comments(&mut outcomes,&mut done)?;Ok(done)
    }
    fn commit_comments(&self,outcomes:&mut Vec<(String,comments::Outcome)>,done:&mut CommentPass)->Result<()> {
        if outcomes.is_empty(){return Ok(())}self.storage.settle_comments(outcomes)?;
        for (_,outcome) in outcomes.drain(..) {match outcome {
            comments::Outcome::Read(_)=>done.read+=1,comments::Outcome::Gone=>done.gone+=1,comments::Outcome::Failed=>done.retry+=1,
        }}Ok(())
    }
    fn read_comments(&self,group:&'static str,reports:bool,now:i64)->Result<usize> {
        let prefer=self.pool.indexing_tier().first().copied().unwrap_or(0);
        let (server,(_,low,high,_))=self.network(self.pool.select_group_on(prefer,group))?;
        ensure!(high<i64::MAX as u64,"Spotnet cursor exceeds PostgreSQL integer representation");
        let binding=self.binding(server)?;
        let cursor=self.storage.with(|tx|Ok(comments::cursor(&Pg::new(tx),group,&binding)?))?;
        let mut from=match cursor {
            Some(c) if c<=high=>c.saturating_add(1).max(low),
            _=>self.network(self.pool.article_at(server,group,low.max(1),high,now-i64::from(self.config.comment_days)*86_400))?,
        };
        if from>high {return self.storage.with(|tx|Ok(comments::note_read(&Pg::new(tx),group,&binding,high,&[],reports,now)?))}
        let mut added=0;
        while from<=high && !self.stopped() {
            let to=high.min(from.saturating_add(ingest::XOVER_STEP-1));
            let found=self.overview(server,group,from,to)?.iter().filter_map(|r| {
                let (spot,rating)=if reports {(comments::report_ref(&r.subject,&r.references)?,0)} else {comments::comment_ref(&r.message_id)?};
                Some(comments::Noted {message_id:r.message_id.trim().trim_matches(['<','>']).to_owned(),spot,
                    at:crate::dates::posted_timestamp(&r.date).unwrap_or(now),rating,poster:comments::poster_name(&r.from),spotter_id:header::spotter_id_of_from(&r.from)})
            }).collect::<Vec<_>>();
            added+=self.storage.with(|tx|Ok(comments::note_read(&Pg::new(tx),group,&binding,to,&found,reports,now)?))?;
            let Some(next)=to.checked_add(1) else {break};from=next;
        }Ok(added)
    }
}
fn absent(error:&anyhow::Error)->bool {error.downcast_ref::<crate::nntp::NntpError>().is_some_and(says_absent)}
