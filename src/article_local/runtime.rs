//! Opening the local article store when a process starts, and the one-time
//! move of the articles an existing catalogue still holds in PostgreSQL.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};
use postgres::binary_copy::BinaryCopyOutIter;
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::Type;

use super::{Envelope, NewRow, Store};

/// Which catalogue a store's ids belong to: its nonce when it has one, since
/// a schema dropped and made again under the same name hands out the same ids.
fn binding(client: &mut impl postgres::GenericClient, schema: &str) -> Result<String> {
    let known: bool = client
        .query_one(
            "select exists(select 1 from information_schema.tables where table_schema = $1 and table_name = 'native_catalog_identity')",
            &[&schema],
        )?
        .try_get(0)?;
    if known
        && let Some(row) = client.query_opt("select encode(catalog_nonce, 'hex') from native_catalog_identity", &[])?
    {
        return Ok(format!("nonce:{}", row.try_get::<_, String>(0)?));
    }
    Ok(format!("schema:{schema}"))
}

/// The store PostgreSQL's counts are of, as the catalogue names it.
fn named(client: &mut impl postgres::GenericClient) -> Result<Option<String>> {
    Ok(client.query_opt("select text_value from meta where key = 'article_store'", &[])?.and_then(|row| row.get(0)))
}

/// The store the indexer writes for `schema`, opened and registered on first
/// use when config.toml turns it on. Saves a dead process left half done are
/// settled, and a catalogue with articles still in PostgreSQL has them moved.
/// The catalogue then names the store: its counts are of the articles in it,
/// so a start with another store, an emptied one or none is refused.
pub(crate) fn ensure_writer(owner: &mut postgres::Client, schema: &str, stop: &AtomicBool) -> Result<Option<Arc<Store>>> {
    let store = match super::of_schema(schema) {
        Some(store) => store,
        None => {
            let Some(root) = configured() else {
                ensure!(
                    named(owner)?.is_none(),
                    "this catalogue's articles are in a local article store: turn [article_store] back on"
                );
                return Ok(None);
            };
            let store = Store::open_writer(&root, &binding(owner, schema)?)?;
            super::register(schema, store)?
        }
    };
    ensure!(store.is_writer(), "the local article store of this schema is open for reading only");
    let id = match store.meta("id")? {
        Some(id) => id,
        None => {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness for the local article store's id: {e}"))?;
            let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            store.set_meta("id", &id)?;
            id
        }
    };
    let named = named(owner)?;
    ensure!(
        named.as_ref().is_none_or(|named| *named == id),
        "the catalogue counts the articles of another local article store than {}: put that store back, or start a new catalogue",
        store.path().display()
    );
    if store.has_unsettled()? {
        store.settle(|xid| super::pg_ended(owner, xid))?;
    }
    if store.meta("imported")?.is_none() {
        ensure!(named.is_none(), "the local article store {} lost its place in the move out of PostgreSQL", store.path().display());
        import(owner, &store, stop)?;
    }
    if named.is_none() {
        // the store's id and import mark on disk before the catalogue names it: its commits arent synced one by one,
        // and a catalogue naming a store that lost them would refuse it on every start
        store.sync()?;
        owner.execute("insert into meta (key, text_value) values ('article_store', $1)", &[&id])?;
    }
    Ok(Some(store))
}

/// The store a reading process uses for `schema`, opened on first use when
/// config.toml turns it on. Until the indexer has made it and finished moving
/// the catalogue's articles into it, reads stay on PostgreSQL, which still has
/// them all. A store filled for another catalogue is refused.
pub(crate) fn ensure_reader(client: &mut impl postgres::GenericClient, schema: &str) -> Result<()> {
    if super::of_schema(schema).is_some() {
        return Ok(());
    }
    let Some(root) = configured() else { return Ok(()) };
    let Some(named) = named(client)? else { return Ok(()) };
    let store = Store::open_reader(&root).context("opening the local article store the catalogue names")?;
    ensure!(
        store.meta("id")?.as_deref() == Some(named.as_str()) && store.catalogue()?.as_deref() == Some(binding(client, schema)?.as_str()),
        "the local article store {} is not the one this catalogue counts",
        store.path().display()
    );
    // Two requests may race to open it; the first one registered is the one used.
    let _ = super::register(schema, store);
    Ok(())
}

