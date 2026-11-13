//! Native data-source adapters reuse Atlas's existing Tantivy document/query/
//! scorer/sort implementation. No SQLite opener or generic PG fulltext query
//! belongs in this path.

use super::*;
use anyhow::{Context, Result, ensure};
use std::io::Read;
use sha2::{Digest, Sha256};

const BUILT: &str = "atlas-native-built";

#[derive(Clone, Copy)]
pub(crate) struct Budget {
    pub rows: usize,
    pub bytes: u64,
    pub commit_bytes: u64,
}
impl Default for Budget {
    fn default() -> Self { Self { rows: 64, bytes: 4 * 1024 * 1024, commit_bytes: 16 * 1024 * 1024 } }
}
impl Budget {
    pub(crate) fn check(self) -> Result<()> {
        ensure!(self.rows != 0 && self.bytes >= 512 && self.commit_bytes >= self.bytes,
            "invalid native word-index admission");
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Record {
    pub id: i64,
    pub name: String,
    pub display_name: Option<String>,
    pub group: String,
    pub posted: Option<String>,
    pub size: Option<i64>,
    pub category: Option<i64>,
    pub known_as: Option<String>,
}
impl Record {
    pub(crate) fn admitted_bytes(&self) -> Result<u64> {
        [&self.name[..], self.display_name.as_deref().unwrap_or(""), &self.group[..],
            self.posted.as_deref().unwrap_or(""), self.known_as.as_deref().unwrap_or("")]
            .into_iter().try_fold(384u64, |bytes, text| {
                bytes.checked_add(text.len() as u64).context("native index input extent overflow")
            })
    }
    fn indexed(self) -> Indexed {
        Indexed { id: self.id, name: self.name, display_name: self.display_name,
            group: self.group, posted: self.posted, size: self.size, category: self.category,
            title_key: None, known_as: self.known_as }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Changed { pub id: i64, pub version: i64 }
pub(crate) struct Changes {
    pub receipts: Vec<Changed>,
    /// Visible rows in the same source snapshot as receipts. An absent row is
    /// a deletion/visibility change, so its previous index term is removed.
    pub rows: Vec<Record>,
}

/// Implementations must admit transport/retained bytes BEFORE returning rows,
/// including every unchosen descriptor/queue row. Post-return checks below
/// validate that contract, but cannot make an unbounded loader bounded.
/// Build pages/count are one authenticated immutable source epoch; changes
/// are compare-version receipts from its owning native queue.
pub(crate) trait SearchSource {
    fn stamp(&mut self) -> Result<[u8; 32]>;
    fn pending(&mut self) -> Result<bool>;
    fn visible_rows(&mut self, ids: &[i64], budget: Budget) -> Result<Vec<ReleaseRow>>;
}
pub(crate) trait BuildSource: SearchSource {
    fn expected_documents(&mut self) -> Result<u64>;
    fn build_page(&mut self, after: i64, budget: Budget) -> Result<Vec<Record>>;
}
pub(crate) trait Source: SearchSource {
    fn changes(&mut self, budget: Budget) -> Result<Changes>;
    fn acknowledge(&mut self, commit: &IndexCommitReceipt, receipts: &[Changed]) -> Result<()>;
}

/// Created only after the actual Tantivy commit succeeds. This is an index
/// delivery receipt, not PostgreSQL ownership, runtime or startup authority.
/// The sole canonical owner separately rechecks its epoch and compare-n ACKs.
pub(crate) struct IndexCommitReceipt {
    stamp: [u8; 32],
    batch: [u8; 32],
    opstamp: u64,
}
impl IndexCommitReceipt {
    pub(crate) fn stamp(&self) -> &[u8; 32] { &self.stamp }
    pub(crate) fn batch_digest(&self) -> &[u8; 32] { &self.batch }
    pub(crate) fn opstamp(&self) -> u64 { self.opstamp }
}

/// Exact ordered delivery identity, including deletions and every document
/// input. This shared pure encoding lets the owner compare its sealed batch to
/// the receipt without accepting a caller-supplied ready flag or digest.
pub(crate) fn batch_digest(stamp: [u8; 32], claims: &[Changed], rows: &[Record]) -> Result<[u8; 32]> {
    let mut by_id = HashMap::new();
    for row in rows { ensure!(by_id.insert(row.id, row).is_none(), "duplicate index document"); }
    let mut hash = Sha256::new();
    hash.update(b"atlas.fresh.word.delivery.v1\0");
    hash.update(stamp);
    hash.update((claims.len() as u64).to_le_bytes());
    let mut seen = std::collections::HashSet::new();
    fn text(hash: &mut Sha256, value: Option<&str>) {
        hash.update([u8::from(value.is_some())]);
        if let Some(value) = value { hash.update((value.len() as u64).to_le_bytes()); hash.update(value.as_bytes()); }
    }
    fn number(hash: &mut Sha256, value: Option<i64>) {
        hash.update([u8::from(value.is_some())]);
        if let Some(value) = value { hash.update(value.to_le_bytes()); }
    }
    for claim in claims {
        ensure!(claim.id > 0 && claim.version > 0 && seen.insert(claim.id), "invalid word queue claim");
        hash.update(claim.id.to_le_bytes()); hash.update(claim.version.to_le_bytes());
        let row = by_id.remove(&claim.id);
        hash.update([u8::from(row.is_some())]);
        if let Some(row) = row {
            text(&mut hash, Some(&row.name)); text(&mut hash, row.display_name.as_deref());
            text(&mut hash, Some(&row.group)); text(&mut hash, row.posted.as_deref());
            number(&mut hash, row.size); number(&mut hash, row.category);
            text(&mut hash, row.known_as.as_deref());
        }
    }
    ensure!(by_id.is_empty(), "index document lacks queue claim");
    Ok(hash.finalize().into())
}

pub(crate) struct Progress { pub complete: bool, pub documents: u64, pub after: i64 }

pub(crate) fn directory(root: &Path, stamp: [u8; 32]) -> PathBuf {
    root.join("pg-words").join(hex(stamp))
}
pub(crate) fn codec_stamp() -> String { version_stamp() }

pub(crate) fn decoded_title(found: Option<&str>, asked: &str) -> Option<String> {
    found.into_iter().chain(std::iter::once(asked)).map(str::trim)
        .find(|title| !title.is_empty() && crate::media::allocine::allocine_id(title).is_none())
        .map(str::to_owned)
}
fn hex(stamp: [u8; 32]) -> String { stamp.iter().map(|byte| format!("{byte:02x}")).collect() }
fn building(stamp: [u8; 32], after: i64, documents: u64) -> String {
    format!("native-building {} {after} {documents}", hex(stamp))
}
fn built(stamp: [u8; 32]) -> String { format!("native-built {}", hex(stamp)) }
fn resumed(payload: &str, stamp: [u8; 32]) -> Option<(i64, u64)> {
    let mut words = payload.split_whitespace();
    if words.next()? != "native-building" || words.next()? != hex(stamp) { return None; }
    let after: i64 = words.next()?.parse().ok()?;
    let documents = words.next()?.parse().ok()?;
    (after >= 0 && words.next().is_none()).then_some((after, documents))
}
fn marker(index: &WordIndex, stamp: [u8; 32]) -> Result<bool> {
    let Ok(file) = std::fs::File::open(index.dir.join(BUILT)) else { return Ok(false); };
    let mut bytes = Vec::with_capacity(65);
    file.take(65).read_to_end(&mut bytes)?;
    Ok(bytes.as_slice() == hex(stamp).as_bytes()
        && index.index.load_metas()?.payload.as_deref() == Some(built(stamp).as_str()))
}
fn row_bytes(rows: &[Record], receipts: usize, budget: Budget) -> Result<u64> {
    ensure!(rows.len() <= budget.rows && receipts <= budget.rows, "native index row admission exceeded");
    let bytes = rows.iter().try_fold((receipts as u64).saturating_mul(24), |sum, row| {
        sum.checked_add(row.admitted_bytes()?).context("native index page extent overflow")
    })?;
    ensure!(bytes <= budget.bytes, "native index byte admission exceeded");
    Ok(bytes)
}

/// One committed Tantivy search snapshot for an entire PG descriptor walk.
/// Catalog identity alone is deliberately insufficient to pin a reload.
pub(crate) struct NativeMatches<'index> {
    index:&'index WordIndex,searcher:Searcher,query:Box<dyn Query>,budget:Budget,
}
impl NativeMatches<'_> {
    pub(crate) fn matching_ids(&self,ids:&[i64])->Result<Vec<i64>> {
        ensure!(ids.len()<=100&&ids.len()<=self.budget.rows,"native membership batch exceeds admission");
        if ids.is_empty(){return Ok(Vec::new());}
        ensure!(WordIndex::numeric_workspace(&self.searcher,ids.len())?<=self.budget.bytes,"native membership collector exceeds admission");
        let terms=ids.iter().map(|id|Term::from_field_u64(self.index.fields.id,*id as u64));
        let query=BooleanQuery::new(vec![(Occur::Must,self.query.box_clone()),(Occur::Must,Box::new(TermSetQuery::new(terms)))]);
        let (found,total)=self.index.top(&self.searcher,&query,By::Newest,true,0,ids.len())?;
        ensure!(total<=ids.len(),"native membership has duplicate public IDs");Ok(found)
    }
}

impl WordIndex {
    /// Request-side open is read-only. The legacy/create constructor can reset
    /// an incompatible directory; a public request must never use that repair
    /// path or create an unbuilt index in place of a missing published one.
    pub(crate) fn read_native(dir: &Path, stamp: [u8; 32]) -> Result<Self> {
        let expected = version_stamp();
        let mut version = Vec::new();
        std::fs::File::open(dir.join("atlas-version"))?.take(expected.len() as u64 + 1).read_to_end(&mut version)?;
        ensure!(version == expected.as_bytes(), "native word index codec differs from its source receipt");
        let (schema, fields) = schema();
        let index = Index::open(MmapDirectory::open(dir)?)?;
        ensure!(index.schema() == schema, "native word index schema differs from the original codec");
        index.tokenizers().register(WORDS, analyzer());
        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        let result = Self {
            index, reader, fields, ranks: category_ranks(), dir: dir.to_path_buf(),
            ready: AtomicBool::new(false), built_from: AtomicI64::new(0), writer: Mutex::new(None),
            keeping: AtomicBool::new(false), pulse: Mutex::new(Pulse::default()), pulsed: Condvar::new(),
            counts: Mutex::new(HashMap::new()),
        };
        ensure!(marker(&result, stamp)?, "native word index has no authenticated completed input");
        result.ready.store(true, Ordering::Release);
        Ok(result)
    }

    pub(crate) fn open_native(dir: &Path, stamp: [u8; 32]) -> Result<Self> {
        let index = Self::open(dir)?;
        index.ready.store(marker(&index, stamp)?, Ordering::Release);
        Ok(index)
    }
    fn native_writer(&self) -> Result<()> {
        let mut writer = lock(&self.writer);
        if writer.is_none() {
            *writer = Some(self.index.writer_with_num_threads(1, WRITER_HEAP)?);
        }
        Ok(())
    }
    pub(crate) fn native_ready(&self, stamp: [u8; 32]) -> Result<bool> {
        Ok(self.ready() && marker(self, stamp)?)
    }

    /// A new RR source transaction has a new input snapshot even when this
    /// catalog/codec stamp is unchanged. Never resume its predecessor's base
    /// cursor; concurrent writes remain in the durable delivery queue.
    pub(crate) fn restart_native_build(&self,stamp:[u8;32])->Result<()> {
        ensure!(!self.native_ready(stamp)?,"a ready native index must use queue delivery, not base restart");
        self.native_writer()?;
        let mut guard=lock(&self.writer);
        let writer=guard.as_mut().context("native word-index writer missing")?;
        writer.rollback()?;
        match std::fs::remove_file(self.dir.join(BUILT)) {
            Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(error.into()),
        }
        self.ready.store(false,Ordering::Release);
        writer.delete_all_documents()?;
        commit_with(writer,&building(stamp,0,0))?;
        self.reader.reload()?;
        ensure!(self.reader.searcher().num_docs()==0,"native base restart retained old source documents");
        Ok(())
    }

    pub(crate) fn build_native(
        &self, source: &mut impl BuildSource, budget: Budget, mut go_on: impl FnMut() -> bool,
    ) -> Result<Progress> {
        budget.check()?;
        let stamp = source.stamp()?;
        let expected = source.expected_documents()?;
        self.native_writer()?;
        self.ready.store(false, Ordering::Release);
        let _ = std::fs::remove_file(self.dir.join(BUILT));
        let mut guard = lock(&self.writer);
        let writer = guard.as_mut().context("native word-index writer missing")?;
        writer.rollback()?;
        let (mut after, mut documents) = match self.index.load_metas()?.payload.as_deref().and_then(|payload| resumed(payload, stamp)) {
            Some(cursor) => cursor,
            None => {
                writer.delete_all_documents()?;
                commit_with(writer, &building(stamp, 0, 0))?;
                (0, 0)
            }
        };
        ensure!(documents <= expected, "native word-index checkpoint exceeds source count");
        self.reader.reload()?;
        ensure!(self.reader.searcher().num_docs() == documents, "native word-index checkpoint document count mismatch");
        let mut pending_bytes = 0u64;
        loop {
            ensure!(source.stamp()? == stamp, "native word-index source changed during build");
            let rows = source.build_page(after, budget)?;
            row_bytes(&rows, 0, budget)?;
            if rows.is_empty() { break; }
            for row in rows {
                ensure!(row.id > after, "native word-index build is not in public-ID order");
                let bytes = row.admitted_bytes()?;
                if pending_bytes != 0 && pending_bytes.saturating_add(bytes) > budget.commit_bytes {
                    commit_with(writer, &building(stamp, after, documents))?;
                    pending_bytes = 0;
                    if !go_on() { return Ok(Progress { complete: false, documents, after }); }
                }
                after = row.id;
                writer.add_document(self.doc(&row.indexed()))?;
                documents = documents.checked_add(1).context("native word-index document count overflow")?;
                ensure!(documents <= expected, "native word-index source exceeded its authenticated count");
                pending_bytes += bytes;
            }
        }
        ensure!(source.stamp()? == stamp && documents == expected, "native word-index source coverage is incomplete");
        commit_with(writer, &built(stamp))?;
        crate::atomic::write_atomic(&self.dir.join(BUILT), hex(stamp))?;
        self.reader.reload()?;
        ensure!(self.reader.searcher().num_docs() == expected, "native word-index committed document count mismatch");
        self.ready.store(true, Ordering::Release);
        Ok(Progress { complete: true, documents, after })
    }

    pub(crate) fn drain_native(&self, source: &mut impl Source, budget: Budget) -> Result<usize> {
        budget.check()?;
        let stamp = source.stamp()?;
        ensure!(self.native_ready(stamp)?, "native word index does not match its public source");
        let changes = source.changes(budget)?;
        row_bytes(&changes.rows, changes.receipts.len(), budget)?;
        if changes.receipts.is_empty() {
            ensure!(changes.rows.is_empty(), "native word-index rows have no queue receipts");
            return Ok(0);
        }
        let mut versions = HashMap::new();
        for change in &changes.receipts {
            ensure!(change.id > 0 && change.version > 0 && versions.insert(change.id, change.version).is_none(),
                "invalid native word-index queue identity/version");
        }
        let mut seen = std::collections::HashSet::new();
        for row in &changes.rows {
            ensure!(versions.contains_key(&row.id) && seen.insert(row.id), "native changed row has no unique queue receipt");
        }
        let batch = batch_digest(stamp, &changes.receipts, &changes.rows)?;
        self.native_writer()?;
        let mut guard = lock(&self.writer);
        let writer = guard.as_mut().context("native word-index writer missing")?;
        writer.rollback()?;
        for receipt in &changes.receipts {
            writer.delete_term(Term::from_field_u64(self.fields.id, receipt.id as u64));
        }
        for row in changes.rows { writer.add_document(self.doc(&row.indexed()))?; }
        ensure!(source.stamp()? == stamp, "native word-index source changed before commit");
        let mut commit = writer.prepare_commit()?;
        commit.set_payload(&built(stamp));
        let opstamp = commit.commit()?;
        let receipt = IndexCommitReceipt { stamp, batch, opstamp };
        self.reader.reload()?;
        // An update queued since the source snapshot remains queued because its
        // version differs. ACK failure leaves committed idempotent replay.
        source.acknowledge(&receipt, &changes.receipts)?;
        Ok(changes.receipts.len())
    }

    fn native_catch_up(&self, pending: bool) {
        if !pending || !self.keeping.load(Ordering::Acquire) { return; }
        let mut pulse = lock(&self.pulse);
        let wanted = pulse.begun + 1;
        pulse.asked = true;
        self.pulsed.notify_all();
        let _ = self.pulsed.wait_timeout_while(pulse, CATCH_UP_WAIT, |pulse| pulse.done < wanted);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_native(
        &self, source: &mut impl SearchSource, words: &str, filter: Option<&CatFilter>, tags: &[&str],
        by: By, descending: bool, offset: usize, limit: usize, budget: Budget,
    ) -> Result<Hits> {
        self.search_native_in_group(source,words,filter,tags,None,by,descending,offset,limit,budget)
    }

    /// The indexed group is an exact STRING field, matching the catalog's
    /// case-sensitive group filter. Word/prefix scoring and all existing
    /// tag/category constraints continue through the original query builder.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_native_in_group(
        &self,source:&mut impl SearchSource,words:&str,filter:Option<&CatFilter>,tags:&[&str],group:Option<&str>,
        by:By,descending:bool,offset:usize,limit:usize,budget:Budget,
    )->Result<Hits> {
        let stamp=source.stamp()?;
        if matches!(by,By::Name|By::Group|By::Posted) {
            // Small generic sources retain the existing oracle without using
            // a String TopDocs collector. Public PG string sorts use the
            // streaming membership reader and have no all-row hydration.
            let (ids,total)=self.ranked_native_ids_in_group(source,words,filter,tags,group,By::Newest,true,0,budget.rows,budget)?;
            ensure!(total<=budget.rows,"native string sort requires the bounded PG membership reader");
            let rows=source.visible_rows(&ids,budget)?;
            let extent=rows.iter().try_fold(self.ranked_workspace(0,budget.rows)?,|sum,row|->Result<u64>{
                let strings=row.name.len()+row.group_name.len()+row.poster.as_ref().map_or(0,String::len)+row.posted_date.as_ref().map_or(0,String::len);
                sum.checked_add((strings as u64).checked_mul(4).and_then(|n|n.checked_add(2048)).context("native string sort extent overflow")?).context("native string sort extent overflow")
            })?;ensure!(extent<=budget.bytes,"native string sort exceeds admission");
            let mut keyed=rows.into_iter().map(|row|{
                let key=match by {By::Name=>Some(row.name.to_ascii_lowercase()),By::Group=>Some(row.group_name.clone()),_=>row.posted_date.clone().filter(|value|!value.is_empty())};(key,row)
            }).collect::<Vec<_>>();
            keyed.sort_unstable_by(|(a,x),(b,y)|match (a,b) {
                (Some(a),Some(b))=>if descending{b.cmp(a)}else{a.cmp(b)},(None,None)=>std::cmp::Ordering::Equal,
                (Some(_),None)=>std::cmp::Ordering::Less,(None,Some(_))=>std::cmp::Ordering::Greater,
            }.then(y.id.cmp(&x.id)));
            ensure!(source.stamp()?==stamp,"native string sort source changed");
            return Ok(Hits {rows:keyed.into_iter().skip(offset).take(limit).map(|(_,row)|row).collect(),total});
        }
        let (found,total)=self.ranked_native_ids_in_group(source,words,filter,tags,group,by,descending,offset,limit,budget)?;
        let rows = source.visible_rows(&found, budget)?;
        ensure!(rows.len() <= found.len() && source.stamp()? == stamp, "native index hydration source mismatch");
        let mut at = 0;
        for row in &rows {
            while at < found.len() && found[at] != row.id { at += 1; }
            ensure!(at < found.len(), "native hydration changed ranked public-ID order");
            at += 1;
        }
        Ok(Hits { rows, total })
    }

    /// Numeric TopDocs keeps two limits per segment before merging. Include
    /// simultaneous segment fruits, merge/output buffers and ID descriptors;
    /// string collectors are deliberately excluded from this bounded API.
    fn numeric_slot_bytes()->u64 {
        // Largest key used below: shown-first, score and nullable native ID.
        // Sixteen slots cover the 2*N segment buffer, harvested/converted
        // fruits, merge buffer, sorted output, IDs and allocator slack.
        (std::mem::size_of::<((u64,Score,Option<u64>),DocAddress)>() as u64)*16
    }
    fn numeric_workspace(searcher:&Searcher,span:usize)->Result<u64> {
        let segments=u64::try_from(searcher.segment_readers().len())?.checked_add(1).context("native segment extent overflow")?;
        segments.checked_mul((span.max(1) as u64).checked_mul(Self::numeric_slot_bytes()).and_then(|n|n.checked_add(65536)).context("native collector extent overflow")?)
            .context("native collector extent overflow")
    }
    pub(crate) fn ranked_workspace(&self,offset:usize,limit:usize)->Result<u64> {
        let span=if offset>=super::MAX_HITS{0}else{offset.checked_add(limit).context("native collector offset overflow")?.min(super::MAX_HITS)};
        Self::numeric_workspace(&self.reader.searcher(),span)
    }
    pub(crate) fn ranked_capacity(&self,maximum:usize,available:u64)->Result<(usize,u64)> {
        let searcher=self.reader.searcher();let fixed=Self::numeric_workspace(&searcher,0)?;
        let segments=u64::try_from(searcher.segment_readers().len())?+1;
        let per_id=segments.checked_mul(Self::numeric_slot_bytes()).and_then(|n|n.checked_add(40)).context("native collector extent overflow")?;
        let limit=maximum.min(super::MAX_HITS).min(usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX).max(1)).min(usize::try_from(available.saturating_sub(fixed)/per_id).unwrap_or(usize::MAX));
        ensure!(limit>0,"native numeric collector exceeds request admission");
        Ok((limit,Self::numeric_workspace(&searcher,limit)?))
    }
    /// String sorts are ordered by PG, then checked in fixed ID batches using
    /// the original word/tag/category matcher and only numeric TopDocs keys.
    pub(crate) fn membership_workspace(&self,rows:usize)->Result<u64> {
        ensure!(rows>0&&rows<=100,"native membership batch exceeds admission");
        Self::numeric_workspace(&self.reader.searcher(),rows)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn native_membership(&self,source:&mut impl SearchSource,words:&str,filter:Option<&CatFilter>,tags:&[&str],group:Option<&str>,budget:Budget)->Result<NativeMatches<'_>> {
        budget.check()?;ensure!(budget.rows>0&&budget.rows<=100,"native membership batch exceeds admission");
        ensure!(words.trim().is_empty()||searchable(words),"query requires the native catalog matcher");
        let stamp=source.stamp()?;ensure!(self.native_ready(stamp)?,"native word index does not match its public source");
        self.native_catch_up(source.pending()?);
        let searcher=self.reader.searcher();
        ensure!(Self::numeric_workspace(&searcher,budget.rows)?<=budget.bytes,"native membership collector exceeds admission");
        let mut query=self.query_in(&searcher,words,filter,tags,PREFIX_POSTINGS);
        if let Some(group)=group {query=Box::new(BooleanQuery::new(vec![(Occur::Must,query),(Occur::Must,Box::new(TermQuery::new(Term::from_field_text(self.fields.group,group),IndexRecordOption::Basic)))]));}
        ensure!(source.stamp()?==stamp,"native membership source changed");
        Ok(NativeMatches {index:self,searcher,query,budget})
    }

    /// The same matcher/ranking without display-row hydration. Native PG
    /// facet readers can select actual scalar facts before decoding the final
    /// page; queue delivery/commit/ACK and source-stamp contracts are unchanged.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ranked_native_ids_in_group(
        &self,source:&mut impl SearchSource,words:&str,filter:Option<&CatFilter>,tags:&[&str],group:Option<&str>,
        by:By,descending:bool,offset:usize,limit:usize,budget:Budget,
    )->Result<(Vec<i64>,usize)> {
        budget.check()?;
        ensure!(words.trim().is_empty() || searchable(words), "query requires the native catalog matcher");
        let stamp = source.stamp()?;
        ensure!(self.native_ready(stamp)?, "native word index does not match its public source");
        self.native_catch_up(source.pending()?);
        ensure!(!matches!(by,By::Name|By::Group|By::Posted),"native string sorting requires bounded PG membership");
        let searcher = self.reader.searcher();
        let span=if offset>=super::MAX_HITS{0}else{offset.checked_add(limit).context("native collector offset overflow")?.min(super::MAX_HITS)};
        ensure!(Self::numeric_workspace(&searcher,span)?<=budget.bytes,"native numeric collector exceeds request admission");
        let mut query=self.query_in(&searcher,words,filter,tags,PREFIX_POSTINGS);
        if let Some(group)=group {
            let scoped=TermQuery::new(Term::from_field_text(self.fields.group,group),IndexRecordOption::Basic);
            query=Box::new(BooleanQuery::new(vec![(Occur::Must,query),(Occur::Must,Box::new(scoped))]));
        }
        let (found, total) = self.top(&searcher,query.as_ref(),by,descending,offset,limit)?;
        ensure!(source.stamp()?==stamp,"native ranked source changed during query");
        Ok((found,total))
    }

