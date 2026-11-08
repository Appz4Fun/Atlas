//! Every word search over every release: a Tantivy index of release names kept next to the database, with what a page
//! sorts and filters by in fast fields, soo a search ranks, sorts and filters all of its matches, not the newest few
//! thousand of them.
//!
//! The database stays what's true. Triggers in each shard put the id of a release that changed, or whose category did,
//! in `word_queue`, and the web server's process takes them from there into the index every few seconds. An index
//! that's missing, or made by another version of this file, is built again from the shards in the background, going
//! on from its last commit after a restart, and searches go to the database as before until it's ready. Each hit is read from the database again before it's shown,
//! soo an index a few seconds behind never shows a release a client may no longer see.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use tantivy::collector::sort_key::{NaturalComparator, SortBySimilarityScore, SortByStaticFastValue, SortByString};
use tantivy::collector::{Collector, Count, SegmentSortKeyComputer, SortKeyComputer, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, EmptyQuery, Occur, Query, RegexQuery, TermQuery, TermSetQuery};
use tantivy::schema::{FAST, Field, INDEXED, IndexRecordOption, STRING, Schema, TextFieldIndexing, TextOptions};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Score, Searcher, TantivyDocument, Term,
};

use crate::category::{CatFilter, TABLE, UNCATEGORIZED, label};
use crate::search::{ReleaseRow, VISIBLE_RELEASE, visible_release};
use crate::store;

/// Bumped whenever what's in a document changes: an index made by another version is built again.
const VERSION: u32 = 1;
/// The tokenizer: words of letters and digits, lowercased and without accents, as SQLite's fts5 splits them.
const WORDS: &str = "atlas_words";
/// The tag filters the page offers, worked out of each name once, when it's indexed.
pub const TAGS: &[&str] = &["2160p", "1080p", "DV", "HDR", "Remux", "WEB-DL", "BluRay"];
/// How often the web server's process takes what changed into the index.
const DRAIN_EVERY: Duration = Duration::from_secs(3);
/// Releases taken from the queues at most per commit.
const DRAIN_BATCH: usize = 20_000;
/// Releases read from a shard at a time when building: short reads, soo the indexer's writes don't wait on one.
const BUILD_BATCH: i64 = 5_000;
/// Releases a build adds between its commits: about what a restart in the middle of one has to read again.
const BUILD_COMMIT_EVERY: usize = 1_000_000;
const WRITER_HEAP: usize = 256 * 1024 * 1024;

/// What a search sorts its matches by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// best match first
    Relevance,
    /// the newest first, as a list with no words has it: what the filters leave of every release
    Newest,
    Size,
    Posted,
    Name,
    Group,
    Category,
}

/// One page of a search, and how many matched in all.
#[derive(Debug, Default)]
pub struct Hits {
    pub rows: Vec<ReleaseRow>,
    pub total: usize,
}

#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    words: Field,
    category: Field,
    cat_rank: Field,
    size: Field,
    posted: Field,
    name_key: Field,
    group: Field,
    tags: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer(WORDS).set_index_option(IndexRecordOption::WithFreqs),
    );
    let fields = Fields {
        id: b.add_u64_field("id", INDEXED | FAST),
        words: b.add_text_field("words", words),
        category: b.add_u64_field("category", INDEXED | FAST),
        cat_rank: b.add_u64_field("cat_rank", FAST),
        size: b.add_u64_field("size", FAST),
        posted: b.add_text_field("posted", STRING | FAST),
        name_key: b.add_text_field("name_key", STRING | FAST),
        group: b.add_text_field("group", STRING | FAST),
        tags: b.add_text_field("tags", STRING),
    };
    (b.build(), fields)
}

/// Whether the index can answer a search for `words`: every word in it is one it keeps. Words that are only
/// punctuation, or one as long as the analyzer drops, go to the database, which matches them as before; read here
/// they would lose the word and match everything.
pub fn searchable(words: &str) -> bool {
    let parts: Vec<&str> = words.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    !parts.is_empty() && parts.iter().all(|w| w.len() < LONGEST_WORD)
}

/// a word this long in bytes, or longer, isn't indexed
const LONGEST_WORD: usize = 64;

/// The deepest a search pages, as the web page's own window: what's past it would have the collector hold it all.
pub const MAX_HITS: usize = 100_000;

/// Postings the words a search word starts may hold, all told, for it to match them too: past it, the word alone.
const PREFIX_POSTINGS: u64 = 200_000;

/// How many postings the words starting with `token` hold across the index, counting no further than `cap`: walks
/// the term dictionaries, never a posting list, and stops at `cap` terms at the most, each holding one or more.
fn prefix_postings(searcher: &Searcher, field: Field, token: &str, cap: u64) -> u64 {
    // no word's UTF-8 has the byte 0xFF, soo every word starting with `token` sorts below this
    let mut past = token.as_bytes().to_vec();
    past.push(0xFF);
    let mut total = 0u64;
    for segment in searcher.segment_readers() {
        let Ok(inverted) = segment.inverted_index(field) else { return cap };
        let Ok(mut terms) = inverted.terms().range().ge(token.as_bytes()).lt(&past).into_stream() else { return cap };
        while terms.advance() {
            total = total.saturating_add(u64::from(terms.value().doc_freq));
            if total >= cap {
                return cap;
            }
        }
    }
    total
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(LONGEST_WORD))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// What a version of the index is made from: this file's version, and the category names, which the category sort
/// goes by.
fn version_stamp() -> String {
    let labels: Vec<String> = sorted_labels().into_iter().map(|(l, _)| l).collect();
    format!("{VERSION}\n{}", labels.join("\n"))
}

/// Every category a release can have, by the name the page writes for it.
fn sorted_labels() -> Vec<(String, i64)> {
    let mut all: Vec<(String, i64)> = TABLE
        .iter()
        .flat_map(|root| root.subs.iter().map(|s| s.id))
        .chain(std::iter::once(UNCATEGORIZED))
        .map(|id| (label(id).unwrap_or_default(), id))
        .collect();
    all.sort();
    all
}

/// Where a category comes in a sort by category name: as many names come before it.
fn category_ranks() -> HashMap<i64, u64> {
    let labels = sorted_labels();
    labels.iter().map(|(name, id)| (*id, labels.iter().filter(|(other, _)| other < name).count() as u64 + 1)).collect()
}

/// The index, and the writer when this process keeps it.
pub struct WordIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    ranks: HashMap<i64, u64>,
    dir: PathBuf,
    ready: AtomicBool,
    /// The database's `word_index` token the index was built from: another one means a new database.
    built_from: AtomicI64,
    writer: Mutex<Option<IndexWriter>>,
    /// Whether this process keeps the index up to date (`keep_up` runs here).
    keeping: AtomicBool,
    /// How a search asks `keep_up` to drain now, and sees when it has.
    pulse: Mutex<Pulse>,
    pulsed: Condvar,
    /// Counts that took a while, kept a moment: a click through a big category counts it once, not on every page.
    counts: Mutex<HashMap<String, (Instant, usize)>>,
}

