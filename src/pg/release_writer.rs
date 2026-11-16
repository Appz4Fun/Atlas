//! The native PostgreSQL writer for parsed releases. A save is one transaction
//! and takes its groups' advisory locks in order. COPY and set-based statements
//! keep round trips proportional to batches instead of articles.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;

use anyhow::{Result, ensure};
use postgres::binary_copy::BinaryCopyInWriter;
use postgres::types::Type;
use postgres::{Client, IsolationLevel, Transaction};
use sha2::{Digest, Sha256};

use crate::parser::{Article, Release};

const MAX_BATCH_ROWS: usize = 16_384;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

#[path = "release_writer_timing.rs"]
mod timing;

/// Rows newly created by this save. A retry of an already committed save
/// returns zero for the rows its previous attempt committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveStats {
    pub releases: u64,
    pub files: u64,
    pub parts: u64,
}

struct Incoming<'a> {
    release: &'a Release,
    release_key: [u8; 32],
    original_name_key: [u8; 32],
    posted_at: i64,
    posted_clock: Option<i64>,
    article: Option<&'a Article>,
}

pub(crate) fn identity(release: &Release) -> (bool, [u8; 32]) {
    let key = Sha256::digest(release.collection.as_deref().unwrap_or(&release.name)).into();
    (release.collection.is_some(), key)
}

impl Incoming<'_> {
    fn approx_bytes(&self) -> usize {
        let r = self.release;
        let article = self
            .article
            .map_or(0, |a| a.filename.as_deref().map_or(0, str::len) + a.subject.len() + a.message_id.len());
        let rare = self.article.filter(|a| a.filename.is_none())
            .map_or(0, |a| a.subject.len() + r.poster.len() + r.date.len() + 32);
        // Include the extra scalar timing fields and the possible ID-derived
        // host/key text, whose combined length cannot exceed the original MID.
        416 + self.article.map_or(0, |a| a.message_id.len())
            + 2*r.name.len() + r.poster.len() + r.date.len() + r.display_name.as_deref().map_or(0, str::len) + article + rare
            + self.article.and_then(|a|a.filename.as_deref()).map_or(0,str::len)
    }
}

/// Save parsed releases together and return only after their transaction
/// commits. The exact article identity is independent of the file name, so a
/// replay or concurrent retry cannot change counts or queue generation.
pub fn save(client: &mut Client, releases: &[Release], now: i64) -> Result<SaveStats> {
    let mut groups = BTreeMap::<&str, Vec<&Release>>::new();
    for release in releases {
        if !crate::filters::group_blocked(&release.group) {
            ensure!(!release.group.as_bytes().contains(&0), "release group contains a NUL byte");
            groups.entry(&release.group).or_default().push(release);
        }
    }
    if groups.is_empty() {
        return Ok(SaveStats::default());
    }

    // A group writer that waited on the advisory lock must see the preceding commit.
    let mut tx = client.build_transaction().isolation_level(IsolationLevel::ReadCommitted).start()?;
    let (stats, local) = save_grouped(&mut tx, groups, now)?;
    tx.commit()?;
    // A commit seen here needs no question later; one that failed is asked about.
    // The save stands either way: one left unconfirmed is asked about later.
    if let Some(Local { store, xid: Some(xid) }) = local {
        let _ = store.confirm(&xid);
    }
    Ok(stats)
}

/// The repair service checks its observed source and commits header/results on
/// its original owner transaction. This entry neither commits nor retries.
/// The same RC/history gate and exact COPY/winner SQL as ordinary saves apply.
pub(crate) fn save_in(tx:&mut Transaction<'_>,releases:&[Release],now:i64)->Result<SaveStats> {
    let mut groups=BTreeMap::<&str,Vec<&Release>>::new();
    for release in releases {
        if !crate::filters::group_blocked(&release.group) {
            ensure!(!release.group.as_bytes().contains(&0),"release group contains a NUL byte");
            groups.entry(&release.group).or_default().push(release);
        }
    }
    Ok(save_grouped(tx,groups,now)?.0)
}

fn save_grouped(tx:&mut Transaction<'_>,groups:BTreeMap<&str,Vec<&Release>>,now:i64)->Result<(SaveStats,Option<Local>)> {
    super::history_gate::native(tx)?;
    tx.batch_execute(CREATE_STAGE)?;
    let mut stats = SaveStats::default();
    let mut timing_key = None;
    // With a local article store the articles go there and PostgreSQL keeps
    // the releases, files and counts. A save that died after writing there is
    // settled first, so its articles are either counted or gone before `has`.
    let mut local = crate::article_local::of(tx)?.map(|store| Local { store, xid: None });
    if local.is_some() {
        tx.batch_execute(CREATE_FOLD_STAGE)?;
    }
    for (group, releases) in groups {
        tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1, 7936))", &[&group])?;
        // Another connection's save of this group has ended now that its lock is ours:
        // its articles are counted or gone before this save asks which it already has.
        if let Some(local) = &local
            && local.store.has_unsettled_besides(local.xid.as_deref())?
        {
            local.store.settle(|xid| crate::article_local::pg_ended(tx, xid))?;
        }
        // A junk marker applies to all fragments with this identity, including
        // fragments that would otherwise fill an earlier COPY batch.
        let identities: Vec<_> = releases.iter().map(|release| identity(release)).collect();
        let mut junk: HashMap<_, _> = releases
            .iter()
            .zip(&identities)
            .filter(|(release, _)| {
                release
                    .articles
                    .iter()
                    .any(|article| article.filename.as_deref().is_some_and(crate::junk::is_executable))
            })
            .map(|(_, &key)| (key, true))
            .collect();
        let mut batch = Vec::new();
        let mut bytes = 0;
        for (release, key) in releases.into_iter().zip(identities) {
            let marker = match junk.get_mut(&key) {
                Some(pending) if *pending => std::mem::take(pending),
                Some(_) => continue,
                None => false,
            };
            // A single marker retracts a release with a program. Other
            // releases stage only articles with IDs, as the shard writer does.
            let original_name_key: [u8;32] = Sha256::digest(release.name.as_bytes()).into();
            let posted_at = crate::dates::article_timestamp(&release.date);
            let posted_clock = crate::dates::naive_timestamp(&release.date);
            let rows = std::iter::once(None)
                .filter(|_| marker)
                .chain(release.articles.iter().filter(|article| !marker && !article.message_id.is_empty()).map(Some));
            for article in rows {
                let row = Incoming { release, release_key: key.1, original_name_key, posted_at, posted_clock, article };
                let size = row.approx_bytes();
                if batch.len() == MAX_BATCH_ROWS || (!batch.is_empty() && bytes + size > MAX_BATCH_BYTES) {
                    flush(tx, group, &batch, now, &mut stats, &mut timing_key, local.as_mut())?;
                    batch.clear();
                    bytes = 0;
                }
                batch.push(row);
                bytes += size;
            }
        }
        if !batch.is_empty() {
            flush(tx, group, &batch, now, &mut stats, &mut timing_key, local.as_mut())?;
        }
    }
    Ok((stats, local))
}

fn pg_text(value: &str) -> Cow<'_, str> {
    if value.as_bytes().contains(&0) { Cow::Owned(value.replace('\0', "\u{fffd}")) } else { Cow::Borrowed(value) }
}

/// The local article store of this save and the PostgreSQL transaction its rows are filed under.
struct Local {
    store: std::sync::Arc<crate::article_local::Store>,
    xid: Option<String>,
}

impl Local {
    /// The id of the transaction this save runs in, asked for once.
    fn xid(&mut self, tx: &mut Transaction<'_>) -> Result<String> {
        if self.xid.is_none() {
            self.xid = Some(tx.query_one("select pg_current_xact_id()::text", &[])?.try_get(0)?);
        }
        Ok(self.xid.clone().expect("set above"))
    }
}

