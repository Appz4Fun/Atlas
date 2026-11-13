//! Provider inventory persisted only from a completed, account-bound LIST.
//! Read paths consume PostgreSQL rows and never contact the provider.

use anyhow::{Result,Context,ensure};
use std::sync::{Arc,atomic::Ordering};
use crate::{nntp::group_capture::{Capture,provider_binding,scope_key},config::UsenetServer};
use super::{fresh_reader::ReadView,indexer_runtime::Owner};

/// Only the original canonical writer can persist the completed network value.
/// Header, rows and current pointer become visible in one ordinary RC commit.
pub(crate) fn store(owner:&Arc<Owner>,capture:Capture)->Result<i64> {
    let epoch=owner.epoch().context("canonical LIST writer epoch is unavailable")?;
    owner.with(|client| {
        let mut tx=client.build_transaction().isolation_level(postgres::IsolationLevel::ReadCommitted).start()?;
        super::history_gate::native(&mut tx)?;epoch.verify(&mut tx)?;
        tx.batch_execute("set local statement_timeout='60s';set local lock_timeout='5s'")?;
        let scope=if capture.pattern().is_some(){"pattern"}else{"full"};let key=scope_key(capture.pattern());
        let input=i64::try_from(capture.input_rows())?;let accepted=i64::try_from(capture.rows().len())?;
        let id:i64=tx.query_one("insert into native_group_captures(provider_binding,scope,requested_pattern,pattern_key,captured_at,input_rows,accepted_rows,source_digest)
            values($1,$2,$3,$4,$5,$6,$7,$8) returning id",
            &[&&capture.provider_binding()[..],&scope,&capture.pattern(),&&key[..],&capture.captured_at(),&input,&accepted,&&capture.source_digest()[..]])?.try_get(0)?;
        {
            use postgres::{binary_copy::BinaryCopyInWriter,types::Type};
            let sink=tx.copy_in("copy native_group_capture_rows(capture_id,ordinal,name,high,low) from stdin binary")?;
            let mut copy=BinaryCopyInWriter::new(sink,&[Type::INT8,Type::INT8,Type::TEXT,Type::INT8,Type::INT8]);
            for(ordinal,name,high,low)in capture.rows() {
                if owner.stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into());}
                copy.write(&[&id,&ordinal,&name,&high,&low])?;
            }ensure!(copy.finish()?==accepted as u64,"LIST COPY row count changed");
        }
        epoch.verify(&mut tx)?;
        tx.execute("insert into native_group_capture_current(provider_binding,scope,pattern_key,capture_id) values($1,$2,$3,$4)
            on conflict(provider_binding,scope,pattern_key) do update set capture_id=excluded.capture_id",
            &[&&capture.provider_binding()[..],&scope,&&key[..],&id])?;
        if owner.stop.load(Ordering::Acquire){return Err(super::outage::AttemptEnded.into());}
        tx.commit()?;Ok(id)
    })
}

