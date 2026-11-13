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
        256 + r.name.len() + r.poster.len() + r.date.len() + r.display_name.as_deref().map_or(0, str::len) + article + rare
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
    super::history_gate::native(&mut tx)?;
    tx.batch_execute(CREATE_STAGE)?;
    let mut stats = SaveStats::default();
    for (group, releases) in groups {
        tx.query_one("select pg_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1, 7936))", &[&group])?;
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
            let rows = std::iter::once(None)
                .filter(|_| marker)
                .chain(release.articles.iter().filter(|article| !marker && !article.message_id.is_empty()).map(Some));
            for article in rows {
                let row = Incoming { release, release_key: key.1, article };
                let size = row.approx_bytes();
                if batch.len() == MAX_BATCH_ROWS || (!batch.is_empty() && bytes + size > MAX_BATCH_BYTES) {
                    flush(&mut tx, group, &batch, now, &mut stats)?;
                    batch.clear();
                    bytes = 0;
                }
                batch.push(row);
                bytes += size;
            }
        }
        if !batch.is_empty() {
            flush(&mut tx, group, &batch, now, &mut stats)?;
        }
    }
    tx.commit()?;
    Ok(stats)
}

fn pg_text(value: &str) -> Cow<'_, str> {
    if value.as_bytes().contains(&0) { Cow::Owned(value.replace('\0', "\u{fffd}")) } else { Cow::Borrowed(value) }
}

fn flush(tx: &mut Transaction<'_>, group: &str, batch: &[Incoming<'_>], now: i64, stats: &mut SaveStats) -> Result<()> {
    tx.batch_execute("truncate pg_temp.atlas_release_input")?;
    copy_input(tx, batch)?;
    let release = tx.query_one(WRITE_RELEASES, &[&group])?;
    stats.releases += release.get::<_, i64>(0) as u64;
    let articles = tx.query_one(WRITE_ARTICLES, &[&group])?;
    stats.files += articles.get::<_, i64>(0) as u64;
    let parts = articles.get::<_, i64>(1) as u64;
    stats.parts += parts;
    if parts > 0 {
        tx.execute(UPDATE_FILES, &[&now, &crate::parser::MAX_PARTS])?;
        let complete_due = now.saturating_add(crate::store::COMPLETE_SETTLE_SECS.load(Ordering::Relaxed).max(1));
        let incomplete_due = now.saturating_add(crate::store::SETTLE_SECS.load(Ordering::Relaxed).max(1));
        tx.execute(UPDATE_RELEASES_AND_QUEUE, &[&complete_due, &incomplete_due])?;
    }
    Ok(())
}

fn copy_input(tx: &mut Transaction<'_>, batch: &[Incoming<'_>]) -> Result<()> {
    let input = tx.copy_in(
        "copy pg_temp.atlas_release_input (
            ordinal, identity_key, is_collection, name, poster, posted_date, display_name, is_obfuscated,
            junk, filename_key, filename, message_key, message_id, subject, part, expected, file_total,
            zero_file_index, bytes, nzb_subject, nzb_poster, nzb_posted_date, nzb_timestamp, nzb_file_index
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
            T::BYTEA,
            T::INT8,
            T::INT8,
        ],
    );
    for (i, row) in batch.iter().enumerate() {
        let r = row.release;
        let a = row.article;
        let filename = a.map(|article| article.filename.as_deref().unwrap_or(""));
        let filename_key: Option<[u8; 32]> = filename.map(|name| Sha256::digest(name).into());
        let message_id = a.map(|article| article.message_id.as_bytes());
        let message_key: Option<[u8; 32]> = message_id.map(|id| Sha256::digest(id).into());
        let filename = filename.map(pg_text);
        let subject = a.map(|article| pg_text(&article.subject));
        let display_name = r.display_name.as_deref().map(pg_text);
        // Sparse original NZB metadata belongs only to literal None inputs,
        // and later only to the MID that actually wins segment acceptance.
        let nameless = a.filter(|article| article.filename.is_none());
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
            &nameless.map(|article|article.subject.as_bytes()),
            &nameless.map(|_|r.poster.as_bytes()),
            &nameless.map(|_|r.date.as_bytes()),
            &nameless.map(|_|crate::dates::article_timestamp(&r.date)),
            &nameless.and_then(|article|article.file_index),
        ])?;
    }
    writer.finish()?;
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
        nzb_subject bytea,
        nzb_poster bytea,
        nzb_posted_date bytea,
        nzb_timestamp bigint,
        nzb_file_index bigint,
        release_id bigint,
        file_id bigint
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
        insert into releases (group_name, is_collection, identity_key, name, poster, posted_date, display_name, is_obfuscated)
        select distinct on (s.is_collection, s.identity_key)
            $1, s.is_collection, s.identity_key, s.name, s.poster, s.posted_date, s.display_name, s.is_obfuscated
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
            r.id as release_id, s.ordinal, s.filename_key, s.filename, s.message_key, s.message_id, s.part, s.bytes,
            s.nzb_subject,s.nzb_poster,s.nzb_posted_date,s.nzb_timestamp,s.expected,s.nzb_file_index,s.file_total
        from pg_temp.atlas_release_input s
        join releases r on r.group_name = $1 and (r.is_collection, r.identity_key) = (s.is_collection, s.identity_key)
        where not s.junk and not exists (
            select 1 from segments x where x.release_id = r.id and x.message_key = s.message_key
        )
        order by r.id, s.message_key, s.ordinal
    ), file_keys as (
        select distinct release_id, filename_key, filename from candidates
    ), added_files as (
        insert into files (release_id, filename_key, filename)
        select release_id, filename_key, filename from file_keys
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
    ), sparse_nzb as (
        insert into segment_nzb_metadata (release_id,message_key,subject,poster,posted_date,nzb_timestamp,expected,subject_file_index,subject_file_total)
        select a.release_id,a.message_key,c.nzb_subject,c.nzb_poster,c.nzb_posted_date,c.nzb_timestamp,c.expected,c.nzb_file_index,c.file_total
        from added a join candidates c using(release_id,message_key) where c.nzb_subject is not null
        on conflict(release_id,message_key) do nothing
        returning 1
    ), accepted as (
        update pg_temp.atlas_release_input s set release_id = a.release_id, file_id = a.file_id
        from added a join candidates c using (release_id, message_key) where s.ordinal = c.ordinal
        returning 1
    )
    select (select count(*) from added_files), (select count(*) from accepted)";