fn flush(tx: &mut Transaction<'_>, group: &str, batch: &[Incoming<'_>], now: i64, stats: &mut SaveStats, timing_key: &mut Option<i64>, local: Option<&mut Local>) -> Result<()> {
    if timing_key.is_none() && batch.iter().any(|row| row.article.is_some()) {
        *timing_key = Some(timing::key(tx)?);
    }
    if let Some(local) = local {
        return flush_local(tx, group, batch, now, stats, *timing_key, local);
    }
    tx.batch_execute("truncate pg_temp.atlas_release_input")?;
    copy_input(tx, batch, *timing_key)?;
    let release = tx.query_one(WRITE_RELEASES, &[&group])?;
    stats.releases += release.get::<_, i64>(0) as u64;
    let articles = tx.query_one(WRITE_ARTICLES, &[&group])?;
    stats.files += articles.get::<_, i64>(0) as u64;
    let parts = articles.get::<_, i64>(1) as u64;
    stats.parts += parts;
    if parts > 0 {
        timing::write(tx, parts)?;
        tx.execute(UPDATE_FILES, &[&now, &crate::parser::MAX_PARTS])?;
        finish(tx, now)?;
    }
    Ok(())
}

/// What one file gained from a batch, folded here the way the per-article
/// statements fold the staged rows, so PostgreSQL is sent one row a file.
struct FileFold<'a> {
    release_id: i64,
    filename: &'a str,
    filename_key: [u8; 32],
    expected: Option<i64>,
    file_total: Option<i64>,
    /// the article whose subject names the file: first by part, a missing part first, then message id
    subject: &'a Article,
    first_date: Option<i64>,
    last_date: Option<i64>,
    first_utc: Option<i64>,
    last_utc: Option<i64>,
    utc_complete: bool,
    number_first: Option<i64>,
    number_last: Option<i64>,
    lines: i64,
    references_seen: i64,
    xref_groups_max: Option<i64>,
    from_first: i64,
    from_last: i64,
    from_variants: i64,
    encoded_subject: i64,
    articles: i64,
    tool: Option<i64>,
    clock_first: Option<i64>,
    clock_last: Option<i64>,
    hw_host: Option<String>,
    hw_key: Option<String>,
    hw_n_first: Option<i64>,
    hw_n_last: Option<i64>,
}

/// What one release gained from a batch.
struct ReleaseFold<'a> {
    parts: i64,
    bytes: i64,
    file_total: Option<i64>,
    /// the release of the last accepted article: its poster and date are the ones kept
    last: &'a Release,
    posted_at: i64,
    posted_clock: Option<i64>,
    display_name: Option<&'a str>,
}

