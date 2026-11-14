//! Timing from the same newly accepted COPY ordinals as segments. No inferred
//! date or replay envelope can become a new observation here.
use anyhow::{Context,Result,ensure};
use postgres::Transaction;

pub(super) fn key(tx:&mut Transaction<'_>)->Result<i64> {
    tx.query_opt("select value from meta where key='key_salt' for share",&[])?
        .context("native writer timing requires the existing catalog key_salt")?
        .try_get::<_,Option<i64>>(0)?.context("native writer catalog key_salt is null")
}
pub(super) fn write(tx:&mut Transaction<'_>,accepted:u64)->Result<()> {
    let rows=tx.execute(WRITE,&[])?;
    ensure!(rows>0&&rows<=accepted,"native timing accepted-file count differs from segment acceptance");Ok(())
}

// At most the existing16,384-row/8MiB COPY batch participates. LAG and first
// facts preserve arrival order after filtering replay losers; each aggregate
// changes exactly once in the SAME RC transaction as its new segments.
const WRITE:&str="
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
insert into file_timing as old(file_id,release_id,first_date,last_date,number_first,number_last,lines,
    references_seen,xref_groups_max,from_hash,from_variants,encoded_subject,late_articles,articles,
    tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last,first_utc,last_utc,utc_complete)
select file_id,release_id,first_date,last_date,number_first,number_last,lines,references_seen,xref_groups_max,
    from_last,from_variants,encoded_subject,0,articles,tool,clock_first,clock_last,hw_host,hw_key,hw_n_first,hw_n_last,first_utc,last_utc,utc_complete
from folded where true
on conflict(file_id) do update set
    release_id=excluded.release_id,
    first_date=least(old.first_date,excluded.first_date),last_date=greatest(old.last_date,excluded.last_date),
    first_utc=least(old.first_utc,excluded.first_utc),last_utc=greatest(old.last_utc,excluded.last_utc),
    utc_complete=old.utc_complete and excluded.utc_complete,
    number_first=least(old.number_first,excluded.number_first),number_last=greatest(old.number_last,excluded.number_last),
    lines=coalesce(old.lines,0)+excluded.lines,references_seen=old.references_seen+excluded.references_seen,
    xref_groups_max=greatest(old.xref_groups_max,excluded.xref_groups_max),
    from_variants=old.from_variants+excluded.from_variants-
        case when old.from_hash=(select f.from_first from folded f where f.file_id=excluded.file_id) then 1 else 0 end,
    from_hash=excluded.from_hash,encoded_subject=greatest(old.encoded_subject,excluded.encoded_subject),
    articles=old.articles+excluded.articles,
    tool=coalesce(old.tool,excluded.tool),clock_first=least(old.clock_first,excluded.clock_first),
    clock_last=greatest(old.clock_last,excluded.clock_last),hw_host=coalesce(old.hw_host,excluded.hw_host),
    hw_key=coalesce(old.hw_key,excluded.hw_key),hw_n_first=least(old.hw_n_first,excluded.hw_n_first),
    hw_n_last=greatest(old.hw_n_last,excluded.hw_n_last)";