const UPDATE_FILES: &str = "
    with new_parts as (
        insert into file_parts (file_id, part)
        select distinct file_id, part from pg_temp.atlas_release_input
        where file_id is not null and part between 0 and $2
        on conflict do nothing
        returning file_id, part
    ), parts as (
        select file_id, count(*) as count, min(part) as first, max(part) as last from new_parts group by file_id
    ), declarations as (
        select file_id, max(expected) as expected, max(file_total) as file_total
        from pg_temp.atlas_release_input where file_id is not null group by file_id
    ), subjects as (
        select distinct on (file_id) file_id, subject, part, message_id, zero_file_index
        from pg_temp.atlas_release_input where file_id is not null
        order by file_id, part nulls first, message_id
    ), combined as (
        select f.id, greatest(f.expected, d.expected) as expected,
            greatest(f.file_total, d.file_total) as file_total,
            f.distinct_parts + coalesce(p.count, 0) as distinct_parts,
            least(f.first_part, p.first) as first_part,
            greatest(f.last_part, p.last) as last_part,
            s.subject, s.part, s.message_id, s.zero_file_index,
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
        zero_file_index = case when c.better_subject then c.zero_file_index else f.zero_file_index end
    from combined c where c.id = f.id";

const UPDATE_RELEASES_AND_QUEUE: &str = "
    with changes as (
        select release_id, count(*) as parts, sum(bytes)::bigint as bytes, max(file_total) as file_total
        from pg_temp.atlas_release_input where release_id is not null group by release_id
    ), metadata as (
        select distinct on (release_id) release_id, poster, posted_date, is_obfuscated
        from pg_temp.atlas_release_input where release_id is not null order by release_id, ordinal desc
    ), display_names as (
        select distinct on (release_id) release_id, display_name from pg_temp.atlas_release_input
        where release_id is not null and display_name is not null order by release_id, ordinal desc
    ), completion as (
        select c.release_id, count(*) as files, bool_and(f.complete) as all_whole,
            bool_or(f.zero_file_index) as zero_indexed
        from changes c join files f on f.release_id = c.release_id group by c.release_id
    ), updated as (
        update releases r set size = r.size + c.bytes, parts = r.parts + c.parts,
            file_total = greatest(r.file_total, c.file_total),
            complete = f.all_whole and (greatest(r.file_total, c.file_total) is null or
                f.files = greatest(r.file_total, c.file_total) + case when f.zero_indexed then 1 else 0 end),
            poster = m.poster, posted_date = m.posted_date,
            display_name = case when r.name_source is null then coalesce(d.display_name, r.display_name) else r.display_name end,
            is_obfuscated = case when r.name_source is null and not r.is_collection then m.is_obfuscated else r.is_obfuscated end
        from changes c join metadata m using (release_id) join completion f using (release_id)
        left join display_names d using (release_id)
        where r.id = c.release_id
        returning r.id, r.complete
    )
    insert into pp_queue (release_id, due_at)
    select id, case when complete then $1::bigint else $2::bigint end from updated
    on conflict (release_id) do update set due_at = excluded.due_at, generation = pp_queue.generation + 1";

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
        let Some(test) = crate::pg::testing::Schema::new() else { return };
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
            103 + crate::store::COMPLETE_SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn later_unnamed_fragments_keep_the_last_display_name_in_a_batch() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
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
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut client = test.pool.get().unwrap();

        let mut first = release("sample", "a\0.rar", "abc\0def", 1);
        first.articles[0].subject = "a\0.rar".into();
        let second = release("sample", "a\u{fffd}.rar", &"x".repeat(4000), 1);
        assert_eq!(save(&mut client, &[first, second], 100).unwrap(), SaveStats { releases: 1, files: 2, parts: 2 });
        let row = client.query_one("select message_id from segments where octet_length(message_id) = 7", &[]).unwrap();
        assert_eq!(row.get::<_, Vec<u8>>(0), b"abc\0def");
    }

    #[test]
    fn late_executable_retracts_release_and_cannot_be_revived_by_replay() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
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
        let Some(test) = crate::pg::testing::Schema::new() else { return };
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
        let Some(test) = crate::pg::testing::Schema::new() else { return };
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
        let schema=crate::pg::testing::Schema::new().expect("explicit sparse fresh schema");let mut client=schema.pool.get().unwrap();
        let subject=format!("rare{}é&subject",char::from(0));let poster=format!("poster{}原",char::from(0));
        let release=|name:&str,file:Option<&str>,mid:&str|crate::parser::Release {name:name.into(),poster:poster.clone(),date:"2020-01-01 00:00:00".into(),group:"alt.binaries.test".into(),
            articles:vec![crate::parser::Article {filename:file.map(str::to_owned),subject:subject.clone(),message_id:mid.into(),part:Some(1),total_parts:Some(1),bytes:17,..Default::default()}],..Default::default()};
        let mut rare=release("Rare.Meta.Example",None,"<rare@example.test>");
        rare.articles[0].total_parts=Some(3);rare.articles[0].file_index=Some(2);rare.articles[0].file_total=Some(7);
        let named=release("Named.Meta.Example",Some("file.bin"),"<named@example.test>");
        let empty=release("Empty.Meta.Example",Some(""),"<empty@example.test>");
        let mut duplicate=rare.clone();duplicate.poster="same-batch replacement".into();duplicate.date="2021-01-01 00:00:00".into();duplicate.articles[0].subject="same-batch replacement".into();
        save(&mut client,&[rare.clone(),duplicate,named,empty],1).unwrap();
        assert_eq!(client.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),3);
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),1);
        let row=client.query_one("select m.subject,m.poster,m.posted_date,m.nzb_timestamp,m.expected,m.subject_file_index,m.subject_file_total from segment_nzb_metadata m join releases r on r.id=m.release_id where r.name='Rare.Meta.Example'",&[]).unwrap();
        assert_eq!(row.get::<_,Vec<u8>>(0),subject.as_bytes());assert_eq!(row.get::<_,Vec<u8>>(1),poster.as_bytes());
        assert_eq!(row.get::<_,Vec<u8>>(2),b"2020-01-01 00:00:00");assert_eq!(row.get::<_,i64>(3),1577836800);assert_eq!(row.get::<_,Option<i64>>(4),Some(3));assert_eq!(row.get::<_,Option<i64>>(5),Some(2));assert_eq!(row.get::<_,Option<i64>>(6),Some(7));
        let mut replay=rare;replay.poster="changed poster".into();replay.date="2022-01-01 00:00:00".into();replay.articles[0].subject="changed subject".into();
        replay.articles[0].total_parts=Some(99);replay.articles[0].file_index=Some(98);replay.articles[0].file_total=Some(99);
        assert_eq!(save(&mut client,&[replay],2).unwrap().parts,0);
        let kept=client.query_one("select subject,poster,posted_date,nzb_timestamp,expected,subject_file_index,subject_file_total from segment_nzb_metadata",&[]).unwrap();
        assert_eq!(kept.get::<_,Vec<u8>>(0),subject.as_bytes());assert_eq!(kept.get::<_,Vec<u8>>(1),poster.as_bytes());
        assert_eq!(kept.get::<_,Vec<u8>>(2),b"2020-01-01 00:00:00");assert_eq!(kept.get::<_,i64>(3),1577836800);
        assert_eq!(kept.get::<_,Option<i64>>(4),Some(3));assert_eq!(kept.get::<_,Option<i64>>(5),Some(2));assert_eq!(kept.get::<_,Option<i64>>(6),Some(7));
        client.execute("delete from releases where name='Rare.Meta.Example'",&[]).unwrap();
        assert_eq!(client.query_one("select count(*) from segment_nzb_metadata",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(client.query_one("select count(*) from segments",&[]).unwrap().get::<_,i64>(0),2);
    }
}