fn least(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn greatest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// A batch with the articles kept in the local article store: PostgreSQL gets
/// the releases, then one folded row for each file and each release that
/// gained an article. No article row goes to PostgreSQL.
fn flush_local(tx: &mut Transaction<'_>, group: &str, batch: &[Incoming<'_>], now: i64, stats: &mut SaveStats, timing_key: Option<i64>, local: &mut Local) -> Result<()> {
    // One row for each release named in the batch: the first staged row of it,
    // which is the one `distinct on ... order by ordinal` inserted.
    let mut heads = Vec::new();
    let mut head_of = HashMap::new();
    for (ordinal, row) in batch.iter().enumerate() {
        head_of.entry((row.release.collection.is_some(), row.release_key)).or_insert_with(|| {
            heads.push(ordinal);
            heads.len() - 1
        });
    }
    tx.batch_execute("truncate pg_temp.atlas_release_fold, pg_temp.atlas_file_fold")?;
    {
        use Type as T;
        let input = tx.copy_in(
            "copy pg_temp.atlas_release_fold (ordinal, identity_key, is_collection, name, poster, posted_date, display_name,
                is_obfuscated, junk, original_name, original_name_key, posted_at, posted_clock) from stdin binary",
        )?;
        let mut writer = BinaryCopyInWriter::new(
            input,
            &[T::INT8, T::BYTEA, T::BOOL, T::TEXT, T::TEXT, T::TEXT, T::TEXT, T::BOOL, T::BOOL, T::BYTEA, T::BYTEA, T::INT8, T::INT8],
        );
        for (head, &ordinal) in heads.iter().enumerate() {
            let row = &batch[ordinal];
            let r = row.release;
            writer.write(&[
                &(head as i64),
                &&row.release_key[..],
                &r.collection.is_some(),
                &pg_text(&r.name).as_ref(),
                &pg_text(&r.poster).as_ref(),
                &pg_text(&r.date).as_ref(),
                &r.display_name.as_deref().map(pg_text).as_deref(),
                &r.is_obfuscated,
                &row.article.is_none(),
                &r.name.as_bytes(),
                &&row.original_name_key[..],
                &row.posted_at,
                &row.posted_clock,
            ])?;
        }
        writer.finish()?;
    }
    // A release with a program is deleted from the catalogue below; its
    // articles leave the store when this transaction commits.
    if heads.iter().any(|&ordinal| batch[ordinal].article.is_none()) {
        let gone: Vec<i64> = tx.query(RETRACTED, &[&group])?.iter().map(|row| row.try_get(0)).collect::<Result<_, _>>()?;
        if !gone.is_empty() {
            let xid = local.xid(tx)?;
            local.store.retract(&xid, &gone)?;
        }
    }
    let release = tx.query_one(&WRITE_RELEASES.replace("pg_temp.atlas_release_input", "pg_temp.atlas_release_fold"), &[&group])?;
    stats.releases += release.get::<_, i64>(0) as u64;
    let mut release_of = vec![None; heads.len()];
    for row in tx.query(RELEASE_IDS, &[&group])? {
        release_of[usize::try_from(row.try_get::<_, i64>(0)?)?] = Some(row.try_get::<_, i64>(1)?);
    }
    // The first copy of a message id in a release wins, and one the release
    // already has is a replay.
    let mut firsts = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for row in batch {
            let Some(article) = row.article else { continue };
            let Some(release) = release_of[head_of[&(row.release.collection.is_some(), row.release_key)]] else { continue };
            if seen.insert((release, article.message_id.as_bytes())) {
                firsts.push((row, article, release));
            }
        }
    }
    let known = local.store.has(&firsts.iter().map(|&(_, article, release)| (release, article.message_id.as_bytes())).collect::<Vec<_>>())?;
    let accepted: Vec<_> = firsts.into_iter().zip(known).filter(|(_, known)| !known).map(|(first, _)| first).collect();
    if accepted.is_empty() {
        return Ok(());
    }
    let timing_key = timing_key.ok_or_else(|| anyhow::anyhow!("native writer timing has no catalog key"))?;
    let mut files = Vec::<FileFold<'_>>::new();
    let mut file_of = HashMap::<(i64, &str), usize>::new();
    let mut file_index = Vec::with_capacity(accepted.len());
    let mut releases = BTreeMap::<i64, ReleaseFold<'_>>::new();
    for &(row, article, release_id) in &accepted {
        let facts = crate::catalog_facts::ArticleFacts::of(article);
        let id = &facts.id;
        let hardware = (id.hw_host.is_some() && id.hw_key.is_some() && id.hw_n.is_some()).then_some(id);
        let number = if article.number > 0 { Some(i64::try_from(article.number)?) } else { None };
        let from = crate::catalog_facts::keyed_hash(timing_key as u64, 0, &article.author);
        let filename = article.filename.as_deref().unwrap_or("");
        let at = *file_of.entry((release_id, filename)).or_insert_with(|| {
            files.push(FileFold {
                release_id,
                filename,
                filename_key: Sha256::digest(filename).into(),
                expected: None,
                file_total: None,
                subject: article,
                first_date: None,
                last_date: None,
                first_utc: None,
                last_utc: None,
                utc_complete: true,
                number_first: None,
                number_last: None,
                lines: 0,
                references_seen: 0,
                xref_groups_max: None,
                from_first: from,
                from_last: from,
                from_variants: 0,
                encoded_subject: 0,
                articles: 0,
                tool: None,
                clock_first: None,
                clock_last: None,
                hw_host: None,
                hw_key: None,
                hw_n_first: None,
                hw_n_last: None,
            });
            files.len() - 1
        });
        file_index.push(at);
        let f = &mut files[at];
        f.expected = greatest(f.expected, article.total_parts);
        f.file_total = greatest(f.file_total, article.file_total);
        if (article.part.is_some(), article.part.unwrap_or(0), article.message_id.as_bytes())
            < (f.subject.part.is_some(), f.subject.part.unwrap_or(0), f.subject.message_id.as_bytes())
        {
            f.subject = article;
        }
        f.first_date = least(f.first_date, facts.date);
        f.last_date = greatest(f.last_date, facts.date);
        f.first_utc = least(f.first_utc, facts.utc);
        f.last_utc = greatest(f.last_utc, facts.utc);
        f.utc_complete &= facts.utc.is_some();
        f.number_first = least(f.number_first, number);
        f.number_last = greatest(f.number_last, number);
        f.lines = f.lines.checked_add(article.lines.max(0)).ok_or_else(|| anyhow::anyhow!("native timing line count overflow"))?;
        f.references_seen += i64::from(!article.references.trim().is_empty());
        f.xref_groups_max = greatest(f.xref_groups_max, article.xref_groups.map(i64::from));
        // A sender counts once for each run of articles it posted in arrival order.
        if f.articles == 0 || f.from_last != from {
            f.from_variants += 1;
        }
        f.from_last = from;
        f.encoded_subject = f.encoded_subject.max(i64::from(article.subject.contains("=?") && article.subject.contains("?=")));
        f.articles += 1;
        f.tool = f.tool.or(id.tool.map(|tool| tool.code()));
        f.clock_first = least(f.clock_first, id.clock_ms);
        f.clock_last = greatest(f.clock_last, id.clock_ms);
        if let Some(hw) = hardware {
            if f.hw_host.is_none() {
                f.hw_host = hw.hw_host.clone();
            }
            if f.hw_key.is_none() {
                f.hw_key = hw.hw_key.clone();
            }
            f.hw_n_first = least(f.hw_n_first, hw.hw_n);
            f.hw_n_last = greatest(f.hw_n_last, hw.hw_n);
        }
        let r = releases.entry(release_id).or_insert(ReleaseFold {
            parts: 0,
            bytes: 0,
            file_total: None,
            last: row.release,
            posted_at: row.posted_at,
            posted_clock: row.posted_clock,
            display_name: None,
        });
        r.parts += 1;
        r.bytes = r.bytes.checked_add(article.bytes).ok_or_else(|| anyhow::anyhow!("bigint out of range"))?;
        r.file_total = greatest(r.file_total, article.file_total);
        (r.last, r.posted_at, r.posted_clock) = (row.release, row.posted_at, row.posted_clock);
        if let Some(name) = row.release.display_name.as_deref() {
            r.display_name = Some(name);
        }
    }
    {
        use Type as T;
        let input = tx.copy_in(
            "copy pg_temp.atlas_file_fold (ord, release_id, filename_key, filename, original_filename, expected, file_total,
                subject, subject_part, subject_mid, zero_file_index, subject_file_index, subject_file_total, subject_expected,
                first_date, last_date, first_utc, last_utc, utc_complete, number_first, number_last, lines, references_seen,
                xref_groups_max, from_first, from_last, from_variants, encoded_subject, articles, tool, clock_first,
                clock_last, hw_host, hw_key, hw_n_first, hw_n_last) from stdin binary",
        )?;
        let mut writer = BinaryCopyInWriter::new(
            input,
            &[
                T::INT8, T::INT8, T::BYTEA, T::TEXT, T::BYTEA, T::INT8, T::INT8,
                T::TEXT, T::INT8, T::BYTEA, T::BOOL, T::INT8, T::INT8, T::INT8,
                T::INT8, T::INT8, T::INT8, T::INT8, T::BOOL, T::INT8, T::INT8, T::INT8, T::INT8,
                T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8,
                T::INT8, T::TEXT, T::TEXT, T::INT8, T::INT8,
            ],
        );
        for (ord, f) in files.iter().enumerate() {
            let s = f.subject;
            writer.write(&[
                &(ord as i64),
                &f.release_id,
                &&f.filename_key[..],
                &pg_text(f.filename).as_ref(),
                &f.filename.as_bytes(),
                &f.expected,
                &f.file_total,
                &pg_text(&s.subject).as_ref(),
                &s.part,
                &s.message_id.as_bytes(),
                &(s.subject.contains('[') && crate::parser::has_zero_file_index(&s.subject)),
                &s.file_index,
                &s.file_total,
                &s.total_parts,
                &f.first_date,
                &f.last_date,
                &f.first_utc,
                &f.last_utc,
                &f.utc_complete,
                &f.number_first,
                &f.number_last,
                &f.lines,
                &f.references_seen,
                &f.xref_groups_max,
                &f.from_first,
                &f.from_last,
                &f.from_variants,
                &f.encoded_subject,
                &f.articles,
                &f.tool,
                &f.clock_first,
                &f.clock_last,
                &f.hw_host.as_deref(),
                &f.hw_key.as_deref(),
                &f.hw_n_first,
                &f.hw_n_last,
            ])?;
        }
        writer.finish()?;
    }
    // Every release whose files or timing this batch changes is in `changed` and bumped once below: the input
    // triggers on files and file_timing leave the bump to this writer meanwhile (migration 0015).
    tx.batch_execute("set local atlas.writer_bumps_inputs = 'on'")?;
    stats.files += tx.query_one(ADD_FILES, &[])?.try_get::<_, i64>(0)? as u64;
    let mut file_ids = vec![None; files.len()];
    for row in tx.query(PLACE_FILES, &[])? {
        file_ids[usize::try_from(row.try_get::<_, i64>(0)?)?] = Some(row.try_get::<_, i64>(1)?);
    }
    let file_ids: Vec<i64> =
        file_ids.into_iter().map(|id| id.ok_or_else(|| anyhow::anyhow!("an accepted article found no file"))).collect::<Result<_>>()?;
    let rows: Vec<_> = accepted
        .iter()
        .zip(&file_index)
        .map(|(&(row, article, release_id), &at)| crate::article_local::NewRow {
            release_id,
            file_id: file_ids[at],
            message_id: article.message_id.as_bytes(),
            part: article.part,
            bytes: article.bytes,
            envelope: article.filename.is_none().then(|| crate::article_local::Envelope {
                subject: article.subject.as_bytes().to_vec(),
                poster: row.release.poster.as_bytes().to_vec(),
                posted_date: row.release.date.as_bytes().to_vec(),
                nzb_timestamp: crate::dates::article_timestamp(&row.release.date),
                expected: article.total_parts,
                subject_file_index: article.file_index,
                subject_file_total: article.file_total,
            }),
        })
        .collect();
    let xid = local.xid(tx)?;
    let gained = local.store.add(&xid, &rows, crate::parser::MAX_PARTS)?;
    let parts = rows.len() as u64;
    stats.parts += parts;
    // The trigger on `segments` told post-processing a release's input changed.
    let changed: Vec<i64> = releases.keys().copied().collect();
    tx.execute("update releases set native_work_revision = native_work_revision + 1 where id = any($1)", &[&changed])?;
    // A file's first proof of UTC needs every article it has to be in this batch.
    let present: Vec<i64> =
        files.iter().zip(&file_ids).map(|(f, &id)| local.store.file_count(id, f.articles + 1)).collect::<Result<_>>()?;
    timing::write_local(tx, parts, &file_ids, &present)?;
    let (ids, counts, firsts, lasts): (Vec<i64>, Vec<i64>, Vec<i64>, Vec<i64>) =
        gained.iter().fold(Default::default(), |mut all: (Vec<i64>, Vec<i64>, Vec<i64>, Vec<i64>), parts| {
            all.0.push(parts.file_id);
            all.1.push(parts.count);
            all.2.push(parts.first);
            all.3.push(parts.last);
            all
        });
    tx.execute(UPDATE_FILES_FOLD, &[&now, &ids, &counts, &firsts, &lasts])?;
    tx.batch_execute("set local atlas.writer_bumps_inputs = 'off'")?;
    let complete_due = now.saturating_add(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(Ordering::Relaxed).max(1));
    let incomplete_due = now.saturating_add(crate::catalog_facts::SETTLE_SECS.load(Ordering::Relaxed).max(1));
    let folds: Vec<&ReleaseFold<'_>> = releases.values().collect();
    tx.execute(
        UPDATE_RELEASES_FOLD,
        &[
            &complete_due,
            &incomplete_due,
            &changed,
            &folds.iter().map(|r| r.parts).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.bytes).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.file_total).collect::<Vec<_>>(),
            &folds.iter().map(|r| pg_text(&r.last.poster)).collect::<Vec<_>>(),
            &folds.iter().map(|r| pg_text(&r.last.date)).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.posted_at).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.posted_clock).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.last.is_obfuscated).collect::<Vec<_>>(),
            &folds.iter().map(|r| r.display_name.map(pg_text)).collect::<Vec<_>>(),
        ],
    )?;
    Ok(())
}

