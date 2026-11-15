//! Real PostgreSQL regressions for the conservative unchanged-chunk proof.
//! The observer belongs only to each disposable Schema; production triggers stay enabled.
use super::*;
use crate::predb::{import::{Counts, Entry, MAX_CATEGORY}, keys::name_key};

const TITLE: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";

fn fixture() -> Option<super::super::testing::Schema> {
    let schema = super::super::testing::Schema::new()?;
    let mut client = schema.pool.get().unwrap();
    if !client.query_one("select to_regclass('ext_predb_title_keys') is not null", &[]).unwrap().get::<_, bool>(0) {
        client.batch_execute(include_str!("migrations/0010_predb_keys.fragment.sql")).unwrap();
    }
    let mut tx = guarded(&mut client);
    tx.batch_execute("insert into ext_predb_feeds(source,key,resume_page,gap_floor,last_ok_at) values(1,'fixture',41,7,1),(9,'other',31,3,1)").unwrap();
    assert!(matches!(advance_in(&mut tx, 512).unwrap(), BuildStatus::Ready(_)));
    tx.commit().unwrap();
    drop(client);
    Some(schema)
}

fn guarded(client: &mut postgres::Client) -> Transaction<'_> {
    let mut tx = client.build_transaction().isolation_level(postgres::IsolationLevel::ReadCommitted).start().unwrap();
    super::super::history_gate::native(&mut tx).unwrap();
    tx
}
fn entry(title: &str) -> Entry { Entry { title: title.into(), ..Default::default() } }
fn applied(status: ImportStatus) -> (Counts, i64) {
    let ImportStatus::Applied { counts, revision } = status else { panic!("completed matcher unexpectedly pending") };
    (counts, revision)
}
fn import(client: &mut postgres::Client, source: i64, entries: &[Entry], now: i64, cursor: Option<(i64, i64)>) -> (Counts, i64) {
    let mut tx = guarded(client);
    let result = applied(import_page_in(&mut tx, source, entries, now, cursor).unwrap());
    tx.commit().unwrap();
    result
}
fn rebuild(client: &mut postgres::Client) {
    loop {
        let mut tx = guarded(client);
        let ready = matches!(advance_in(&mut tx, 512).unwrap(), BuildStatus::Ready(_));
        tx.commit().unwrap();
        if ready { break }
    }
}
fn id(client: &mut impl GenericClient, title: &str) -> i64 {
    client.query_one("select id from ext_predb_titles where title=$1", &[&title]).unwrap().get(0)
}
fn revision(client: &mut impl GenericClient) -> i64 {
    certified_revision(client).unwrap().expect("completed matcher")
}

