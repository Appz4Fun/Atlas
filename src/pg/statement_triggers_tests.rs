//! Migration 0014: the release triggers that run once per statement keep the catalog metrics a recount gives, and
//! mark each changed release for search once per statement.
use anyhow::Result;

const RECOUNT: &str = "select group_name,count(*)::bigint,sum(greatest(size,0))::bigint,sum(complete::int)::bigint,
    sum((parts>0)::int)::bigint,sum((display_name is not null)::int)::bigint,
    sum((is_obfuscated and display_name is null)::int)::bigint,
    sum((exists(select 1 from predb_match p where p.release_id=r.id))::int)::bigint
    from releases r group by group_name order by group_name";
const KEPT: &str = "select group_name,sum(total)::bigint,sum(bytes)::bigint,sum(complete)::bigint,sum(nzb_ready)::bigint,
    sum(named)::bigint,sum(still_obfuscated)::bigint,sum(predb_matched)::bigint
    from native_metric_catalog_groups group by group_name order by group_name";

fn rows(client: &mut postgres::Client, sql: &str) -> Result<Vec<(String, Vec<i64>)>> {
    Ok(client
        .query(sql, &[])?
        .iter()
        .map(|r| (r.get(0), (1..8).map(|i| r.get::<_, i64>(i)).collect()))
        .collect())
}

fn agree(client: &mut postgres::Client, step: &str) -> Result<()> {
    assert_eq!(rows(client, KEPT)?, rows(client, RECOUNT)?, "catalog metrics after {step}");
    Ok(())
}

fn revision(client: &mut postgres::Client) -> Result<i64> {
    // a sequence since migration 0016; nextval in this session moves last_value
    Ok(client.query_one("select last_value from native_catalog_revision_seq", &[])?.get(0))
}

fn marks(client: &mut postgres::Client) -> Result<Vec<(i64, i64)>> {
    Ok(client.query("select release_id,n from word_queue order by release_id", &[])?.iter().map(|r| (r.get(0), r.get(1))).collect())
}

#[test]
#[ignore = "requires isolated PostgreSQL"]
fn statement_release_triggers_keep_metrics_and_search_marks() {
    let schema = super::testing::Schema::new().expect("explicit isolated PostgreSQL");
    let runtime = super::stage_queue_tests::runtime(&schema);
    runtime
        .owner
        .with(|client| {
            client.batch_execute("delete from word_queue")?;
            let before = revision(client)?;
            client.execute(
                "insert into releases(group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
                 select case when n<=4 then 'a.group' else 'b.group' end,false,decode(lpad(to_hex(n),64,'0'),'hex'),
                    'Release.'||n,'p','d',n%2=0
                 from generate_series(1,6) n",
                &[],
            )?;
            agree(client, "insert")?;
            assert_eq!(revision(client)?, before + 1, "one revision bump per insert statement");
            let ids: Vec<i64> = client.query("select id from releases order by id", &[])?.iter().map(|r| r.get(0)).collect();
            let first: Vec<(i64, i64)> = marks(client)?;
            assert_eq!(first.iter().map(|m| m.0).collect::<Vec<_>>(), ids, "every inserted release marked");
            let start = first[0].1;
            assert!(first.iter().all(|m| m.1 == start), "marked once each");

            client.batch_execute(
                "insert into ext_predb_feeds(source,key) values(7,'feed') on conflict do nothing;
                 insert into ext_predb_titles(id,title,source,added_at) values(1,'T',7,1) on conflict do nothing",
            )?;
            client.execute("insert into predb_match(release_id,title_id,kind,category,matched_at) values($1,1,1,'Movies',1)", &[&ids[1]])?;

            // sizes, parts, completion and names of three releases, one of them moving to the other group
            let before = revision(client)?;
            client.execute(
                "update releases set size=size+100, parts=parts+2, complete=(id=$1), display_name=case when id=$2 then 'Named' end,
                    group_name=case when id=$3 then 'b.group' else group_name end
                 where id = any($4)",
                &[&ids[0], &ids[1], &ids[2], &vec![ids[0], ids[1], ids[2]]],
            )?;
            agree(client, "update")?;
            assert_eq!(revision(client)?, before + 1, "one revision bump per update statement");
            let n: Vec<i64> = marks(client)?.iter().map(|m| m.1).collect();
            let s = start;
            assert_eq!(n, vec![s + 1, s + 1, s + 1, s, s, s]);

            // a change no search field sees marks nothing
            let before = revision(client)?;
            client.execute("update releases set native_work_revision=native_work_revision+1", &[])?;
            agree(client, "work revision")?;
            assert_eq!(revision(client)?, before);
            assert_eq!(marks(client)?.iter().map(|m| m.1).collect::<Vec<_>>(), vec![s + 1, s + 1, s + 1, s, s, s]);

            // every release of a.group moves away: its row goes, b.group gains them
            client.execute("update releases set group_name='b.group' where group_name='a.group'", &[])?;
            agree(client, "emptying a group")?;
            client.execute("update releases set group_name='c.group', size=size+1 where id=$1", &[&ids[5]])?;
            agree(client, "moving one")?;
            client.execute("delete from releases where id=$1", &[&ids[5]])?;
            agree(client, "delete")?;
            Ok(())
        })
        .unwrap();
    runtime.owner.close();
}