/// `keep_up`'s steps: asked for, begun, and done, counted.
#[derive(Default)]
struct Pulse {
    asked: bool,
    begun: u64,
    done: u64,
}

/// How long a count with no words that took `COUNT_SLOW` or more is kept.
const COUNT_KEPT: Duration = Duration::from_secs(30);
/// A count this slow is kept: the quick ones are counted again each time.
const COUNT_SLOW: Duration = Duration::from_millis(50);
/// Counts kept at the most, one for each category and filters asked lately.
const COUNTS_KEPT: usize = 256;

/// The longest a search waits for `keep_up` to take in what's queued: past that it reads what's indexed.
const CATCH_UP_WAIT: Duration = Duration::from_millis(500);

impl WordIndex {
    /// The index in `dir`, made empty when it's missing or another version made it.
    pub fn open(dir: &Path) -> tantivy::Result<WordIndex> {
        let (schema, fields) = schema();
        let stamp = version_stamp();
        let stamp_file = dir.join("atlas-version");
        let same = std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp);
        if !same && dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        index.tokenizers().register(WORDS, analyzer());
        std::fs::write(&stamp_file, &stamp)?;
        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        let built_from =
            std::fs::read_to_string(dir.join("atlas-built")).ok().and_then(|s| s.trim().parse::<i64>().ok());
        Ok(WordIndex {
            index,
            reader,
            fields,
            ranks: category_ranks(),
            dir: dir.to_path_buf(),
            ready: AtomicBool::new(built_from.is_some()),
            built_from: AtomicI64::new(built_from.unwrap_or(0)),
            writer: Mutex::new(None),
            keeping: AtomicBool::new(false),
            pulse: Mutex::new(Pulse::default()),
            pulsed: Condvar::new(),
            counts: Mutex::new(HashMap::new()),
        })
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Whether `conn` is the database the index was built from. A database wiped and indexed again reuses
    /// release ids, and words left from the old one would find the new releases that took their ids.
    fn built_from(&self, conn: &Connection) -> rusqlite::Result<bool> {
        let token = database_token(conn)?;
        Ok(token == Some(self.built_from.load(Ordering::Acquire)))
    }

    /// Takes the writer: only one process at a time keeps the index. False: another one has it.
    fn take_writer(&self) -> bool {
        let mut writer = lock(&self.writer);
        if writer.is_none() {
            *writer = self.index.writer(WRITER_HEAP).ok();
        }
        writer.is_some()
    }

    fn doc(&self, row: &Indexed) -> TantivyDocument {
        let f = &self.fields;
        let mut doc = TantivyDocument::default();
        doc.add_u64(f.id, row.id as u64);
        doc.add_text(f.words, &row.name);
        if let Some(shown) = &row.display_name {
            doc.add_text(f.words, shown);
        }
        if let Some(title) = &row.known_as {
            doc.add_text(f.words, title);
        }
        let category = row.category.unwrap_or(UNCATEGORIZED);
        doc.add_u64(f.category, category as u64);
        doc.add_u64(f.cat_rank, self.ranks.get(&category).copied().unwrap_or(0));
        if let Some(size) = row.size.filter(|s| *s >= 0) {
            doc.add_u64(f.size, size as u64);
        }
        if let Some(posted) = row.posted.as_deref().filter(|p| !p.is_empty()) {
            doc.add_text(f.posted, posted);
        }
        let shown = row.display_name.as_deref().unwrap_or(&row.name);
        // as SQLite's lower(), which the sort over a category without words goes by
        doc.add_text(f.name_key, shown.to_ascii_lowercase());
        doc.add_text(f.group, &row.group);
        // most names have no word a tag could come from, and parsing one is most of what indexing it costs
        if may_have_tags(shown) {
            let tags = crate::release_tags::parse(shown);
            for tag in TAGS {
                if tag_matches(tag, &tags) {
                    doc.add_text(f.tags, tag);
                }
            }
        }
        doc
    }

    /// Everything again from the shards. What changes meanwhile is queued, and taken in by the next drain.
    pub fn build(&self, conn: &Connection) -> anyhow::Result<usize> {
        self.build_in(conn, BUILD_BATCH, BUILD_COMMIT_EVERY, || true)
    }

    /// `build`, reading `batch` releases at a time and committing about every `every` of them, with where it got to
    /// in the commit's payload: a build cut short by a restart goes on from its last commit instead of starting again.
    /// It stops after a commit when `go_on` says so.
    fn build_in(
        &self,
        conn: &Connection,
        batch: i64,
        every: usize,
        mut go_on: impl FnMut() -> bool,
    ) -> anyhow::Result<usize> {
        let mut guard = lock(&self.writer);
        let Some(writer) = guard.as_mut() else { anyhow::bail!("another process keeps the word index") };
        let _ = std::fs::remove_file(self.dir.join("atlas-built"));
        self.ready.store(false, Ordering::Release);
        // made once per database, the same `meta` the id counter is kept in
        conn.execute_batch(
            "create table if not exists main.meta (key TEXT PRIMARY KEY, value INTEGER);
             insert or ignore into main.meta (key, value) values ('word_index', random());",
        )?;
        let token = database_token(conn)?.unwrap_or_default();
        // what an attempt that failed added after its last commit: the cursor below is from before it
        writer.rollback()?;
        let resumed = self.index.load_metas()?.payload.as_deref().and_then(|p| building_at(p, token));
        let (first, mut after) = match resumed {
            Some(at) => at,
            None => {
                // forgotten first: whatever changes from here on is queued again
                for s in 0..store::SHARDS {
                    conn.execute(&format!("delete from s{s}.word_queue"), [])?;
                }
                writer.delete_all_documents()?;
                commit_with(writer, &building(token, 0, 0))?;
                (0, 0)
            }
        };
        let mut added = 0;
        let mut since_commit = 0;
        for s in first..store::SHARDS {
            loop {
                let mut rows = read_rows(conn, s, &format!("r.id > ?1 order by r.id limit {batch}"), after)?;
                let Some(last) = rows.last() else { break };
                after = last.id;
                known_as(conn, &mut rows);
                for doc in self.docs(&rows) {
                    writer.add_document(doc)?;
                }
                added += rows.len();
                since_commit += rows.len();
                if since_commit >= every {
                    commit_with(writer, &building(token, s, after))?;
                    since_commit = 0;
                    if !go_on() {
                        return Ok(added);
                    }
                }
            }
            after = 0;
        }
        commit_with(writer, &format!("built {token}"))?;
        std::fs::write(self.dir.join("atlas-built"), token.to_string())?;
        self.reader.reload()?;
        self.built_from.store(token, Ordering::Release);
        self.ready.store(true, Ordering::Release);
        Ok(added)
    }

    /// The documents of `rows`, worked out on every core: reading a name's tags is the slow part.
    fn docs(&self, rows: &[Indexed]) -> Vec<TantivyDocument> {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get()).max(1);
        let per = rows.len().div_ceil(cores).max(1);
        std::thread::scope(|scope| {
            let parts: Vec<_> = rows
                .chunks(per)
                .map(|part| scope.spawn(move || part.iter().map(|r| self.doc(r)).collect::<Vec<_>>()))
                .collect();
            parts.into_iter().flat_map(|p| p.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
        })
    }