// JSON is only a lossless relational snapshot, never an expected-policy oracle.
// Include all columns, not just matcher fields, plus raw cache/page and history facts.
fn facts(client: &mut impl GenericClient) -> Vec<String> {
    [
        "select coalesce(jsonb_agg(to_jsonb(t) order by id)::text,'[]') from ext_predb_titles t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by key,title_id)::text,'[]') from ext_predb_title_keys t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by singleton)::text,'[]') from ext_predb_key_meta t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by singleton)::text,'[]') from ext_predb_key_build t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by source)::text,'[]') from ext_predb_feeds t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by key)::text,'[]') from ext_predb_meta t",
        "select coalesce(jsonb_agg(to_jsonb(t))::text,'[]') from ext_predb_pages t",
        "select coalesce(jsonb_agg(to_jsonb(t))::text,'[]') from ext_predb_page_events t",
        "select coalesce(jsonb_agg(to_jsonb(t))::text,'[]') from ext_http_cache t",
        "select coalesce(jsonb_agg(to_jsonb(t) order by owner_code)::text,'[]') from history_companion_receipts t",
    ].into_iter().map(|sql| client.query_one(sql, &[]).unwrap().get(0)).collect()
}
fn install_observer(client: &mut postgres::Client) {
    client.batch_execute("create table qa_predb_title_updates(affected bigint not null);
        create function qa_predb_observe_title_update() returns trigger language plpgsql as $$
        begin insert into qa_predb_title_updates(affected) select count(*) from changed_titles; return null; end $$;
        create trigger qa_predb_observe_title_update after update on ext_predb_titles
        referencing new table as changed_titles for each statement execute function qa_predb_observe_title_update()").unwrap();
}
fn observed(client: &mut impl GenericClient) -> (i64, i64) {
    let row = client.query_one("select count(*)::bigint,coalesce(sum(affected),0)::bigint from qa_predb_title_updates", &[]).unwrap();
    (row.get(0), row.get(1))
}
fn reset_observer(client: &mut postgres::Client) {
    client.execute("delete from qa_predb_title_updates", &[]).unwrap();
}
fn title_fact(client: &mut impl GenericClient, title: &str) -> (String, i64, Option<String>, Option<i64>, Option<i64>, Option<i64>, i64, Option<String>, i64) {
    let r = client.query_one("select title,source,category,size_bytes,files,pre_at,nuked,nuke_reason,added_at from ext_predb_titles where title=$1", &[&title]).unwrap();
    (r.get(0),r.get(1),r.get(2),r.get(3),r.get(4),r.get(5),r.get(6),r.get(7),r.get(8))
}
fn feed(client: &mut impl GenericClient) -> (i64, i64, i64, i64) {
    let row = client.query_one("select resume_page,gap_floor,inserted,updated from ext_predb_feeds where source=1", &[]).unwrap();
    (row.get(0),row.get(1),row.get(2),row.get(3))
}

#[test]
fn unchanged_chunk_avoids_per_entry_title_update_statements() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    let seeded = Entry { size_bytes: Some(123), files: Some(4), pre_at: Some(10), ..entry(TITLE) };
    let (seed_counts, _) = import(&mut client, 1, &[seeded.clone()], 5, None);
    assert_eq!(seed_counts, Counts { inserted: 1, ..Default::default() });
    let known = id(&mut *client, TITLE);
    install_observer(&mut client);
    // A positive affected-row control makes a broken observer fail before the cost assertion.
    let changed = Entry { category: Some("TV".into()), ..seeded };
    let (positive, _) = import(&mut client, 9, &[changed.clone()], 6, None);
    assert_eq!(positive, Counts { updated: 1, min_known_id: Some(known), ..Default::default() });
    assert_eq!(observed(&mut *client), (1, 1));
    reset_observer(&mut client); // Only QA observation rows are cleared.
    let before = facts(&mut *client);
    let (counts, _) = import(&mut client, 9, &vec![changed; 512], 99, Some((900, 900)));
    assert_eq!(counts, Counts { inserted: 0, updated: 0, skipped: 512, min_known_id: Some(known) });
    assert_eq!(facts(&mut *client), before, "unchanged page altered durable source/cache/key/feed/certification facts");
    let (statements, affected) = observed(&mut *client);
    // Root archives --nocapture terminal output, including this sanitized evidence before RED.
    println!("predb unchanged primary: entries=512 inserted={} updated={} skipped={} title_update_statements={statements} affected_rows={affected}", counts.inserted, counts.updated, counts.skipped);
    assert_eq!(affected, 0, "unchanged entries must not affect title rows");
    assert_eq!(statements, 0, "unchanged chunk must avoid per-entry title UPDATE statements");
}

#[test]
fn unchanged_fastpath_preserves_case_fill_nuke_and_min_known() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    let unicode_title = "İstanbul.Alpha.Beta.2024-GRP";
    // 255 ASCII bytes followed by a two-byte character must truncate before that character.
    let long_reason = format!("{}é", "r".repeat(255));
    let full = Entry { category: Some("TV".into()), size_bytes: Some(123), files: Some(4), pre_at: Some(10), nuked: 7, nuke_reason: Some(long_reason.clone()), ..entry(unicode_title) };
    let missing_files = Entry { category: Some("TV".into()), size_bytes: Some(99), pre_at: Some(8), ..entry("Files.Only.Fill-GRP") };
    let empty = entry("Empty.Fields.Title-GRP");
    import(&mut client, 1, &[full.clone(), missing_files.clone(), empty.clone()], 5, None);
    let known = id(&mut *client, unicode_title);
    let before = facts(&mut *client);
    let mut variants = vec![
        Entry { title: format!("  {}  ", unicode_title.to_lowercase()), category: Some("MOVIE".into()), size_bytes: Some(999), files: Some(999), pre_at: Some(999), nuked: 0, nuke_reason: Some("ignored".into()) },
        Entry { nuke_reason: Some("".into()), ..full.clone() },
        full,
        Entry { category: Some("x".repeat(MAX_CATEGORY + 1)), ..empty },
        entry("short"),
        entry("bad/slash.title-GRP"),
    ];
    // Also preserve Rust's expanding Unicode lowercase comparison and empty-category filtering.
    variants.push(Entry { title: unicode_title.to_lowercase(), category: Some("".into()), ..Default::default() });
    let (counts, _) = import(&mut client, 9, &variants, 88, Some((900, 900)));
    assert_eq!(counts, Counts { skipped: 7, min_known_id: Some(known), ..Default::default() });
    assert_eq!(facts(&mut *client), before);
    assert_eq!(title_fact(&mut *client, unicode_title), (unicode_title.into(), 1, Some("TV".into()), Some(123), Some(4), Some(10), 7, Some("r".repeat(255)), 5));
    install_observer(&mut client);
    let (counts, _) = import(&mut client, 9, &[Entry { files: Some(6), ..missing_files }], 89, None);
    assert_eq!(counts, Counts { updated: 1, min_known_id: Some(id(&mut *client, "Files.Only.Fill-GRP")), ..Default::default() });
    assert_eq!(observed(&mut *client), (1, 1), "files-only fill must fall through to a real write");
    assert_eq!(title_fact(&mut *client, "Files.Only.Fill-GRP").4, Some(6));
    // The shared import footer repairs the title total even for an invalid-only no-op.
    let before_revision = revision(&mut *client);
    client.execute("delete from ext_predb_meta where key='titles'", &[]).unwrap();
    let (invalid, after_revision) = import(&mut client, 9, &[entry("short")], 90, None);
    assert_eq!(invalid, Counts { skipped: 1, ..Default::default() });
    assert_eq!(after_revision, before_revision);
    assert_eq!(client.query_one("select value from ext_predb_meta where key='titles'", &[]).unwrap().get::<_, i64>(0), 3);
}