fn finish(tx: &mut Transaction<'_>, now: i64) -> Result<()> {
    {
        let complete_due = now.saturating_add(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(Ordering::Relaxed).max(1));
        let incomplete_due = now.saturating_add(crate::catalog_facts::SETTLE_SECS.load(Ordering::Relaxed).max(1));
        tx.execute(UPDATE_RELEASES_AND_QUEUE, &[&complete_due, &incomplete_due])?;
    }
    Ok(())
}

fn copy_input(tx: &mut Transaction<'_>, batch: &[Incoming<'_>], timing_key: Option<i64>) -> Result<()> {
    let input = tx.copy_in(
        "copy pg_temp.atlas_release_input (
            ordinal, identity_key, is_collection, name, poster, posted_date, display_name, is_obfuscated,
            junk, filename_key, filename, message_key, message_id, subject, part, expected, file_total,
            zero_file_index, bytes, original_name, original_filename, file_index, original_name_key, posted_at, posted_clock,
            nzb_subject, nzb_poster, nzb_posted_date, nzb_timestamp, nzb_file_index,
            t_date, t_utc, t_number, t_lines, t_references, t_xref, t_from, t_encoded, t_tool, t_clock,
            t_hw_host, t_hw_key, t_hw_n
         ) from stdin binary",
    )?;
    use Type as T;
    let mut writer = BinaryCopyInWriter::new(
        input,
        &[
            T::INT8,
            T::BYTEA,
            T::BOOL,
            T::TEXT,
            T::TEXT,
            T::TEXT,
            T::TEXT,
            T::BOOL,
            T::BOOL,
            T::BYTEA,
            T::TEXT,
            T::BYTEA,
            T::BYTEA,
            T::TEXT,
            T::INT8,
            T::INT8,
            T::INT8,
            T::BOOL,
            T::INT8,
            T::BYTEA,
            T::BYTEA,
            T::INT8,
            T::BYTEA,
            T::INT8,
            T::INT8,
            T::BYTEA,
            T::BYTEA,
            T::BYTEA,
            T::INT8,
            T::INT8,
            T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8, T::INT8,
            T::TEXT, T::TEXT, T::INT8,
        ],
    );
    for (i, row) in batch.iter().enumerate() {
        let r = row.release;
        let a = row.article;
        let filename = a.map(|article| article.filename.as_deref().unwrap_or(""));
        let original_filename=filename.map(str::as_bytes);
        let filename_key: Option<[u8; 32]> = filename.map(|name| Sha256::digest(name).into());
        let message_id = a.map(|article| article.message_id.as_bytes());
        let message_key: Option<[u8; 32]> = message_id.map(|id| Sha256::digest(id).into());
        let filename = filename.map(pg_text);
        let subject = a.map(|article| pg_text(&article.subject));
        let display_name = r.display_name.as_deref().map(pg_text);
        // Sparse original NZB metadata belongs only to literal None inputs,
        // and later only to the MID that actually wins segment acceptance.
        let nameless = a.filter(|article| article.filename.is_none());
        // Only one article's parsing scratch is live. No parallel Timing map
        // or clone of the original batch is retained beside the COPY input.
        let facts = a.map(crate::catalog_facts::ArticleFacts::of);
        let id = facts.as_ref().map(|facts| &facts.id);
        let hardware = id.filter(|id| id.hw_host.is_some() && id.hw_key.is_some() && id.hw_n.is_some());
        let number = a.filter(|article| article.number > 0)
            .map(|article| i64::try_from(article.number)).transpose()?;
        let from = a.map(|article| {
            timing_key.map(|key| crate::catalog_facts::keyed_hash(key as u64, 0, &article.author))
                .ok_or_else(|| anyhow::anyhow!("native writer timing has no catalog key"))
        }).transpose()?;
        writer.write(&[
            &(i as i64),
            &&row.release_key[..],
            &r.collection.is_some(),
            &pg_text(&r.name).as_ref(),
            &pg_text(&r.poster).as_ref(),
            &pg_text(&r.date).as_ref(),
            &display_name.as_deref(),
            &r.is_obfuscated,
            &a.is_none(),
            &filename_key.as_ref().map(<[u8; 32]>::as_slice),
            &filename.as_deref(),
            &message_key.as_ref().map(<[u8; 32]>::as_slice),
            &message_id,
            &subject.as_deref(),
            &a.and_then(|article| article.part),
            &a.and_then(|article| article.total_parts),
            &a.and_then(|article| article.file_total),
            &a.is_some_and(|article| {
                article.subject.contains('[') && crate::parser::has_zero_file_index(&article.subject)
            }),
            &a.map_or(0, |article| article.bytes),
            &r.name.as_bytes(),
            &original_filename,
            &a.and_then(|article|article.file_index),
            &&row.original_name_key[..],
            &row.posted_at,
            &row.posted_clock,
            &nameless.map(|article|article.subject.as_bytes()),
            &nameless.map(|_|r.poster.as_bytes()),
            &nameless.map(|_|r.date.as_bytes()),
            &nameless.map(|_|crate::dates::article_timestamp(&r.date)),
            &nameless.and_then(|article|article.file_index),
            &facts.as_ref().and_then(|facts| facts.date),
            &facts.as_ref().and_then(|facts| facts.utc),
            &number,
            &a.map(|article| article.lines.max(0)),
            &a.map(|article| i64::from(!article.references.trim().is_empty())),
            &a.and_then(|article| article.xref_groups.map(i64::from)),
            &from,
            &a.map(|article| i64::from(article.subject.contains("=?") && article.subject.contains("?="))),
            &id.and_then(|id| id.tool.map(|tool| tool.code())),
            &id.and_then(|id| id.clock_ms),
            &hardware.and_then(|id| id.hw_host.as_deref()),
            &hardware.and_then(|id| id.hw_key.as_deref()),
            &hardware.and_then(|id| id.hw_n),
        ])?;
    }
    writer.finish()?;
    // Build against this filled batch: an index made on the empty stage left
    // winner-join cardinality underestimated even when ordinal probes used it.
    tx.batch_execute("drop index if exists pg_temp.atlas_release_input_ordinal_idx;
        create unique index atlas_release_input_ordinal_idx on pg_temp.atlas_release_input(ordinal)")?;
    Ok(())
}

// The connection-local table is reused when a pool connection is reused. A
// commit or rollback leaves no rows for the next save.
const CREATE_STAGE: &str = "
    set local synchronous_commit = on;
    create temporary table if not exists pg_temp.atlas_release_input (
        ordinal bigint not null,
        identity_key bytea not null,
        is_collection boolean not null,
        name text not null,
        poster text not null,
        posted_date text not null,
        display_name text,
        is_obfuscated boolean not null,
        junk boolean not null,
        filename_key bytea,
        filename text,
        message_key bytea,
        message_id bytea,
        subject text,
        part bigint,
        expected bigint,
        file_total bigint,
        zero_file_index boolean not null,
        bytes bigint not null,
        original_name bytea not null,
        original_filename bytea,
        file_index bigint,
        original_name_key bytea not null,
        posted_at bigint not null,
        posted_clock bigint,
        nzb_subject bytea,
        nzb_poster bytea,
        nzb_posted_date bytea,
        nzb_timestamp bigint,
        nzb_file_index bigint,
        t_date bigint,
        t_utc bigint,
        t_number bigint,
        t_lines bigint,
        t_references bigint,
        t_xref bigint,
        t_from bigint,
        t_encoded bigint,
        t_tool bigint,
        t_clock bigint,
        t_hw_host text,
        t_hw_key text,
        t_hw_n bigint,
        release_id bigint,
        file_id bigint
    ) on commit delete rows";