    /// Takes what changed into the index: each release queued is read again, and indexed if a client may see it.
    pub fn drain(&self, conn: &Connection) -> anyhow::Result<usize> {
        let mut guard = lock(&self.writer);
        let Some(writer) = guard.as_mut() else { anyhow::bail!("another process keeps the word index") };
        let mut taken: Vec<(usize, i64, i64)> = Vec::new();
        for s in 0..store::SHARDS {
            let left = DRAIN_BATCH - taken.len();
            if left == 0 {
                break;
            }
            let mut stmt = conn
                .prepare_cached(&format!("select release_id, n from s{s}.word_queue order by release_id limit ?1"))?;
            let queued = stmt.query_map([left as i64], |r| Ok((s, r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            taken.extend(queued.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        if taken.is_empty() {
            return Ok(0);
        }
        let mut rows = Vec::new();
        for &(s, id, _) in &taken {
            writer.delete_term(Term::from_field_u64(self.fields.id, id as u64));
            rows.extend(read_rows(conn, s, "r.id = ?1", id)?);
        }
        known_as(conn, &mut rows);
        for doc in self.docs(&rows) {
            writer.add_document(doc)?;
        }
        writer.commit()?;
        // searched straight after, not once the reader looks again
        self.reader.reload()?;
        // a release queued again since it was read keeps its place: its count went up. One shard at a time, each in
        // its own transaction: one over them all would keep the shards it already wrote locked while it waits for
        // the next one's writer, and their writers out (shard 0's saves failed as locked). What a failure leaves
        // queued is read and indexed again the next time
        for s in 0..store::SHARDS {
            if !taken.iter().any(|t| t.0 == s) {
                continue;
            }
            let tx = conn.unchecked_transaction()?;
            {
                let mut delete =
                    tx.prepare_cached(&format!("delete from s{s}.word_queue where release_id = ?1 and n = ?2"))?;
                for &(_, id, n) in taken.iter().filter(|t| t.0 == s) {
                    delete.execute([id, n])?;
                }
            }
            tx.commit()?;
        }
        Ok(taken.len())
    }

    /// A page of what matches `words` in `filter` with every tag in `tags`, sorted `by`, and how many match.
    #[allow(clippy::too_many_arguments)]
    pub fn search(
        &self,
        conn: &Connection,
        words: &str,
        filter: Option<&CatFilter>,
        tags: &[&str],
        by: By,
        descending: bool,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Hits> {
        self.catch_up(conn);
        let query = self.query(words, filter, tags);
        let searcher = self.reader.searcher();
        let (found, total) = self.top(&searcher, query.as_ref(), by, descending, offset, limit)?;
        let mut rows = Vec::with_capacity(found.len());
        for id in found {
            // read again: what a client may see is the database's to say
            if let Some(row) = visible_release(conn, id)? {
                rows.push(row);
            }
        }
        Ok(Hits { rows, total })
    }

    /// What's queued taken in before a search, soo a release saved a moment ago is found and not only once the
    /// background drain comes round. The search only asks `keep_up` to drain now and waits a moment for it: the
    /// drain itself, its reads and its commit, never runs on a request's thread or holds the writer for one.
    fn catch_up(&self, conn: &Connection) {
        if !self.keeping.load(Ordering::Acquire) {
            return;
        }
        let queued = (0..store::SHARDS).any(|s| {
            conn.query_row(&format!("select exists (select 1 from s{s}.word_queue)"), [], |r| r.get::<_, bool>(0))
                .unwrap_or(false)
        });
        if !queued {
            return;
        }
        let mut pulse = lock(&self.pulse);
        // a step already begun may have read the queue before this release was in it: the next one has it
        let wanted = pulse.begun + 1;
        pulse.asked = true;
        self.pulsed.notify_all();
        let _ = self.pulsed.wait_timeout_while(pulse, CATCH_UP_WAIT, |p| p.done < wanted);
    }

    /// `keep_up` waits here between steps: `every`, or until a search asks.
    fn pause(&self, every: Duration) {
        let pulse = lock(&self.pulse);
        let (mut pulse, _) =
            self.pulsed.wait_timeout_while(pulse, every, |p| !p.asked).unwrap_or_else(|e| e.into_inner());
        pulse.asked = false;
    }

    /// A `keep_up` step begins; the number it gets is the one `finished` takes.
    fn begin(&self) -> u64 {
        let mut pulse = lock(&self.pulse);
        pulse.asked = false;
        pulse.begun += 1;
        pulse.begun
    }

    fn finished(&self, step: u64) {
        lock(&self.pulse).done = step;
        self.pulsed.notify_all();
    }

    /// How many releases match, as `search` counts them. A count with no words, a category's or a filter's, may be up
    /// to `COUNT_KEPT` old: a big one reads millions of documents, and a few seconds' new releases don't change it much.
    pub fn count(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str]) -> anyhow::Result<usize> {
        let browsing = words.trim().is_empty();
        if !browsing {
            // what was saved a moment ago is found, soo it's counted too
            let _ = crate::db::with_reader(|conn| {
                self.catch_up(conn);
                Ok(())
            });
        }
        let searcher = self.reader.searcher();
        if browsing && filter.is_none() && tags.is_empty() {
            // every release: what the index holds, with no document read
            return Ok(searcher.num_docs() as usize);
        }
        let key = format!("{words}\u{1f}{filter:?}\u{1f}{tags:?}");
        if browsing
            && let Some(&(at, n)) = lock(&self.counts).get(&key)
            && at.elapsed() < COUNT_KEPT
        {
            return Ok(n);
        }
        let began = Instant::now();
        let n = searcher.search(self.query(words, filter, tags).as_ref(), &Count)?;
        if browsing && began.elapsed() >= COUNT_SLOW {
            let mut counts = lock(&self.counts);
            counts.retain(|_, (at, _)| at.elapsed() < COUNT_KEPT);
            if counts.len() < COUNTS_KEPT {
                counts.insert(key, (Instant::now(), n));
            }
        }
        Ok(n)
    }

    fn query(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str]) -> Box<dyn Query> {
        self.query_with(words, filter, tags, PREFIX_POSTINGS)
    }

    /// `query`, where a word is matched from its start only while the words it starts hold fewer than `cap` postings.
    fn query_with(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str], cap: u64) -> Box<dyn Query> {
        let f = &self.fields;
        let searcher = self.reader.searcher();
        let mut must: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut tokens = Vec::new();
        let mut analyzer = analyzer();
        let mut stream = analyzer.token_stream(words);
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens.sort();
        tokens.dedup();
        // words with nothing in them the index keeps match nothing, not everything in the filter
        if tokens.is_empty() && !words.trim().is_empty() {
            return Box::new(EmptyQuery);
        }
        // each word from its start, as fts5's prefix search: a whole word scores, a longer one only matches. A start
        // so common that reading every word it starts would be the work of seconds, as `a` is, is the word alone, as
        // the database's search takes a common word
        for token in tokens {
            let exact = TermQuery::new(Term::from_field_text(f.words, &token), IndexRecordOption::WithFreqs);
            let pattern = format!("{}.*", regex_escape(&token));
            let prefix = (prefix_postings(&searcher, f.words, &token, cap) < cap)
                .then(|| RegexQuery::from_pattern(&pattern, f.words).ok())
                .flatten();
            let either: Box<dyn Query> = match prefix {
                Some(prefix) => Box::new(BooleanQuery::new(vec![
                    (Occur::Should, Box::new(exact) as Box<dyn Query>),
                    (Occur::Should, Box::new(prefix)),
                ])),
                None => Box::new(exact),
            };
            must.push((Occur::Must, either));
        }
        if let Some(filter) = filter {
            let mut ids: Vec<u64> = filter.leaves.iter().map(|&c| c as u64).collect();
            if filter.uncategorized {
                ids.push(UNCATEGORIZED as u64);
            }
            let terms = ids.into_iter().map(|c| Term::from_field_u64(f.category, c));
            must.push((Occur::Must, Box::new(TermSetQuery::new(terms))));
        }
        for tag in tags {
            let term = Term::from_field_text(f.tags, tag);
            must.push((Occur::Must, Box::new(TermQuery::new(term, IndexRecordOption::Basic))));
        }
        if must.is_empty() {
            return Box::new(AllQuery);
        }
        Box::new(BooleanQuery::new(must))
    }

    /// The ids of a page, best first, and how many matched. Missing values come last either way, and among equals the
    /// newest comes first, as the database's sorts have it.
    fn top(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        by: By,
        descending: bool,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<i64>, usize)> {
        if offset >= MAX_HITS {
            return Ok((Vec::new(), searcher.search(query, &Count)?));
        }
        let limit = limit.min(MAX_HITS - offset);
        let order = if descending { Order::Desc } else { Order::Asc };
        let page = TopDocs::with_limit(limit.max(1)).and_offset(offset);
        let newest = (SortByStaticFastValue::<u64>::for_field("id"), Order::Desc);
        let (found, total) = match by {
            By::Relevance => run(
                searcher,
                query,
                page.order_by(((ShownFirst, Order::Desc), (SortBySimilarityScore, Order::Desc), newest)),
            )?,
            By::Newest => run(searcher, query, page.order_by(newest))?,
            By::Size => {
                run(searcher, query, page.order_by(((SortByStaticFastValue::<u64>::for_field("size"), order), newest)))?
            }
            By::Category => run(
                searcher,
                query,
                page.order_by(((SortByStaticFastValue::<u64>::for_field("cat_rank"), order), newest)),
            )?,
            By::Posted => run(searcher, query, page.order_by(((SortByString::for_field("posted"), order), newest)))?,
            By::Name => run(searcher, query, page.order_by(((SortByString::for_field("name_key"), order), newest)))?,
            By::Group => run(searcher, query, page.order_by(((SortByString::for_field("group"), order), newest)))?,
        };
        let found = if limit == 0 { Vec::new() } else { found };
        let mut ids = Vec::with_capacity(found.len());
        let mut columns = HashMap::new();
        for address in found {
            let column = match columns.entry(address.segment_ord) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(searcher.segment_reader(address.segment_ord).fast_fields().u64("id")?)
                }
            };
            if let Some(id) = column.first(address.doc_id) {
                ids.push(id as i64);
            }
        }
        Ok((ids, total))
    }
}

/// Whether `category` is one of Movies' or TV's: the releases this index is kept for.
fn shown_first(category: u64) -> bool {
    matches!(category / 1000, 2 | 5)
}

/// Movies and episodes ahead of every other match, the best match first within each. A weight on the score wasn't
/// enough: a word counts for more in a short name, and a short album name with the word twice (in its name and its
/// shown name) outscored `Matchbox the Movie`, found by a title added to a long release name, by more than any weight
/// short of this one.
#[derive(Clone, Copy, Debug)]
struct ShownFirst;

impl SortKeyComputer for ShownFirst {
    /// 1 for a movie or an episode, 0 for anything else
    type SortKey = u64;
    type Child = ShownFirstIn;
    type Comparator = NaturalComparator;