#[test]
fn mixed_chunk_falls_back_before_any_effect_and_preserves_order() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    import(&mut client, 1, &[entry(TITLE)], 5, None);
    let known = id(&mut *client, TITLE);
    let new = "New.Mixed.Title-GRP";
    let page = vec![
        entry(TITLE),
        Entry { category: Some("TV".into()), files: Some(1), ..entry(new) },
        Entry { category: Some("TV".into()), files: Some(4), ..entry(&TITLE.to_lowercase()) },
        Entry { category: Some("MOVIE".into()), files: Some(7), nuked: 2, nuke_reason: Some("first.reason".into()), ..entry(new) },
        Entry { nuked: 2, nuke_reason: Some("later.reason".into()), ..entry(new) },
        Entry { category: Some("MOVIE".into()), ..entry(TITLE) },
    ];
    install_observer(&mut client);
    let (counts, _) = import(&mut client, 9, &page, 10, Some((42, 7)));
    assert_eq!(counts, Counts { inserted: 1, updated: 3, skipped: 2, min_known_id: Some(known) });
    assert_eq!(title_fact(&mut *client, TITLE), (TITLE.into(), 1, Some("TV".into()), None, Some(4), None, 0, None, 5));
    assert_eq!(title_fact(&mut *client, new), (new.into(), 9, Some("TV".into()), None, Some(1), None, 2, Some("later.reason".into()), 10));
    assert_eq!(observed(&mut *client).1, 3);
    // Reversing fill-only duplicates changes which value wins; no speculative probe write may win first.
    for (name, first, second) in [("Order.Forward.Title-GRP", "TV", "MOVIE"), ("Order.Reversed.Title-GRP", "MOVIE", "TV")] {
        let (counts, _) = import(&mut client, 1, &[Entry { category: Some(first.into()), ..entry(name) }, Entry { category: Some(second.into()), ..entry(name) }], 11, None);
        assert_eq!(counts, Counts { inserted: 1, skipped: 1, ..Default::default() });
        assert_eq!(title_fact(&mut *client, name).2.as_deref(), Some(first));
    }
    // Real, naturally equal name keys nominate distinct literal titles. At 16 rows the
    // last literal candidate must still be selected, rather than trusting the first key hit.
    let variants = (0..16).map(|mask| {
        let s = |bit| if mask & (1 << bit) == 0 { "." } else { "_" };
        entry(&format!("Collision{}Alpha{}Beta{}Gamma{}Title-GRP", s(0), s(1), s(2), s(3)))
    }).collect::<Vec<_>>();
    assert!(variants.iter().all(|e| name_key(&e.title) == name_key(&variants[0].title)));
    assert_eq!(import(&mut client, 1, &variants, 12, None).0.inserted, 16);
    let before = facts(&mut *client);
    let target = id(&mut *client, &variants[15].title);
    assert_eq!(import(&mut client, 1, &[variants[15].clone()], 13, None).0, Counts { skipped: 1, min_known_id: Some(target), ..Default::default() });
    assert_eq!(facts(&mut *client), before);
    // Adding another separator spelling creates 17 genuine candidates; legacy admission
    // must still fail, never let a bulk proof treat overflow as a miss or certified no-op.
    let seventeenth = entry("Collision Alpha.Beta.Gamma.Title-GRP");
    assert_eq!(name_key(&seventeenth.title), name_key(&variants[0].title));
    assert_eq!(import(&mut client, 1, &[seventeenth], 14, None).0.inserted, 1);
    let before = facts(&mut *client);
    let mut tx = guarded(&mut client);
    let error = import_page_in(&mut tx, 1, &[variants[15].clone()], 15, None).unwrap_err();
    assert!(error.to_string().contains("candidate key exceeds admission"));
    tx.rollback().unwrap();
    assert_eq!(facts(&mut *client), before);
}