// With the articles kept locally the writer folds a batch itself: a row for
// each release it names and a row for each file that gained an article.
const CREATE_FOLD_STAGE: &str = "
    create temporary table if not exists pg_temp.atlas_release_fold (
        ordinal bigint primary key,
        identity_key bytea not null,
        is_collection boolean not null,
        name text not null,
        poster text not null,
        posted_date text not null,
        display_name text,
        is_obfuscated boolean not null,
        junk boolean not null,
        original_name bytea not null,
        original_name_key bytea not null,
        posted_at bigint not null,
        posted_clock bigint
    ) on commit delete rows;
    create temporary table if not exists pg_temp.atlas_file_fold (
        ord bigint primary key,
        release_id bigint not null,
        file_id bigint,
        filename_key bytea not null,
        filename text not null,
        original_filename bytea not null,
        expected bigint,
        file_total bigint,
        subject text not null,
        subject_part bigint,
        subject_mid bytea not null,
        zero_file_index boolean not null,
        subject_file_index bigint,
        subject_file_total bigint,
        subject_expected bigint,
        first_date bigint,
        last_date bigint,
        first_utc bigint,
        last_utc bigint,
        utc_complete boolean not null,
        number_first bigint,
        number_last bigint,
        lines bigint,
        references_seen bigint not null,
        xref_groups_max bigint,
        from_first bigint not null,
        from_last bigint not null,
        from_variants bigint not null,
        encoded_subject bigint not null,
        articles bigint not null,
        tool bigint,
        clock_first bigint,
        clock_last bigint,
        hw_host text,
        hw_key text,
        hw_n_first bigint,
        hw_n_last bigint
    ) on commit delete rows";

const WRITE_RELEASES: &str = "
    with marked as (
        select distinct is_collection, identity_key from pg_temp.atlas_release_input where junk
    ), remember as (
        insert into release_writer_junk
        select $1, is_collection, identity_key from marked
        on conflict do nothing
    ), retract as (
        delete from releases r using marked j
        where r.group_name = $1 and (r.is_collection, r.identity_key) = (j.is_collection, j.identity_key)
    ), added as (
        insert into releases (group_name, is_collection, identity_key, name, poster, posted_date, display_name, is_obfuscated, original_name, original_name_key, posted_at, posted_clock)
        select distinct on (s.is_collection, s.identity_key)
            $1, s.is_collection, s.identity_key, s.name, s.poster, s.posted_date, s.display_name, s.is_obfuscated, s.original_name, s.original_name_key, s.posted_at, s.posted_clock
        from pg_temp.atlas_release_input s
        where not exists (
            select 1 from marked j where (j.is_collection, j.identity_key) = (s.is_collection, s.identity_key)
        ) and not exists (
            select 1 from release_writer_junk j
            where j.group_name = $1 and (j.is_collection, j.identity_key) = (s.is_collection, s.identity_key)
        )
        order by s.is_collection, s.identity_key, s.ordinal
        on conflict (group_name, is_collection, identity_key) do nothing
        returning id
    )
    select count(*) from added";

const WRITE_ARTICLES: &str = "
    with candidates as materialized (
        select distinct on (r.id, s.message_key)
            r.id as release_id, s.ordinal, s.filename_key, s.filename, s.original_filename, s.message_key, s.message_id, s.part, s.bytes,
            s.nzb_subject,s.nzb_poster,s.nzb_posted_date,s.nzb_timestamp,s.expected,s.nzb_file_index,s.file_total
        from pg_temp.atlas_release_input s
        join releases r on r.group_name = $1 and (r.is_collection, r.identity_key) = (s.is_collection, s.identity_key)
        where not s.junk and not exists (
            select 1 from segments x where x.release_id = r.id and x.message_key = s.message_key
        )
        order by r.id, s.message_key, s.ordinal
    ), file_keys as (
        select distinct release_id, filename_key, filename, original_filename from candidates
    ), added_files as (
        insert into files (release_id, filename_key, filename, original_filename)
        select release_id, filename_key, filename, original_filename from file_keys
        on conflict (release_id, filename_key) do nothing
        returning id, release_id, filename_key
    ), file_map as (
        select id, release_id, filename_key from added_files
        union all
        select f.id, f.release_id, f.filename_key from files f join file_keys k using (release_id, filename_key)
    ), added as (
        insert into segments (release_id, file_id, message_key, message_id, part, bytes)
        select c.release_id, f.id, c.message_key, c.message_id, c.part, c.bytes
        from candidates c join file_map f using (release_id, filename_key)
        on conflict (release_id, message_key) do nothing
        returning release_id, file_id, message_key
    ), winners as materialized (
        select release_id, message_key, max(ordinal) as ordinal, max(file_id) as file_id
        from (
            select release_id, message_key, ordinal, null::bigint as file_id from candidates
            union all
            select release_id, message_key, null::bigint as ordinal, file_id from added
        ) mapping_input
        group by release_id, message_key
        having max(file_id) is not null
    ), sparse_nzb as (
        insert into segment_nzb_metadata (release_id,message_key,subject,poster,posted_date,nzb_timestamp,expected,subject_file_index,subject_file_total)
        select w.release_id,w.message_key,s.nzb_subject,s.nzb_poster,s.nzb_posted_date,s.nzb_timestamp,s.expected,s.nzb_file_index,s.file_total
        from winners w join pg_temp.atlas_release_input s on s.ordinal=w.ordinal where s.nzb_subject is not null
        on conflict(release_id,message_key) do nothing
        returning 1
    ), accepted as (
        update pg_temp.atlas_release_input s set release_id = w.release_id, file_id = w.file_id
        from winners w where s.ordinal = w.ordinal
        returning 1
    )
    select (select count(*) from added_files), (select count(*) from accepted)";

// The part of `UPDATE_FILES` that reads each file's declarations and naming
// article from the staged articles.
macro_rules! update_files_inputs {
    () => {
        "), declarations as (
        select file_id, max(expected) as expected, max(file_total) as file_total
        from pg_temp.atlas_release_input where file_id is not null group by file_id
    ), subjects as (
        select distinct on (file_id) file_id, subject, part, message_id, zero_file_index, file_index, file_total, expected
        from pg_temp.atlas_release_input where file_id is not null
        order by file_id, part nulls first, message_id
"
    };
}

// The part of `UPDATE_FILES` after every input of a file is known.
macro_rules! update_files_combine {
    () => {
        "    ), combined as (
        select f.id, greatest(f.expected, d.expected) as expected,
            greatest(f.file_total, d.file_total) as file_total,
            f.distinct_parts + coalesce(p.count, 0) as distinct_parts,
            least(f.first_part, p.first) as first_part,
            greatest(f.last_part, p.last) as last_part,
            s.subject, s.part, s.message_id, s.zero_file_index,
            s.file_index as subject_file_index,s.file_total as subject_file_total,s.expected as subject_total_parts,
            f.subject_mid is null or
            (s.part is not null, coalesce(s.part, 0), s.message_id) <
            (f.subject_part is not null, coalesce(f.subject_part, 0), f.subject_mid) as better_subject
        from declarations d join files f on f.id = d.file_id
        join subjects s on s.file_id = d.file_id left join parts p on p.file_id = d.file_id
    )
    update files f set expected = c.expected, file_total = c.file_total,
        distinct_parts = c.distinct_parts, first_part = c.first_part, last_part = c.last_part,
        touched_at = $1,
        complete = coalesce(c.expected > 0 and c.distinct_parts = c.expected
            and c.last_part = c.expected - case when c.first_part = 0 then 1 else 0 end, false),
        subject = case when c.better_subject then c.subject else f.subject end,
        subject_part = case when c.better_subject then c.part else f.subject_part end,
        subject_mid = case when c.better_subject then c.message_id else f.subject_mid end,
        subject_file_index = case when c.better_subject then c.subject_file_index else f.subject_file_index end,
        subject_file_total = case when c.better_subject then c.subject_file_total else f.subject_file_total end,
        subject_total_parts = case when c.better_subject then c.subject_total_parts else f.subject_total_parts end,
        zero_file_index = case when c.better_subject then c.zero_file_index else f.zero_file_index end
    from combined c where c.id = f.id"
    };
}

