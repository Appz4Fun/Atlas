//! Finite, readonly detailed catalog walks on an existing native read observer.
//! Each page owns one short ReadView. No pool, owner session, saved SQLite
//! reading, feed fetch, canonical mutation or implicit retry is introduced.
use anyhow::{Context,Result,ensure};
use std::sync::atomic::{AtomicBool,Ordering};
use super::{fresh_reader::ReadView,indexer_runtime::{CacheDestination,destination_identity_in},stage::CatalogIdentity};
use crate::metrics::catalog::{Row,Tally};

const PAGE_ROWS:usize=128;

/// The caller retains the original checked destination and catalog, opens a
/// fresh <=10s ReadView on its existing read observer for each page, checks its
/// stop between pages, and joins that observer before closing the native Owner.
/// On any error discard the entire Scan and propagate the original error chain;
/// only finish() permits publication. A new session requires a new scan.
pub(crate) struct Scan {
    destination:CacheDestination,
    catalog:CatalogIdentity,
    high:Option<i64>,
    after:Option<i64>,
    tally:Tally,
    complete:bool,
    failed:bool,
    relation_bytes:Option<u64>,
}

pub(crate) struct Completed {
    pub(crate) tally:Tally,
    /// Physical table, TOAST and index bytes in this native schema at the
    /// completed page's snapshot. Excludes cluster/WAL/other-schema storage.
    pub(crate) relation_bytes:u64,
}

impl Completed {
    pub(crate) fn publish(&self,took:std::time::Duration) {
        crate::metrics::names::DB_BYTES.replace([(
            crate::metrics::label_key(&[("file","native_schema"),("kind","relations")]),self.relation_bytes as f64)]);
        self.tally.publish(took);
    }
}

fn stopping(stop:&AtomicBool)->Result<()> {
    if stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}Ok(())
}

fn verify(view:&ReadView<'_>,destination:&CacheDestination,storage:&super::StorageConfig,catalog:&CatalogIdentity,stop:&AtomicBool)->Result<()> {
    stopping(stop)?;
    ensure!(*storage==destination.storage,"native catalog metrics storage destination changed");
    view.before_query()?;
    ensure!(view.tx.borrow_mut().query_one("select current_schema()=$1",&[&storage.schema])?.try_get::<_,bool>(0)?,
        "native catalog metrics observer schema changed");
    view.before_query()?;
    ensure!(destination_identity_in(&mut *view.tx.borrow_mut(),&storage.schema)?==destination.identity,
        "native catalog metrics destination identity changed");
    view.before_query()?;
    ensure!(view.tx.borrow_mut().query_one("select coalesce(max(version),0) from schema_migrations",&[])?.try_get::<_,i32>(0)?==destination.version,
        "native catalog metrics schema version changed");
    view.before_query()?;
    let current=CatalogIdentity::read_in(&mut *view.tx.borrow_mut())?;
    ensure!(current==*catalog,"native catalog metrics catalog identity changed");
    stopping(stop)
}

impl Scan {
    pub(crate) fn begin(view:&ReadView<'_>,destination:&CacheDestination,storage:&super::StorageConfig,catalog:&CatalogIdentity,stop:&AtomicBool)->Result<Self> {
        verify(view,destination,storage,catalog,stop)?;
        view.before_query()?;
        let high=view.tx.borrow_mut().query_one("select max(id) from releases",&[])?.try_get(0)?;
        // Even an empty high-water observation needs its final checked page;
        // absence of a completed read is never promoted into empty success.
        Ok(Self {destination:destination.clone(),catalog:catalog.clone(),high,after:None,tally:Tally::default(),complete:false,failed:false,relation_bytes:None})
    }

    pub(crate) fn is_complete(&self)->bool {self.complete&&!self.failed}

    pub(crate) fn page(&mut self,view:&ReadView<'_>,storage:&super::StorageConfig,stop:&AtomicBool)->Result<()> {
        ensure!(!self.complete&&!self.failed,"native catalog metrics walk already ended");
        let result=self.read_page(view,storage,stop);
        if result.is_err() {self.failed=true;}
        result
    }