#[test]
fn multichunk_noop_tail_preserves_changed_revision_and_page_cursor() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    let known_entry = entry(TITLE);
    import(&mut client, 1, &[known_entry.clone()], 5, None);
    let known = id(&mut *client, TITLE);
    install_observer(&mut client);
    for (reverse, name, cursor) in [(false, "Earlier.Chunk.New.Title-GRP", 42), (true, "Final.Chunk.New.Title-GRP", 43)] {
        let before_revision = revision(&mut *client);
        let mut page = vec![known_entry.clone(); 513];
        if reverse { page[512] = entry(name); } else { page[0] = entry(name); page[512] = entry(name); }
        let (counts, after_revision) = import(&mut client, 1, &page, 10, Some((cursor, 7)));
        assert_eq!(counts, Counts { inserted: 1, skipped: 512, min_known_id: Some(known), ..Default::default() });
        // The first changed chunk advances once; an unchanged final chunk must retain it.
        // The reversed arrangement also checks that a no-op first chunk leaves later writes visible.
        assert_eq!(after_revision, before_revision + 1);
        assert_eq!(revision(&mut *client), after_revision);
        assert_eq!(feed(&mut *client), (cursor, 7, cursor - 40, 0));
        assert_eq!(observed(&mut *client).1, 0);
        let row = client.query_one("select title_rows,(select count(*) from ext_predb_titles),key_rows,(select count(*) from ext_predb_title_keys),top_id,(select max(id) from ext_predb_titles) from ext_predb_key_meta", &[]).unwrap();
        assert_eq!(row.get::<_,i64>(0), row.get::<_,i64>(1));
        assert_eq!(row.get::<_,i64>(2), row.get::<_,i64>(3));
        assert_eq!(row.get::<_,Option<i64>>(4), row.get::<_,Option<i64>>(5));
        let before = facts(&mut *client);
        let (noop, same_revision) = import(&mut client, 1, &page, 20, Some((999, 999)));
        assert_eq!(noop, Counts { skipped: 513, min_known_id: Some(known), ..Default::default() });
        assert_eq!(same_revision, after_revision);
        assert_eq!(facts(&mut *client), before);
    }
}