#[cfg(not(test))]
fn configured() -> Option<std::path::PathBuf> {
    let config = crate::config::article_store_config();
    config.enabled.then(|| config.root())
}

/// Tests register the stores they want themselves and never read config.toml.
#[cfg(test)]
fn configured() -> Option<std::path::PathBuf> {
    None
}

const IMPORT_RELEASES: i64 = 2_000;
const IMPORT_ROWS: usize = 20_000;

/// Copy every article PostgreSQL holds into the store, a range of release ids
/// at a time, remembering how far it got so a restart carries on from there.
fn import(client: &mut postgres::Client, store: &Store, stop: &AtomicBool) -> Result<()> {
    let bounds = client.query_one("select min(release_id), max(release_id) from segments", &[])?;
    let (Some(first), Some(last)): (Option<i64>, Option<i64>) = (bounds.try_get(0)?, bounds.try_get(1)?) else {
        return store.set_meta("imported", "0");
    };
    let mut next = match store.meta("import_next")? {
        Some(next) => next.parse::<i64>().context("the local article store's import position is not a number")?,
        None => first,
    };
    let started = std::time::Instant::now();
    let mut moved = 0u64;
    eprintln!("local article store: moving the articles of releases {next}..={last} out of PostgreSQL");
    while next <= last {
        if stop.load(Ordering::Acquire) {
            bail!("stopped while moving articles into the local article store; the next start carries on");
        }
        let end = next.saturating_add(IMPORT_RELEASES);
        moved += import_range(client, store, next, end)?;
        next = end;
        store.set_meta("import_next", &next.to_string())?;
        if started.elapsed().as_secs() > 0 && (next - first) / IMPORT_RELEASES % 50 == 0 {
            eprintln!(
                "local article store: {moved} articles moved, release {next} of {last}, {:.0} a second",
                moved as f64 / started.elapsed().as_secs_f64()
            );
        }
    }
    eprintln!("local article store: {moved} articles moved in {:.0}s", started.elapsed().as_secs_f64());
    store.set_meta("imported", &moved.to_string())
}

struct Owned {
    release_id: i64,
    file_id: i64,
    message_id: Vec<u8>,
    part: Option<i64>,
    bytes: i64,
    envelope: Option<Envelope>,
}

