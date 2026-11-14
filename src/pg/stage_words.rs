//! The canonical owner's sealed Tantivy delivery lane. Public web readers do
//! not acknowledge queues or obtain an index writer from this adapter.
use anyhow::{Context,Result,ensure};
use std::{sync::Arc,time::{Duration,Instant}};
use super::{indexer_runtime::Owner,fresh_reader::{ReadView,Budget as ReadBudget}};
use crate::word_index::native::{self,Budget,Changes,Changed,Record,SearchSource,Source,BuildSource,IndexCommitReceipt};

struct WordBatch {epoch:[u8;32],stamp:[u8;32],claims:Vec<Changed>,rows:Vec<Record>,digest:[u8;32]}
pub(crate) struct Delivery {owner:Arc<Owner>,batch:Option<WordBatch>}
impl Delivery {
    pub(crate) fn for_owner(owner:&Arc<Owner>)->Result<Self> {
        ensure!(owner.epoch().is_some(),"word delivery requires canonical owner");
        Ok(Self {owner:owner.clone(),batch:None})
    }
    fn lane(&self)->Result<()> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native word delivery requires a standard owner lane");Ok(())
    }
    fn read_batch(&self,budget:Budget)->Result<WordBatch> {
        self.lane()?;budget.check()?;
        ensure!(budget.rows<=256 && budget.bytes<=64*1024*1024,"native word delivery exceeds owner admission");
        let destination=self.owner.cache_destination().context("word delivery destination unavailable")?;
        self.owner.with(|client| {
            let view=ReadView::open(client,&destination.storage.schema,ReadBudget {rows:budget.rows,bytes:budget.bytes},Instant::now()+Duration::from_secs(10))?;
            let search=super::fresh_words::Search::open(&view)?;
            let mut claims=Vec::new();
            for row in view.tx.borrow_mut().query("select release_id,n from word_queue order by release_id limit $1",&[&(budget.rows as i64)])? {
                claims.push(Changed {id:row.try_get(0)?,version:row.try_get(1)?});
            }
            let ids=claims.iter().map(|claim|claim.id).collect::<Vec<_>>();
            let rows=search.records(&ids,budget)?;
            let identity=super::stage::CatalogIdentity::read_in(&mut *view.tx.borrow_mut())?;
            let stamp=super::fresh_words::catalog_stamp(&identity);
            let digest=native::batch_digest(stamp,&claims,&rows)?;
            Ok(WordBatch {epoch:*self.owner.epoch().context("word delivery owner ended")?.id(),stamp,claims,rows,digest})
        })
    }
}
impl SearchSource for Delivery {
    fn stamp(&mut self)->Result<[u8;32]> {
        self.lane()?;self.owner.with(|_|Ok(super::fresh_words::catalog_stamp(self.owner.epoch().context("word delivery owner ended")?.catalog())))
    }
    fn pending(&mut self)->Result<bool> {
        self.lane()?;self.owner.with(|client|Ok(client.query_one("select exists(select 1 from word_queue)",&[])?.try_get(0)?))
    }
    fn visible_rows(&mut self,ids:&[i64],budget:Budget)->Result<Vec<crate::search::ReleaseRow>> {
        self.lane()?;budget.check()?;ensure!(ids.len()<=budget.rows && budget.rows<=256 && budget.bytes<=64*1024*1024,"native word hydration exceeds owner admission");
        let schema=&self.owner.cache_destination().context("word delivery destination unavailable")?.storage.schema;
        self.owner.with(|client| {
            let view=ReadView::open(client,schema,ReadBudget {rows:budget.rows,bytes:budget.bytes},Instant::now()+Duration::from_secs(10))?;view.rows(ids)
        })
    }
}
impl Source for Delivery {
    fn changes(&mut self,budget:Budget)->Result<Changes> {
        ensure!(self.batch.is_none(),"previous native word delivery has not been acknowledged");
        let batch=self.read_batch(budget)?;
        let changes=Changes {receipts:batch.claims.clone(),rows:batch.rows.clone()};
        if !batch.claims.is_empty() {self.batch=Some(batch);}Ok(changes)
    }
    fn acknowledge(&mut self,commit:&IndexCommitReceipt,receipts:&[Changed])->Result<()> {
        self.lane()?;
        let batch=self.batch.take().context("native word delivery receipt has no sealed batch")?;
        ensure!(batch.claims==receipts && commit.stamp()==&batch.stamp && commit.batch_digest()==&batch.digest
            && native::batch_digest(batch.stamp,&batch.claims,&batch.rows)?==batch.digest,"native word index commit differs from its sealed input");
        // The receipt's fields are private to the actual Tantivy commit minter.
        // Its opstamp is delivery evidence, never PostgreSQL ownership.
        let _committed_opstamp=commit.opstamp();
        self.owner.with(|client| {
            let epoch=self.owner.epoch().context("native word delivery owner ended")?;
            ensure!(epoch.id()==&batch.epoch && super::fresh_words::catalog_stamp(epoch.catalog())==batch.stamp,"native word ACK belongs to an ended catalog or attempt");
            let mut tx=client.transaction()?;super::history_gate::native(&mut tx)?;
            let ids=receipts.iter().map(|claim|claim.id).collect::<Vec<_>>();
            let versions=receipts.iter().map(|claim|claim.version).collect::<Vec<_>>();
            tx.execute("delete from word_queue q using unnest($1::bigint[],$2::bigint[]) as sent(id,n) where q.release_id=sent.id and q.n=sent.n",&[&ids,&versions])?;
            if self.owner.stop.load(std::sync::atomic::Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
            tx.commit()?;Ok(())
        })
    }
}

/// One bounded standard lane per native attempt. It owns the Tantivy writer;
/// both PostgreSQL batch reads and ACKs still use the original Owner session.
pub(crate) struct Keeper {
    stopping:Arc<std::sync::atomic::AtomicBool>,
    worker:std::thread::JoinHandle<Result<()>>,
}
impl Keeper {
    pub(crate) fn start(owner:&Arc<Owner>,headers:crate::config::HeaderStoreConfig)->Result<Self> {
        ensure!(tokio::runtime::Handle::try_current().is_err(),"native word keeper must start outside Tokio");
        owner.with(|_| {owner.epoch().context("native word keeper requires canonical owner")?.catalog().word_index_root(&headers)?;Ok(())})?;
        let stopping=Arc::new(std::sync::atomic::AtomicBool::new(false));let worker_stop=stopping.clone();let owner=owner.clone();
        let worker=std::thread::Builder::new().name("atlas-native-words".into()).spawn(move|| {
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||run(&owner,&headers,&worker_stop)))
                .unwrap_or_else(|_|Err(anyhow::anyhow!("native word keeper panicked")));
            if let Err(error)=&result {
                let canceled=super::outage::is_attempt_ended(error)
                    && (worker_stop.load(std::sync::atomic::Ordering::Acquire)||owner.stop.load(std::sync::atomic::Ordering::Acquire));
                if !canceled || owner.failure().is_some() {
                    eprintln!("native word keeper failed; prior_failure={:?}; cause={}",owner.failure(),super::linker_runtime::failure_diagnostic(error));
                    owner.fail(super::outage::classify(error));
                }
            }
            result
        })?;
        Ok(Self {stopping,worker})
    }
    /// Join before closing the original owner or acquiring a replacement.
    pub(crate) fn finish(self)->Result<()> {
        self.stopping.store(true,std::sync::atomic::Ordering::Release);
        match self.worker.join() {
            Ok(Err(error)) if super::outage::is_attempt_ended(&error)=>Ok(()),
            Ok(result)=>result,
            Err(_)=>Err(anyhow::anyhow!("native word keeper panicked")),
        }
    }
}
fn canceled(owner:&Owner,stop:&std::sync::atomic::AtomicBool)->Result<()> {
    if stop.load(std::sync::atomic::Ordering::Acquire)||owner.stop.load(std::sync::atomic::Ordering::Acquire) {Err(super::outage::AttemptEnded.into())} else {Ok(())}
}
struct OwnedBuild<'owner,'view,'client> {
    owner:&'owner Arc<Owner>,stop:&'owner std::sync::atomic::AtomicBool,
    source:super::fresh_words::Search<'view,'client>,
}
impl OwnedBuild<'_,'_,'_> {
    fn check(&self)->Result<()> {canceled(self.owner,self.stop)?;self.owner.with(|_|Ok(()))}
}
impl SearchSource for OwnedBuild<'_,'_,'_> {
    fn stamp(&mut self)->Result<[u8;32]> {self.check()?;self.source.stamp()}
    fn pending(&mut self)->Result<bool> {self.check()?;self.source.pending()}
    fn visible_rows(&mut self,ids:&[i64],budget:Budget)->Result<Vec<crate::search::ReleaseRow>> {self.check()?;self.source.visible_rows(ids,budget)}
}
impl native::BuildSource for OwnedBuild<'_,'_,'_> {
    fn expected_documents(&mut self)->Result<u64> {
        let mut after=0;let mut count=0u64;
        loop {
            self.check()?;let rows=self.source.page(after,Budget::default())?;
            let Some(last)=rows.last() else {return Ok(count)};
            after=last.id;count=count.checked_add(rows.len() as u64).context("native word count overflow")?;
        }
    }
    fn build_page(&mut self,after:i64,budget:Budget)->Result<Vec<Record>> {self.check()?;self.source.build_page(after,budget)}
}
pub(super) fn open_index(owner:&Arc<Owner>,headers:&crate::config::HeaderStoreConfig,stop:&std::sync::atomic::AtomicBool)->Result<crate::word_index::WordIndex> {
    canceled(owner,stop)?;
    let (stamp,directory)=owner.with(|_| {
        let catalog=owner.epoch().context("native word keeper owner ended")?.catalog();
        Ok((super::fresh_words::catalog_stamp(catalog),catalog.word_index_root(headers)?))
    })?;
    let index=crate::word_index::WordIndex::open_native(&directory,stamp)?;
    if !index.native_ready(stamp)? {
        // An unfinished checkpoint belongs to the old RR snapshot. New reads
        // start a base build at zero; completed indexes only need delivery.
        index.restart_native_build(stamp)?;
        let destination=owner.cache_destination().context("native word keeper destination unavailable")?;
        let readers=super::Pool::new(&destination.storage)?;
        let mut client=readers.get()?;
        // A separate client is permitted only for this bounded READ-ONLY RR
        // base stream. It never receives queue/result/cursor/ACK authority.
        client.batch_execute("set default_transaction_read_only=on")?;
        ensure!(super::indexer_runtime::destination_identity(&mut client,&destination.storage.schema)?==destination.identity
            && super::migrate::version(&mut client,&destination.storage.schema)?==destination.version,"native word build destination changed");
        let budget=Budget::default();
        let view=ReadView::open(&mut client,&destination.storage.schema,ReadBudget {rows:budget.rows,bytes:budget.bytes},Instant::now()+Duration::from_secs(120))?;
        let source=super::fresh_words::Search::open(&view)?;
        let mut source=OwnedBuild {owner,stop,source};
        ensure!(source.stamp()?==stamp,"native word build catalog changed");
        let progress=index.build_native(&mut source,budget,||canceled(owner,stop).is_ok())?;
        canceled(owner,stop)?;ensure!(progress.complete,"native word base build did not complete");
    }
    owner.with(|_|Ok(()))?;Ok(index)
}
fn run(owner:&Arc<Owner>,headers:&crate::config::HeaderStoreConfig,stop:&std::sync::atomic::AtomicBool)->Result<()> {
    let index=open_index(owner,headers,stop)?;let mut source=Delivery::for_owner(owner)?;
    loop {
        canceled(owner,stop)?;
        if index.drain_native(&mut source,Budget::default())?==0 {
            for _ in 0..4 {canceled(owner,stop)?;std::thread::sleep(Duration::from_millis(25));}
        }
    }
}
