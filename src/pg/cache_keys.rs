//! Preserve the original private derivation namespace without installing it as
//! a fresh catalog's identity. No allocator/cursor/operational meta is copied.

use anyhow::{Result, ensure};
use rusqlite::{Connection, DatabaseName};
use sha2::{Digest, Sha256};

const KEYS:[&str;3]=["id_key0","id_key1","key_salt"];

/// Values remain private and deliberately have no Debug/Serialize interface.
/// Construction requires an actual read-only SQLite snapshot; callers cannot
/// supply a digest/ready flag or arbitrary key/value collection.
pub(crate) struct CapturedKeys { source:[u8;32], values:Vec<(String,i64)> }

#[derive(Debug)]
pub(crate) struct PreservedKeys {
    pub source_digest:[u8;32],
    pub present:Vec<String>,
    pub missing:Vec<&'static str>,
}

pub(crate) fn capture(source:&Connection)->Result<CapturedKeys> {
    ensure!(source.is_readonly(DatabaseName::Main)?,"cache keys require a read-only SQLite source");
    ensure!(!source.is_autocommit(),"cache keys require an explicit source snapshot");
    let has=source.prepare("select 1 from sqlite_schema where type='table' and name='meta'")?.exists([])?;
    let mut values=Vec::new();
    if has {
        let mut statement=source.prepare("select key,value from meta where key in ('id_key0','id_key1','key_salt') order by key")?;
        let mut rows=statement.query([])?;
        while let Some(row)=rows.next()? {
            ensure!(values.len()<3,"duplicate private cache key");
            let key:String=row.get(0)?;
            ensure!(!values.iter().any(|(prior,_)|prior==&key),"duplicate private cache key");
            values.push((key,row.get::<_,i64>(1)?));
        }
    }
    let mut digest=Sha256::new();digest.update(b"atlas.private-cache-namespace.v1\0");
    for key in KEYS {
        digest.update((key.len() as u64).to_be_bytes());digest.update(key.as_bytes());
        if let Some((_,value))=values.iter().find(|(name,_)|name.as_str()==key) {
            digest.update([1]);digest.update(value.to_le_bytes());
        } else {digest.update([0]);}
    }
    Ok(CapturedKeys {source:digest.finalize().into(),values})
}

impl CapturedKeys {
    pub(crate) fn preserve_in(&self,tx:&mut postgres::Transaction<'_>)->Result<PreservedKeys> {
        for (key,value) in &self.values {
            let written=tx.execute("insert into ext_cache_identity_keys(source_digest,key,value) values($1,$2,$3)
              on conflict(source_digest,key) do update set value=excluded.value where ext_cache_identity_keys.value=excluded.value",
              &[&&self.source[..],key,value])?;
            ensure!(written==1,"private cache namespace conflict");
            let actual:i64=tx.query_one("select value from ext_cache_identity_keys where source_digest=$1 and key=$2",&[&&self.source[..],key])?.try_get(0)?;
            ensure!(actual==*value,"private cache key read-back mismatch");
        }
        Ok(PreservedKeys {
            source_digest:self.source,
            present:self.values.iter().map(|(name,_)|name.clone()).collect(),
            missing:KEYS.into_iter().filter(|key|!self.values.iter().any(|(name,_)|name.as_str()==*key)).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_cache_key_capture_never_initializes_or_generates_missing_keys() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("source.db");
        let writer=Connection::open(&path).unwrap();writer.execute_batch("create table meta(key text primary key,value integer);insert into meta values('id_key0',-9),('late_after_id',99)").unwrap();
        assert!(capture(&writer).is_err());drop(writer);
        let source=Connection::open_with_flags(&path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(capture(&source).is_err());source.execute_batch("begin").unwrap();
        let captured=capture(&source).unwrap();assert_eq!(captured.values,[("id_key0".into(),-9)]);
        assert_eq!(source.query_row("select count(*) from meta",[],|row|row.get::<_,i64>(0)).unwrap(),2);
    }

    #[test]
    fn private_cache_keys_preserve_signed_bits_and_are_separate_from_native_identity() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("source.db");
        let writer=Connection::open(&path).unwrap();writer.execute_batch("create table meta(key text primary key,value integer);
          insert into meta values('id_key0',-9223372036854775808),('id_key1',9223372036854775807),('key_salt',-7),('header_cursor',123)").unwrap();drop(writer);
        let source=Connection::open_with_flags(&path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();source.execute_batch("begin").unwrap();
        let captured=capture(&source).unwrap();
        let mut client=schema.pool.get().unwrap();let mut tx=client.transaction().unwrap();
        let summary=captured.preserve_in(&mut tx).unwrap();assert_eq!(summary.present.len(),3);assert!(summary.missing.is_empty());
        assert_eq!(summary.source_digest,captured.source);
        captured.preserve_in(&mut tx).unwrap();
        assert_eq!(tx.query_one("select value from ext_cache_identity_keys where key='id_key0'",&[]).unwrap().get::<_,i64>(0),i64::MIN);
        assert_eq!(tx.query_one("select count(*) from meta where key in ('id_key0','id_key1','key_salt','header_cursor')",&[]).unwrap().get::<_,i64>(0),0);
        tx.commit().unwrap();
    }
}