const UPDATE_FILES: &str = concat!(
    "
    with new_parts as (
        insert into file_parts (file_id, part)
        select distinct file_id, part from pg_temp.atlas_release_input
        where file_id is not null and part between 0 and $2
        on conflict do nothing
        returning file_id, part
    ), parts as (
        select file_id, count(*) as count, min(part) as first, max(part) as last from new_parts group by file_id
    ",
    update_files_inputs!(),
    update_files_combine!()
);

// The same update from the writer's own fold of each file, with the part
// numbers the local article store said each file gained.
const UPDATE_FILES_FOLD: &str = concat!(
    "
    with parts as (
        select * from unnest($2::bigint[], $3::bigint[], $4::bigint[], $5::bigint[]) as p(file_id, count, first, last)
    ), declarations as (
        select file_id, expected, file_total from pg_temp.atlas_file_fold
    ), subjects as (
        select file_id, subject, subject_part as part, subject_mid as message_id, zero_file_index,
            subject_file_index as file_index, subject_file_total as file_total, subject_expected as expected
        from pg_temp.atlas_file_fold
",
    update_files_combine!()
);

// The release each folded release row belongs to.
const RELEASE_IDS: &str = "
    select s.ordinal, r.id
    from pg_temp.atlas_release_fold s
    join releases r on r.group_name = $1 and (r.is_collection, r.identity_key) = (s.is_collection, s.identity_key)
    where not s.junk";

// The releases the junk markers of a batch are about to delete.
const RETRACTED: &str = "
    select r.id
    from pg_temp.atlas_release_fold s
    join releases r on r.group_name = $1 and (r.is_collection, r.identity_key) = (s.is_collection, s.identity_key)
    where s.junk";

// Create the files a batch names for the first time.
const ADD_FILES: &str = "
    with added as (
        insert into files (release_id, filename_key, filename, original_filename)
        select release_id, filename_key, filename, original_filename from pg_temp.atlas_file_fold
        on conflict (release_id, filename_key) do nothing
        returning 1
    )
    select count(*) from added";

// Give each folded file row its file.
const PLACE_FILES: &str = "
    update pg_temp.atlas_file_fold s set file_id = f.id
    from files f where (f.release_id, f.filename_key) = (s.release_id, s.filename_key)
    returning s.ord, s.file_id";

// The part of `UPDATE_RELEASES_AND_QUEUE` after what each release gained is known.
macro_rules! update_releases_rest {
    () => {
        "    ), completion as (
        select c.release_id, count(*) as files, bool_and(f.complete) as all_whole,
            bool_or(f.zero_file_index) as zero_indexed
        from changes c join files f on f.release_id = c.release_id group by c.release_id
    ), updated as (
        update releases r set size = r.size + c.bytes, parts = r.parts + c.parts,
            file_total = greatest(r.file_total, c.file_total),
            complete = f.all_whole and (greatest(r.file_total, c.file_total) is null or
                f.files = greatest(r.file_total, c.file_total) + case when f.zero_indexed then 1 else 0 end),
            poster = m.poster, posted_date = m.posted_date, posted_at = m.posted_at, posted_clock = m.posted_clock,
            display_name = case when r.name_source is null then coalesce(d.display_name, r.display_name) else r.display_name end,
            is_obfuscated = case when r.name_source is null and not r.is_collection then m.is_obfuscated else r.is_obfuscated end
        from changes c join metadata m using (release_id) join completion f using (release_id)
        left join display_names d using (release_id)
        where r.id = c.release_id
        returning r.id, r.complete
    )
    insert into pp_queue (release_id, due_at)
    select id, case when complete then $1::bigint else $2::bigint end from updated
    on conflict (release_id) do update set due_at = excluded.due_at, generation = pp_queue.generation + 1"
    };
}

const UPDATE_RELEASES_AND_QUEUE: &str = concat!(
    "
    with changes as (
        select release_id, count(*) as parts, sum(bytes)::bigint as bytes, max(file_total) as file_total
        from pg_temp.atlas_release_input where release_id is not null group by release_id
    ), metadata as (
        select distinct on (release_id) release_id, poster, posted_date, posted_at, posted_clock, is_obfuscated
        from pg_temp.atlas_release_input where release_id is not null order by release_id, ordinal desc
    ), display_names as (
        select distinct on (release_id) release_id, display_name from pg_temp.atlas_release_input
        where release_id is not null and display_name is not null order by release_id, ordinal desc
",
    update_releases_rest!()
);

// The same update from the writer's own fold of each release.
const UPDATE_RELEASES_FOLD: &str = concat!(
    "
    with gained as (
        select * from unnest($3::bigint[], $4::bigint[], $5::bigint[], $6::bigint[], $7::text[], $8::text[],
            $9::bigint[], $10::bigint[], $11::boolean[], $12::text[])
            as g(release_id, parts, bytes, file_total, poster, posted_date, posted_at, posted_clock, is_obfuscated, display_name)
    ), changes as (
        select release_id, parts, bytes, file_total from gained
    ), metadata as (
        select release_id, poster, posted_date, posted_at, posted_clock, is_obfuscated from gained
    ), display_names as (
        select release_id, display_name from gained where display_name is not null
",
    update_releases_rest!()
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

    fn release(name: &str, file: &str, message_id: &str, part: i64) -> Release {
        Release {
            name: name.into(),
            group: "alt.binaries.test".into(),
            articles: vec![Article {
                filename: Some(file.into()),
                message_id: message_id.into(),
                subject: format!("{file} yEnc ({part}/2)"),
                part: Some(part),
                total_parts: Some(2),
                bytes: 17,
                ..Article::default()
            }],
            ..Release::default()
        }
    }

    #[test]
    fn late_parts_complete_a_release_and_replayed_ids_do_not_requeue_it() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();

        let first = release("sample", "sample.rar", "<first@example.test>", 1);
        let duplicate = release("sample", "other.rar", "<first@example.test>", 1);
        let alternate_id = release("sample", "sample.rar", "<alternate@example.test>", 1);
        let late = release("sample", "sample.rar", "<late@example.test>", 2);
        let (first_stats, duplicate_stats, alternate_stats, late_stats, retry_stats) = (
            save(&mut client, &[first], 100).unwrap(),
            save(&mut client, &[duplicate], 101).unwrap(),
            save(&mut client, &[alternate_id], 102).unwrap(),
            save(&mut client, std::slice::from_ref(&late), 103).unwrap(),
            save(&mut client, &[late], 1000).unwrap(),
        );
        assert_eq!(first_stats, SaveStats { releases: 1, files: 1, parts: 1 });
        assert_eq!(duplicate_stats, SaveStats::default());
        assert_eq!(alternate_stats, SaveStats { releases: 0, files: 0, parts: 1 });
        assert_eq!(late_stats, SaveStats { releases: 0, files: 0, parts: 1 });
        assert_eq!(retry_stats, SaveStats::default());
        let row = client
            .query_one("select size, parts, complete, due_at, generation from releases r join pp_queue q on q.release_id = r.id", &[])
            .unwrap();
        assert_eq!(row.get::<_, i64>("size"), 51);
        assert_eq!(row.get::<_, i64>("parts"), 3);
        assert!(row.get::<_, bool>("complete"));
        assert_eq!(row.get::<_, i64>("generation"), 3);
        assert_eq!(
            row.get::<_, i64>("due_at"),
            103 + crate::catalog_facts::COMPLETE_SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn later_unnamed_fragments_keep_the_last_display_name_in_a_batch() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();
        let first = release("sample", "sample.rar", "<first@example.test>", 1);
        let mut named = release("sample", "sample.rar", "<named@example.test>", 1);
        named.display_name = Some("Name from overview".into());
        let last = release("sample", "sample.rar", "<last@example.test>", 2);
        save(&mut client, &[first, named, last], 100).unwrap();
        let row = client.query_one("select display_name from releases", &[]).unwrap();
        assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("Name from overview"));
    }

    #[test]
    fn malformed_text_preserves_message_ids_and_distinct_file_identity() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();

        let mut first = release("sample", "a\0.rar", "abc\0def", 1);
        first.articles[0].subject = "a\0.rar".into();
        let second = release("sample", "a\u{fffd}.rar", &"x".repeat(4000), 1);
        assert_eq!(save(&mut client, &[first, second], 100).unwrap(), SaveStats { releases: 1, files: 2, parts: 2 });
        let kept = crate::article_local::Store::articles_for_test(&mut *client);
        assert!(kept.iter().any(|article| article.message_id == b"abc\0def"));
    }

    #[test]
    fn late_executable_retracts_release_and_cannot_be_revived_by_replay() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();

        let good = release("sample", "sample.rar", "<first@example.test>", 1);
        let executable = release("sample", "sample.exe", "<exe@example.test>", 1);
        save(&mut client, std::slice::from_ref(&good), 100).unwrap();
        save(&mut client, &[executable], 101).unwrap();
        assert_eq!(save(&mut client, &[good], 102).unwrap(), SaveStats::default());
        for table in ["releases", "files", "segments", "pp_queue"] {
            let row = client.query_one(&format!("select count(*) from {table}"), &[]).unwrap();
            assert_eq!(row.get::<_, i64>(0), 0, "{table}");
        }
    }

    #[test]
    fn collection_keys_group_late_files_without_merging_another_collection() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();

        let mut first = release("sample", "first.rar", "<first@example.test>", 1);
        first.collection = Some("first collection".into());
        let mut late = release("new proposed name", "second.rar", "<late@example.test>", 1);
        late.collection = first.collection.clone();
        let mut other = release("sample", "first.rar", "<other@example.test>", 1);
        other.collection = Some("second collection".into());
        assert_eq!(
            save(&mut client, &[first, late, other], 100).unwrap(),
            SaveStats { releases: 2, files: 3, parts: 3 }
        );
        let rows = client.query("select name, parts from releases order by parts desc", &[]).unwrap();
        assert_eq!(rows[0].get::<_, String>(0), "sample");
        assert_eq!(rows[0].get::<_, i64>(1), 2);
        assert_eq!(rows[1].get::<_, i64>(1), 1);
    }

    #[test]
    fn a_failed_second_group_rolls_back_the_entire_save() {
        let Some(test) = crate::pg::testing::Schema::native_writer() else { return };
        let mut client = test.pool.get().unwrap();

        let first = release("sample", "sample.rar", "<first@example.test>", 1);
        let mut failing = release("sample", "sample.rar", "<other@example.test>", 1);
        failing.group = "alt.binaries.zzz".into();
        failing.articles[0].bytes = i64::MAX;
        failing.articles.push(Article { message_id: "<overflow@example.test>".into(), ..failing.articles[0].clone() });
        assert!(save(&mut client, &[first.clone(), failing], 100).is_err());
        let row = client.query_one("select count(*) from releases", &[]).unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
        assert_eq!(save(&mut client, &[first], 101).unwrap(), SaveStats { releases: 1, files: 1, parts: 1 });
    }
}

