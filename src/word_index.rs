//! Native Tantivy word search with stable tokenization, prefix scoring, tag
//! fields and ordering. PostgreSQL is the source of visible rows. Its canonical
//! owner performs bounded builds and queue delivery through the native module;
//! request readers neither open SQLite nor become index writers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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
use crate::search::ReleaseRow;
pub(crate) mod native;

/// Bumped whenever what's in a document changes: an index made by another version is built again.
const VERSION: u32 = 1;
/// The tokenizer: words of letters and digits, lowercased and without accents, as SQLite's fts5 splits them.
const WORDS: &str = "atlas_words";
/// The tag filters the page offers, worked out of each name once, when it's indexed.
pub const TAGS: &[&str] = &["2160p", "1080p", "DV", "HDR", "Remux", "WEB-DL", "BluRay"];
/// The sole native keeper opens its writer under this heap admission.
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
    writer: Mutex<Option<IndexWriter>>,
    /// Whether this process keeps the index up to date (the native keeper runs here).
    keeping: AtomicBool,
    /// How a search asks the native keeper to drain now, and sees when it has.
    pulse: Mutex<Pulse>,
    pulsed: Condvar,
    /// Counts that took a while, kept a moment: a click through a big category counts it once, not on every page.
    counts: Mutex<HashMap<String, (Instant, usize)>>,
}

/// the native keeper's steps: asked for, begun, and done, counted.
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

