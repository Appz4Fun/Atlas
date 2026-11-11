//! Durable, paged construction of immutable late/fresh publication plans.
//! Intermediate rows are not publishable until the epoch's final emit flip.
use anyhow::{Context, Result, ensure};
use postgres::{Client, Row, Transaction};
use serde::{Deserialize, Serialize};

type PartKey = i64;
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Output {
    stage: u8,
    late: bool,
    chain: i64,
    seed: Option<i64>,
    first: i64,
    last: i64,
    hour: i64,
    last_hour: i64,
    file_count: i64,
    publish: bool,
    file_ordinal: i64,
    file: Option<i64>,
    after: Option<PartKey>,
    release: String,
    group: String,
    release_poster: Vec<u8>,
    date: String,
    before_files: i64,
    generation: i64,
    poster: Vec<u8>,
    plan: i64,
    file_number: i64,
    total_files: i64,
    before_parts: i64,
    articles: i64,
    oldest: i64,
    parts: i64,
    member: Option<i64>,
    leads: i64,
    previous_group: Option<i64>,
}

fn next_part(tx: &mut Transaction<'_>, epoch: i64, file: i64, after: &Option<PartKey>) -> Result<Option<Row>> {
    super::linker_sort::next(tx, epoch, 0, file, *after)
}
fn next_late(tx: &mut Transaction<'_>, epoch: i64, poster: &[u8], after: &Option<PartKey>) -> Result<Option<Row>> {
    let file: i64 = tx
        .query_one(
            "select encounter_rank from linker_late_files where epoch=$1 and poster_key=sha256($2) and poster=$2",
            &[&epoch, &poster],
        )?
        .get(0);
    super::linker_sort::next(tx, epoch, 1, file, *after)
}
fn header(tx: &mut Transaction<'_>, epoch: i64, s: &mut Output, posted: i64) -> Result<()> {
    if let Some(row)=tx.query_opt("select release_poster,release_date from linker_emission_files where epoch=$1 and release_name=$2 and group_name=$3 order by id limit 1", &[&epoch,&s.release,&s.group])? {
        s.release_poster=row.get(0);s.date=row.get(1);
    } else {s.release_poster=s.poster.clone();s.date=crate::xref::iso_pub(posted);}
    Ok(())
}

