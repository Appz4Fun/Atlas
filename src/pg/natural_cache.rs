//! Read costly preserved facts on the caller's original PostgreSQL session.
//! A row's presence is not proof of reuse eligibility: content, step stamp and
//! format checks still belong to the typed consumer. Database errors never
//! become cache misses. No connection, SQLite fallback or provider is opened.

use anyhow::{Result, ensure};
use postgres::{GenericClient, Row};

/// Closed registry of the costly natural-key families; no caller SQL/table name.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Family {
    Probe, Cover, Archive, InnerFiles, Par2, Par2Files, RecoveryVolumes,
    NfoPassword, Stills, AudioPrints,
}

impl Family {
    fn shape(self) -> (&'static str, &'static str, &'static [&'static str], &'static str) {
        match self {
            Self::Probe => ("ext_raw_probes", "name", &["crc32","agent","info","moviehash","parts","complete","config_stamp","probed_at"], "probed_at"),
            Self::Cover => ("ext_raw_covers", "name", &["mime","sha256"], "sha256"),
            Self::Archive => ("ext_raw_archives", "name", &["password","encryption","names_hidden","password_check","parts","complete","listing_complete","listing_damaged","content_fp","packer","twin_key","owner_key","zone_key","key_salt","facts"], "password"),
            Self::InnerFiles => ("ext_raw_archive_inner_files", "release_name", &["seq","name","size","source","crc32","mtime","host_os"], "seq"),
            Self::Par2 => ("ext_raw_par2", "name", &["covers_all","posted","par2_block","par2_blocks","parts","complete","content_fp"], "covers_all"),
            Self::Par2Files => ("ext_raw_par2_files", "name", &["file_id","set_id","md5","md5_16k","size","file_name","crc32","bound","sealed","complete","creator"], "file_name collate \"C\", file_id"),
            Self::RecoveryVolumes => ("ext_raw_rev_volumes", "name", &["volume","size","crc32"], "volume"),
            Self::NfoPassword => ("ext_raw_nfo_passwords", "name", &["password"], "password"),
            Self::Stills => ("ext_raw_stills", "name", &["position","at_ms","sha256","width","height","phash"], "position"),
            Self::AudioPrints => ("ext_raw_audio_prints", "name", &["position","at_ms","items"], "position"),
        }
    }

    fn variable(self) -> &'static [&'static str] {
        match self {
            Self::Probe => &["agent","info","moviehash"],
            Self::Cover => &["mime","sha256"],
            Self::Archive => &["password_check","content_fp","packer"],
            Self::InnerFiles => &["name"],
            Self::Par2 => &["posted","content_fp"],
            Self::Par2Files => &["file_id","set_id","md5","md5_16k","file_name","creator"],
            Self::RecoveryVolumes => &[],
            Self::NfoPassword => &["password"],
            Self::Stills => &["sha256"],
            Self::AudioPrints => &["items"],
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Key<'a> {
    pub name: &'a str,
    pub group: &'a str,
    pub content_id: &'a [u8],
}

/// Includes all retained PostgreSQL row/field envelopes as well as payloads.
/// Excess is an explicit error, never a truncated result or a miss.
#[derive(Clone, Copy)]
pub(crate) struct Budget { pub bytes: usize, pub rows: usize }

impl Budget {
    fn checked(self) -> Result<(i64,i64,i64)> {
        ensure!(self.bytes>0 && self.bytes<=64*1024*1024,"natural cache byte budget outside 1..64MiB");
        ensure!(self.rows>0 && self.rows<=10_000,"natural cache row budget outside 1..10000");
        Ok((self.bytes as i64,self.rows as i64,self.rows as i64+1))
    }
}

#[derive(Debug)]
pub(crate) struct Facts { pub group: String, pub rows: Vec<Row> }

/// Preserved originals, including unsupported/old fingerprints. Callers must
/// validate their format before lending or reusing them; this does not certify
/// that a probe/PAR2/archive result is current or trusted.
pub(crate) fn read(
    client:&mut impl GenericClient,family:Family,key:Key<'_>,budget:Budget,
) -> Result<Option<Facts>> {
    read_in_group(client,family,key,budget,false)
}

fn read_in_group(
    client:&mut impl GenericClient,family:Family,key:Key<'_>,budget:Budget,exact:bool,
) -> Result<Option<Facts>> {
    read_scoped(client,family,key,budget,exact,"")
}

fn read_scoped(
    client:&mut impl GenericClient,family:Family,key:Key<'_>,budget:Budget,exact:bool,scope:&str,
) -> Result<Option<Facts>> {
    let (byte_limit,row_limit,limit)=budget.checked()?;
    let encoded_name;
    let key = if matches!(family,Family::NfoPassword) {
        ensure!(key.name.len()<=budget.bytes.saturating_sub(crate::raw_store::nfo_key::PREFIX.len())/2,"NFO natural name exceeds byte budget");
        encoded_name=crate::raw_store::nfo_key::encode(key.name);
        Key {name:&encoded_name,..key}
    } else {key};
    if key.content_id.is_empty() {return Ok(None)}
    ensure!(key.name.len().checked_add(key.group.len()).and_then(|n|n.checked_add(key.content_id.len())).is_some_and(|n|n<=budget.bytes),"natural cache key exceeds byte budget");
    let (table,name,columns,order)=family.shape();
    // octet_length detoasts only on the server. It does not ship a giant value
    // before admission, unlike checking Row's allocation after query(). 128B
    // per field plus 256B per row covers scalar/wire/Row envelopes; text/blob
    // bytes and the returned group String are charged separately.
    let mut size=format!("{}::bigint+octet_length(group_name)::bigint",256+128*(columns.len()+4));
    for field in family.variable() {size.push_str(&format!("+coalesce(octet_length({field})::bigint,0)"));}
    let guarded=columns.iter().map(|field|format!("case when admitted then {field} end as {field}")).collect::<Vec<_>>().join(",");
    let group_filter=if exact {"and group_name=$2"} else {""};
    let sql=format!("with chosen as (
        select natural_group_key,group_name from {table}
        where natural_name_key=sha256(convert_to($1,'UTF8')) and {name}=$1 and content_id=$3 {group_filter} {scope}
        order by (group_name<>$2),group_name collate \"C\" limit 1
      ), matching as (
        select t.*,(sum({size}) over())::bigint as bytes,count(*) over() as row_count
        from {table} t join chosen c using(natural_group_key,group_name)
        where t.natural_name_key=sha256(convert_to($1,'UTF8')) and t.{name}=$1 and t.content_id=$3 {scope}
      ), checked as (select *,bytes<=$4 and row_count<=$5 as admitted from matching)
      select admitted,case when admitted then group_name end as chosen_group,{guarded}
      from checked order by {order} limit $6");
    let rows=client.query(&sql,&[&key.name,&key.group,&key.content_id,&byte_limit,&row_limit,&limit])?;
    let Some(first)=rows.first() else {return Ok(None)};
    ensure!(first.try_get::<_,bool>("admitted")?,"natural cache result exceeds byte/row budget");
    let group=first.try_get("chosen_group")?;
    Ok(Some(Facts {group,rows}))
}

/// Match legacy probe reuse exactly: choose the same-content own group first,
/// check stamp/parts/completeness, and read companions only from that group.
pub(crate) fn probe(
    client:&mut impl GenericClient,key:Key<'_>,parts:i64,complete:bool,required_stamp:u64,budget:Budget,
) -> Result<Option<crate::raw_store::SavedProbe>> {
    budget.checked()?;
    // Three Row collections and the copied SavedProbe strings can coexist.
    // Reserve twice each collection's admitted allocation for that overlap.
    let part=Budget {bytes:budget.bytes/6,rows:1};
    let Some(facts)=read(client,Family::Probe,key,part)? else {return Ok(None)};
    let row=&facts.rows[0];
    let stamp:Option<i64>=row.try_get("config_stamp")?;
    let saved_parts:Option<i64>=row.try_get("parts")?;
    let saved_complete:Option<i64>=row.try_get("complete")?;
    if stamp.is_none_or(|stamp|(required_stamp as i64)&!stamp!=0)
        || parts>saved_parts.unwrap_or(0) || (complete && saved_complete.unwrap_or(0)==0) {return Ok(None)}
    let selected=Key {group:&facts.group,..key};
    let nfo=read_in_group(client,Family::NfoPassword,selected,part,true)?;
    let cover=read_in_group(client,Family::Cover,selected,part,true)?;
    Ok(Some(crate::raw_store::SavedProbe {
        crc32:row.try_get("crc32")?,agent:row.try_get("agent")?,info:row.try_get("info")?,moviehash:row.try_get("moviehash")?,
        nfo_password:nfo.as_ref().map(|facts|facts.rows[0].try_get("password")).transpose()?,
        cover:cover.as_ref().map(|facts|Ok::<_,postgres::Error>((facts.rows[0].try_get("mime")?,facts.rows[0].try_get("sha256")?))).transpose()?,
    }))
}

pub(crate) fn nfo_password(client:&mut impl GenericClient,key:Key<'_>,budget:Budget)->Result<Option<String>> {
    budget.checked()?;
    let half=Budget {bytes:budget.bytes/2,rows:1};
    read(client,Family::NfoPassword,key,half)?.map(|facts|facts.rows[0].try_get("password").map_err(Into::into)).transpose()
}

pub(crate) fn stills(client:&mut impl GenericClient,key:Key<'_>,budget:Budget)->Result<Vec<crate::raw_store::SavedStill>> {
    budget.checked()?;
    let half=Budget {bytes:budget.bytes/2,rows:budget.rows};
    let Some(facts)=read(client,Family::Stills,key,half)? else {return Ok(Vec::new())};
    facts.rows.iter().map(|row|Ok(crate::raw_store::SavedStill {
        position:u8::try_from(row.try_get::<_,i64>("position")?)?,at_ms:row.try_get("at_ms")?,
        sha256:row.try_get("sha256")?,
        width:u32::try_from(row.try_get::<_,Option<i64>>("width")?.unwrap_or(0).max(0))?,
        height:u32::try_from(row.try_get::<_,Option<i64>>("height")?.unwrap_or(0).max(0))?,phash:row.try_get("phash")?,
    })).collect()
}

pub(crate) fn audio_prints(client:&mut impl GenericClient,key:Key<'_>,budget:Budget)->Result<Vec<crate::raw_store::SavedPrint>> {
    budget.checked()?;
    let half=Budget {bytes:budget.bytes/2,rows:budget.rows};
    let Some(facts)=read(client,Family::AudioPrints,key,half)? else {return Ok(Vec::new())};
    facts.rows.iter().map(|row|Ok(crate::raw_store::SavedPrint {
        position:u8::try_from(row.try_get::<_,i64>("position")?)?,at_ms:row.try_get("at_ms")?,items:row.try_get("items")?,
    })).collect()
}

fn inventory(facts:Facts)->Result<Option<crate::par2::Inventory>> {
    let mut inventory=crate::par2::Inventory::default();
    let mut creators=std::collections::BTreeSet::new();
    let mut any=false;
    for row in facts.rows {
        let Ok(file_id)=<[u8;16]>::try_from(row.try_get::<_,Vec<u8>>("file_id")?) else {return Ok(None)};
        let Ok(set_id)=<[u8;16]>::try_from(row.try_get::<_,Vec<u8>>("set_id")?) else {return Ok(None)};
        let Ok(md5)=<[u8;16]>::try_from(row.try_get::<_,Vec<u8>>("md5")?) else {return Ok(None)};
        let Ok(md5_16k)=<[u8;16]>::try_from(row.try_get::<_,Vec<u8>>("md5_16k")?) else {return Ok(None)};
        let Ok(size)=u64::try_from(row.try_get::<_,i64>("size")?) else {return Ok(None)};
        let bound=row.try_get::<_,i64>("bound")?!=0;
        let complete=row.try_get::<_,i64>("complete")?!=0;
        if !any {inventory.complete=complete;any=true} else {inventory.complete&=complete}
        if bound && row.try_get::<_,i64>("sealed")?!=0 {inventory.sealed.push((set_id,file_id));}
        if let Some(creator)=row.try_get::<_,Option<String>>("creator")? {
            if creators.insert(set_id) {inventory.creators.push((set_id,creator));}
        }
        inventory.files.push(crate::par2::Par2File {file_id,set_id,md5,md5_16k,size,name:row.try_get("file_name")?,bound,
          crc32:row.try_get::<_,Option<i64>>("crc32")?.and_then(|crc|u32::try_from(crc).ok())});
    }
    Ok(any.then_some(inventory))
}

pub(crate) fn par2_files(client:&mut impl GenericClient,key:Key<'_>,budget:Budget)->Result<Option<crate::par2::Inventory>> {
    budget.checked()?;
    let half=Budget {bytes:budget.bytes/2,rows:budget.rows};
    read(client,Family::Par2Files,key,half)?.map(inventory).transpose().map(Option::flatten)
}

/// Only the current versioned posted fingerprint is reusable. Preserve legacy
/// 20-byte and unknown-version originals in storage, but refuse them here;
/// indexer-startup cleanup is not part of this eligibility boundary.
pub(crate) fn par2(
    client:&mut impl GenericClient,key:Key<'_>,parts:i64,complete:bool,budget:Budget,
) -> Result<Option<crate::raw_store::SavedPar2>> {
    budget.checked()?;
    let quarter=Budget {bytes:budget.bytes/4,rows:budget.rows};
    let Some(facts)=read(client,Family::Par2,key,quarter)? else {return Ok(None)};
    let row=&facts.rows[0];
    if row.try_get::<_,Option<i64>>("parts")?.unwrap_or(0)!=parts
      || (row.try_get::<_,Option<i64>>("complete")?.unwrap_or(0)!=0)!=complete {return Ok(None)}
    let posted:Option<Vec<u8>>=row.try_get("posted")?;
    let block:Option<i64>=row.try_get("par2_block")?;
    let blocks:Option<i64>=row.try_get("par2_blocks")?;
    let covers=row.try_get::<_,i64>("covers_all")?!=0;
    if posted.as_ref().is_some_and(|posted|posted.len()!=21 || posted.first()!=Some(&2))
      || (posted.is_none() && (block.is_some() || blocks.is_some()))
      || (covers && (posted.is_none() || block.is_none() || blocks.is_none())) {return Ok(None)}
    let selected=Key {group:&facts.group,..key};
    let inventory=read_in_group(client,Family::Par2Files,selected,quarter,true)?.map(inventory).transpose()?.flatten();
    Ok(Some(crate::raw_store::SavedPar2 {covers_all:covers,posted,sizes:block.zip(blocks),content_fp:row.try_get("content_fp")?,inventory}))
}

pub(crate) fn recovery_volumes(client:&mut impl GenericClient,key:Key<'_>,budget:Budget)->Result<Option<Vec<crate::archive::rev::Volume>>> {
    budget.checked()?;
    let half=Budget {bytes:budget.bytes/2,rows:budget.rows};
    let Some(facts)=read(client,Family::RecoveryVolumes,key,half)? else {return Ok(None)};
    let mut volumes=Vec::with_capacity(facts.rows.len());
    for row in facts.rows {
        let Ok(index)=u16::try_from(row.try_get::<_,i64>("volume")?) else {return Ok(None)};
        let Ok(size)=u64::try_from(row.try_get::<_,i64>("size")?) else {return Ok(None)};
        let Ok(crc32)=u32::try_from(row.try_get::<_,i64>("crc32")?) else {return Ok(None)};
        volumes.push(crate::archive::rev::Volume {index,size,crc32});
    }
    Ok((!volumes.is_empty()).then_some(volumes))
}

fn inner_files(facts:Facts)->Result<Vec<crate::raw_store::SavedArchiveFile>> {
    facts.rows.iter().map(|row|{
      let source=row.try_get::<_,Option<i64>>("source")?.unwrap_or(2);
      ensure!((1..=5).contains(&source),"unknown preserved archive source");
      Ok(crate::raw_store::SavedArchiveFile {
        seq:row.try_get("seq")?,name:row.try_get("name")?,
        size:row.try_get::<_,Option<i64>>("size")?.and_then(|size|u64::try_from(size).ok()),
        source,
        crc32:row.try_get::<_,Option<i64>>("crc32")?.and_then(|crc|u32::try_from(crc).ok()),
        mtime:row.try_get("mtime")?,host_os:row.try_get::<_,Option<i64>>("host_os")?.and_then(|host|u8::try_from(host).ok()),
    })}).collect()
}

pub(crate) fn archive_files(client:&mut impl GenericClient,key:Key<'_>,source:i64,budget:Budget)->Result<Vec<crate::raw_store::SavedArchiveFile>> {
    budget.checked()?;ensure!((1..=5).contains(&source),"unknown archive source");
    let half=Budget {bytes:budget.bytes/2,rows:budget.rows};
    let scope=format!("and coalesce(source,2)={source}");
    read_scoped(client,Family::InnerFiles,key,half,false,&scope)?.map(inner_files).transpose().map(Option::unwrap_or_default)
}

pub(crate) fn archive(client:&mut impl GenericClient,key:Key<'_>,parts:i64,complete:bool,budget:Budget)->Result<Option<crate::raw_store::SavedArchive>> {
    budget.checked()?;
    let quarter=Budget {bytes:budget.bytes/4,rows:budget.rows};
    let Some(facts)=read(client,Family::Archive,key,quarter)? else {return Ok(None)};
    let row=&facts.rows[0];
    let selected=Key {group:&facts.group,..key};
    let files=read_scoped(client,Family::InnerFiles,selected,quarter,true,"and coalesce(source,2) not in (1,5)")?.map(inner_files).transpose()?.unwrap_or_default();
    let hidden=row.try_get::<_,Option<i64>>("names_hidden")?.unwrap_or(0)!=0;
    let listing_complete=row.try_get::<_,Option<i64>>("listing_complete")?.unwrap_or(0)!=0;
    let listing_damaged=row.try_get::<_,Option<i64>>("listing_damaged")?.unwrap_or(0)!=0;
    let password=row.try_get::<_,i64>("password")?;
    if files.is_empty() && !hidden {return Ok(None)}
    let inconclusive=!listing_complete || listing_damaged || (files.is_empty() && password==crate::archive::Password::Maybe.code());
    if inconclusive && let Some(saved_parts)=row.try_get::<_,Option<i64>>("parts")? {
        if parts>saved_parts || (complete && row.try_get::<_,Option<i64>>("complete")?.unwrap_or(0)==0) {return Ok(None)}
    }
    Ok(Some(crate::raw_store::SavedArchive {password:Some(password),encryption:row.try_get("encryption")?,names_hidden:hidden,
      password_check:row.try_get("password_check")?,listing_complete,listing_damaged,content_fp:row.try_get("content_fp")?,files,
      keys:crate::archive::store::ArchiveKeys {packer:row.try_get("packer")?,twin_key:row.try_get("twin_key")?,owner_key:row.try_get("owner_key")?,zone_key:row.try_get("zone_key")?},
      key_salt:row.try_get("key_salt")?,facts:row.try_get::<_,Option<i64>>("facts")?.unwrap_or(0)}))
}

// Mutation helpers deliberately require the caller's existing transaction.
// They neither acquire a connection nor commit/retry a partial result. The
// canonical native owner/operation actor wraps this transaction and settles it.
fn archive_current(tx:&mut postgres::Transaction<'_>,family:Family,key:Key<'_>)->Result<()> {
    let encoded_name;
    let key = if matches!(family,Family::NfoPassword) {
        encoded_name=crate::raw_store::nfo_key::encode(key.name);
        Key {name:&encoded_name,..key}
    } else {key};
    let (table,name,_,_)=family.shape();
    let statement=tx.prepare(&format!("select * from {table} limit 0"))?;
    let descriptor=serde_json::to_string(&statement.columns().iter().map(|column|(column.name(),column.type_().name())).collect::<Vec<_>>())?;
    tx.execute(&format!("insert into ext_preserved_cache_rows
      (family,source_digest,row_digest,name,group_name,content_id,schema_descriptor,row_format,row_bytes)
      select $4,sha256(convert_to('atlas.native-cache-replacement.v1:'||$4,'UTF8')),
             sha256(convert_to('atlas.cache-row-json.v1:'||to_jsonb(prior)::text,'UTF8')),
             prior.{name},prior.group_name,prior.content_id,$5,1,convert_to(to_jsonb(prior)::text,'UTF8')
      from (select * from {table} where natural_name_key=sha256(convert_to($1,'UTF8'))
        and natural_group_key=sha256(convert_to($2,'UTF8')) and {name}=$1 and group_name=$2 for update) prior
      where $3::bytea is not null on conflict do nothing"),&[&key.name,&key.group,&key.content_id,&table,&descriptor])?;
    Ok(())
}

fn write_budget(key:Key<'_>,bytes:usize,rows:usize,budget:Budget)->Result<()> {
    budget.checked()?;
    ensure!(!key.content_id.is_empty(),"natural cache write requires a content identity");
    let total=key.name.len().checked_add(key.group.len()).and_then(|n|n.checked_add(key.content_id.len()))
        .and_then(|n|n.checked_add(bytes)).and_then(|n|n.checked_add(rows.checked_mul(2048)?));
    ensure!(rows<=budget.rows && total.is_some_and(|n|n<=budget.bytes),"natural cache write exceeds byte/row budget");
    Ok(())
}

pub(crate) fn put_probe(tx:&mut postgres::Transaction<'_>,record:&crate::raw_store::ProbeRecord<'_>,budget:Budget)->Result<()> {
    let key=Key {name:record.name,group:record.group_name,content_id:record.content_id};
    let mut bytes=record.info.len();
    for field in [record.agent,record.moviehash,record.nfo_password,record.cover.map(|cover|cover.0),record.cover.map(|cover|cover.1)].into_iter().flatten() {
        bytes=bytes.checked_add(field.len()).ok_or_else(||anyhow::anyhow!("natural cache write size overflow"))?;
    }
    write_budget(key,bytes,3,budget)?;
    archive_current(tx,Family::Probe,key)?;
    let n=tx.execute("insert into ext_raw_probes(name,group_name,content_id,crc32,agent,info,moviehash,parts,complete,config_stamp,probed_at)
      values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
      on conflict(natural_name_key,natural_group_key) do update set
       content_id=excluded.content_id,crc32=excluded.crc32,agent=excluded.agent,info=excluded.info,moviehash=excluded.moviehash,
       parts=excluded.parts,complete=excluded.complete,config_stamp=excluded.config_stamp,probed_at=excluded.probed_at
      where ext_raw_probes.name=excluded.name and ext_raw_probes.group_name=excluded.group_name",
      &[&record.name,&record.group_name,&record.content_id,&record.crc32,&record.agent,&record.info,&record.moviehash,
        &record.parts,&i64::from(record.complete),&(record.config_stamp as i64),&chrono::Utc::now().timestamp()])?;
    ensure!(n==1,"natural cache key collision");
    if let Some(password)=record.nfo_password {
        put_nfo_password(tx,key,password,budget)?;
    }
    archive_current(tx,Family::Cover,key)?;
    if let Some((mime,digest))=record.cover {
        let n=tx.execute("insert into ext_raw_covers(name,group_name,content_id,mime,sha256) values($1,$2,$3,$4,$5)
          on conflict(natural_name_key,natural_group_key) do update set content_id=excluded.content_id,mime=excluded.mime,sha256=excluded.sha256
          where ext_raw_covers.name=excluded.name and ext_raw_covers.group_name=excluded.group_name",
          &[&key.name,&key.group,&key.content_id,&mime,&digest])?;
        ensure!(n==1,"natural cache key collision");
    } else {
        tx.execute("delete from ext_raw_covers where natural_name_key=sha256(convert_to($1,'UTF8'))
          and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
    }
    Ok(())
}

pub(crate) fn put_nfo_password(tx:&mut postgres::Transaction<'_>,key:Key<'_>,password:&str,budget:Budget)->Result<()> {
    ensure!(key.name.len()<=budget.bytes.saturating_sub(crate::raw_store::nfo_key::PREFIX.len())/2,"NFO natural name exceeds byte budget");
    let encoded_name=crate::raw_store::nfo_key::encode(key.name);
    write_budget(Key {name:&encoded_name,..key},password.len(),1,budget)?;
    archive_current(tx,Family::NfoPassword,key)?;
    let key=Key {name:&encoded_name,..key};
    let n=tx.execute("insert into ext_raw_nfo_passwords(name,group_name,content_id,password) values($1,$2,$3,$4)
      on conflict(natural_name_key,natural_group_key) do update set content_id=excluded.content_id,password=excluded.password
      where ext_raw_nfo_passwords.name=excluded.name and ext_raw_nfo_passwords.group_name=excluded.group_name",
      &[&key.name,&key.group,&key.content_id,&password])?;
    ensure!(n==1,"natural cache key collision");Ok(())
}

pub(crate) fn put_stills(tx:&mut postgres::Transaction<'_>,key:Key<'_>,stills:&[crate::raw_store::SavedStill],budget:Budget)->Result<()> {
    let bytes=stills.iter().try_fold(0usize,|n,still|n.checked_add(still.sha256.len())).ok_or_else(||anyhow::anyhow!("natural cache write size overflow"))?;
    write_budget(key,bytes,stills.len(),budget)?;
    archive_current(tx,Family::Stills,key)?;
    tx.execute("delete from ext_raw_stills where natural_name_key=sha256(convert_to($1,'UTF8'))
      and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
    for still in stills {
        tx.execute("insert into ext_raw_stills(name,group_name,content_id,position,at_ms,sha256,width,height,phash)
          values($1,$2,$3,$4,$5,$6,$7,$8,$9)",&[&key.name,&key.group,&key.content_id,&i64::from(still.position),&still.at_ms,
          &still.sha256,&i64::from(still.width),&i64::from(still.height),&still.phash])?;
    }
    Ok(())
}

pub(crate) fn put_audio_prints(tx:&mut postgres::Transaction<'_>,key:Key<'_>,prints:&[crate::raw_store::SavedPrint],budget:Budget)->Result<()> {
    let bytes=prints.iter().try_fold(0usize,|n,print|n.checked_add(print.items.len())).ok_or_else(||anyhow::anyhow!("natural cache write size overflow"))?;
    write_budget(key,bytes,prints.len(),budget)?;
    archive_current(tx,Family::AudioPrints,key)?;
    tx.execute("delete from ext_raw_audio_prints where natural_name_key=sha256(convert_to($1,'UTF8'))
      and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
    for print in prints {
        tx.execute("insert into ext_raw_audio_prints(name,group_name,content_id,position,at_ms,items) values($1,$2,$3,$4,$5,$6)",
          &[&key.name,&key.group,&key.content_id,&i64::from(print.position),&print.at_ms,&print.items])?;
    }
    Ok(())
}

fn scalar_upsert(tx:&mut postgres::Transaction<'_>,family:Family,key:Key<'_>,fields:&[&(dyn postgres::types::ToSql+Sync)])->Result<()> {
    ensure!(matches!(family,Family::Archive|Family::Par2),"unknown scalar cache writer");
    let (table,name,columns,_)=family.shape();ensure!(columns.len()==fields.len(),"cache writer column mismatch");
    archive_current(tx,family,key)?;
    let names=columns.join(",");let params=(4..4+columns.len()).map(|i|format!("${i}")).collect::<Vec<_>>().join(",");
    let setters=columns.iter().map(|name|format!("{name}=excluded.{name}")).collect::<Vec<_>>().join(",");
    let mut values:Vec<&(dyn postgres::types::ToSql+Sync)>=vec![&key.name,&key.group,&key.content_id];values.extend_from_slice(fields);
    let n=tx.execute(&format!("insert into {table}({name},group_name,content_id,{names}) values($1,$2,$3,{params})
      on conflict(natural_name_key,natural_group_key) do update set content_id=excluded.content_id,{setters}
      where {table}.{name}=excluded.{name} and {table}.group_name=excluded.group_name"),&values)?;
    ensure!(n==1,"natural cache key collision");Ok(())
}

pub(crate) fn put_archive(tx:&mut postgres::Transaction<'_>,record:&crate::raw_store::ArchiveRecord<'_>,budget:Budget)->Result<()> {
    ensure!((1..=5).contains(&record.source),"unknown archive source");
    let key=Key {name:record.name,group:record.group_name,content_id:record.content_id};
    let names=record.files.iter().try_fold(0usize,|size,file|size.checked_add(file.name.len())).ok_or_else(||anyhow::anyhow!("cache size overflow"))?;
    ensure!((names as u128)<=(record.max_names_bytes.max(0) as u128),"archive cache exceeds admitted names budget");
    let bytes=names.checked_add(record.password_check.map_or(0,<[u8]>::len)).and_then(|size|size.checked_add(record.content_fp.map_or(0,<[u8]>::len)))
      .and_then(|size|size.checked_add(record.keys.packer.as_ref().map_or(0,String::len))).ok_or_else(||anyhow::anyhow!("cache size overflow"))?;
    write_budget(key,bytes,record.files.len()+1,budget)?;
    archive_current(tx,Family::InnerFiles,key)?;
    tx.execute("delete from ext_raw_archive_inner_files where natural_name_key=sha256(convert_to($1,'UTF8'))
      and natural_group_key=sha256(convert_to($2,'UTF8')) and release_name=$1 and group_name=$2 and coalesce(source,2)=$3",&[&key.name,&key.group,&record.source])?;
    for (ordinal,file) in record.files.iter().enumerate() {
        let size=file.size.map(i64::try_from).transpose()?;
        let seq=record.source*65536+i64::try_from(ordinal)?;
        tx.execute("insert into ext_raw_archive_inner_files(release_name,group_name,content_id,seq,name,size,source,crc32,mtime,host_os)
          values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",&[&key.name,&key.group,&key.content_id,&seq,&file.name,&size,&record.source,
          &file.crc32.map(i64::from),&file.mtime,&file.host_os.map(i64::from)])?;
    }
    if let Some(password)=record.password {
        if record.files.is_empty() && !record.names_hidden {
            archive_current(tx,Family::Archive,key)?;
            tx.execute("delete from ext_raw_archives where natural_name_key=sha256(convert_to($1,'UTF8'))
              and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
        } else {
            scalar_upsert(tx,Family::Archive,key,&[&password,&record.encryption,&i64::from(record.names_hidden),&record.password_check,&record.parts,
              &i64::from(record.complete),&i64::from(record.listing_complete),&i64::from(record.listing_damaged),&record.content_fp,
              &record.keys.packer,&record.keys.twin_key,&record.keys.owner_key,&record.keys.zone_key,&record.key_salt,&record.facts])?;
        }
    }
    Ok(())
}

pub(crate) fn put_par2_files(tx:&mut postgres::Transaction<'_>,key:Key<'_>,inventory:&crate::par2::Inventory,budget:Budget)->Result<()> {
    ensure!(inventory.creators.len()<=budget.rows && inventory.sealed.len()<=budget.rows,"PAR2 metadata exceeds row budget");
    let mut bytes=0usize;
    for file in &inventory.files {bytes=bytes.checked_add(file.name.len()).and_then(|size|size.checked_add(64)).ok_or_else(||anyhow::anyhow!("cache size overflow"))?;}
    for (_,creator) in &inventory.creators {bytes=bytes.checked_add(creator.len()).ok_or_else(||anyhow::anyhow!("cache size overflow"))?;}
    write_budget(key,bytes,inventory.files.len(),budget)?;
    archive_current(tx,Family::Par2Files,key)?;
    tx.execute("delete from ext_raw_par2_files where natural_name_key=sha256(convert_to($1,'UTF8'))
      and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
    for file in &inventory.files {
        let size=i64::try_from(file.size)?;
        let sealed=file.bound && inventory.sealed.contains(&(file.set_id,file.file_id));
        tx.execute("insert into ext_raw_par2_files(name,group_name,content_id,file_id,set_id,md5,md5_16k,size,file_name,crc32,bound,sealed,complete,creator)
          values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",&[&key.name,&key.group,&key.content_id,&&file.file_id[..],&&file.set_id[..],&&file.md5[..],&&file.md5_16k[..],
          &size,&file.name,&file.crc32.map(i64::from),&i64::from(file.bound),&i64::from(sealed),&i64::from(inventory.complete),&inventory.creator_of(&file.set_id)])?;
    }
    Ok(())
}

pub(crate) fn put_par2(tx:&mut postgres::Transaction<'_>,record:&crate::raw_store::Par2Record<'_>,budget:Budget)->Result<()> {
    ensure!(record.posted.is_none_or(|posted|posted.len()==21 && posted.first()==Some(&2)),"unsupported posted fingerprint");
    ensure!(!(record.posted.is_none() && record.sizes.is_some()) && (!record.covers_all || (record.posted.is_some() && record.sizes.is_some())),"incomplete PAR2 reuse proof");
    let key=Key {name:record.name,group:record.group_name,content_id:record.content_id};
    let bytes=record.posted.map_or(0,<[u8]>::len).checked_add(record.content_fp.map_or(0,<[u8]>::len)).ok_or_else(||anyhow::anyhow!("cache size overflow"))?;
    write_budget(key,bytes,1,budget)?;
    if let Some(inventory)=record.inventory {ensure!(inventory.files.len()<budget.rows,"PAR2 header and inventory exceed row budget");}
    scalar_upsert(tx,Family::Par2,key,&[&i64::from(record.covers_all),&record.posted,&record.sizes.map(|size|size.0),&record.sizes.map(|size|size.1),
      &record.parts,&i64::from(record.complete),&record.content_fp])?;
    if let Some(inventory)=record.inventory {put_par2_files(tx,key,inventory,budget)?;}
    Ok(())
}

pub(crate) fn put_recovery_volumes(tx:&mut postgres::Transaction<'_>,key:Key<'_>,volumes:&[crate::archive::rev::Volume],budget:Budget)->Result<()> {
    write_budget(key,0,volumes.len(),budget)?;
    archive_current(tx,Family::RecoveryVolumes,key)?;
    tx.execute("delete from ext_raw_rev_volumes where natural_name_key=sha256(convert_to($1,'UTF8'))
      and natural_group_key=sha256(convert_to($2,'UTF8')) and name=$1 and group_name=$2",&[&key.name,&key.group])?;
    for volume in volumes {
        tx.execute("insert into ext_raw_rev_volumes(name,group_name,content_id,volume,size,crc32) values($1,$2,$3,$4,$5,$6)",
          &[&key.name,&key.group,&key.content_id,&i64::from(volume.index),&i64::try_from(volume.size)?,&i64::from(volume.crc32)])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET:Budget=Budget {bytes:1024*1024,rows:32};

    #[test]
    fn natural_probe_preserves_crosspost_precedence_stamp_and_same_group_companions() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.batch_execute("insert into ext_raw_probes(name,group_name,content_id,info,parts,complete,config_stamp,probed_at) values
          ('Kept','a.cross',decode('0102','hex'),'cross',5,1,7,11),
          ('Kept','z.own',decode('0102','hex'),'own',4,0,1,13);
          insert into ext_raw_nfo_passwords(name,group_name,content_id,password) values('atlas.nfo-name.v1:4b657074','a.cross',decode('0102','hex'),'');
          insert into ext_raw_covers(name,group_name,content_id,mime,sha256) values('Kept','a.cross',decode('0102','hex'),'image/jpeg','cover')").unwrap();
        let key=Key {name:"Kept",group:"z.own",content_id:&[1,2]};
        assert!(probe(&mut *client,key,5,true,7,BUDGET).unwrap().is_none(),"stale own group does not borrow a different group's fresher result");
        let own=probe(&mut *client,key,4,false,1,BUDGET).unwrap().unwrap();
        assert_eq!(own.info,"own");assert!(own.nfo_password.is_none());assert!(own.cover.is_none());
        let cross=probe(&mut *client,Key {group:"missing",..key},5,true,7,BUDGET).unwrap().unwrap();
        assert_eq!(cross.info,"cross");assert_eq!(cross.nfo_password.as_deref(),Some(""));
        assert_eq!(cross.cover,Some(("image/jpeg".into(),"cover".into())));
        assert!(probe(&mut *client,Key {content_id:&[9],..key},1,false,0,BUDGET).unwrap().is_none());
    }

    #[test]
    fn natural_nfo_codec_roundtrips_all_names_and_preserves_replaced_originals() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        let names=["Kept","kept","Kept\0nzb","atlas.nfo-name.v1:4b657074","é\0nzb",""];
        for name in names {
            let key=Key {name,group:"g",content_id:&[1]};
            let mut tx=client.transaction().unwrap();
            put_nfo_password(&mut tx,key,"original",BUDGET).unwrap();tx.commit().unwrap();
            assert_eq!(nfo_password(&mut *client,key,BUDGET).unwrap().as_deref(),Some("original"));
            let encoded=crate::raw_store::nfo_key::encode(name);
            let row=client.query_one("select name,password,content_id from ext_raw_nfo_passwords where name=$1",&[&encoded]).unwrap();
            assert_eq!(crate::raw_store::nfo_key::decode(&row.get::<_,String>(0)).unwrap(),name);
            assert_eq!(row.get::<_,String>(1),"original");assert_eq!(row.get::<_,Vec<u8>>(2),[1]);
            let mut tx=client.transaction().unwrap();
            put_nfo_password(&mut tx,Key {content_id:&[2],..key},"",BUDGET).unwrap();tx.commit().unwrap();
            assert!(nfo_password(&mut *client,key,BUDGET).unwrap().is_none());
            assert_eq!(nfo_password(&mut *client,Key {content_id:&[2],..key},BUDGET).unwrap().as_deref(),Some(""));
            let row=client.query_one("select name,content_id,row_bytes from ext_preserved_cache_rows where family='ext_raw_nfo_passwords' and name=$1",&[&encoded]).unwrap();
            assert_eq!(crate::raw_store::nfo_key::decode(&row.get::<_,String>(0)).unwrap(),name);
            assert_eq!(row.get::<_,Vec<u8>>(1),[1]);
            let original:serde_json::Value=serde_json::from_slice(&row.get::<_,Vec<u8>>(2)).unwrap();
            assert_eq!(crate::raw_store::nfo_key::decode(original["name"].as_str().unwrap()).unwrap(),name);
            assert_eq!(original["password"],"original");assert_eq!(original["content_id"],"\\x01");
        }
        assert_eq!(client.query_one("select count(*) from ext_raw_nfo_passwords",&[]).unwrap().get::<_,i64>(0),names.len() as i64);
        assert_eq!(client.query_one("select count(*) from ext_preserved_cache_rows where family='ext_raw_nfo_passwords'",&[]).unwrap().get::<_,i64>(0),names.len() as i64);
    }

    #[test]
    fn natural_nfo_codec_budget_and_database_failures_are_not_misses() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        let name="n".repeat(3000);let key=Key {name:&name,group:"g",content_id:&[1]};
        let budget=Budget {bytes:5000,rows:1};
        assert!(nfo_password(&mut *client,key,budget).is_err(),"admit encoded name before allocation/query");
        let mut tx=client.transaction().unwrap();assert!(put_nfo_password(&mut tx,key,"",budget).is_err());tx.rollback().unwrap();
        assert_eq!(client.query_one("select count(*) from ext_raw_nfo_passwords",&[]).unwrap().get::<_,i64>(0),0);
        client.batch_execute("drop table ext_raw_nfo_passwords").unwrap();
        let key=Key {name:"Kept\0nzb",group:"g",content_id:&[1]};
        let error=nfo_password(&mut *client,key,BUDGET).unwrap_err();
        assert!(error.downcast_ref::<postgres::Error>().is_some());
        let mut tx=client.transaction().unwrap();let error=put_nfo_password(&mut tx,key,"",BUDGET).unwrap_err();
        assert!(error.downcast_ref::<postgres::Error>().is_some());tx.rollback().unwrap();
    }

    #[test]
    fn natural_cache_budget_errors_do_not_truncate_or_become_misses() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.batch_execute("insert into ext_raw_audio_prints(name,group_name,content_id,position,at_ms,items) values
          ('Kept','g',decode('01','hex'),1,0,decode(repeat('ff',20000),'hex')),
          ('Kept','g',decode('01','hex'),2,0,decode('00','hex'))").unwrap();
        let key=Key {name:"Kept",group:"g",content_id:&[1]};
        assert!(read(&mut *client,Family::AudioPrints,key,Budget {bytes:10_000,rows:2}).unwrap_err().to_string().contains("exceeds"));
        assert!(read(&mut *client,Family::AudioPrints,key,Budget {bytes:100_000,rows:1}).unwrap_err().to_string().contains("exceeds"));
        let facts=read(&mut *client,Family::AudioPrints,key,BUDGET).unwrap().unwrap();
        assert_eq!(facts.rows.len(),2);assert_eq!(facts.rows[0].try_get::<_,Vec<u8>>("items").unwrap().len(),20_000);
    }

    #[test]
    fn natural_cache_database_failure_is_an_error_not_a_miss() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.batch_execute("drop table ext_raw_probes").unwrap();
        let error=match read(&mut *client,Family::Probe,Key {name:"Kept",group:"g",content_id:&[1]},BUDGET) {
            Err(error)=>error,Ok(_)=>panic!("database failure was converted into a hit/miss"),
        };
        assert!(error.downcast_ref::<postgres::Error>().is_some(),"retain typed driver provenance");
    }

    #[test]
    fn natural_cache_probe_write_is_atomic_non_pruning_and_keeps_prior_originals() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        let record=crate::raw_store::ProbeRecord {name:"Kept",group_name:"g",content_id:&[1],crc32:Some(31),agent:None,
          info:"first",moviehash:None,parts:3,complete:true,config_stamp:7,nfo_password:Some(""),cover:Some(("image/jpeg","cover"))};
        let key=Key {name:record.name,group:record.group_name,content_id:record.content_id};
        let mut tx=client.transaction().unwrap();put_probe(&mut tx,&record,BUDGET).unwrap();tx.commit().unwrap();
        let mut changed=record;changed.info="second";
        let mut tx=client.transaction().unwrap();put_probe(&mut tx,&changed,BUDGET).unwrap();tx.rollback().unwrap();
        assert_eq!(probe(&mut *client,key,3,true,7,BUDGET).unwrap().unwrap().info,"first");
        let mut tx=client.transaction().unwrap();put_probe(&mut tx,&changed,BUDGET).unwrap();tx.commit().unwrap();
        let saved=probe(&mut *client,key,3,true,7,BUDGET).unwrap().unwrap();
        assert_eq!(saved.info,"second");assert_eq!(saved.nfo_password.as_deref(),Some(""));
        let old:Vec<u8>=client.query_one("select row_bytes from ext_preserved_cache_rows where family='ext_raw_probes'",&[]).unwrap().get(0);
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&old).unwrap()["info"],"first");
        // A second release's stills never prune another costly result, even
        // when the legacy raw store's row-cap policy would have done so.
        let still=crate::raw_store::SavedStill {position:25,at_ms:None,sha256:"still".into(),width:1,height:2,phash:None};
        let mut tx=client.transaction().unwrap();put_stills(&mut tx,key,&[still.clone()],BUDGET).unwrap();
        put_stills(&mut tx,Key {name:"Other",..key},&[still],BUDGET).unwrap();tx.commit().unwrap();
        assert_eq!(stills(&mut *client,key,BUDGET).unwrap().len(),1);
        let mut tx=client.transaction().unwrap();put_audio_prints(&mut tx,key,&[crate::raw_store::SavedPrint {position:50,at_ms:17,items:vec![0,255]}],BUDGET).unwrap();tx.commit().unwrap();
        assert_eq!(audio_prints(&mut *client,key,BUDGET).unwrap()[0].items,[0,255]);
    }

    #[test]
    fn natural_par2_rejects_legacy_and_unknown_posted_versions_without_deleting_originals() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.batch_execute("insert into ext_raw_par2(name,group_name,content_id,covers_all,posted,par2_block,par2_blocks,parts,complete)
          values('Kept','g',decode('01','hex'),1,decode('02'||repeat('00',20),'hex'),1024,3,4,1)").unwrap();
        let key=Key {name:"Kept",group:"g",content_id:&[1]};
        let valid=par2(&mut *client,key,4,true,BUDGET).unwrap().unwrap();
        assert!(valid.covers_all);assert_eq!(valid.sizes,Some((1024,3)));
        assert!(par2(&mut *client,key,5,true,BUDGET).unwrap().is_none());
        assert!(par2(&mut *client,key,4,false,BUDGET).unwrap().is_none());
        for invalid in [vec![0;20],vec![3;21],vec![]] {
            client.execute("update ext_raw_par2 set posted=$1",&[&invalid]).unwrap();
            assert!(par2(&mut *client,key,4,true,BUDGET).unwrap().is_none());
            assert_eq!(client.query_one("select posted from ext_raw_par2",&[]).unwrap().get::<_,Vec<u8>>(0),invalid);
        }
    }

    #[test]
    fn natural_archive_keeps_source_qualified_crosspost_and_growth_rules() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        client.batch_execute("insert into ext_raw_archives(name,group_name,content_id,password,parts,complete,listing_complete,owner_key,key_salt)
          values('Kept','own',decode('01','hex'),0,2,0,0,-9,17);
          insert into ext_raw_archive_inner_files(release_name,group_name,content_id,seq,name,size,source) values
           ('Kept','own',decode('01','hex'),1,'archive',31,2),
           ('Kept','own',decode('01','hex'),2,'parity',41,1),
           ('OnlyParityOwn','own',decode('01','hex'),1,'own-parity',41,1),
           ('OnlyParityOwn','cross',decode('01','hex'),1,'cross-archive',51,2)").unwrap();
        let key=Key {name:"Kept",group:"own",content_id:&[1]};
        let cached=archive(&mut *client,key,2,false,BUDGET).unwrap().unwrap();
        assert_eq!(cached.files.len(),1);assert_eq!(cached.files[0].name,"archive");
        assert_eq!((cached.keys.owner_key,cached.key_salt),(Some(-9),Some(17)));
        assert!(archive(&mut *client,key,3,false,BUDGET).unwrap().is_none());
        assert!(archive(&mut *client,key,2,true,BUDGET).unwrap().is_none());
        let files=archive_files(&mut *client,Key {name:"OnlyParityOwn",..key},2,BUDGET).unwrap();
        assert_eq!(files.len(),1);assert_eq!(files[0].name,"cross-archive","select groups with the requested source, before applying own-group precedence");
    }

    #[test]
    fn natural_archive_par2_and_recovery_writes_round_trip_without_other_key_eviction() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let mut client=schema.pool.get().unwrap();
        let key=Key {name:"Kept",group:"g",content_id:&[1]};
        let files=[crate::archive::InnerFile {name:"inner".into(),size:Some(31),crc32:Some(19),mtime:Some(-7),host_os:Some(2)}];
        let record=crate::raw_store::ArchiveRecord {name:key.name,group_name:key.group,content_id:key.content_id,source:2,files:&files,password:Some(0),encryption:None,
          names_hidden:false,password_check:None,parts:4,complete:true,listing_complete:true,listing_damaged:false,content_fp:Some(&[3]),max_names_bytes:1024,
          keys:crate::archive::store::ArchiveKeys {owner_key:Some(-9),..Default::default()},key_salt:Some(17),facts:1};
        let mut tx=client.transaction().unwrap();put_archive(&mut tx,&record,BUDGET).unwrap();tx.commit().unwrap();
        let saved=archive(&mut *client,key,4,true,BUDGET).unwrap().unwrap();
        assert_eq!(saved.files[0].clone().into_inner(),files[0]);assert_eq!((saved.keys.owner_key,saved.key_salt),(Some(-9),Some(17)));
        let inventory=crate::par2::Inventory {files:vec![crate::par2::Par2File {set_id:[1;16],file_id:[2;16],md5:[3;16],md5_16k:[4;16],size:31,name:"inner".into(),bound:true,crc32:Some(19)}],
          complete:true,sealed:vec![([1;16],[2;16])],creators:vec![([1;16],"tool".into())],..Default::default()};
        let posted=[2;21];let par=crate::raw_store::Par2Record {name:key.name,group_name:key.group,content_id:key.content_id,covers_all:true,posted:Some(&posted),sizes:Some((1024,3)),parts:4,complete:true,content_fp:None,inventory:Some(&inventory)};
        let mut tx=client.transaction().unwrap();put_par2(&mut tx,&par,BUDGET).unwrap();
        let volume=crate::archive::rev::Volume {index:0,size:31,crc32:19};put_recovery_volumes(&mut tx,key,&[volume],BUDGET).unwrap();tx.commit().unwrap();
        let saved=par2(&mut *client,key,4,true,BUDGET).unwrap().unwrap();let saved=saved.inventory.unwrap();
        assert_eq!(saved.files,inventory.files);assert_eq!(saved.sealed,inventory.sealed);assert_eq!(saved.creators,inventory.creators);assert!(saved.complete);
        assert_eq!(recovery_volumes(&mut *client,key,BUDGET).unwrap(),Some(vec![volume]));
        let bad=crate::raw_store::Par2Record {posted:Some(&[0;20]),..par};
        let mut tx=client.transaction().unwrap();assert!(put_par2(&mut tx,&bad,BUDGET).is_err());tx.rollback().unwrap();
        assert!(par2(&mut *client,key,4,true,BUDGET).unwrap().is_some());
    }
}