#[cfg(test)]
mod sparse_nzb_metadata_tests {
    use super::*;
    #[test]
    #[ignore="requires explicit fresh schema9 sparse metadata and canonical writer admission"]
    fn only_accepted_none_filename_messages_keep_raw_envelope_and_cascade_with_the_segment() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit sparse fresh schema");let mut client=schema.pool.get().unwrap();
        let subject=format!("rare{}é&subject",char::from(0));let poster=format!("poster{}原",char::from(0));
        let release=|name:&str,file:Option<&str>,mid:&str|crate::parser::Release {name:name.into(),poster:poster.clone(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),group:"alt.binaries.test".into(),
            articles:vec![crate::parser::Article {filename:file.map(str::to_owned),subject:subject.clone(),message_id:mid.into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()};
        let mut rare=release("Rare.Meta.Example",None,"<rare@example.test>");
        rare.articles[0].total_parts=Some(3);rare.articles[0].file_index=Some(2);rare.articles[0].file_total=Some(7);
        let named=release("Named.Meta.Example",Some("file.bin"),"<named@example.test>");
        let empty=release("Empty.Meta.Example",Some(""),"<empty@example.test>");
        let mut duplicate=rare.clone();duplicate.poster="same-batch replacement".into();duplicate.date="2021-01-01 00:00:00".into();duplicate.articles[0].subject="same-batch replacement".into();
        save(&mut client,&[rare.clone(),duplicate,named,empty],1).unwrap();
        assert_eq!(crate::pg::testing::articles(&mut *client),3);
        if let Some(store)=crate::article_local::of(&mut *client).unwrap() {
            // The same envelope, kept beside the article in the local store. A deleted
            // release's articles are left there unread, so the cascade is not asked about.
            let id:i64=client.query_one("select id from releases where name='Rare.Meta.Example'",&[]).unwrap().get(0);
            let kept=store.envelope(id,b"<rare@example.test>").unwrap().expect("the nameless article keeps its envelope");
            assert_eq!((kept.subject.as_slice(),kept.poster.as_slice(),kept.posted_date.as_slice()),(subject.as_bytes(),poster.as_bytes(),&b"Wed, 01 Jan 2020 00:00:00 +0000"[..]));
            assert_eq!((kept.nzb_timestamp,kept.expected,kept.subject_file_index,kept.subject_file_total),(1577836800,Some(3),Some(2),Some(7)));
            for (name,mid) in [("Named.Meta.Example","<named@example.test>"),("Empty.Meta.Example","<empty@example.test>")] {
                let id:i64=client.query_one("select id from releases where name=$1",&[&name]).unwrap().get(0);
                assert!(store.envelope(id,mid.as_bytes()).unwrap().is_none());
            }
            let mut replay=rare;replay.poster="changed poster".into();replay.articles[0].subject="changed subject".into();
            assert_eq!(save(&mut client,&[replay],2).unwrap().parts,0);
            assert_eq!(store.envelope(id,b"<rare@example.test>").unwrap().unwrap().subject,subject.as_bytes());
            return;
        }
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),1);
        let row=client.query_one("select m.subject,m.poster,m.posted_date,m.nzb_timestamp,m.expected,m.subject_file_index,m.subject_file_total from segment_nzb_metadata m join releases r on r.id=m.release_id where r.name='Rare.Meta.Example'",&[]).unwrap();
        assert_eq!(row.get::<_,Vec<u8>>(0),subject.as_bytes());assert_eq!(row.get::<_,Vec<u8>>(1),poster.as_bytes());
        assert_eq!(row.get::<_,Vec<u8>>(2),b"Wed, 01 Jan 2020 00:00:00 +0000");assert_eq!(row.get::<_,i64>(3),1577836800);assert_eq!(row.get::<_,Option<i64>>(4),Some(3));assert_eq!(row.get::<_,Option<i64>>(5),Some(2));assert_eq!(row.get::<_,Option<i64>>(6),Some(7));
        let mut replay=rare;replay.poster="changed poster".into();replay.date="2022-01-01 00:00:00".into();replay.articles[0].subject="changed subject".into();
        replay.articles[0].total_parts=Some(99);replay.articles[0].file_index=Some(98);replay.articles[0].file_total=Some(99);
        assert_eq!(save(&mut client,&[replay],2).unwrap().parts,0);
        let kept=client.query_one("select subject,poster,posted_date,nzb_timestamp,expected,subject_file_index,subject_file_total from segment_nzb_metadata",&[]).unwrap();
        assert_eq!(kept.get::<_,Vec<u8>>(0),subject.as_bytes());assert_eq!(kept.get::<_,Vec<u8>>(1),poster.as_bytes());
        assert_eq!(kept.get::<_,Vec<u8>>(2),b"Wed, 01 Jan 2020 00:00:00 +0000");assert_eq!(kept.get::<_,i64>(3),1577836800);
        assert_eq!(kept.get::<_,Option<i64>>(4),Some(3));assert_eq!(kept.get::<_,Option<i64>>(5),Some(2));assert_eq!(kept.get::<_,Option<i64>>(6),Some(7));
        client.execute("delete from releases where name='Rare.Meta.Example'",&[]).unwrap();
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),2);
    }
}