pub(crate) fn step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "output reducer budget must be positive");
    if !super::linker_sort::step(client, epoch, 0, budget)? {
        return Ok(false);
    }
    if !super::linker_sort::step(client, epoch, 1, budget)? {
        return Ok(false);
    }
    let mut tx = super::linker::durable_transaction(client)?;
    let row = tx.query_one("select phase,logical_now from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = row.get(0);
    let now: i64 = row.get(1);
    if ["emit", "ack", "done"].contains(&phase.as_str()) {
        return Ok(true);
    }
    ensure!(phase == "plan", "output planning requires completed chains");
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='output'", &[&epoch])?;
    let mut s: Output = saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        match s.stage {
            0 => {
                if let Some(row)=tx.query_opt("select f.poster,f.articles,p.release_name,p.group_name,p.file_number,p.file_total,p.parts,p.oldest
                    from linker_late_files f join linker_places p on p.poster_key=f.poster_key and p.poster=f.poster where f.epoch=$1 and not f.planned order by f.encounter_rank limit 1", &[&epoch])? {
                    s=Output {late:true,poster:row.get(0),articles:row.get(1),release:row.get(2),group:row.get(3),
                        file_number:row.get(4),total_files:row.get(5),before_parts:row.get(6),oldest:row.get(7),stage:4,..Default::default()};
                    let first=next_late(&mut tx,epoch,&s.poster,&None)?.context("late file has no parts")?;
                    header(&mut tx,epoch,&mut s,first.get(5))?;
                    continue;
                }
                let next = tx.query_opt(
                    "select id,seed,first_posted,last_posted,articles,new_material,file_count,fresh_first,fresh_last
                    from linker_chains where epoch=$1 and file_count>0 and not planned order by id limit 1",
                    &[&epoch],
                )?;
                let Some(row) = next else {
                    tx.execute("update linker_epochs set phase='emit' where id=$1", &[&epoch])?;
                    done = true;
                    break;
                };
                let chain: i64 = row.get(0);
                let seed: Option<i64> = row.get(1);
                s = Output {
                    stage: 1,
                    chain,
                    seed,
                    publish: row.get::<_, bool>(5)
                        && (seed.is_some() || row.get::<_, i64>(4) >= crate::xref::MIN_UPLOAD_ARTICLES),
                    first: row.get(2),
                    last: row.get(3),
                    file_count: row.get(6),
                    hour: row.get::<_, i64>(7).div_euclid(3600),
                    last_hour: row.get::<_, i64>(8).div_euclid(3600),
                    file_ordinal: 1,
                    ..Default::default()
                };
            }
            1 => {
                let ready: bool = tx
                    .query_one(
                        "select exists(select 1 from linker_linked_hours where hour=$2)
                    or exists(select 1 from linker_epoch_hours where epoch=$1 and hour=$2)",
                        &[&epoch, &s.hour],
                    )?
                    .get(0);
                if !ready {
                    let first: i64 = tx
                        .query_one(
                            "select fresh_first from linker_chains where epoch=$1 and id=$2",
                            &[&epoch, &s.chain],
                        )?
                        .get(0);
                    tx.execute("update linker_epochs set waiting_from=case when waiting_from is null then $2 else least(waiting_from,$2) end
                        where id=$1 and exists(select 1 from linker_epoch_hours where epoch=$1)", &[&epoch,&first])?;
                    tx.execute("update linker_chains set planned=true where epoch=$1 and id=$2", &[&epoch, &s.chain])?;
                    s = Output::default();
                } else if s.hour == s.last_hour {
                    if s.publish {
                        s.stage = if s.seed.is_some() { 3 } else { 2 };
                    } else {
                        tx.execute(
                            "update linker_chains set planned=true where epoch=$1 and id=$2",
                            &[&epoch, &s.chain],
                        )?;
                        s = Output::default();
                    }
                } else {
                    s.hour += 1;
                }
            }
            2 => {
                if s.file.is_none() {
                    let next = tx.query_opt(
                        "select file_id from linker_chain_files where epoch=$1 and chain_id=$2 and ordinal=$3",
                        &[&epoch, &s.chain, &s.file_ordinal],
                    )?;
                    if let Some(row) = next {
                        s.file = Some(row.get(0));
                        s.after = None;
                    } else {
                        s.stage = 3;
                        s.file_ordinal = 1;
                        s.file = None;
                    }
                    continue;
                }
                if let Some(row) = next_part(&mut tx, epoch, s.file.unwrap(), &s.after)? {
                    let group: i64 = row.get(0);
                    let source: i64 = row.get(3);
                    let name: String =
                        tx.query_one("select group_name from linker_source_blocks where id=$1", &[&source])?.get(0);
                    tx.execute("insert into linker_plan_groups values($1,$2,$3,1,$4) on conflict(epoch,chain_id,group_id)
                        do update set articles=linker_plan_groups.articles+1,group_name=greatest(linker_plan_groups.group_name collate \"C\",excluded.group_name collate \"C\")", &[&epoch,&s.chain,&group,&name])?;
                    s.after = Some(row.get(6));
                } else {
                    s.file_ordinal += 1;
                    s.file = None;
                    s.after = None;
                }
            }
            3 => {
                let file: i64 = tx
                    .query_one(
                        "select file_id from linker_chain_files where epoch=$1 and chain_id=$2 and ordinal=1",
                        &[&epoch, &s.chain],
                    )?
                    .get(0);
                let row = tx.query_one(
                    "select poster,first_posted,again from linker_files where epoch=$1 and id=$2",
                    &[&epoch, &file],
                )?;
                s.poster = row.get(0);
                let posted = next_part(&mut tx, epoch, file, &None)?.context("chain first file has no parts")?.get(5);
                if let Some(seed) = s.seed {
                    let row=tx.query_one("select release_name,group_name,files,generation from linker_epoch_seeds where epoch=$1 and id=$2", &[&epoch,&seed])?;
                    s.release = row.get(0);
                    s.group = row.get(1);
                    s.before_files = row.get(2);
                    s.generation = row.get::<_, i64>(3) + 1;
                } else {
                    let poster = std::str::from_utf8(&s.poster)?;
                    let key = if row.get::<_, bool>(2) {
                        format!("{poster}\0{}", row.get::<_, i64>(1).div_euclid(3600))
                    } else {
                        poster.to_owned()
                    };
                    s.release = crate::xref::upload_name(&key);
                    s.group=tx.query_one("select group_name from linker_plan_groups where epoch=$1 and chain_id=$2 order by articles desc,group_id limit 1", &[&epoch,&s.chain])?.get(0);
                }
                s.total_files = s.before_files + s.file_count;
                header(&mut tx, epoch, &mut s, posted)?;
                s.file_ordinal = 1;
                s.file = None;
                s.stage = 4;
            }
            4 => {
                if !s.late {
                    let next = tx.query_opt(
                        "select file_id from linker_chain_files where epoch=$1 and chain_id=$2 and ordinal=$3",
                        &[&epoch, &s.chain, &s.file_ordinal],
                    )?;
                    let Some(row) = next else {
                        s.stage = 10;
                        continue;
                    };
                    let file: i64 = row.get(0);
                    s.file = Some(file);
                    let row = tx.query_one(
                        "select poster,articles from linker_files where epoch=$1 and id=$2",
                        &[&epoch, &file],
                    )?;
                    s.poster = row.get(0);
                    s.articles = row.get(1);
                    s.before_parts = 0;
                    s.oldest = now;
                    s.file_number = s.before_files + s.file_ordinal;
                }
                s.plan = tx.query_one("select next_plan_file from linker_epochs where id=$1", &[&epoch])?.get(0);
                tx.execute(
                    "insert into linker_emission_files values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                    &[
                        &epoch,
                        &s.plan,
                        &s.release,
                        &s.group,
                        &s.release_poster,
                        &s.date,
                        &s.poster,
                        &s.file_number,
                        &s.total_files,
                        &(s.before_parts + s.articles),
                    ],
                )?;
                tx.execute("insert into linker_planned_places values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) on conflict(epoch,poster_key) do update set
                    plan_file=excluded.plan_file,release_name=excluded.release_name,group_name=excluded.group_name,file_number=excluded.file_number,
                    file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
                    &[&epoch,&s.poster,&s.plan,&s.release,&s.group,&s.file_number,&s.total_files,&(s.before_parts+s.articles),&s.oldest,&now])?;
                tx.execute("update linker_epochs set next_plan_file=next_plan_file+1 where id=$1", &[&epoch])?;
                s.stage = 5;
                s.parts = 0;
                s.leads = 0;
                s.member = None;
                s.after = None;
                s.previous_group = None;
            }
            5 | 6 => {
                let (table, key) = if s.stage == 5 {
                    ("linker_planned_firsts", "group_id")
                } else {
                    ("linker_planned_leads", "ordinal")
                };
                let n=tx.execute(&format!("delete from {table} where epoch=$1 and poster_key=sha256($2) and poster=$2 and {key}=(select {key} from {table} where epoch=$1 and poster_key=sha256($2) and poster=$2 order by {key} limit 1)"), &[&epoch,&s.poster])?;
                if n == 0 {
                    s.stage = if s.stage == 5 {
                        6
                    } else if s.late {
                        7
                    } else {
                        9
                    };
                    s.member = None;
                }
            }
            7 | 8 => {
                let (live, planned, key, value) = if s.stage == 7 {
                    ("linker_place_firsts", "linker_planned_firsts", "group_id", "article_number")
                } else {
                    ("linker_place_leads", "linker_planned_leads", "ordinal", "part")
                };
                let base = format!("select {key},{value} from {live} where poster_key=sha256($1) and poster=$1");
                let next = match s.member {
                    Some(member) => {
                        tx.query_opt(&format!("{base} and {key}>$2 order by {key} limit 1"), &[&s.poster, &member])?
                    }
                    None => tx.query_opt(&format!("{base} order by {key} limit 1"), &[&s.poster])?,
                };
                if let Some(row) = next {
                    let member: i64 = row.get(0);
                    s.member = Some(member);
                    tx.execute(
                        &format!("insert into {planned} values($1,$2,$3,$4)"),
                        &[&epoch, &s.poster, &member, &row.get::<_, i64>(1)],
                    )?;
                    if s.stage == 8 {
                        s.leads = member;
                    }
                } else {
                    s.stage += 1;
                    s.member = None;
                }
            }
            9 => {
                let next = if s.late {
                    next_late(&mut tx, epoch, &s.poster, &s.after)?
                } else {
                    next_part(&mut tx, epoch, s.file.unwrap(), &s.after)?
                };
                let Some(row) = next else {
                    ensure!(s.parts == s.articles, "immutable plan lost a part");
                    if s.late {
                        tx.execute(
                            "update linker_late_files set planned=true where epoch=$1 and poster_key=sha256($2) and poster=$2",
                            &[&epoch, &s.poster],
                        )?;
                        s = Output::default();
                    } else {
                        s.file_ordinal += 1;
                        s.stage = 4;
                    }
                    continue;
                };
                s.parts += 1;
                let part = s.before_parts + s.parts;
                let group: i64 = row.get(0);
                let number: i64 = row.get(1);
                let ordinal: i64 = tx
                    .query_one(
                        "update linker_epochs set planned_rows=planned_rows+1 where id=$1 returning planned_rows",
                        &[&epoch],
                    )?
                    .get(0);
                tx.execute(
                    "insert into linker_emission_rows values($1,$2,$3,$4,$5,$6)",
                    &[&epoch, &ordinal, &s.plan, &row.get::<_, i64>(3), &row.get::<_, i64>(4), &part],
                )?;
                if s.previous_group != Some(group) {
                    let old = if s.late {
                        tx.query_opt(
                            "select article_number from linker_place_firsts where poster_key=sha256($1) and poster=$1 and group_id=$2",
                            &[&s.poster, &group],
                        )?
                        .map(|r| r.get::<_, i64>(0))
                    } else {
                        None
                    };
                    tx.execute("insert into linker_planned_firsts values($1,$2,$3,$4) on conflict(epoch,poster_key,group_id) do update set article_number=least(linker_planned_firsts.article_number,excluded.article_number)", &[&epoch,&s.poster,&group,&number])?;
                    if old.is_none_or(|old| number < old) {
                        s.leads += 1;
                        tx.execute(
                            "insert into linker_planned_leads values($1,$2,$3,$4)",
                            &[&epoch, &s.poster, &s.leads, &part],
                        )?;
                    }
                    s.previous_group = Some(group);
                }
                s.after = Some(row.get(6));
            }
            10 => {
                tx.execute("insert into linker_planned_uploads values($1,$2,$3,$4,$5,$6,$7,0,$8) on conflict(epoch,release_name) do update set
                    group_name=excluded.group_name,first_posted=excluded.first_posted,last_posted=excluded.last_posted,
                    files=excluded.files,generation=excluded.generation,probed=excluded.probed,added=excluded.added",
                    &[&epoch,&s.release,&s.group,&s.first,&s.last,&s.total_files,&s.generation,&now])?;
                s.stage = 11;
            }
            11 => {
                let n=tx.execute("delete from linker_planned_upload_groups where epoch=$1 and release_name=$2 and group_id=(select group_id from linker_planned_upload_groups where epoch=$1 and release_name=$2 order by group_id limit 1)", &[&epoch,&s.release])?;
                if n == 0 {
                    s.stage = 12;
                    s.member = None;
                }
            }
            12 => {
                let next=match s.member {
                    Some(group)=>tx.query_opt("select group_id from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=0 and group_id>$3 order by group_id limit 1", &[&epoch,&s.chain,&group])?,
                    None=>tx.query_opt("select group_id from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=0 order by group_id limit 1", &[&epoch,&s.chain])?,
                };
                if let Some(row) = next {
                    let group: i64 = row.get(0);
                    s.member = Some(group);
                    tx.execute(
                        "insert into linker_planned_upload_groups values($1,$2,$3)",
                        &[&epoch, &s.release, &group],
                    )?;
                } else {
                    tx.execute("update linker_chains set planned=true where epoch=$1 and id=$2", &[&epoch, &s.chain])?;
                    s = Output::default();
                }
            }
            _ => unreachable!(),
        }
    }
    tx.execute(
        "insert into linker_work values($1,'output',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&s)?],
    )?;
    tx.commit()?;
    Ok(done)
}