    fn segment_sort_key_computer(&self, segment: &tantivy::SegmentReader) -> tantivy::Result<ShownFirstIn> {
        Ok(ShownFirstIn(segment.fast_fields().u64("category")?))
    }
}

/// `ShownFirst` over one segment, with its categories.
struct ShownFirstIn(tantivy::columnar::Column<u64>);

impl SegmentSortKeyComputer for ShownFirstIn {
    type SortKey = u64;
    type SegmentSortKey = u64;
    type SegmentComparator = NaturalComparator;

    fn segment_sort_key(&mut self, doc: tantivy::DocId, _score: Score) -> u64 {
        u64::from(self.0.first(doc).is_some_and(shown_first))
    }

    fn convert_segment_sort_key(&self, first: u64) -> u64 {
        first
    }
}

fn run<K, C>(searcher: &Searcher, query: &dyn Query, top: C) -> tantivy::Result<(Vec<DocAddress>, usize)>
where
    C: Collector<Fruit = Vec<(K, DocAddress)>>,
{
    let (found, total) = searcher.search(query, &(top, Count))?;
    Ok((found.into_iter().map(|(_, address)| address).collect(), total))
}

/// The tag filters, as the page has always told them from a name.
/// Whether `name` holds anything `ptt` makes one of `TAGS` from: a resolution of 1080 or 2160 lines however it's
/// written, a `WxH` size, BluRay, BD, BR or UHD, a remux, WEB, HDR or Dolby Vision. False is certain; true only means
/// the name is worth parsing.
fn may_have_tags(name: &str) -> bool {
    const WORDS: &[&str] = &[
        "1080", "2160", "4k", "fhd", "full hd", "1920", "3840", "4096", "blu", "bd", "br", "uhd", "remux", "web",
        "hdr", "dv", "dolby", "dovi",
    ];
    let lower = name.to_ascii_lowercase();
    if WORDS.iter().any(|w| lower.contains(w)) {
        return true;
    }
    // a frame size, `1998x1080`: the size ptt reads a resolution from
    let b = lower.as_bytes();
    b.iter().enumerate().any(|(i, &c)| {
        c == b'x'
            && i >= 3
            && b[i - 3..i].iter().all(u8::is_ascii_digit)
            && b.get(i + 3).is_some()
            && b[i + 1..i + 4].iter().all(u8::is_ascii_digit)
    })
}

pub fn tag_matches(filter: &str, tags: &crate::release_tags::Tags) -> bool {
    let quality = tags.quality.as_deref().unwrap_or_default();
    match filter {
        "2160p" | "1080p" => tags.resolution.as_deref() == Some(filter),
        "DV" => tags.hdr.iter().any(|hdr| hdr == "DV"),
        "HDR" => tags.hdr.iter().any(|hdr| hdr == "HDR" || hdr == "HDR10+"),
        "Remux" => quality.contains("REMUX"),
        "WEB-DL" => quality == "WEB-DL",
        "BluRay" => quality.starts_with("BluRay"),
        _ => true,
    }
}

fn regex_escape(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        if !c.is_alphanumeric() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A release as it's indexed.
struct Indexed {
    id: i64,
    name: String,
    display_name: Option<String>,
    group: String,
    posted: Option<String>,
    size: Option<i64>,
    category: Option<i64>,
    /// The media title key of a release named by an id in place of its title (`AC317772.2026...`, AlloCiné's).
    title_key: Option<i64>,
    /// What that id is the title of, as the media lookup decoded it: a search for the title finds the release.
    known_as: Option<String>,
}

/// A name that starts with an AlloCiné id in place of the title, as SQLite's `glob` has it.
const ID_NAMED: &str = "'AC[0-9][0-9][0-9][0-9]*'";

/// The releases of shard `s` that a client may see, picked by `clause` with `?1` bound to `value`.
fn read_rows(conn: &Connection, s: usize, clause: &str, value: i64) -> rusqlite::Result<Vec<Indexed>> {
    let sql = format!(
        "select r.id, r.name, r.display_name, r.group_name, r.posted_date, r.size, c.category,
                iif(r.name glob {ID_NAMED} or r.display_name glob {ID_NAMED}, m.title_key, null)
           from s{s}.releases r left join s{s}.release_category c on c.release_id = r.id
                left join s{s}.media_release m on m.release_id = r.id
          where {VISIBLE_RELEASE} and {clause}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    stmt.query_map([value], |r| {
        Ok(Indexed {
            id: r.get(0)?,
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            display_name: r.get(2)?,
            group: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            posted: r.get(4)?,
            size: r.get(5)?,
            category: r.get(6)?,
            title_key: r.get(7)?,
            known_as: None,
        })
    })?
    .collect()
}

/// The decoded titles of the `rows` named by an id, from the media lookup's titles beside the main database. One
/// it hasn't decoded yet, or a media database that can't be read, leaves the release found by its name alone: the
/// lookup's writes to `media_release` queue it again.
fn known_as(conn: &Connection, rows: &mut [Indexed]) {
    if rows.iter().all(|r| r.title_key.is_none()) {
        return;
    }
    let Some(main) = conn.path().filter(|p| !p.is_empty()) else { return };
    let path = crate::media::store::path_for(Path::new(main));
    if !path.exists() {
        return;
    }
    let Ok(media) = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return };
    let Ok(mut stmt) = media.prepare("select found_title, title from media_titles where title_key = ?1") else {
        return;
    };
    for row in rows.iter_mut() {
        let Some(key) = row.title_key else { continue };
        let titles: Option<(Option<String>, String)> =
            stmt.query_row([key], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
        let Some((found, asked)) = titles else { continue };
        // a title cached before ids were decoded is the id itself, which the name already has
        row.known_as = [found, Some(asked)]
            .into_iter()
            .flatten()
            .map(|t| t.trim().to_string())
            .find(|t| !t.is_empty() && crate::media::allocine::allocine_id(t).is_none());
    }
}

/// The queue and the triggers that fill it, in a shard. Its `releases` and `release_category` are there already.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let queue = |id: &str| {
        format!("insert into word_queue (release_id) values ({id}) on conflict (release_id) do update set n = n + 1;")
    };
    conn.execute_batch(&format!(
        "
        -- releases to read again into the word index (`word_index.rs`), with how often they were queued
        create table if not exists word_queue (release_id INTEGER PRIMARY KEY, n INTEGER NOT NULL DEFAULT 1);
        create trigger if not exists word_queue_ai after insert on releases begin {new} end;
        create trigger if not exists word_queue_ad after delete on releases begin {old} end;
        create trigger if not exists word_queue_au after update of name, display_name, group_name, size, posted_date,
            is_obfuscated on releases
        when old.name is not new.name or old.display_name is not new.display_name
            or old.group_name is not new.group_name or old.size is not new.size
            or old.posted_date is not new.posted_date or old.is_obfuscated is not new.is_obfuscated
        begin {new} end;
        create trigger if not exists word_queue_ci after insert on release_category begin {new_c} end;
        create trigger if not exists word_queue_cd after delete on release_category begin {old_c} end;
        create trigger if not exists word_queue_cu after update on release_category begin {new_c} {old_c} end;
        -- a release named by an AlloCiné id is found by the title it decodes to (`known_as`), once the lookup has it
        create trigger if not exists word_queue_mi after insert on media_release
        when exists (select 1 from releases where id = new.release_id and {id_named})
        begin {new_c} end;
        create trigger if not exists word_queue_mu after update of title_key, tvdb_id, tvmaze_id, trakt_id, tmdb_id,
            imdb_id on media_release
        when (old.title_key is not new.title_key or old.tvdb_id is not new.tvdb_id
            or old.tvmaze_id is not new.tvmaze_id or old.trakt_id is not new.trakt_id
            or old.tmdb_id is not new.tmdb_id or old.imdb_id is not new.imdb_id)
            and exists (select 1 from releases where id = new.release_id and {id_named})
        begin {new_c} end;
        ",
        new = queue("new.id"),
        old = queue("old.id"),
        new_c = queue("new.release_id"),
        old_c = queue("old.release_id"),
        id_named = format!("(name glob {ID_NAMED} or display_name glob {ID_NAMED})"),
    ))
}

/// The token `build` leaves in the main database, None when it was never built from this one.
fn database_token(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    let has_meta: bool = conn.query_row(
        "select exists (select 1 from main.sqlite_master where type = 'table' and name = 'meta')",
        [],
        |r| r.get(0),
    )?;
    if !has_meta {
        return Ok(None);
    }
    conn.query_row("select value from main.meta where key = 'word_index'", [], |r| r.get(0)).optional()
}

/// A build's commit payload: the database it's built from, and the shard and last release id it got to.
fn building(token: i64, shard: usize, after: i64) -> String {
    format!("building {token} {shard} {after}")
}

/// Where a build left off, when `payload` is one of `building`'s for the database of `token`.
fn building_at(payload: &str, token: i64) -> Option<(usize, i64)> {
    let mut words = payload.strip_prefix("building ")?.split(' ');
    if words.next()?.parse::<i64>().ok()? != token {
        return None;
    }
    let shard = words.next()?.parse().ok()?;
    let after = words.next()?.parse().ok()?;
    Some((shard, after))
}

fn commit_with(writer: &mut IndexWriter, payload: &str) -> tantivy::Result<()> {
    let mut commit = writer.prepare_commit()?;
    commit.set_payload(payload);
    commit.commit()?;
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- the one this process uses

static INDEX: OnceLock<Option<Arc<WordIndex>>> = OnceLock::new();

/// Where the index of the database at `main` lives.
pub fn dir_for(main: &Path) -> PathBuf {
    main.with_extension("words")
}

/// The index, once it has been built: None sends a search to the database.
pub fn ready() -> Option<Arc<WordIndex>> {
    INDEX.get()?.as_ref().filter(|i| i.ready()).cloned()
}

/// Opens the index and keeps it up to date from here on, building it first when it has to be. Once per process.
pub fn start(main: PathBuf) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(move || {
            let index = match WordIndex::open(&dir_for(&main)) {
                Ok(index) => Arc::new(index),
                Err(e) => {
                    eprintln!("word index: can't open it, searches use the database: {e}");
                    let _ = INDEX.set(None);
                    return;
                }
            };
            let _ = INDEX.set(Some(index.clone()));
            if !index.take_writer() {
                eprintln!("word index: another process keeps it");
                return;
            }
            keep_up(&index, &main);
        });
    });
}

