//! Content identity over native normalized rows, using the exact raw-cache hash
//! encoding. Release/file database IDs never contribute to the digest.

use anyhow::{Result,ensure};
use postgres::GenericClient;
use sha2::{Digest,Sha256};
use std::sync::atomic::{AtomicBool,Ordering};

/// Caller owns the read snapshot (or the locked release in its write txn).
/// Each page and value is admitted before PostgreSQL returns its payload.
pub(crate) fn read(client:&mut impl GenericClient,release:i64,stop:&AtomicBool)->Result<Option<Vec<u8>>> {
    let mut hash=Sha256::new();let mut after:Option<(Vec<u8>,i64)>=None;let mut files=0u64;
    loop {
        if stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
        let rows=client.query("select id,case when octet_length(original_filename)<=65536 then original_filename else null end,
            case when octet_length(subject_mid)<=65536 then subject_mid else null end,expected,
            octet_length(original_filename),octet_length(subject_mid)
            from files where release_id=$1 and ($2::bytea is null or (original_filename,id)>($2,$3::bigint))
            order by original_filename,id limit 64",
            &[&release,&after.as_ref().map(|(name,_)|name.as_slice()),&after.as_ref().map(|(_,id)|*id)])?;
        // A nullable original is not part of the keyset ordering. Refuse it
        // explicitly instead of silently omitting it or treating it as a miss.
        let missing:bool=client.query_one("select exists(select 1 from files where release_id=$1 and original_filename is null)",&[&release])?.try_get(0)?;
        ensure!(!missing,"native content identity lacks original filename bytes");
        if rows.is_empty() {break;}
        for row in rows {
            if stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
            let id:i64=row.try_get(0)?;
            let length:Option<i32>=row.try_get(4)?;
            ensure!(length.is_some_and(|bytes|(0..=65536).contains(&bytes)),"native content filename exceeds read admission");
            let filename:Vec<u8>=row.try_get(1)?;
            let mid_length:Option<i32>=row.try_get(5)?;
            ensure!(mid_length.is_none_or(|bytes|(0..=65536).contains(&bytes)),"native first message ID exceeds read admission");
            let subject:Option<Vec<u8>>=row.try_get(2)?;
            let first=match subject.filter(|mid|!mid.is_empty()) {
                Some(mid)=>mid,
                None=>{
                    let row=client.query_opt("select case when octet_length(message_id)<=65536 then message_id else null end,
                        octet_length(message_id) from segments where file_id=$1 and release_id=$2 and octet_length(message_id)>0
                        order by part nulls first,message_id limit 1",&[&id,&release])?;
                    match row {
                        Some(row)=>{
                            ensure!(row.try_get::<_,i32>(1)?<=65536,"native first message ID exceeds read admission");row.try_get(0)?
                        },
                        None=>Vec::new(),
                    }
                }
            };
            let mut seen=Vec::new();let mut part_after=None;
            loop {
                if stop.load(Ordering::Acquire) {return Err(super::outage::AttemptEnded.into());}
                let parts=client.query("select part from file_parts where file_id=$1 and ($2::bigint is null or part>$2)
                    order by part limit 4096",&[&id,&part_after])?;
                if parts.is_empty() {break;}
                for part in parts {
                    let part:i64=part.try_get(0)?;
                    ensure!((0..=crate::parser::MAX_PARTS).contains(&part),"native part bitmap contains an unsupported part");
                    crate::store::insert_part(&mut seen,part);part_after=Some(part);
                }
            }
            crate::raw_store::hash_file_fields_into(&mut hash,std::str::from_utf8(&filename)?,std::str::from_utf8(&first)?,row.try_get(3)?,&seen);
            after=Some((filename,id));files=files.checked_add(1).ok_or_else(||anyhow::anyhow!("native file count overflow"))?;
        }
    }
    Ok((files>0).then(||hash.finalize().to_vec()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_content_identity_matches_the_legacy_cache_encoding() {
        let Some(schema)=crate::pg::testing::Schema::native_writer() else {return};
        let release=crate::parser::Release {name:"Example.Release".into(),group:"example.group".into(),
            articles:vec![
                crate::parser::Article {filename:Some("a\0.bin".into()),message_id:"<one@example.test>".into(),
                    part:Some(1),total_parts:Some(2),bytes:10,..Default::default()},
                crate::parser::Article {filename:Some("a\0.bin".into()),message_id:"<two@example.test>".into(),
                    part:Some(2),total_parts:Some(2),bytes:11,..Default::default()},
            ],..Default::default()};
        let mut client=schema.pool.get().unwrap();
        crate::pg::release_writer::save(&mut client,&[release],1).unwrap();
        let id:i64=client.query_one("select id from releases",&[]).unwrap().get(0);
        let sqlite=rusqlite::Connection::open_in_memory().unwrap();
        sqlite.execute_batch("create table files(id integer primary key,release_id integer,filename text,subject_mid text,expected integer,seen blob,blob blob);").unwrap();
        sqlite.execute("insert into files values(99,77,?1,?2,2,?3,NULL)",
            rusqlite::params!["a\0.bin","<one@example.test>",vec![6u8]]).unwrap();
        let expected=crate::raw_store::release_content_id_in(&sqlite,"main",77).unwrap();
        let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start().unwrap();
        assert_eq!(super::read(&mut tx,id,&std::sync::atomic::AtomicBool::new(false)).unwrap(),Some(expected));
    }
}
