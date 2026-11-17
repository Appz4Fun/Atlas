//! Timing from the same newly accepted COPY ordinals as segments. No inferred
//! date or replay envelope can become a new observation here.
use anyhow::{Context,Result,ensure};
use postgres::Transaction;

pub(super) fn key(tx:&mut Transaction<'_>)->Result<i64> {
    tx.query_typed_opt("select value from meta where key='key_salt' for share",&[])?
        .context("native writer timing requires the existing catalog key_salt")?
        .try_get::<_,Option<i64>>(0)?.context("native writer catalog key_salt is null")
}
pub(super) fn write(tx:&mut Transaction<'_>,accepted:u64)->Result<()> {
    let rows=tx.execute_typed(WRITE,&[])?;
    ensure!(rows>0&&rows<=accepted,"native timing accepted-file count differs from segment acceptance");Ok(())
}
pub(super) fn write_local(tx:&mut Transaction<'_>,accepted:u64,files:&[i64],present:&[i64])->Result<()> {
    let rows=tx.execute_typed(WRITE_FOLD,&[(&files,postgres::types::Type::INT8_ARRAY),(&present,postgres::types::Type::INT8_ARRAY)])?;
    ensure!(rows>0&&rows<=accepted,"native timing accepted-file count differs from segment acceptance");Ok(())
}