fn keep_up(index: &WordIndex, main: &Path) {
    // said once, not every few seconds while it lasts
    let mut last_error = String::new();
    index.keeping.store(true, Ordering::Release);
    loop {
        let begun = index.begin();
        let step = crate::db::open_with_shards(main).map_err(anyhow::Error::from).and_then(|conn| {
            // its commits to the shards dont copy their WALs each time (see the constant)
            conn.execute_batch(&format!(
                "pragma wal_autocheckpoint = {}",
                crate::db::OCCASIONAL_WRITER_AUTOCHECKPOINT
            ))?;
            if index.ready() && !index.built_from(&conn)? {
                // the database was wiped and started again: what's in the index belongs to the old one
                index.ready.store(false, Ordering::Release);
            }
            if index.ready() {
                index.drain(&conn)
            } else {
                // what's built is in the index's own folder; the menu's screen isn't told
                index.build(&conn).map(|_| 0)
            }
        });
        index.finished(begun);
        match step {
            // more queued than one batch: straight on
            Ok(n) if n >= DRAIN_BATCH => continue,
            Ok(_) => last_error.clear(),
            Err(e) => {
                let error = e.to_string();
                if error != last_error {
                    eprintln!("word index: {error}");
                    last_error = error;
                }
            }
        }
        index.pause(DRAIN_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    struct Fixture {
        dir: tempfile::TempDir,
        main: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        Fixture { dir, main }
    }

    fn release(name: &str, group: &str) -> crate::parser::Release {
        let article = crate::parser::Article {
            message_id: format!("<{name}@x>"),
            subject: format!("\"{name}\" yEnc (1/1)"),
            author: "p <p@x>".into(),
            filename: Some(format!("{name}.bin")),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        };
        crate::parser::Release {
            name: name.into(),
            group: group.into(),
            articles: vec![article],
            size: 10,
            complete: true,
            ..Default::default()
        }
    }

    impl Fixture {
        fn add(&self, name: &str, group: &str, size: i64, category: Option<i64>) -> i64 {
            store::save(&self.main, &[release(name, group)]).unwrap();
            let shard = self.shard(group);
            let id: i64 = shard.query_row("select id from releases where name = ?", [name], |r| r.get(0)).unwrap();
            shard.execute("update releases set size = ?2 where id = ?1", [id, size]).unwrap();
            if let Some(c) = category {
                shard.execute("insert into release_category (release_id, category) values (?, ?)", [id, c]).unwrap();
            }
            id
        }

        fn shard(&self, group: &str) -> Connection {
            db::open_shard(&store::shard_path(&self.main, store::shard_of(group))).unwrap()
        }

        fn index(&self) -> WordIndex {
            let index = WordIndex::open(&self.dir.path().join("atlas.words")).unwrap();
            assert!(index.take_writer());
            index
        }

        fn conn(&self) -> Connection {
            db::open_with_shards(&self.main).unwrap()
        }
    }

    fn ids(hits: &Hits) -> Vec<i64> {
        hits.rows.iter().map(|r| r.id).collect()
    }

    const UHD: i64 = 2045;
    const HD: i64 = 2040;

    #[test]
    fn names_without_a_tag_word_skip_parsing_and_lose_no_tag() {
        let tagged = [
            "Movie.2020.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC-X",
            "Movie 2020 1080p WEB-DL DDP5 1 H 264-Y",
            "Movie.2020.4K.HDR10+.WEB.h265",
            "Movie.2020.1920x800.BRRip",
            "Movie.2020.3840x1600.HEVC",
            "Movie.2020.1998x1080.x264",
            "Movie.2020.FHD.WEBRip",
            "Movie 2020 Full HD x264",
            "Movie.2020.BDRemux",
            "Movie.2020.Dolby.Vision.mkv",
            "Movie.2020.DoVi.mkv",
            "Movie [BD] 2020",
            "Movie.2020.10800p.mkv",
        ];
        let mut with_tags = 0;
        for name in tagged {
            let tags = crate::release_tags::parse(name);
            with_tags += usize::from(TAGS.iter().any(|t| tag_matches(t, &tags)));
            assert!(may_have_tags(name), "{name}");
        }
        assert!(with_tags >= 10, "{with_tags}");
        for name in ["Some.Show.S01E02.720p.HDTV", "a8f3c1d2e4b5", "Album - Artist (2001) [FLAC]", "IMG_0042.jpg"] {
            assert!(!may_have_tags(name), "{name}");
        }
    }

    #[test]
    fn a_build_cut_short_goes_on_from_its_last_commit() {
        let f = fixture();
        for i in 0..40 {
            f.add(&format!("Show.S01E{i:02}.1080p.WEB-DL-G{i}"), &format!("alt.binaries.g{}", i % 5), 10, Some(HD));
        }
        let conn = f.conn();
        {
            let index = f.index();
            let mut commits = 0;
            let added = index
                .build_in(&conn, 3, 6, || {
                    commits += 1;
                    commits < 2
                })
                .unwrap();
            assert!(added > 0 && added < 40, "{added}");
            assert!(!index.ready());
        }
        // the process restarts: a new index on the same folder, which isn't ready
        let index = f.index();
        assert!(!index.ready());
        // an attempt that failed after adding some, past its last commit
        let rows = read_rows(&conn, 0, "r.id > ?1 order by r.id limit 3", 0).unwrap();
        for doc in index.docs(&rows) {
            lock(&index.writer).as_mut().unwrap().add_document(doc).unwrap();
        }
        let rest = index.build_in(&conn, 3, 6, || true).unwrap();
        assert!(rest < 40, "it went on rather than starting again: {rest}");
        assert!(index.ready());
        assert_eq!(index.count("show", None, &[]).unwrap(), 40, "each release once");
        // a finished build starts again from nothing the next time it's asked for
        assert_eq!(index.build(&conn).unwrap(), 40);
        assert_eq!(index.count("show", None, &[]).unwrap(), 40);
    }

    #[test]
    fn a_search_sorts_every_match_and_counts_them() {
        let f = fixture();
        let old = f.add("Batman.Begins.2005.2160p.UHD.BluRay.REMUX-X", "alt.binaries.a", 61_000, Some(UHD));
        let mut newer = Vec::new();
        for i in 0..30 {
            newer.push(f.add(&format!("Batman.Show.S01E{i:02}.1080p.WEB-DL-Y"), "alt.binaries.b", 100 + i, Some(HD)));
        }
        f.add("Superman.2025.1080p.WEB-DL-Z", "alt.binaries.c", 99_000, Some(HD));
        let index = f.index();
        let conn = f.conn();
        assert_eq!(index.build(&conn).unwrap(), 32);
        let movies = crate::category::parse_filter("2000").unwrap();
        let hits = index.search(&conn, "batman", Some(&movies), &[], By::Size, true, 0, 3).unwrap();
        assert_eq!(hits.total, 31);
        assert_eq!(ids(&hits), [old, newer[29], newer[28]]);
        let up = index.search(&conn, "batman", Some(&movies), &[], By::Size, false, 29, 5).unwrap();
        assert_eq!(ids(&up), [newer[29], old], "the last page, past whatever a window held");
        // words from their start, any case, in any order
        assert_eq!(index.count("BAT begins", None, &[]).unwrap(), 1);
        assert_eq!(index.count("atman", None, &[]).unwrap(), 0);
        // tags over all of them
        let remux = index.search(&conn, "batman", None, &["Remux"], By::Relevance, true, 0, 10).unwrap();
        assert_eq!(ids(&remux), [old]);
        assert_eq!(index.count("", Some(&movies), &["WEB-DL"]).unwrap(), 31);
        let uhd = crate::category::parse_filter("2045").unwrap();
        assert_eq!(index.count("batman", Some(&uhd), &[]).unwrap(), 1);
        // with no words: a category's, every release's, and its newest filtered by tag over all of them
        assert_eq!(index.count("", Some(&movies), &[]).unwrap(), 32);
        assert_eq!(index.count("", Some(&uhd), &[]).unwrap(), 1);
        assert_eq!(index.count("", None, &[]).unwrap(), 32);
        let newest = index.search(&conn, "", Some(&movies), &["WEB-DL"], By::Newest, true, 0, 3).unwrap();
        assert_eq!((newest.total, newest.rows.len()), (31, 3));
        assert!(newest.rows.windows(2).all(|w| w[0].id > w[1].id), "newest first: {:?}", ids(&newest));
        // newest first among equals, and by name
        let by_name = index.search(&conn, "show", None, &[], By::Name, false, 0, 2).unwrap();
        assert_eq!(ids(&by_name), [newer[0], newer[1]]);
        // words with nothing the index keeps match nothing here, and go to the database instead
        assert_eq!(index.count("---", Some(&movies), &[]).unwrap(), 0);
        assert!(!searchable("---") && !searchable("") && !searchable(&"a".repeat(64)));
        assert!(searchable("batman 1080p") && searchable(&"a".repeat(63)));
        // a start too common to read every word of is the word alone: `bat` starts 31 releases' words and is none
        let count_with = |words: &str, cap| {
            index.reader.searcher().search(index.query_with(words, None, &[], cap).as_ref(), &Count).unwrap()
        };
        assert_eq!(prefix_postings(&index.reader.searcher(), index.fields.words, "bat", 100), 31);
        assert_eq!(prefix_postings(&index.reader.searcher(), index.fields.words, "bat", 5), 5);
        assert_eq!(count_with("bat", 100), 31);
        assert_eq!(count_with("bat", 5), 0);
        assert_eq!(count_with("batman", 5), 31, "the word itself is still found");
        // past the deepest page: nothing, and the count still
        let far = index.search(&conn, "batman", None, &[], By::Relevance, true, usize::MAX / 2, 50).unwrap();
        assert!(far.rows.is_empty());
        assert_eq!(far.total, 31);
    }

    #[test]
    fn a_drain_writes_one_shard_at_a_time() {
        // only with more than one shard
        if store::SHARDS < 2 {
            return;
        }
        let f = fixture();
        // alt.binaries.c is in shard 0, alt.binaries.b in shard 3
        assert_eq!((store::shard_of("alt.binaries.c"), store::shard_of("alt.binaries.b")), (0, 3));
        let index = f.index();
        let conn = f.conn();
        index.build(&conn).unwrap();
        f.add("Gamma.Movie.2020.1080p-X", "alt.binaries.c", 5, Some(HD));
        f.add("Delta.Movie.2021.1080p-X", "alt.binaries.b", 9, Some(HD));
        let queued = |group: &str| -> i64 {
            f.shard(group).query_row("select count(*) from word_queue", [], |r| r.get(0)).unwrap()
        };
        // shard 3's writer is in a save: the drain waits for it, and fails
        let busy = f.shard("alt.binaries.b");
        busy.execute_batch("begin immediate").unwrap();
        conn.busy_timeout(Duration::ZERO).unwrap();
        assert!(index.drain(&conn).is_err());
        // shard 0 was done and committed before: its writer isn't kept waiting for shard 3's
        assert_eq!((queued("alt.binaries.c"), queued("alt.binaries.b")), (0, 1));
        let other = f.shard("alt.binaries.c");
        other.busy_timeout(Duration::ZERO).unwrap();
        other.execute_batch("begin immediate; commit").unwrap();
        busy.execute_batch("commit").unwrap();
        // the rest is taken in the next time
        assert_eq!(index.drain(&conn).unwrap(), 1);
        assert_eq!(queued("alt.binaries.b"), 0);
        index.reader.reload().unwrap();
        assert_eq!(index.count("delta", None, &[]).unwrap(), 1);
        assert_eq!(index.count("gamma", None, &[]).unwrap(), 1);
    }

    #[test]
    fn a_release_named_by_an_allocine_id_is_found_by_its_decoded_title() {
        let f = fixture();
        let index = f.index();
        let conn = f.conn();
        let group = "alt.binaries.r2d2";
        let ac = f.add("AC317772.2026.MULTi.VFF.2160p.DV.WEB.H265-FRQC", group, 5, Some(UHD));
        let plain = f.add("Other.Movie.2026.2160p.WEB.H265-FRQC", group, 5, Some(UHD));
        let old = f.add("AC267335.2026.MULTi.1080p.WEB.H264-FRQC", group, 5, Some(HD));
        index.build(&conn).unwrap();
        assert_eq!(index.count("matchbox", None, &[]).unwrap(), 0);
        // the media lookup's titles: one decoded, one cached as the id itself before ids were decoded
        let media = crate::media::store::open(&crate::media::store::path_for(&f.main)).unwrap();
        let title = |key: i64, title: &str, found: Option<&str>| crate::media::store::TitleRow {
            key,
            kind: 1,
            title: title.into(),
            found_title: found.map(str::to_string),
            ..Default::default()
        };
        crate::media::store::put(&media, &title(11, "Matchbox: The Movie", Some("Matchbox the Movie"))).unwrap();
        crate::media::store::put(&media, &title(22, "AC267335", None)).unwrap();
        crate::media::store::put(&media, &title(33, "Coyote vs. Acme", None)).unwrap();
        let shard = f.shard(group);
        shard.execute("delete from word_queue", []).unwrap();
        let link = "insert into media_release (release_id, kind, title_key, quality) values (?1, 1, ?2, 0)";
        for (id, key) in [(ac, 11), (old, 22), (plain, 33)] {
            shard.execute(link, [id, key]).unwrap();
        }
        let queued: Vec<i64> = shard
            .prepare("select release_id from word_queue order by release_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(queued, vec![ac, old], "a release named by its title isn't read again for its media row");
        index.drain(&conn).unwrap();
        let found = |words: &str| ids(&index.search(&conn, words, None, &[], By::Relevance, true, 0, 10).unwrap());
        assert_eq!(found("matchbox"), vec![ac]);
        assert_eq!(found("matchbox movie 2160p"), vec![ac], "with the words of its name too");
        assert_eq!(found("ac317772"), vec![ac], "and still by the id");
        assert!(found("coyote").is_empty(), "a title is only added to a release named by an id");
        // the ids found later queue it again, and a build reads the titles too
        shard.execute("update media_release set tmdb_id = 1148365 where release_id = ?1", [ac]).unwrap();
        let n: i64 = shard.query_row("select count(*) from word_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        crate::media::store::put(&media, &title(22, "AC267335", Some("Coyote vs. Acme"))).unwrap();
        index.build(&conn).unwrap();
        assert_eq!(found("matchbox"), vec![ac]);
        assert_eq!(found("coyote acme"), vec![old]);
    }

    #[test]
    fn movies_and_episodes_come_before_every_other_match() {
        let f = fixture();
        let index = f.index();
        let conn = f.conn();
        let g = "alt.binaries.r2d2";
        // as prod has it: a film named by its AlloCiné id, found by the title it decodes to, among short album names
        // that have the word in their shown name too
        let film = f.add("AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC", g, 5, Some(UHD));
        let media = crate::media::store::open(&crate::media::store::path_for(&f.main)).unwrap();
        let title = crate::media::store::TitleRow {
            key: 11,
            kind: 2,
            title: "Matchbox: The Movie".into(),
            found_title: Some("Matchbox: The Movie".into()),
            ..Default::default()
        };
        crate::media::store::put(&media, &title).unwrap();
        f.shard(g)
            .execute("insert into media_release (release_id, kind, title_key, quality) values (?1, 2, 11, 0)", [film])
            .unwrap();
        let mut albums = Vec::new();
        for (name, category) in [
            ("Matchbox Twenty - Unwell", 3010),
            ("Matchbox Twenty - Mad Season", 3040),
            ("Matchbox Twenty - Exile On Mainstream", 3050),
            ("Matchbox.Twenty-Push-1996-MP3", 3010),
            ("Matchbox Twenty - Matchbox Twenty Live", 3050),
        ] {
            let id = f.add(name, g, 5, Some(category));
            f.shard(g).execute("update releases set display_name = name where id = ?1", [id]).unwrap();
            albums.push(id);
        }
        let episode = f.add("Matchbox.Show.S01E02.Second.Episode.Title.1080p.WEB.H264-GRP", g, 5, Some(5040));
        let exact = f.add("Match.Point.Soundtrack.2005.MP3", g, 5, Some(3010));
        index.build(&conn).unwrap();
        let found = |words: &str| ids(&index.search(&conn, words, None, &[], By::Relevance, true, 0, 10).unwrap());
        let first = found("matchbox");
        assert_eq!(first[..2].iter().copied().collect::<std::collections::HashSet<_>>(), [film, episode].into());
        assert_eq!(
            first[2..].iter().copied().collect::<std::collections::HashSet<_>>(),
            albums.iter().copied().collect()
        );
        assert_eq!(found("matchbox twenty").len(), 5, "only the albums have both words");
        // among the others, the best match leads as before
        let other = found("match");
        assert_eq!(other.iter().position(|&id| id == exact), Some(2), "{other:?}");
    }

    #[test]
    fn what_changes_is_taken_in_by_the_next_drain() {
        let f = fixture();
        let a = f.add("Alpha.Movie.2020.1080p-X", "alt.binaries.a", 5, Some(HD));
        let index = f.index();
        let conn = f.conn();
        index.build(&conn).unwrap();
        let b = f.add("Alpha.Other.2021.1080p-X", "alt.binaries.b", 9, None);
        assert_eq!(index.drain(&conn).unwrap(), 1);
        index.reader.reload().unwrap();
        assert_eq!(index.count("alpha", None, &[]).unwrap(), 2);
        let other = CatFilter { leaves: Vec::new(), uncategorized: true };
        assert_eq!(ids(&index.search(&conn, "alpha", Some(&other), &[], By::Relevance, true, 0, 5).unwrap()), [b]);
        // a new name, a category, a size, and a release gone
        f.shard("alt.binaries.a")
            .execute("update releases set display_name = 'Beta.Movie.2020.1080p-X', size = 50 where id = ?", [a])
            .unwrap();
        f.shard("alt.binaries.b").execute("insert into release_category values (?, ?)", [b, UHD]).unwrap();
        index.drain(&conn).unwrap();
        index.reader.reload().unwrap();
        assert_eq!(index.count("beta", None, &[]).unwrap(), 1);
        assert_eq!(index.count("alpha", Some(&crate::category::parse_filter("2045").unwrap()), &[]).unwrap(), 1);
        f.shard("alt.binaries.b").execute("delete from releases where id = ?", [b]).unwrap();
        index.drain(&conn).unwrap();
        index.reader.reload().unwrap();
        assert_eq!(index.count("alpha", None, &[]).unwrap(), 1, "Alpha.Movie is still in its first name");
        assert_eq!(index.drain(&conn).unwrap(), 0, "the queue is empty");
    }

    #[test]
    fn hidden_releases_are_left_out_and_a_new_version_starts_over() {
        let f = fixture();
        f.add("a8f3k2j4h5g6d7s8a9q0", "alt.binaries.a", 5, None);
        let index = f.index();
        let conn = f.conn();
        assert_eq!(index.build(&conn).unwrap(), 0, "an obfuscated name without a real one isn't shown");
        assert!(index.ready());
        drop(index);
        std::fs::write(f.dir.path().join("atlas.words").join("atlas-version"), "0").unwrap();
        assert!(!f.index().ready(), "made by another version: built again");
    }

    #[test]
    fn a_database_started_again_is_indexed_again() {
        let f = fixture();
        f.add("Alpha.Movie.2020.1080p", "alt.binaries.a", 5, None);
        let index = f.index();
        let conn = f.conn();
        assert!(!index.built_from(&conn).unwrap(), "never built from it");
        assert_eq!(index.build(&conn).unwrap(), 1);
        assert!(index.built_from(&conn).unwrap());
        drop(index);
        let index = f.index();
        assert!(index.ready() && index.built_from(&conn).unwrap(), "opened again: the same database");
        // wiped and indexed again: release ids start over, and the token the build left is gone with it
        conn.execute("delete from main.meta where key = 'word_index'", []).unwrap();
        assert!(!index.built_from(&conn).unwrap(), "a new database: built again");
    }
}
