//! Direct fresh-catalog reads. A transaction contains actual native rows,
//! including an ordinary empty database; there is no historical publication,
//! imported release identity, or caller-supplied empty-fact certificate.
use std::{cell::{RefCell,Cell}, time::{Duration, Instant}};
use anyhow::{Context, Result, ensure};
use postgres::{Client, IsolationLevel, Transaction};
use crate::search::ReleaseRow;

#[derive(Clone, Copy)]
pub(crate) struct Budget { pub rows: usize, pub bytes: u64 }
impl Default for Budget { fn default() -> Self { Self { rows: 64, bytes: 4 * 1024 * 1024 } } }

pub(crate) struct ReadView<'client> {
    pub(super) tx: RefCell<Transaction<'client>>,
    pub(super) budget: Budget,
    until: Instant,
    owned: Cell<u64>,
}

impl<'client> ReadView<'client> {
    /// This is read data, not a runtime/startup/activation capability. The
    /// outer fresh runtime separately binds its actual owner and cache import.
    pub(crate) fn open(client: &'client mut Client, schema: &str, budget: Budget, until: Instant) -> Result<Self> {
        ensure!(super::valid_schema(schema) && budget.rows > 0 && budget.bytes >= 131_072,
            "invalid native reader admission");
        let tx = client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
        let view = Self { tx: RefCell::new(tx), budget, until, owned:Cell::new(0) };
        view.before_query()?;
        let row = view.tx.borrow_mut().query_one("select current_schema()=$1 and current_setting('transaction_read_only')='on'
            and current_setting('transaction_isolation')='repeatable read'", &[&schema])?;
        ensure!(row.try_get::<_,bool>(0)?, "native reader destination changed");
        Ok(view)
    }

    pub(crate) fn header_config(&self,configured:&crate::config::HeaderStoreConfig)->Result<crate::config::HeaderStoreConfig> {
        self.before_query()?;
        super::stage::CatalogIdentity::read_in(&mut *self.tx.borrow_mut())?.header_config(configured)
    }

    pub(crate) fn word_index_root(&self,configured:&crate::config::HeaderStoreConfig)->Result<std::path::PathBuf> {
        self.before_query()?;
        super::stage::CatalogIdentity::read_in(&mut *self.tx.borrow_mut())?.word_index_root(configured)
    }

    pub(super) fn admit_owned(&self,bytes:u64)->Result<()> {
        let total=self.owned.get().checked_add(bytes).context("native request allocation admission overflow")?;
        ensure!(total<=self.budget.bytes,"native request owned data exceeds byte admission");self.owned.set(total);Ok(())
    }
    pub(crate) fn visible(&self,id:i64)->Result<bool> {
        self.before_query()?;
        let row=self.tx.borrow_mut().query_opt("select group_name from releases where id=$1 and octet_length(group_name)<=$2
            and (display_name is not null or (not is_obfuscated and (length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]')))",&[&id,&(crate::filters::MAX_GROUP_NAME as i32)])?;
        Ok(match row {Some(row)=>!crate::filters::group_blocked(&row.try_get::<_,String>(0)?),None=>false})
    }
    pub(super) fn download_head(&self,id:i64)->Result<Option<(String,String,bool)>> {
        if !self.visible(id)? {return Ok(None);}
        self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select octet_length(group_name)::bigint,octet_length(posted_date)::bigint from releases where id=$1",&[&id])?;
        let bytes=[row.try_get::<_,i64>(0)?,row.try_get::<_,i64>(1)?].into_iter().try_fold(4096u64,|sum,n|->Result<u64>{ensure!(n>=0,"native head extent is negative");Ok(sum.checked_add((n as u64).checked_mul(3).context("native head overflow")?).context("native head overflow")?)})?;
        self.admit_owned(bytes)?;self.before_query()?;
        let row=self.tx.borrow_mut().query_one("select group_name,posted_date,complete from releases where id=$1",&[&id])?;
        Ok(Some((row.try_get(0)?,row.try_get(1)?,row.try_get(2)?)))
    }

    pub(super) fn check_deadline(&self) -> Result<()> {
        ensure!(Instant::now() < self.until, "native reader deadline expired"); Ok(())
    }

    pub(super) fn before_query(&self) -> Result<()> {
        let remaining = self.until.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
            .context("native reader deadline expired")?;
        let timeout = remaining.as_millis().clamp(1, i32::MAX as u128).to_string();
        self.tx.borrow_mut().query_one("select set_config('statement_timeout',$1,true)", &[&timeout])?;
        Ok(())
    }

    pub(crate) fn count(&self) -> Result<i64> {
        self.before_query()?;
        let count: i64 = self.tx.borrow_mut().query_one("select count(*) from releases", &[])?.try_get(0)?;
        ensure!(count >= 0, "native release count is invalid"); Ok(count)
    }

    pub(crate) fn visible_count(&self)->Result<u64> {
        use postgres::fallible_iterator::FallibleIterator;
        self.before_query()?;
        let mut tx=self.tx.borrow_mut();
        let mut rows=tx.query_raw("select group_name,count(*) from releases where octet_length(group_name)<=$1
            and (display_name is not null or (not is_obfuscated and (length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]')))
            group by group_name",[&(crate::filters::MAX_GROUP_NAME as i32)])?;
        let mut total=0u64;
        while let Some(row)=rows.next()? {
            self.check_deadline()?;
            let group:String=row.try_get(0)?;
            ensure!(group.len()<=crate::filters::MAX_GROUP_NAME,"native group exceeded visibility admission");
            if !crate::filters::group_blocked(&group){total=total.checked_add(u64::try_from(row.try_get::<_,i64>(1)?)?).context("native visible count overflow")?;}
        }Ok(total)
    }

    pub(crate) fn release(&self, id: i64) -> Result<Option<ReleaseRow>> {
        let mut rows = self.rows(&[id])?;
        Ok(rows.pop())
    }

    /// Bound owned strings BEFORE postgres decodes any selected text. SQL
    /// lengths and payload are read in the same RR snapshot. Large text for
    /// streamed NZB/metadata is handled by a separate range reader, never cut.
    pub(crate) fn rows(&self, ids: &[i64]) -> Result<Vec<ReleaseRow>> {
        ensure!(ids.len() <= self.budget.rows, "native row count exceeds admission");
        if ids.is_empty() { return Ok(Vec::new()); }
        self.before_query()?;
        let extents = self.tx.borrow_mut().query("select id,octet_length(coalesce(display_name,name))::bigint,
            octet_length(group_name)::bigint,octet_length(poster)::bigint,octet_length(posted_date)::bigint
            from releases where id=any($1) and (display_name is not null or (not is_obfuscated and (length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]'))) order by id", &[&ids])?;
        let mut expected = std::collections::HashMap::new();
        let mut owned = 65_536u64 + ids.len() as u64 * 2048;
        for row in &extents {
            let id: i64 = row.try_get(0)?; ensure!(ids.contains(&id), "native row identity changed");
            let lengths: [i64;4] = [row.try_get(1)?,row.try_get(2)?,row.try_get(3)?,row.try_get(4)?];
            for length in lengths {
                ensure!(length >= 0, "native text extent is negative");
                owned = owned.checked_add((length as u64).checked_mul(2).context("native text extent overflow")?)
                    .context("native page admission overflow")?;
            }
            ensure!(expected.insert(id,lengths).is_none(), "native row identity repeated");
        }
        self.admit_owned(owned)?;
        self.before_query()?;
        let rows = self.tx.borrow_mut().query("select id,coalesce(display_name,name),group_name,poster,posted_date,size,complete,parts
            from releases where id=any($1) and (display_name is not null or (not is_obfuscated and (length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]'))) order by id", &[&ids])?;
        ensure!(rows.len() == expected.len(), "native read changed within snapshot");
        let mut found = std::collections::HashMap::new();
        for row in rows {
            let row = ReleaseRow { id: row.try_get(0)?,name: row.try_get(1)?,group_name: row.try_get(2)?,
                poster: row.try_get(3)?,posted_date: row.try_get(4)?,size: row.try_get(5)?,complete: row.try_get(6)?,parts: row.try_get(7)? };
            let lengths = expected.remove(&row.id).context("native row lost its extent")?;
            ensure!([row.name.len() as i64,row.group_name.len() as i64,row.poster.as_ref().map_or(0,|s|s.len()) as i64,
                row.posted_date.as_ref().map_or(0,|s|s.len()) as i64] == lengths, "native text extent changed");
            if !crate::filters::group_blocked(&row.group_name) { found.insert(row.id,row); }
        }
        Ok(ids.iter().filter_map(|id|found.remove(id)).collect())
    }

    pub(crate) fn recent(&self, offset: i64, limit: usize) -> Result<Vec<ReleaseRow>> {
        ensure!(offset >= 0 && limit <= self.budget.rows, "native recent page exceeds admission");
        if limit == 0 { return Ok(Vec::new()); }
        let mut before: Option<i64> = None;
        let mut skipped = 0i64;
        let mut rejected = 0usize;
        let mut found = Vec::new();
        loop {
            self.before_query()?;
            let ids = self.tx.borrow_mut().query("select id from releases
                where (display_name is not null or (not is_obfuscated and (length(name)<16 or name collate \"C\" ~ '[^A-Za-z0-9]')))
                  and ($1::bigint is null or id<$1) order by id desc limit $2", &[&before,&i64::try_from(self.budget.rows)?])?
                .into_iter().map(|row|row.try_get(0)).collect::<std::result::Result<Vec<i64>,_>>()?;
            if ids.is_empty() { break; }
            let mut page: std::collections::HashMap<_,_> = self.rows(&ids)?.into_iter().map(|row|(row.id,row)).collect();
            for id in &ids {
                before = Some(*id);
                if let Some(row) = page.remove(id) {
                    if skipped < offset { skipped += 1; } else { found.push(row); }
                } else { rejected += 1; }
                if found.len() == limit || rejected > crate::search::Bounds::DEFAULT.rejects { return Ok(found); }
            }
            if ids.len() < self.budget.rows { break; }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Real empty SQL results are the baseline. This does not insert a nonce,
    // epoch, historical generation, receipt or hand-written ready flag.
    #[test]
    #[ignore = "requires explicit isolated fresh PostgreSQL schema and runtime admission"]
    fn actual_empty_native_catalog_returns_zero_empty_and_missing() {
        let db = super::super::testing::Schema::new().expect("explicit native PG admission requires a test database");
        let mut client = db.pool.get().unwrap();
        let view = ReadView::open(&mut client, &db.name, Budget::default(), Instant::now()+Duration::from_secs(3)).unwrap();
        assert_eq!(view.count().unwrap(),0);
        assert!(view.recent(0,50).unwrap().is_empty());
        assert!(view.release(1).unwrap().is_none());
    }
}