    fn read_page(&mut self,view:&ReadView<'_>,storage:&super::StorageConfig,stop:&AtomicBool)->Result<()> {
        verify(view,&self.destination,storage,&self.catalog,stop)?;
        let limit=i64::try_from(view.budget.rows.min(PAGE_ROWS))?;
        view.before_query()?;
        // Select only fixed-size extents first. All strings and labels are
        // admitted before postgres decodes them, in the same RR snapshot.
        let extents=view.tx.borrow_mut().query("select r.id,
            octet_length(r.group_name)::bigint,octet_length(coalesce(r.display_name,r.name))::bigint,
            coalesce(octet_length(p.category),0)::bigint,
            coalesce(octet_length(case when p.release_id is not null then coalesce(f.key,t.source::text,'unknown') end),0)::bigint
            from releases r left join predb_match p on p.release_id=r.id
            left join ext_predb_titles t on t.id=p.title_id left join ext_predb_feeds f on f.source=t.source
            where ($1::bigint is null or r.id>$1) and r.id<=$2 order by r.id limit $3",
            &[&self.after,&self.high,&limit])?;
        let mut expected=std::collections::BTreeMap::new();let mut owned=65_536u64;
        for row in extents {
            stopping(stop)?;let id:i64=row.try_get(0)?;
            let lengths:[i64;4]=[row.try_get(1)?,row.try_get(2)?,row.try_get(3)?,row.try_get(4)?];
            owned=owned.checked_add(4096).context("native catalog metrics extent overflow")?;
            for length in lengths {
                ensure!(length>=0,"native catalog metrics negative text extent");
                owned=owned.checked_add((length as u64).checked_mul(3).context("native catalog metrics extent overflow")?)
                    .context("native catalog metrics page overflow")?;
            }
            ensure!(expected.insert(id,lengths).is_none(),"native catalog metrics repeated release identity");
        }
        view.admit_owned(owned)?;
        let ids:Vec<i64>=expected.keys().copied().collect();
        stopping(stop)?;view.before_query()?;
        let rows=view.tx.borrow_mut().query("select r.id,r.group_name,r.size,r.complete,r.parts,r.is_obfuscated,
            r.display_name is not null,r.name_source,coalesce(r.display_name,r.name),c.category,
            p.kind,p.category,p.title_id,
            case when p.release_id is not null then coalesce(f.key,t.source::text,'unknown') end
            from releases r left join release_category c on c.release_id=r.id
            left join predb_match p on p.release_id=r.id
            left join ext_predb_titles t on t.id=p.title_id left join ext_predb_feeds f on f.source=t.source
            where r.id=any($1) order by r.id",&[&ids])?;
        ensure!(rows.len()==expected.len(),"native catalog metrics page changed within snapshot");
        let count=rows.len();
        for row in rows {
            stopping(stop)?;view.check_deadline()?;
            let id:i64=row.try_get(0)?;let group:String=row.try_get(1)?;let name:String=row.try_get(8)?;
            let predb_category:Option<String>=row.try_get(11)?;let feed:Option<String>=row.try_get(13)?;
            let lengths=expected.remove(&id).context("native catalog metrics release lost extent")?;
            ensure!([group.len() as i64,name.len() as i64,predb_category.as_ref().map_or(0,|s|s.len()) as i64,
                feed.as_ref().map_or(0,|s|s.len()) as i64]==lengths,"native catalog metrics text extent changed");
            let kind:Option<i64>=row.try_get(10)?;let title:Option<i64>=row.try_get(12)?;
            ensure!(kind.is_some()==title.is_some()&&kind.is_some()==feed.is_some(),"native catalog metrics incomplete PreDB join");
            self.tally.add_native(&Row {group:&group,size:row.try_get(2)?,complete:row.try_get(3)?,parts:row.try_get(4)?,
                obfuscated:row.try_get(5)?,named:row.try_get(6)?,name_source:row.try_get(7)?,name:&name,
                category:row.try_get(9)?,predb:kind.zip(title).map(|(kind,title)|(kind,predb_category.as_deref(),title))},feed.as_deref())?;
            self.after=Some(id);
        }
        stopping(stop)?;view.check_deadline()?;
        if count<usize::try_from(limit)?||self.after==self.high {
            view.before_query()?;
            let relations:i64=view.tx.borrow_mut().query_one("select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace
                where n.nspname=$1 and c.relkind in ('r','m')",&[&storage.schema])?.try_get(0)?;
            ensure!((0..=10_000).contains(&relations),"native catalog metrics relation inventory exceeds admission");
            stopping(stop)?;view.before_query()?;
            let bytes:i64=view.tx.borrow_mut().query_one("select coalesce(sum(pg_total_relation_size(c.oid)),0)::bigint
                from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname=$1 and c.relkind in ('r','m')",
                &[&storage.schema])?.try_get(0)?;
            let bytes=u64::try_from(bytes).context("native catalog metrics relation size is negative")?;
            stopping(stop)?;view.check_deadline()?;
            self.relation_bytes=Some(bytes);self.complete=true;
        }
        Ok(())
    }

    pub(crate) fn finish(self,stop:&AtomicBool)->Result<Completed> {
        stopping(stop)?;
        ensure!(self.is_complete(),"native catalog metrics walk is incomplete or failed");
        Ok(Completed {tally:self.tally,relation_bytes:self.relation_bytes.context("native catalog metrics size was not observed")?})
    }
}

#[cfg(test)]
#[path="catalog_metrics_tests.rs"]
mod tests;
