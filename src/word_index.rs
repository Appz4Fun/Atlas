//! Every word search over every release: a Tantivy index of release names kept next to the database, with what a page
//! sorts and filters by in fast fields, soo a search ranks, sorts and filters all of its matches, not the newest few
//! thousand of them.
//!
//! The database stays what's true. Triggers in each shard put the id of a release that changed, or whose category did,
//! in `word_queue`, and the web server's process takes them from there into the index every few seconds. An index
//! that's missing, or made by another version of this file, is built again from the shards in the background, and
//! searches go to the database as before until it's ready. Each hit is read from the database again before it's shown,
//! soo an index a few seconds behind never shows a release a client may no longer see.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use tantivy::collector::sort_key::{SortBySimilarityScore, SortByStaticFastValue, SortByString};
use tantivy::collector::{Collector, Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, EmptyQuery, Occur, Query, RegexQuery, TermQuery, TermSetQuery};
use tantivy::schema::{FAST, Field, INDEXED, IndexRecordOption, STRING, Schema, TextFieldIndexing, TextOptions};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Searcher, TantivyDocument, Term};

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
const WRITER_HEAP: usize = 256 * 1024 * 1024;

/// What a search sorts its matches by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// best match first
    Relevance,
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
}

/// `keep_up`'s steps: asked for, begun, and done, counted.
#[derive(Default)]
struct Pulse {
    asked: bool,
    begun: u64,
    done: u64,
}

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
        let tags = crate::release_tags::parse(shown);
        for tag in TAGS {
            if tag_matches(tag, &tags) {
                doc.add_text(f.tags, tag);
            }
        }
        doc
    }

    /// Everything again from the shards. What changes meanwhile is queued, and taken in by the next drain.
    pub fn build(&self, conn: &Connection) -> anyhow::Result<usize> {
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
        // forgotten first: whatever changes from here on is queued again
        for s in 0..store::SHARDS {
            conn.execute(&format!("delete from s{s}.word_queue"), [])?;
        }
        writer.delete_all_documents()?;
        let mut added = 0;
        for s in 0..store::SHARDS {
            let mut after = 0i64;
            loop {
                let rows = read_rows(conn, s, &format!("r.id > ?1 order by r.id limit {BUILD_BATCH}"), after)?;
                let Some(last) = rows.last() else { break };
                after = last.id;
                for doc in self.docs(&rows) {
                    writer.add_document(doc)?;
                }
                added += rows.len();
            }
        }
        writer.commit()?;
        std::fs::write(self.dir.join("atlas-built"), token.to_string())?;
        self.reader.reload()?;
        self.built_from.store(token, Ordering::Release);
        self.ready.store(true, Ordering::Release);
        Ok(added)
    }

    /// The documents of `rows`, worked out on every core: reading a name's tags is the slow part.
    fn docs(&self, rows: &[Indexed]) -> Vec<TantivyDocument> {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
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
        for doc in self.docs(&rows) {
            writer.add_document(doc)?;
        }
        writer.commit()?;
        // searched straight after, not once the reader looks again
        self.reader.reload()?;
        // a release queued again since it was read keeps its place: its count went up
        let tx = conn.unchecked_transaction()?;
        for &(s, id, n) in &taken {
            tx.prepare_cached(&format!("delete from s{s}.word_queue where release_id = ?1 and n = ?2"))?
                .execute([id, n])?;
        }
        tx.commit()?;
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

    /// How many releases match, as `search` counts them.
    pub fn count(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str]) -> anyhow::Result<usize> {
        let _ = crate::db::with_reader(|conn| {
            self.catch_up(conn);
            Ok(())
        });
        Ok(self.reader.searcher().search(self.query(words, filter, tags).as_ref(), &Count)?)
    }

    fn query(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str]) -> Box<dyn Query> {
        let f = &self.fields;
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
        // each word from its start, as fts5's prefix search: a whole word scores, a longer one only matches
        for token in tokens {
            let exact = TermQuery::new(Term::from_field_text(f.words, &token), IndexRecordOption::WithFreqs);
            let pattern = format!("{}.*", regex_escape(&token));
            let either: Box<dyn Query> = match RegexQuery::from_pattern(&pattern, f.words) {
                Ok(prefix) => Box::new(BooleanQuery::new(vec![
                    (Occur::Should, Box::new(exact) as Box<dyn Query>),
                    (Occur::Should, Box::new(prefix)),
                ])),
                Err(_) => Box::new(exact),
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
            By::Relevance => run(searcher, query, page.order_by(((SortBySimilarityScore, Order::Desc), newest)))?,
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

fn run<K, C>(searcher: &Searcher, query: &dyn Query, top: C) -> tantivy::Result<(Vec<DocAddress>, usize)>
where
    C: Collector<Fruit = Vec<(K, DocAddress)>>,
{
    let (found, total) = searcher.search(query, &(top, Count))?;
    Ok((found.into_iter().map(|(_, address)| address).collect(), total))
}

/// The tag filters, as the page has always told them from a name.
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
}

/// The releases of shard `s` that a client may see, picked by `clause` with `?1` bound to `value`.
fn read_rows(conn: &Connection, s: usize, clause: &str, value: i64) -> rusqlite::Result<Vec<Indexed>> {
    let sql = format!(
        "select r.id, r.name, r.display_name, r.group_name, r.posted_date, r.size, c.category
           from s{s}.releases r left join s{s}.release_category c on c.release_id = r.id
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
        })
    })?
    .collect()
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
        ",
        new = queue("new.id"),
        old = queue("old.id"),
        new_c = queue("new.release_id"),
        old_c = queue("old.release_id"),
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
        // newest first among equals, and by name
        let by_name = index.search(&conn, "show", None, &[], By::Name, false, 0, 2).unwrap();
        assert_eq!(ids(&by_name), [newer[0], newer[1]]);
        // words with nothing the index keeps match nothing here, and go to the database instead
        assert_eq!(index.count("---", Some(&movies), &[]).unwrap(), 0);
        assert!(!searchable("---") && !searchable("") && !searchable(&"a".repeat(64)));
        assert!(searchable("batman 1080p") && searchable(&"a".repeat(63)));
        // past the deepest page: nothing, and the count still
        let far = index.search(&conn, "batman", None, &[], By::Relevance, true, usize::MAX / 2, 50).unwrap();
        assert!(far.rows.is_empty());
        assert_eq!(far.total, 31);
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