    pub(crate) fn count_native(
        &self, source: &mut impl SearchSource, words: &str, filter: Option<&CatFilter>, tags: &[&str],
    ) -> Result<usize> {
        let stamp = source.stamp()?;
        ensure!(words.trim().is_empty() || searchable(words), "query requires the native catalog matcher");
        ensure!(self.native_ready(stamp)?, "native word index does not match its public source");
        if !words.trim().is_empty() { self.native_catch_up(source.pending()?); }
        self.count_indexed(words, filter, tags)
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn delivery_digest_binds_versions_deletions_and_every_document_input() {
        let claim = Changed { id: 1, version: 3 };
        let row = Record { id: 1, name: "Example".into(), display_name: None, group: "example.group".into(),
            posted: None, size: None, category: None, known_as: None };
        let baseline = batch_digest([7;32], &[claim], std::slice::from_ref(&row)).unwrap();
        let changed = |row| batch_digest([7;32], &[claim], &[row]).unwrap();
        for field in 0..8 {
            let mut other = row.clone();
            match field {
                0 => other.name.push('x'), 1 => other.display_name=Some(String::new()),
                2 => other.group.push('x'), 3 => other.posted=Some(String::new()),
                4 => other.size=Some(0), 5 => other.category=Some(0),
                6 => other.known_as=Some(String::new()), _ => other.name=String::new(),
            }
            assert_ne!(baseline,changed(other));
        }
        assert_ne!(baseline,batch_digest([7;32], &[claim], &[]).unwrap());
        assert_ne!(baseline,batch_digest([7;32], &[Changed {version:4,..claim}], &[row.clone()]).unwrap());
        assert_ne!(baseline,batch_digest([8;32], &[claim], &[row.clone()]).unwrap());
        assert!(batch_digest([7;32], &[claim,claim], &[row.clone()]).is_err());
        assert!(batch_digest([7;32], &[], &[row]).is_err());
    }
}