// At most the existing16,384-row/8MiB COPY batch participates. LAG and first
// facts preserve arrival order after filtering replay losers; each aggregate
// changes exactly once in the SAME RC transaction as its new segments.
// A missing old Timing row is not proof of a new file. Its first UTC proof
// also requires every retained segment to be among this accepted batch: the
// count stops at batch articles+1 (at most twice COPY rows across all files).
macro_rules! write_sql {
    ($head:literal, $variants:literal, $complete:literal, $variants_on_conflict:literal) => {
        concat!($head, "insert into file_timing as old(file_id,release_id,first_date,last_date,number_first,number_last,lines,
    references_seen,xref_groups_max,from_hash,from_variants,encoded_subject,late_articles,articles,
    tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last,first_utc,last_utc,utc_complete)
select file_id,release_id,first_date,last_date,number_first,number_last,lines,references_seen,xref_groups_max,
    from_last,", $variants, ",encoded_subject,0,articles,tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last,first_utc,last_utc,
    ", $complete, "
from folded where true
on conflict(file_id) do update set
    release_id=excluded.release_id,
    first_date=least(old.first_date,excluded.first_date),last_date=greatest(old.last_date,excluded.last_date),
    first_utc=least(old.first_utc,excluded.first_utc),last_utc=greatest(old.last_utc,excluded.last_utc),
    utc_complete=old.utc_complete and excluded.utc_complete,
    number_first=least(old.number_first,excluded.number_first),number_last=greatest(old.number_last,excluded.number_last),
    lines=coalesce(old.lines,0)+excluded.lines,references_seen=old.references_seen+excluded.references_seen,
    xref_groups_max=greatest(old.xref_groups_max,excluded.xref_groups_max),
    from_variants=", $variants_on_conflict, ",
    from_hash=excluded.from_hash,encoded_subject=greatest(old.encoded_subject,excluded.encoded_subject),
    articles=old.articles+excluded.articles,
    tool=coalesce(old.tool,excluded.tool),clock_first=least(old.clock_first,excluded.clock_first),
    clock_last=greatest(old.clock_last,excluded.clock_last),hw_host=coalesce(old.hw_host,excluded.hw_host),
    hw_key=coalesce(old.hw_key,excluded.hw_key),hw_n_first=least(old.hw_n_first,excluded.hw_n_first),
    hw_n_last=greatest(old.hw_n_last,excluded.hw_n_last)")
    };
}
const WRITE:&str=write_sql!("
with ordered as materialized (
    select *,lag(t_from) over(partition by file_id order by ordinal) as previous_from
    from pg_temp.atlas_release_input where file_id is not null
), folded as (
    select file_id,release_id,min(t_date) as first_date,max(t_date) as last_date,
        min(t_utc) as first_utc,max(t_utc) as last_utc,bool_and(t_utc is not null) as utc_complete,
        min(t_number) as number_first,max(t_number) as number_last,sum(t_lines)::bigint as lines,
        sum(t_references)::bigint as references_seen,max(t_xref) as xref_groups_max,
        (array_agg(t_from order by ordinal))[1] as from_first,
        (array_agg(t_from order by ordinal desc))[1] as from_last,
        count(*) filter(where previous_from is distinct from t_from) as from_variants,
        max(t_encoded) as encoded_subject,count(*) as articles,
        (array_agg(t_tool order by ordinal) filter(where t_tool is not null))[1] as tool,
        min(t_clock) as clock_first,max(t_clock) as clock_last,
        (array_agg(t_hw_host order by ordinal) filter(where t_hw_host is not null))[1] as hw_host,
        (array_agg(t_hw_key order by ordinal) filter(where t_hw_key is not null))[1] as hw_key,
        min(t_hw_n) as hw_n_first,max(t_hw_n) as hw_n_last
    from ordered group by file_id,release_id
)
", "from_variants", "utc_complete and (exists(select 1 from file_timing where file_id=folded.file_id)
        or articles=(select count(*) from(select 1 from segments where file_id=folded.file_id limit folded.articles+1) present))",
"old.from_variants+excluded.from_variants-
        case when old.from_hash=(select f.from_first from folded f where f.file_id=excluded.file_id) then 1 else 0 end");
// The writer folded each file's accepted articles itself and the local article
// store counted the file's articles, no further than this batch's plus one.
// Each file's local count is read from the arrays by the stage row's `ord`
// (`files`/`present` are aligned with the stage, `files` checks it) and its old
// Timing row (whether there was one, and its last sender) joined by primary key
// once up front, instead of looked up per row by correlated subqueries, which
// made this statement quadratic in the batch; no join on an unanalyzed stage is
// left for the planner to misjudge (the stage is not analyzed). The
// sender run joined across batches is taken off here, so a conflict only adds
// `excluded`'s count. Reading file_timing before the insert is the same as
// reading it in ON CONFLICT only because no other writer changes these files'
// Timing meanwhile: every save of them holds the group#shard advisory lock
// (`lock_parts`) for the whole transaction.
pub(super) const WRITE_FOLD:&str=write_sql!("
with folded as materialized (
    select f.*,case when ($1::bigint[])[f.ord+1]=f.file_id then ($2::bigint[])[f.ord+1] end as present,
        o.from_hash as old_from,o.file_id is not null as had
    from pg_temp.atlas_file_fold f
    left join file_timing o on o.file_id=f.file_id
)
", "from_variants-case when old_from=from_first then 1 else 0 end", "utc_complete and (had or articles=present)",
"old.from_variants+excluded.from_variants");

#[cfg(test)]
mod fold_tests {
    use super::*;
    use postgres::types::Type;

    // WRITE_FOLD as it was before its per-row lookups were joined up front.
    const WRITE_FOLD_CORRELATED:&str=write_sql!("
with folded as (select * from pg_temp.atlas_file_fold)
", "from_variants", "utc_complete and (exists(select 1 from file_timing where file_id=folded.file_id)
        or articles=(select p.present from unnest($1::bigint[],$2::bigint[]) as p(file_id,present) where p.file_id=folded.file_id))",
"old.from_variants+excluded.from_variants-
        case when old.from_hash=(select f.from_first from folded f where f.file_id=excluded.file_id) then 1 else 0 end");

    fn run(tx:&mut Transaction<'_>,sql:&str,files:&[i64],present:&[i64])->(u64,Vec<String>) {
        let n=tx.execute_typed(sql,&[(&files,Type::INT8_ARRAY),(&present,Type::INT8_ARRAY)]).unwrap();
        let rows=tx.query("select t::text from file_timing t order by file_id",&[]).unwrap();
        (n,rows.iter().map(|r|r.get(0)).collect())
    }

    #[test]
    #[ignore = "needs ATLAS_PG_TEST_URL"]
    fn joined_fold_writes_the_same_timing_as_the_correlated_one() {
        let Some(s)=crate::pg::testing::Schema::new() else {return};
        let mut c=s.pool.get().unwrap();
        c.batch_execute("insert into releases(id,group_name,is_collection,identity_key,name,poster,posted_date,is_obfuscated)
            values(1,'a.b',false,'\\x01','R','p','d',false),(2,'a.b',false,'\\x02','S','p','d',false);
            insert into files(id,release_id,filename_key,filename)
            select n,1+n%2,int8send(n),'f' from generate_series(1,120) n").unwrap();
        // Files 1..=60 had Timing (last sender 1..=3 or none); 61..=120 are new. Senders repeat across the batch.
        c.batch_execute("insert into file_timing(file_id,release_id,first_date,last_date,number_first,number_last,lines,
                references_seen,xref_groups_max,from_hash,from_variants,encoded_subject,articles,first_utc,last_utc,utc_complete)
            select n,1+n%2,100+n,200+n,n,n+5,10,1,2,case when n%4=0 then null else n%3+1 end,2,0,3,
                case when n%5=0 then null else 50 end,case when n%5=0 then null else 60 end,n%5<>0 and n%2=0
            from generate_series(1,60) n").unwrap();
        let mut tx=c.transaction().unwrap();
        tx.batch_execute(super::super::CREATE_FOLD_STAGE).unwrap();
        tx.batch_execute("insert into pg_temp.atlas_file_fold(ord,release_id,file_id,filename_key,filename,original_filename,
                subject,subject_mid,zero_file_index,first_date,last_date,first_utc,last_utc,utc_complete,number_first,number_last,
                lines,references_seen,xref_groups_max,from_first,from_last,from_variants,encoded_subject,articles,tool,
                clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last)
            select n-1,1,n*2,'\\x00','f','\\x00','s','\\x00',false,90+n,300,
                case when n%3=0 then null else 40 end,case when n%3=0 then null else 70 end,n%3<>0,
                n-1,n+9,5,n%2,n%7,n%3+1,(n+1)%3+1,1+n%2,n%2,2+n%3,case when n%2=0 then 7 end,
                n,n+1,case when n%3=0 then 'h' end,null,n,n+2
            from generate_series(1,60) n").unwrap();
        // fold file ids 2,4,..,120: half existing, half new; present matches articles for some new ones only
        let files:Vec<i64>=(1..=60).map(|n|n*2).collect();
        let present:Vec<i64>=(1..=60).map(|n|if n%4==0 {2+n%3} else {2+n%3+1}).collect();
        tx.batch_execute("savepoint a").unwrap();
        let old=run(&mut tx,WRITE_FOLD_CORRELATED,&files,&present);
        tx.batch_execute("rollback to savepoint a").unwrap();
        let new=run(&mut tx,WRITE_FOLD,&files,&present);
        assert_eq!(old.0,60);
        assert_eq!(old,new,"the joined fold must leave file_timing exactly as the correlated one");
        // the data covered each branch: a sender run joined across batches, a new file proved whole, and one not
        assert!(new.1.len()==90);
    }
}