#[test]
fn bulk_probe_budget_fallback_and_rollback_preserve_certification() {
    let Some(schema) = fixture() else { return };
    let mut client = schema.pool.get().unwrap();
    // Imported feeds can coexist with preserved legacy titles. Separator expansion
    // supplies real large candidates for the same indexed key without synthetic keys.
    let page = (0..512).map(|n| entry(&format!("Budget{n:04}.Known.Title-GRP"))).collect::<Vec<_>>();
    let mut titles = Vec::with_capacity(1024);
    for e in &page {
        let decoy = e.title.replacen('.', &".".repeat(3500), 1);
        assert_eq!(name_key(&decoy), name_key(&e.title));
        titles.push(decoy);
        titles.push(e.title.clone());
    }
    let mut tx = guarded(&mut client);
    tx.execute("insert into ext_predb_titles(title,source,added_at) select title,1,5 from unnest($1::text[]) as incoming(title)", &[&titles]).unwrap();
    tx.batch_execute("insert into ext_predb_meta(key,value) values('titles',1024)").unwrap();
    tx.commit().unwrap();
    rebuild(&mut client);
    let keys = page.iter().map(|e| name_key(&e.title)).collect::<Vec<_>>();
    let extents = client.query_one("select count(*)::bigint,sum(octet_length(t.title))::bigint,max(octet_length(t.title))::bigint,count(distinct k.key)::bigint from ext_predb_title_keys k join ext_predb_titles t on t.id=k.title_id where k.key=any($1::bigint[])", &[&keys]).unwrap();
    assert_eq!(extents.get::<_, i64>(0), 1024);
    assert!(extents.get::<_, i64>(1) > 1024 * 1024);
    assert!(extents.get::<_, i64>(2) <= TEXT_BYTES);
    assert_eq!(extents.get::<_, i64>(3), 512);
    let incoming_titles = page.iter().map(|e| e.title.clone()).collect::<Vec<_>>();
    let known: i64 = client.query_one("select min(id) from ext_predb_titles where title=any($1::text[])", &[&incoming_titles]).unwrap().get(0);
    install_observer(&mut client);
    let before = facts(&mut *client);
    let (counts, _) = import(&mut client, 1, &page, 10, Some((999, 999)));
    assert_eq!(counts, Counts { skipped: 512, min_known_id: Some(known), ..Default::default() });
    assert_eq!(facts(&mut *client), before);
    assert_eq!(observed(&mut *client), (512, 0), "over-budget candidate payload must retain the original sequential path");
    reset_observer(&mut client);
    let before = facts(&mut *client);
    let before_revision = revision(&mut *client);
    let mut tx = guarded(&mut client);
    let pending_page = [entry("Rollback.New.Title-GRP"), Entry { files: Some(3), ..page[0].clone() }];
    let (counts, changed_revision) = applied(import_page_in(&mut tx, 1, &pending_page, 11, Some((42, 7))).unwrap());
    assert_eq!(counts, Counts { inserted: 1, updated: 1, min_known_id: Some(known), ..Default::default() });
    assert!(changed_revision > before_revision);
    assert_eq!(feed(&mut tx), (42, 7, 1, 1));
    assert_eq!(observed(&mut tx).1, 1);
    tx.rollback().unwrap();
    assert_eq!(facts(&mut *client), before);
    assert_eq!(observed(&mut *client), (0, 0));
    // A wrong expected revision refuses settlement. An unbuilt/in-progress index
    // stays Pending, preserving the existing build cursor rather than certifying it.
    let previous_revision = revision(&mut *client);
    let mut tx = guarded(&mut client);
    assert!(verify_read_in(&mut tx, previous_revision + 1).unwrap_err().is::<StaleRead>());
    tx.batch_execute("delete from ext_predb_key_meta; insert into ext_predb_key_build(singleton,after_id,title_rows,key_rows) values(1,17,3,9)").unwrap();
    let unbuilt = facts(&mut tx);
    assert!(matches!(import_page_in(&mut tx, 1, &page, 12, Some((42, 7))).unwrap(), ImportStatus::Pending));
    assert_eq!(facts(&mut tx), unbuilt);
    tx.rollback().unwrap();
    assert_eq!(facts(&mut *client), before);
    // Expected errors have their own rolled-back transaction; they never escape
    // through a retained native Owner callback or leave an aborted attempt to reuse.
    for inadmissible in [vec![page[0].clone(); 5001], vec![entry(&"x".repeat(8 * 1024 * 1024 + 1))]] {
        let mut tx = guarded(&mut client);
        assert!(import_page_in(&mut tx, 1, &inadmissible, 13, None).is_err());
        tx.rollback().unwrap();
        assert_eq!(facts(&mut *client), before);
    }
    // A genuine server-side error is not a budget fallback or a cache miss.
    // Roll back the isolated DDL/error transaction before reusing this client.
    let mut tx = guarded(&mut client);
    tx.batch_execute("alter table ext_predb_title_keys rename to qa_unavailable_predb_keys").unwrap();
    let error = import_page_in(&mut tx, 1, &[page[0].clone()], 14, None).unwrap_err();
    assert!(error.chain().any(|cause| cause.downcast_ref::<postgres::Error>().is_some()));
    tx.rollback().unwrap();
    assert_eq!(facts(&mut *client), before);
    assert_eq!(observed(&mut *client), (0, 0));
    // Existing per-candidate 4096-byte admission is independent of the bulk budget.
    let mut tx = guarded(&mut client);
    tx.execute("update ext_predb_titles set category=$1 where title=$2", &[&"c".repeat(4097), &page[0].title]).unwrap();
    tx.commit().unwrap();
    rebuild(&mut client);
    let oversized = facts(&mut *client);
    let mut tx = guarded(&mut client);
    let error = import_page_in(&mut tx, 1, &[page[0].clone()], 14, None).unwrap_err();
    assert!(error.to_string().contains("candidate exceeds matcher admission"));
    tx.rollback().unwrap();
    assert_eq!(facts(&mut *client), oversized);
}