fn import_range(client: &mut postgres::Client, store: &Store, from: i64, until: i64) -> Result<u64> {
    let reader = client.copy_out(&format!(
        "copy (select s.release_id, s.file_id, s.message_id, s.part, s.bytes,
                m.subject, m.poster, m.posted_date, m.nzb_timestamp, m.expected, m.subject_file_index, m.subject_file_total
            from segments s left join segment_nzb_metadata m using (release_id, message_key)
            where s.release_id >= {from} and s.release_id < {until}) to stdout binary"
    ))?;
    use Type as T;
    let mut rows = BinaryCopyOutIter::new(
        reader,
        &[T::INT8, T::INT8, T::BYTEA, T::INT8, T::INT8, T::BYTEA, T::BYTEA, T::BYTEA, T::INT8, T::INT8, T::INT8, T::INT8],
    );
    let mut batch = Vec::with_capacity(IMPORT_ROWS);
    let mut moved = 0;
    let mut flush = |batch: &mut Vec<Owned>| -> Result<()> {
        let rows: Vec<NewRow<'_>> = batch
            .iter()
            .map(|row| NewRow {
                release_id: row.release_id,
                file_id: row.file_id,
                message_id: &row.message_id,
                part: row.part,
                bytes: row.bytes,
                envelope: row.envelope.clone(),
            })
            .collect();
        moved += store.import(&rows)?;
        batch.clear();
        Ok(())
    };
    while let Some(row) = rows.next()? {
        let subject: Option<Vec<u8>> = row.try_get(5)?;
        batch.push(Owned {
            release_id: row.try_get(0)?,
            file_id: row.try_get(1)?,
            message_id: row.try_get(2)?,
            part: row.try_get(3)?,
            bytes: row.try_get(4)?,
            envelope: match subject {
                Some(subject) => Some(Envelope {
                    subject,
                    poster: row.try_get(6)?,
                    posted_date: row.try_get(7)?,
                    nzb_timestamp: row.try_get(8)?,
                    expected: row.try_get(9)?,
                    subject_file_index: row.try_get(10)?,
                    subject_file_total: row.try_get(11)?,
                }),
                None => None,
            },
        });
        if batch.len() == IMPORT_ROWS {
            flush(&mut batch)?;
        }
    }
    flush(&mut batch)?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

    #[test]
    fn articles_already_in_postgresql_move_into_a_new_store_once() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        if crate::article_local::of_schema(&schema.name).is_some() {
            return; // this run keeps every test's articles local from the start
        }
        let mut client = schema.pool.get().unwrap();
        let article = |file: Option<&str>, id: &str, part: Option<i64>| Article {
            filename: file.map(str::to_owned),
            message_id: id.into(),
            subject: format!("{} yEnc", file.unwrap_or("nameless")),
            part,
            total_parts: Some(2),
            bytes: 11,
            ..Article::default()
        };
        let release = |name: &str, articles: Vec<Article>| Release {
            name: name.into(),
            group: "alt.binaries.test".into(),
            poster: "poster".into(),
            date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
            articles,
            ..Release::default()
        };
        crate::pg::release_writer::save(
            &mut client,
            &[
                release("one", vec![article(Some("a.rar"), "<a1@x>", Some(1)), article(Some("a.rar"), "<a2@x>", Some(2))]),
                release("two", vec![article(None, "<n1@x>", None)]),
            ],
            1,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_writer(dir.path(), "test").unwrap();
        let stop = AtomicBool::new(false);
        import(&mut client, &store, &stop).unwrap();
        assert_eq!(store.meta("imported").unwrap().as_deref(), Some("3"));
        let ids: Vec<(i64, String)> = client
            .query("select id, name from releases order by name", &[])
            .unwrap()
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        assert_eq!(store.has(&[(ids[0].0, b"<a1@x>"), (ids[0].0, b"<a2@x>"), (ids[1].0, b"<n1@x>"), (ids[1].0, b"<a1@x>")]).unwrap(), vec![true, true, true, false]);
        let envelope = store.envelope(ids[1].0, b"<n1@x>").unwrap().expect("a nameless article keeps its envelope");
        assert_eq!(envelope.subject, b"nameless yEnc");
        assert_eq!(envelope.poster, b"poster");
        assert_eq!(envelope.expected, Some(2));
        assert!(store.envelope(ids[0].0, b"<a1@x>").unwrap().is_none());
        // With the store registered, a replay is a replay and a new part counts once.
        crate::article_local::register(&schema.name, store).unwrap();
        let again = crate::pg::release_writer::save(
            &mut client,
            &[release("one", vec![article(Some("a.rar"), "<a2@x>", Some(2)), article(Some("a.rar"), "<a3@x>", Some(2))])],
            2,
        );
        crate::article_local::unregister(&schema.name);
        assert_eq!(again.unwrap(), crate::pg::release_writer::SaveStats { releases: 0, files: 0, parts: 1 });
        let row = client.query_one("select parts, size, complete from releases where name = 'one'", &[]).unwrap();
        assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1), row.get::<_, bool>(2)), (3, 33, true));
    }

    #[test]
    fn a_save_postgresql_rolled_back_leaves_nothing_in_the_store() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        let registered_here = crate::article_local::of_schema(&schema.name).is_none();
        let dir = tempfile::tempdir().unwrap();
        if registered_here {
            crate::article_local::register(&schema.name, Store::open_writer(dir.path(), "test").unwrap()).unwrap();
        }
        let store = crate::article_local::of_schema(&schema.name).unwrap();
        let mut client = schema.pool.get().unwrap();
        let release = |id: &str, part: i64| Release {
            name: "sample".into(),
            group: "alt.binaries.test".into(),
            articles: vec![Article {
                filename: Some("sample.rar".into()),
                message_id: id.into(),
                subject: "sample.rar yEnc".into(),
                part: Some(part),
                total_parts: Some(2),
                bytes: 7,
                ..Article::default()
            }],
            ..Release::default()
        };
        crate::pg::release_writer::save(&mut client, &[release("<kept@x>", 1)], 1).unwrap();
        let id: i64 = client.query_one("select id from releases", &[]).unwrap().get(0);
        {
            let mut tx = client.transaction().unwrap();
            crate::pg::release_writer::save_in(&mut tx, &[release("<lost@x>", 2)], 2).unwrap();
            assert_eq!(store.has(&[(id, b"<lost@x>")]).unwrap(), vec![true], "written before PostgreSQL commits");
            // dropped without a commit: PostgreSQL rolls the save back
        }
        // The next save settles it, so the same article is new again and counted once.
        let replay = crate::pg::release_writer::save(&mut client, &[release("<lost@x>", 2)], 3);
        let row = client.query_one("select parts, complete from releases", &[]).unwrap();
        if registered_here {
            crate::article_local::unregister(&schema.name);
        }
        assert_eq!(replay.unwrap().parts, 1);
        assert_eq!((row.get::<_, i64>(0), row.get::<_, bool>(1)), (2, true));
        assert_eq!(store.has(&[(id, b"<kept@x>"), (id, b"<lost@x>")]).unwrap(), vec![true, true]);
    }

    #[test]
    fn a_catalogue_names_its_store_and_refuses_another() {
        let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
        if crate::article_local::of_schema(&schema.name).is_some() {
            crate::article_local::unregister(&schema.name);
        }
        let mut owner = postgres::Client::connect(&schema.url, postgres::NoTls).unwrap();
        owner.batch_execute(&format!("set search_path to \"{}\"", schema.name)).unwrap();
        let stop = AtomicBool::new(false);
        assert!(ensure_writer(&mut owner, &schema.name, &stop).unwrap().is_none(), "nothing configured, nothing named");
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        crate::article_local::register(&schema.name, Store::open_writer(first.path(), "test").unwrap()).unwrap();
        let store = ensure_writer(&mut owner, &schema.name, &stop).unwrap().unwrap();
        assert_eq!(named(&mut owner).unwrap(), store.meta("id").unwrap());
        assert!(ensure_writer(&mut owner, &schema.name, &stop).is_ok(), "the same store again");
        crate::article_local::unregister(&schema.name);
        // Turned off again, or an emptied store put in its place: the counts would be added twice.
        assert!(ensure_writer(&mut owner, &schema.name, &stop).is_err());
        crate::article_local::register(&schema.name, Store::open_writer(second.path(), "test").unwrap()).unwrap();
        let refused = ensure_writer(&mut owner, &schema.name, &stop);
        crate::article_local::unregister(&schema.name);
        assert!(refused.is_err());
    }

    /// How fast the writer takes articles, with them local and with them in PostgreSQL.
    #[test]
    #[ignore = "a measurement, not a check: prints articles a second"]
    fn measure_save_rate() {
        for with_store in [true, false] {
            let Some(schema) = crate::pg::testing::Schema::native_writer() else { return };
            let dir = tempfile::tempdir().unwrap();
            let already = crate::article_local::of_schema(&schema.name).is_some();
            if with_store && !already {
                crate::article_local::register(&schema.name, Store::open_writer(dir.path(), "bench").unwrap()).unwrap();
            }
            if !with_store && already {
                continue;
            }
            let mut client = schema.pool.get().unwrap();
            let (releases, per) = (40usize, 5000usize);
            let mut total = std::time::Duration::ZERO;
            for round in 0..5 {
                let batch: Vec<Release> = (0..releases)
                    .map(|r| Release {
                        name: format!("Bench.Release.{round}.{r}"),
                        group: "alt.binaries.bench".into(),
                        poster: "poster@example.test".into(),
                        date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
                        articles: (0..per)
                            .map(|a| Article {
                                filename: Some(format!("bench.{round}.{r}.part{:03}.rar", a / 100)),
                                message_id: format!("<{round:02}{r:04}{a:06}abcdef0123456789@bench.example.test>"),
                                subject: format!("Bench.Release.{round}.{r} [{}/50] \"bench.part{:03}.rar\" yEnc ({}/100)", a / 100 + 1, a / 100, a % 100 + 1),
                                part: Some((a % 100 + 1) as i64),
                                total_parts: Some(100),
                                bytes: 750_000,
                                ..Article::default()
                            })
                            .collect(),
                        ..Release::default()
                    })
                    .collect();
                let started = std::time::Instant::now();
                crate::pg::release_writer::save(&mut client, &batch, 1 + round as i64).unwrap();
                total += started.elapsed();
            }
            if with_store && !already {
                crate::article_local::unregister(&schema.name);
            }
            let articles = 5 * releases * per;
            eprintln!(
                "save rate, articles {}: {articles} in {:.2}s = {:.0} a second",
                if with_store { "local" } else { "in PostgreSQL" },
                total.as_secs_f64(),
                articles as f64 / total.as_secs_f64()
            );
        }
    }
}