pub(crate) struct Groups {pub groups:Vec<String>,pub capture_id:i64,pub captured_at:i64}
impl ReadView<'_> {
    /// None is absence of a covering capture, distinct from a checked empty
    /// provider result. Full captures cover any query; pattern captures must
    /// name the exact original wildcard request as well as its fixed hash.
    pub(crate) fn captured_groups(&self,servers:&[UsenetServer],query:&str)->Result<Option<Groups>> {
        let query=query.trim();ensure!(query.chars().count()>=3&&query.len()<=4094&&!query.chars().any(char::is_control),"invalid native group search");
        ensure!(servers.len()<=256,"provider inventory configuration exceeds read admission");
        self.admit_owned(65536)?;
        let bindings=servers.iter().filter(|server|server.index!=Some(false)).map(provider_binding)
            .map(|binding|binding.map(|binding|binding.to_vec())).collect::<crate::nntp::Result<Vec<_>>>()?;
        if bindings.is_empty(){return Ok(None);}
        let pattern=format!("*{query}*");let full=scope_key(None);let specific=scope_key(Some(&pattern));
        self.before_query()?;
        let row=self.tx.borrow_mut().query_opt("select c.id,c.scope,c.requested_pattern,c.pattern_key,c.captured_at,c.input_rows,c.accepted_rows
            from native_group_capture_current h join native_group_captures c
            on(c.id,c.provider_binding,c.scope,c.pattern_key)=(h.capture_id,h.provider_binding,h.scope,h.pattern_key)
            where h.provider_binding=any($1::bytea[]) and((c.scope='full' and c.pattern_key=$2 and c.requested_pattern is null)
                or(c.scope='pattern' and c.pattern_key=$3)) order by c.captured_at desc,c.id desc limit 1",
            &[&bindings,&&full[..],&&specific[..]])?;
        let Some(row)=row else{return Ok(None)};
        let id:i64=row.try_get(0)?;let scope:String=row.try_get(1)?;let stored:Option<String>=row.try_get(2)?;let key:Vec<u8>=row.try_get(3)?;
        let captured_at:i64=row.try_get(4)?;let input:i64=row.try_get(5)?;let accepted:i64=row.try_get(6)?;
        ensure!(id>0&&(0..=2_000_000).contains(&input)&&(0..=input).contains(&accepted),"invalid provider capture extent");
        ensure!((scope=="full"&&stored.is_none()&&key.as_slice()==&full[..])||(scope=="pattern"&&stored.as_deref()==Some(pattern.as_str())&&key.as_slice()==&specific[..]),"provider capture scope changed");
        self.before_query()?;
        let count:i64=self.tx.borrow_mut().query_one("select count(*) from native_group_capture_rows where capture_id=$1",&[&id])?.try_get(0)?;
        ensure!(count==accepted,"provider inventory is incomplete");
        let lower=query.to_lowercase();let mut before:Option<String>=None;let mut ordinal=0i64;let mut groups=Vec::new();
        loop {
            let page=self.budget.rows.min(128).min(usize::try_from(self.remaining_owned()/4096)?);
            ensure!(page>0,"provider group workspace exceeds read admission");self.before_query()?;
            let rows=self.tx.borrow_mut().query("select ordinal,name,high,low from native_group_capture_rows where capture_id=$1
                and($2::text is null or approximate<($2::text)::numeric or(approximate=($2::text)::numeric and ordinal>$3))
                order by approximate desc,ordinal limit $4",&[&id,&before,&ordinal,&i64::try_from(page)?])?;
            let read=rows.len();
            for row in rows {
                self.check_deadline()?;ordinal=row.try_get(0)?;let name:String=row.try_get(1)?;
                ensure!(ordinal>0&&name.len()<=512&&!name.chars().any(char::is_control),"invalid provider inventory row");
                let high:i64=row.try_get(2)?;let low:i64=row.try_get(3)?;let approximate=i128::from(high)-i128::from(low);
                ensure!(approximate>0,"empty provider inventory row");before=Some(approximate.to_string());
                let name_lower=name.to_lowercase();
                if name_lower.contains(&lower)&&name_lower.contains(".binaries.")&&!crate::filters::group_blocked(&name) {
                    self.admit_owned(name.len() as u64*48+2048)?;groups.push(name);
                    if groups.len()==300{return Ok(Some(Groups {groups,capture_id:id,captured_at}));}
                }
            }
            if read<page{break;}
        }
        Ok(Some(Groups {groups,capture_id:id,captured_at}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::{Config,HeaderStoreConfig,UsenetServer},nntp::{Pool,group_capture::Limits},pg::{self,fresh_reader::{ReadView,Budget}}};
    use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader};
    use std::time::{Duration,Instant};

    #[test]
    #[ignore="requires isolated schema11 plus explicit owned loopback LIST and canonical owner admission"]
    fn actual_completed_list_persists_and_native_search_keeps_order_and_account_scope() {
        let schema=pg::testing::Schema::new().expect("explicit isolated PostgreSQL schema11");let root=tempfile::tempdir().unwrap();
        let mut config=Config::default();config.postprocess.enabled=false;config.predb.enabled=false;config.spots.enabled=false;config.inferred_parts=false;
        config.media.tvmaze.enabled=false;config.media.allocine.enabled=false;config.media.details_per_minute=0;
        let storage=pg::StorageConfig {backend:pg::Backend::Postgres,url:schema.url.clone(),schema:schema.name.clone(),max_connections:1,allow_plaintext:true};
        let headers=HeaderStoreConfig {enabled:true,dir:root.path().join("headers").to_str().unwrap().into()};
        let runtime=pg::indexer_runtime::Runtime::open(&config,storage,headers,&[],&mut None).unwrap();
        let(mut server,capture)=tokio::runtime::Runtime::new().unwrap().block_on(async {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
            let mut server=UsenetServer::new("127.0.0.1","","",address.port());server.ssl=Some(false);server.compress=Some(false);
            let task=tokio::spawn(async move {
                let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                socket.get_mut().write_all(b"200 fixture\r\n").await.unwrap();let mut command=String::new();socket.read_line(&mut command).await.unwrap();assert_eq!(command,"LIST ACTIVE\r\n");
                socket.get_mut().write_all(b"215 inventory follows\r\nalt.binaries.films.b 30 10 y\r\nmalformed\r\nalt.binaries.films.a 10 -10 y\r\nalt.discussion.films 99 0 y\r\nalt.binaries.games 99 0 y\r\n.\r\n").await.unwrap();
            });
            let pool=Pool::new(std::slice::from_ref(&server));let capture=pool.capture_active_groups(None,Limits::default()).await.unwrap();task.await.unwrap();(server,capture)
        });
        let mut reader=schema.pool.get().unwrap();
        {
            let view=ReadView::open(&mut reader,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            assert!(view.captured_groups(std::slice::from_ref(&server),"films").unwrap().is_none(),"missing capture is not a successful empty inventory");
        }
        let capture_id=store(&runtime.owner,capture).unwrap();
        let row=reader.query_one("select input_rows,accepted_rows from native_group_captures where id=$1",&[&capture_id]).unwrap();assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(5,4));
        {
            let view=ReadView::open(&mut reader,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
            let found=view.captured_groups(std::slice::from_ref(&server),"FiLmS").unwrap().unwrap();
            assert_eq!(found.groups,vec!["alt.binaries.films.b","alt.binaries.films.a"]);assert_eq!(found.capture_id,capture_id);
            assert!(view.captured_groups(std::slice::from_ref(&server),"unmatched").unwrap().unwrap().groups.is_empty());
        }
        server.username="another-account".into();
        let view=ReadView::open(&mut reader,&schema.name,Budget::default(),Instant::now()+Duration::from_secs(10)).unwrap();
        assert!(view.captured_groups(&[server],"films").unwrap().is_none(),"another account cannot inherit this provider inventory");drop(view);
        runtime.owner.close();
    }
}