#[cfg(test)]
mod native_subject_counter_tests {
    use super::*;
    #[test]
    #[ignore="requires explicit fresh schema9 representative-column admission"]
    fn representative_counters_follow_subject_winner_not_aggregate_maxima_or_replay() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit fresh PostgreSQL schema9");let mut client=schema.pool.get().unwrap();
        let release=|mid:&str,part:Option<i64>,index:Option<i64>,files:Option<i64>,parts:Option<i64>|Release {
            name:"Counters.Example.Release".into(),group:"alt.binaries.test".into(),poster:"poster".into(),date:"2020-01-01".into(),
            articles:vec![Article {filename:Some("sample.bin".into()),subject:format!("subject of {mid}"),message_id:mid.into(),part,file_index:index,file_total:files,total_parts:parts,bytes:17,..Default::default()}],..Default::default()
        };
        save(&mut client,&[release("<one@example.test>",Some(1),Some(2),Some(4),Some(2))],1).unwrap();
        save(&mut client,&[release("<zero@example.test>",Some(0),Some(1),Some(7),Some(3))],2).unwrap();
        save(&mut client,&[release("<two@example.test>",Some(2),Some(6),Some(99),Some(9))],3).unwrap();
        let read=|client:&mut Client|{
            let row=client.query_one("select subject,subject_mid,subject_file_index,subject_file_total,subject_total_parts,expected,file_total,original_filename from files",&[]).unwrap();
            (row.get::<_,Option<String>>(0),row.get::<_,Option<Vec<u8>>>(1),row.get::<_,Option<i64>>(2),row.get::<_,Option<i64>>(3),row.get::<_,Option<i64>>(4),row.get::<_,Option<i64>>(5),row.get::<_,Option<i64>>(6),row.get::<_,Option<Vec<u8>>>(7))
        };
        let expected=(Some("subject of <zero@example.test>".into()),Some(b"<zero@example.test>".to_vec()),Some(1),Some(7),Some(3),Some(9),Some(99),Some(b"sample.bin".to_vec()));
        assert_eq!(read(&mut client),expected);
        let replay=save(&mut client,&[release("<zero@example.test>",Some(0),Some(111),Some(222),Some(250000))],4).unwrap();
        assert_eq!(replay.parts,0);assert_eq!(read(&mut client),expected);
        save(&mut client,&[release("<unknown@example.test>",None,None,None,None)],5).unwrap();
        let row=client.query_one("select subject_file_index,subject_file_total,subject_total_parts,original_filename from files",&[]).unwrap();
        assert_eq!(row.get::<_,Option<i64>>(0),None);assert_eq!(row.get::<_,Option<i64>>(1),None);assert_eq!(row.get::<_,Option<i64>>(2),None);
        assert_eq!(row.get::<_,Option<Vec<u8>>>(3),Some(b"sample.bin".to_vec()));
    }
}

#[cfg(test)]
mod original_name_key_tests {
    use super::*;
    #[test]
    #[ignore="requires isolated fresh PostgreSQL with the owner-approved original_name_key schema"]
    fn original_name_key_hashes_accepted_raw_name_and_keeps_first_collection_identity() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL schema");let mut client=schema.pool.get().unwrap();
        let mut release=Release {name:"Original\0.Movie".into(),collection:Some("Shared.Collection".into()),group:"alt.binaries.test".into(),poster:"poster".into(),
            date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/2)".into(),
                message_id:"name-key-first@example.invalid".into(),part:Some(1),total_parts:Some(2),bytes:13,..Default::default()}],..Default::default()};
        assert_eq!(save(&mut client,&[release.clone()],1).unwrap(),SaveStats {releases:1,files:1,parts:1});
        let first=client.query_one("select id,encode(original_name_key,'hex'),original_name_key<>identity_key from releases",&[]).unwrap();
        let id:i64=first.get(0);assert_eq!(first.get::<_,String>(1),"c5629d0e0516eaa3e9fba6a784b13d73ca8c08d0355e1856a3feb15ab46b6ac5");assert!(first.get::<_,bool>(2));
        release.name="Different\0.Title".into();release.articles[0].message_id="name-key-second@example.invalid".into();release.articles[0].part=Some(2);
        assert_eq!(save(&mut client,&[release],2).unwrap(),SaveStats {releases:0,files:0,parts:1});
        let kept=client.query_one("select id,encode(original_name_key,'hex') from releases",&[]).unwrap();
        assert_eq!(kept.get::<_,i64>(0),id);assert_eq!(kept.get::<_,String>(1),"c5629d0e0516eaa3e9fba6a784b13d73ca8c08d0355e1856a3feb15ab46b6ac5");
    }
}

#[cfg(test)]
mod posted_at_tests {
    use super::*;
    #[test]
    #[ignore="requires isolated fresh PostgreSQL with the owner-approved posted_at schema"]
    fn parsed_posted_time_follows_the_same_accepted_envelope_and_ignores_replay() {
        let schema=crate::pg::testing::Schema::native_writer().expect("explicit isolated PostgreSQL schema");let mut client=schema.pool.get().unwrap();
        let mut first=Release {name:"Envelope.Date.Release".into(),group:"alt.binaries.test".into(),poster:"first".into(),date:"Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles:vec![Article {filename:Some("video.mkv".into()),subject:"video.mkv yEnc (1/3)".into(),message_id:"date-first@example.invalid".into(),part:Some(1),total_parts:Some(3),bytes:13,..Default::default()}],..Default::default()};
        let mut second=first.clone();second.poster="second".into();second.date="Thu, 02 Jan 2020 00:00:00 +0000".into();second.articles[0].message_id="date-second@example.invalid".into();second.articles[0].part=Some(2);second.articles[0].subject="video.mkv yEnc (2/3)".into();
        assert_eq!(save(&mut client,&[first.clone(),second.clone()],1).unwrap(),SaveStats {releases:1,files:1,parts:2});
        let read=|client:&mut Client|{let row=client.query_one("select poster,posted_date,posted_at from releases where name='Envelope.Date.Release'",&[]).unwrap();(row.get::<_,String>(0),row.get::<_,String>(1),row.get::<_,i64>(2))};
        assert_eq!(read(&mut client),("second".into(),"Thu, 02 Jan 2020 00:00:00 +0000".into(),1577923200));
        second.date="Fri, 03 Jan 2020 00:00:00 +0000".into();assert_eq!(save(&mut client,&[second],2).unwrap().parts,0);
        assert_eq!(read(&mut client),("second".into(),"Thu, 02 Jan 2020 00:00:00 +0000".into(),1577923200));
        first.poster="third".into();first.date="Fri, 03 Jan 2020 00:00:00 +0000".into();first.articles[0].message_id="date-third@example.invalid".into();first.articles[0].part=Some(3);first.articles[0].subject="video.mkv yEnc (3/3)".into();
        assert_eq!(save(&mut client,&[first],3).unwrap().parts,1);
        assert_eq!(read(&mut client),("third".into(),"Fri, 03 Jan 2020 00:00:00 +0000".into(),1578009600));
    }
}

#[cfg(test)]
#[path="release_writer_inferred_tests.rs"]
mod inferred_tests;

#[cfg(test)]
#[path="release_writer_timing_tests.rs"]
mod accepted_timing_regressions;

#[cfg(test)]
#[path="release_writer_timing_controls.rs"]
mod timing_controls;

#[cfg(test)]
#[path="release_writer_plan_tests.rs"]
mod plan_experiment;

#[cfg(test)]
#[path="release_writer_multipart_plan_tests.rs"]
mod multipart_plan_experiment;