/// The longest a search waits for the native keeper to take in what's queued: past that it reads what's indexed.
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
        Ok(WordIndex {
            index,
            reader,
            fields,
            ranks: category_ranks(),
            dir: dir.to_path_buf(),
            ready: AtomicBool::new(false),
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

    /// A page of what matches `words` in `filter` with every tag in `tags`, sorted `by`, and how many match.
    #[allow(clippy::too_many_arguments)]

    /// the native keeper waits here between steps: `every`, or until a search asks.
    fn pause(&self, every: Duration) {
        let pulse = lock(&self.pulse);
        let (mut pulse, _) =
            self.pulsed.wait_timeout_while(pulse, every, |p| !p.asked).unwrap_or_else(|e| e.into_inner());
        pulse.asked = false;
    }

    /// A the native keeper step begins; the number it gets is the one `finished` takes.
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

    fn count_indexed(&self, words: &str, filter: Option<&CatFilter>, tags: &[&str]) -> anyhow::Result<usize> {
        let browsing = words.trim().is_empty();
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
        self.query_in(&self.reader.searcher(), words, filter, tags, cap)
    }

    /// Build prefix/scoring decisions against the same committed searcher
    /// that will evaluate them, including a multi-batch native PG scan.
    fn query_in(
        &self,
        searcher: &Searcher,
        words: &str,
        filter: Option<&CatFilter>,
        tags: &[&str],
        cap: u64,
    ) -> Box<dyn Query> {
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
        // each word from its start, as fts5's prefix search: a whole word scores, a longer one only matches. A start
        // so common that reading every word it starts would be the work of seconds, as `a` is, is the word alone, as
        // the database's search takes a common word
        for token in tokens {
            let exact = TermQuery::new(Term::from_field_text(f.words, &token), IndexRecordOption::WithFreqs);
            let pattern = format!("{}.*", regex_escape(&token));
            let prefix = (prefix_postings(searcher, f.words, &token, cap) < cap)
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
    /// What that id is the title of, as the media lookup decoded it: a search for the title finds the release.
    known_as: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        rows: std::cell::RefCell<Vec<native::Record>>,
    }
    fn fixture() -> Fixture {
        Fixture { dir: tempfile::tempdir().unwrap(), rows: std::cell::RefCell::new(Vec::new()) }
    }
    impl Fixture {
        fn add(&self, name: &str, group: &str, size: i64, category: Option<i64>) -> i64 {
            let mut rows = self.rows.borrow_mut();
            let id = rows.len() as i64 + 1;
            rows.push(native::Record {
                id,
                name: name.into(),
                display_name: None,
                group: group.into(),
                posted: None,
                size: Some(size),
                category,
                known_as: None,
            });
            id
        }
        fn set_display(&self, id: i64, name: &str) {
            self.rows.borrow_mut().iter_mut().find(|row| row.id == id).unwrap().display_name = Some(name.into());
        }
        fn set_known(&self, id: i64, name: &str) {
            self.rows.borrow_mut().iter_mut().find(|row| row.id == id).unwrap().known_as = Some(name.into());
        }
        fn remove(&self, id: i64) {
            self.rows.borrow_mut().retain(|row| row.id != id);
        }
    }
    fn ids(hits: &Hits) -> Vec<i64> {
        hits.rows.iter().map(|row| row.id).collect()
    }
    const UHD: i64 = 2045;
    const HD: i64 = 2040;
    struct NativeFixtureSource {
        rows: Vec<native::Record>,
        releases: Vec<ReleaseRow>,
        queued: Vec<native::Changed>,
        fail_ack: bool,
        expected_extra: u64,
    }
    impl NativeFixtureSource {
        fn from_fixture(fixture: &Fixture) -> Self {
            let rows = fixture.rows.borrow().clone();
            let releases = rows
                .iter()
                .map(|row| ReleaseRow {
                    id: row.id,
                    name: row.display_name.as_ref().unwrap_or(&row.name).clone(),
                    group_name: row.group.clone(),
                    posted_date: row.posted.clone(),
                    size: row.size,
                    complete: true,
                    ..Default::default()
                })
                .collect();
            Self { rows, releases, queued: Vec::new(), fail_ack: false, expected_extra: 0 }
        }
    }
    impl native::SearchSource for NativeFixtureSource {
        fn stamp(&mut self) -> anyhow::Result<[u8; 32]> {
            Ok([7; 32])
        }
        fn pending(&mut self) -> anyhow::Result<bool> {
            Ok(!self.queued.is_empty())
        }
        fn visible_rows(&mut self, ids: &[i64], _budget: native::Budget) -> anyhow::Result<Vec<ReleaseRow>> {
            Ok(ids.iter().filter_map(|id| self.releases.iter().find(|row| row.id == *id).cloned()).collect())
        }
    }
    impl native::BuildSource for NativeFixtureSource {
        fn expected_documents(&mut self) -> anyhow::Result<u64> {
            Ok(self.rows.len() as u64 + self.expected_extra)
        }
        fn build_page(&mut self, after: i64, budget: native::Budget) -> anyhow::Result<Vec<native::Record>> {
            Ok(self.rows.iter().filter(|row| row.id > after).take(budget.rows).cloned().collect())
        }
    }
    impl native::Source for NativeFixtureSource {
        fn changes(&mut self, budget: native::Budget) -> anyhow::Result<native::Changes> {
            let receipts: Vec<_> = self.queued.iter().take(budget.rows).copied().collect();
            let rows =
                self.rows.iter().filter(|row| receipts.iter().any(|receipt| receipt.id == row.id)).cloned().collect();
            Ok(native::Changes { receipts, rows })
        }
        fn acknowledge(
            &mut self,
            commit: &native::IndexCommitReceipt,
            receipts: &[native::Changed],
        ) -> anyhow::Result<()> {
            assert_eq!(commit.stamp(), &[7; 32]);
            assert_eq!(
                *commit.batch_digest(),
                native::batch_digest(
                    [7; 32],
                    receipts,
                    &self
                        .rows
                        .iter()
                        .filter(|row| receipts.iter().any(|claim| claim.id == row.id))
                        .cloned()
                        .collect::<Vec<_>>()
                )?
            );
            if self.fail_ack {
                self.fail_ack = false;
                anyhow::bail!("synthetic lost acknowledgment");
            }
            self.queued.retain(|queued| !receipts.contains(queued));
            Ok(())
        }
    }

    #[test]
    fn native_prefix_display_tag_and_sort_golden_vectors() {
        let f = fixture();
        f.add("Café.Alpha.Movie.2020.2160p.UHD.BluRay.REMUX-X", "alt.binaries.a", 61000, Some(UHD));
        let renamed = f.add("Alpha.Movie.2021.1080p.WEB-DL-Y", "alt.binaries.b", 500, Some(HD));
        f.add("Alpha.Show.S01E01.1080p.WEB-DL-Y", "alt.binaries.c", 400, Some(HD));
        f.add("Unrelated.2025.1080p-W", "alt.binaries.d", 900, None);
        f.set_display(renamed, "Beta.Alpha.2021.1080p.WEB-DL-Y");
        let mut source = NativeFixtureSource::from_fixture(&f);
        let index = WordIndex::open_native(&f.dir.path().join("native.words"), [7; 32]).unwrap();
        assert!(index.build_native(&mut source, native::Budget::default(), || true).unwrap().complete);
        let hd = crate::category::parse_filter("2040").unwrap();
        for (words, matching) in [
            ("", vec![1, 2, 3, 4]),
            ("alpha", vec![1, 2, 3]),
            ("alph", vec![1, 2, 3]),
            ("cafe", vec![1]),
            ("beta", vec![2]),
        ] {
            for by in [By::Relevance, By::Newest, By::Size, By::Posted, By::Name, By::Group, By::Category] {
                for descending in [false, true] {
                    for (filter, tags) in [(None, Vec::new()), (Some(&hd), Vec::new()), (None, vec!["2160p"])] {
                        let mut expected = matching.clone();
                        if filter.is_some() {
                            expected.retain(|id| [2, 3].contains(id));
                        }
                        if !tags.is_empty() {
                            expected.retain(|id| *id == 1);
                        }
                        let hits = index
                            .search_native(
                                &mut source,
                                words,
                                filter,
                                &tags,
                                by,
                                descending,
                                0,
                                10,
                                native::Budget::default(),
                            )
                            .unwrap();
                        let mut found = ids(&hits);
                        found.sort_unstable();
                        assert_eq!(
                            (found, hits.total),
                            (expected.clone(), expected.len()),
                            "{words} {by:?} {descending}"
                        );
                    }
                }
            }
        }
        for (by, descending, expected) in [
            (By::Size, false, vec![3, 2, 4, 1]),
            (By::Size, true, vec![1, 4, 2, 3]),
            (By::Name, false, vec![3, 2, 1, 4]),
            (By::Name, true, vec![4, 1, 2, 3]),
            (By::Group, false, vec![1, 2, 3, 4]),
            (By::Group, true, vec![4, 3, 2, 1]),
            (By::Category, false, vec![3, 2, 1, 4]),
            (By::Category, true, vec![4, 1, 3, 2]),
            (By::Posted, false, vec![4, 3, 2, 1]),
            (By::Posted, true, vec![4, 3, 2, 1]),
        ] {
            let hits = index
                .search_native(&mut source, "", None, &[], by, descending, 0, 10, native::Budget::default())
                .unwrap();
            assert_eq!(ids(&hits), expected, "{by:?} {descending}");
        }
    }

    #[test]
    fn native_membership_pins_one_committed_searcher_across_reload_between_batches() {
        let f = fixture();
        f.add("Alpha.Movie.2020.1080p-X", "alt.binaries.a", 10, Some(HD));
        f.add("Alpha.Movie.2021.1080p-X", "alt.binaries.b", 11, Some(HD));
        let mut source = NativeFixtureSource::from_fixture(&f);
        let index = WordIndex::open_native(&f.dir.path().join("native.words"), [7; 32]).unwrap();
        assert!(index.build_native(&mut source, native::Budget::default(), || true).unwrap().complete);
        let first = source.rows[0].id;
        let second = source.rows[1].id;
        let pinned =
            index.native_membership(&mut source, "alph", None, &["1080p"], None, native::Budget::default()).unwrap();
        assert_eq!(pinned.matching_ids(&[first]).unwrap(), [first]);
        source.rows[1].name = "Unrelated.Movie.2021.2160p-X".into();
        source.releases.iter_mut().find(|row| row.id == second).unwrap().name = source.rows[1].name.clone();
        source.queued.push(native::Changed { id: second, version: 1 });
        assert_eq!(index.drain_native(&mut source, native::Budget::default()).unwrap(), 1);
        assert_eq!(
            index.count_native(&mut source, "alph", None, &["1080p"]).unwrap(),
            1,
            "new committed reader really changed"
        );
        assert_eq!(
            pinned.matching_ids(&[second]).unwrap(),
            [second],
            "later batch retains the original committed membership"
        );
        let fresh =
            index.native_membership(&mut source, "alph", None, &["1080p"], None, native::Budget::default()).unwrap();
        assert!(fresh.matching_ids(&[second]).unwrap().is_empty());
        assert_eq!(fresh.matching_ids(&[first]).unwrap(), [first]);
    }

    #[test]
    fn native_request_open_cannot_create_or_reset_an_index() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert!(WordIndex::read_native(&missing, [7; 32]).is_err());
        assert!(!missing.exists());
        let old = root.path().join("different-codec");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("atlas-version"), b"another-codec").unwrap();
        std::fs::write(old.join("keep"), b"preserve-existing-index-bytes").unwrap();
        assert!(WordIndex::read_native(&old, [7; 32]).is_err());
        assert_eq!(std::fs::read(old.join("atlas-version")).unwrap(), b"another-codec");
        assert_eq!(std::fs::read(old.join("keep")).unwrap(), b"preserve-existing-index-bytes");
    }

    #[test]
    fn native_new_read_snapshot_restarts_unfinished_base_and_preserves_ready_index() {
        let f = fixture();
        for i in 0..5 {
            f.add(&format!("Alpha.Movie.202{i}.1080p-X"), "alt.binaries.a", 10 + i, Some(HD));
        }
        let mut first = NativeFixtureSource::from_fixture(&f);
        let directory = f.dir.path().join("native.words");
        let stamp = [7; 32];
        let index = WordIndex::open_native(&directory, stamp).unwrap();
        let budget = native::Budget { rows: 1, bytes: 512, commit_bytes: 512 };
        let partial = index.build_native(&mut first, budget, || false).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.documents, 1);
        let removed = first.rows[0].id;
        drop(first);
        f.remove(removed);
        let mut changed = NativeFixtureSource::from_fixture(&f);
        assert_eq!(changed.rows.len(), 4);
        index.restart_native_build(stamp).unwrap();
        assert_eq!(index.build_native(&mut changed, budget, || true).unwrap().documents, 4);
        let found = index
            .search_native(&mut changed, "alpha", None, &[], By::Newest, true, 0, 10, native::Budget::default())
            .unwrap();
        assert_eq!(found.rows.len(), 4);
        assert!(!found.rows.iter().any(|row| row.id == removed));
        assert!(index.restart_native_build(stamp).is_err(), "ready indexes catch up through the durable queue");
        assert!(index.native_ready(stamp).unwrap());
    }

    #[test]
    fn native_tantivy_resume_and_ack_replay_never_duplicate_public_ids() {
        let f = fixture();
        for i in 0..5 {
            f.add(&format!("Alpha.Movie.202{i}.1080p-X"), "alt.binaries.a", 10 + i, Some(HD));
        }

        let mut source = NativeFixtureSource::from_fixture(&f);
        let directory = f.dir.path().join("native.words");
        let budget = native::Budget { rows: 1, bytes: 512, commit_bytes: 512 };
        {
            let index = WordIndex::open_native(&directory, [7; 32]).unwrap();
            let progress = index.build_native(&mut source, budget, || false).unwrap();
            assert!(!progress.complete && progress.documents == 1 && progress.after == source.rows[0].id);
            assert!(!index.native_ready([7; 32]).unwrap());
        }
        let index = WordIndex::open_native(&directory, [7; 32]).unwrap();
        assert_eq!(index.build_native(&mut source, budget, || true).unwrap().documents, 5);
        let request_index = WordIndex::read_native(&directory, [7; 32]).unwrap();
        assert_eq!(request_index.count_native(&mut source, "alpha", None, &[]).unwrap(), 5);
        assert!(WordIndex::read_native(&directory, [8; 32]).is_err());
        assert!(!index.native_ready([8; 32]).unwrap(), "another generation cannot reuse saved public IDs");
        let changed = source.rows[0].id;
        source.rows[0].name = "Gamma.Movie.2020.1080p-X".into();
        source.releases.iter_mut().find(|row| row.id == changed).unwrap().name = source.rows[0].name.clone();
        source.queued.push(native::Changed { id: changed, version: 1 });
        source.fail_ack = true;
        assert!(index.drain_native(&mut source, native::Budget::default()).is_err());
        assert_eq!(source.queued.len(), 1);
        assert_eq!(index.drain_native(&mut source, native::Budget::default()).unwrap(), 1);
        assert!(source.queued.is_empty());
        let hits = index
            .search_native(&mut source, "", None, &[], By::Newest, true, 0, 10, native::Budget::default())
            .unwrap();
        assert_eq!(hits.rows.len(), 5);
        assert_eq!(ids(&hits).into_iter().collect::<std::collections::HashSet<_>>().len(), 5);
        assert_eq!(index.count_native(&mut source, "gamma", None, &[]).unwrap(), 1);
    }

    #[test]
    fn native_tantivy_cannot_mark_incomplete_source_coverage_ready() {
        let f = fixture();
        f.add("Alpha.Movie.2020.1080p-X", "alt.binaries.a", 10, Some(HD));
        let mut source = NativeFixtureSource::from_fixture(&f);
        source.expected_extra = 1;
        let index = WordIndex::open_native(&f.dir.path().join("native.words"), [7; 32]).unwrap();
        assert!(index.build_native(&mut source, native::Budget::default(), || true).is_err());
        assert!(!index.native_ready([7; 32]).unwrap());
    }

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
    fn a_search_sorts_every_match_and_counts_them() {
        let f = fixture();
        let old = f.add("Batman.Begins.2005.2160p.UHD.BluRay.REMUX-X", "alt.binaries.a", 61_000, Some(UHD));
        let mut newer = Vec::new();
        for i in 0..30 {
            newer.push(f.add(&format!("Batman.Show.S01E{i:02}.1080p.WEB-DL-Y"), "alt.binaries.b", 100 + i, Some(HD)));
        }
        f.add("Superman.2025.1080p.WEB-DL-Z", "alt.binaries.c", 99_000, Some(HD));
        let mut source = NativeFixtureSource::from_fixture(&f);
        let index = WordIndex::open_native(&f.dir.path().join("native.words"), [7; 32]).unwrap();
        assert_eq!(index.build_native(&mut source, native::Budget::default(), || true).unwrap().documents, 32);
        let movies = crate::category::parse_filter("2000").unwrap();
        let hits = index
            .search_native(&mut source, "batman", Some(&movies), &[], By::Size, true, 0, 3, native::Budget::default())
            .unwrap();
        assert_eq!(hits.total, 31);
        assert_eq!(ids(&hits), [old, newer[29], newer[28]]);
        let up = index
            .search_native(&mut source, "batman", Some(&movies), &[], By::Size, false, 29, 5, native::Budget::default())
            .unwrap();
        assert_eq!(ids(&up), [newer[29], old], "the last page, past whatever a window held");
        // words from their start, any case, in any order
        assert_eq!(index.count_native(&mut source, "BAT begins", None, &[]).unwrap(), 1);
        assert_eq!(index.count_native(&mut source, "atman", None, &[]).unwrap(), 0);
        // tags over all of them
        let remux = index
            .search_native(
                &mut source,
                "batman",
                None,
                &["Remux"],
                By::Relevance,
                true,
                0,
                10,
                native::Budget::default(),
            )
            .unwrap();
        assert_eq!(ids(&remux), [old]);
        assert_eq!(index.count_native(&mut source, "", Some(&movies), &["WEB-DL"]).unwrap(), 31);
        let uhd = crate::category::parse_filter("2045").unwrap();
        assert_eq!(index.count_native(&mut source, "batman", Some(&uhd), &[]).unwrap(), 1);
        // with no words: a category's, every release's, and its newest filtered by tag over all of them
        assert_eq!(index.count_native(&mut source, "", Some(&movies), &[]).unwrap(), 32);
        assert_eq!(index.count_native(&mut source, "", Some(&uhd), &[]).unwrap(), 1);
        assert_eq!(index.count_native(&mut source, "", None, &[]).unwrap(), 32);
        let newest = index
            .search_native(
                &mut source,
                "",
                Some(&movies),
                &["WEB-DL"],
                By::Newest,
                true,
                0,
                3,
                native::Budget::default(),
            )
            .unwrap();
        assert_eq!((newest.total, newest.rows.len()), (31, 3));
        assert!(newest.rows.windows(2).all(|w| w[0].id > w[1].id), "newest first: {:?}", ids(&newest));
        // newest first among equals, and by name
        let by_name = index
            .search_native(&mut source, "show", None, &[], By::Name, false, 0, 2, native::Budget::default())
            .unwrap();
        assert_eq!(ids(&by_name), [newer[0], newer[1]]);
        // words with nothing the index keeps match nothing here, and go to the database instead
        assert_eq!(index.count_native(&mut source, "---", Some(&movies), &[]).unwrap(), 0);
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
        let far = index
            .search_native(
                &mut source,
                "batman",
                None,
                &[],
                By::Relevance,
                true,
                usize::MAX / 2,
                50,
                native::Budget::default(),
            )
            .unwrap();
        assert!(far.rows.is_empty());
        assert_eq!(far.total, 31);
    }

    #[test]
    fn movies_and_episodes_come_before_every_other_match() {
        let f = fixture();
        let g = "alt.binaries.r2d2";
        // as prod has it: a film named by its AlloCiné id, found by the title it decodes to, among short album names
        // that have the word in their shown name too
        let film = f.add("AC317772.2026.MULTi.VFF.AD.2160p.HDR.DV.WEB.EAC3.5.1.Atmos.H265-FRQC", g, 5, Some(UHD));
        f.set_known(film, "Matchbox: The Movie");
        let mut albums = Vec::new();
        for (name, category) in [
            ("Matchbox Twenty - Unwell", 3010),
            ("Matchbox Twenty - Mad Season", 3040),
            ("Matchbox Twenty - Exile On Mainstream", 3050),
            ("Matchbox.Twenty-Push-1996-MP3", 3010),
            ("Matchbox Twenty - Matchbox Twenty Live", 3050),
        ] {
            let id = f.add(name, g, 5, Some(category));
            f.set_display(id, name);
            albums.push(id);
        }
        let episode = f.add("Matchbox.Show.S01E02.Second.Episode.Title.1080p.WEB.H264-GRP", g, 5, Some(5040));
        let exact = f.add("Match.Point.Soundtrack.2005.MP3", g, 5, Some(3010));
        let mut source = NativeFixtureSource::from_fixture(&f);
        let index = WordIndex::open_native(&f.dir.path().join("native.words"), [7; 32]).unwrap();
        assert!(index.build_native(&mut source, native::Budget::default(), || true).unwrap().complete);
        let mut found = |words: &str| {
            ids(&index
                .search_native(&mut source, words, None, &[], By::Relevance, true, 0, 10, native::Budget::default())
                .unwrap())
        };
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
}
