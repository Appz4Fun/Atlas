//! Completed provider LIST input. Construction stays at the checked NNTP
//! boundary; web readers and database callers cannot manufacture completion.

use super::{Conn,Pool,NntpError,Result};
use sha2::{Digest,Sha256};
use std::time::{Duration,SystemTime,UNIX_EPOCH};
use std::sync::{Arc,LazyLock};

// A completed inventory remains charged until it is consumed or dropped.
// Sharing this slot across pools bounds retained capture memory process-wide.
pub(crate) const REFRESH_TTL:Duration=Duration::from_secs(600);

static ADMISSION:LazyLock<Arc<tokio::sync::Semaphore>>=LazyLock::new(||Arc::new(tokio::sync::Semaphore::new(1)));

#[derive(Clone,Copy)]
pub(crate) struct Limits {pub rows:u64,pub bytes:u64,pub line_bytes:usize,pub deadline:Duration}
impl Default for Limits {
    fn default()->Self {Self {rows:1_000_000,bytes:128*1024*1024,line_bytes:4096,deadline:Duration::from_secs(120)}}
}
impl Limits {
    fn check(self)->Result<()> {
        if !(1..=2_000_000).contains(&self.rows)||!(64*1024..=512*1024*1024).contains(&self.bytes)||!(1..=4096).contains(&self.line_bytes)
            ||self.deadline.is_zero()||self.deadline>Duration::from_secs(300) {return Err(invalid("invalid LIST capture admission"));}Ok(())
    }
}
struct Row {ordinal:i64,name:String,high:i64,low:i64}
/// No constructor, deserializer or caller-provided completion flag. A value
/// exists only after this module consumed the successful LIST terminator.
pub(crate) struct Capture {
    provider:[u8;32],pattern:Option<String>,captured_at:i64,input_rows:u64,digest:[u8;32],rows:Vec<Row>,
    _admission:tokio::sync::OwnedSemaphorePermit,
}
impl Capture {
    pub(crate) fn provider_binding(&self)->&[u8;32] {&self.provider}
    pub(crate) fn pattern(&self)->Option<&str> {self.pattern.as_deref()}
    pub(crate) fn captured_at(&self)->i64 {self.captured_at}
    pub(crate) fn input_rows(&self)->u64 {self.input_rows}
    pub(crate) fn source_digest(&self)->&[u8;32] {&self.digest}
    pub(crate) fn rows(&self)->impl ExactSizeIterator<Item=(i64,&str,i64,i64)>+'_ {
        self.rows.iter().map(|row|(row.ordinal,row.name.as_str(),row.high,row.low))
    }
}
fn invalid(message:&str)->NntpError {NntpError::Protocol(message.into())}
fn field(hash:&mut Sha256,bytes:&[u8]) {hash.update((bytes.len() as u64).to_le_bytes());hash.update(bytes);}
/// Match the actual leased endpoint and account, not the metrics server label.
/// Password rotation does not rename the account and no secret is persisted.
pub(crate) fn provider_binding(server:&crate::config::UsenetServer)->Result<[u8;32]> {
    let host=server.host.trim().trim_matches(['[',']']).to_ascii_lowercase();
    if host.is_empty()||host.len()>512||server.username.len()>4096||server.key.as_ref().is_some_and(|key|key.len()>4096)
        ||[host.as_str(),server.username.as_str(),server.key.as_deref().unwrap_or("")].iter().any(|value|value.bytes().any(|byte|matches!(byte,0|b'\r'|b'\n'))) {
        return Err(invalid("invalid LIST provider identity"));
    }
    let mut hash=Sha256::new();hash.update(b"atlas.native.group.provider.v1\0");field(&mut hash,host.as_bytes());
    hash.update(server.port.to_le_bytes());hash.update([u8::from(server.use_ssl())]);field(&mut hash,server.username.as_bytes());
    hash.update([u8::from(server.key.is_some())]);field(&mut hash,server.key.as_deref().unwrap_or("").as_bytes());Ok(hash.finalize().into())
}
pub(crate) fn scope_key(pattern:Option<&str>)->[u8;32] {
    let mut hash=Sha256::new();hash.update(b"atlas.native.group.scope.v1\0");hash.update([u8::from(pattern.is_some())]);
    field(&mut hash,pattern.unwrap_or("").as_bytes());hash.finalize().into()
}
fn check_pattern(pattern:Option<&str>)->Result<()> {
    if pattern.is_some_and(|pattern|pattern.is_empty()||pattern.len()>4096||pattern.bytes().any(|byte|matches!(byte,0|b'\r'|b'\n'))) {
        return Err(invalid("invalid LIST capture pattern"));
    }Ok(())
}
impl Pool {
    /// Explicit capture only. GET readers never call this method. It borrows
    /// the existing provider permit and preserves its connection lifecycle.
    pub(crate) async fn capture_active_groups(&self,pattern:Option<&str>,limits:Limits)->Result<Capture> {
        self.capture_groups_on(self.active_index(),pattern,limits).await
    }
    pub(crate) fn capture_index_bindings(&self)->Result<Vec<(usize,[u8;32])>> {
        if self.servers.len()>256 {return Err(invalid("LIST provider inventory exceeds admission"));}
        self.servers.iter().enumerate().filter(|(_,server)|server.cfg.indexes()).map(|(index,server)|Ok((index,provider_binding(&server.cfg)?))).collect()
    }
    /// The selected account is explicit; no global active-provider switch or
    /// independent connection pool participates in background refresh.
    pub(crate) async fn capture_groups_on(&self,index:usize,pattern:Option<&str>,limits:Limits)->Result<Capture> {
        limits.check()?;check_pattern(pattern)?;
        if pattern.is_some()&&limits.bytes<64*1024+4*1024*1024 {return Err(invalid("LIST matcher exceeds ownership admission"));}
        if index>=self.servers.len() {return Err(invalid("LIST provider index is unavailable"));}
        let pending=async {
            let admission=ADMISSION.clone().acquire_owned().await.map_err(|_|invalid("LIST capture admission closed"))?;
            let mut lease=self.lease_for_with_status_bound(index,None,false,Some(512)).await?;
            let binding=provider_binding(&lease.server.cfg)?;
            let result=lease.begin().capture_active_groups_in(binding,pattern,limits,admission).await;
            lease.check(result)
        };
        // Dropping an unfinished lease closes its busy connection and returns
        // the permit. Slow drip and rate-limiter waits share this absolute cap.
        match tokio::time::timeout(limits.deadline,pending).await {Ok(result)=>result,Err(_)=>Err(super::timed_out())}
    }
}
impl Conn {
    async fn capture_active_groups_in(&mut self,provider:[u8;32],pattern:Option<&str>,limits:Limits,admission:tokio::sync::OwnedSemaphorePermit)->Result<Capture> {
        // Reserve connection/COPY scratch plus bounded regex state before
        // parsing names. Per-row charge also covers old+new Vec allocations
        // during growth, String ownership and allocator metadata.
        let mut owned=64*1024u64+if pattern.is_some(){4*1024*1024}else{0};
        if owned>limits.bytes {return Err(invalid("LIST scratch exceeds ownership admission"));}
        let argument=pattern.map_or_else(||"ACTIVE".into(),|pattern|format!("ACTIVE {pattern}"));
        let(code,message)=self.command("LIST",Some(&argument)).await?;
        let(pattern,message)=if code!=215&&pattern.is_some() {
            let(code,message)=self.command("LIST",Some("ACTIVE")).await?;
            if code!=215{return Err(NntpError::Reply {code,message});}(None,message)
        }else{if code!=215{return Err(NntpError::Reply {code,message});}(pattern,message)};
        // Existing compressed/BODY paths are untouched. This narrow capture
        // refuses compressed LIST input rather than use an unbounded inflater.
        if message.to_ascii_uppercase().contains("COMPRESS=GZIP") {return Err(invalid("compressed LIST capture is unavailable"));}
        let captured_at=i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|invalid("LIST capture clock unavailable"))?.as_secs())
            .map_err(|_|invalid("LIST capture clock overflow"))?;
        // Same literal * / ? semantics as legacy wildmatch, without retaining
        // arbitrary request patterns in that helper's process-global cache.
        let matcher=pattern.map(|pattern|regex::RegexBuilder::new(&format!("^(?is:{})$",regex::escape(pattern).replace(r"\*",".*").replace(r"\?","."))).size_limit(1<<20).dfa_size_limit(1<<20).build())
            .transpose().map_err(|_|invalid("LIST capture pattern exceeds matcher admission"))?;
        let mut digest=Sha256::new();digest.update(b"atlas.native.group.list.v1\0");digest.update(provider);digest.update(scope_key(pattern));
        digest.update(captured_at.to_le_bytes());let wire=self.wire;let mut input_rows=0u64;let mut rows=Vec::new();
        loop {
            let Some(mut line)=self.read_line_capped(limits.line_bytes).await? else{return Err(invalid("LIST line exceeds capture admission"));};
            if line.len()>limits.line_bytes||self.wire.checked_sub(wire).is_none_or(|bytes|bytes>limits.bytes) {return Err(invalid("LIST input exceeds capture admission"));}
            if line==b"." {digest.update(b"\0LIST_EOF");break;}
            if line.starts_with(b"..") {line.remove(0);}
            input_rows=input_rows.checked_add(1).ok_or_else(||invalid("LIST row count overflow"))?;
            if input_rows>limits.rows {return Err(invalid("LIST row count exceeds capture admission"));}
            field(&mut digest,&line);
            let text=std::str::from_utf8(&line).map_err(|_|invalid("LIST group line is not UTF-8"))?;let mut parts=text.split_whitespace();
            let(Some(name),Some(high),Some(low))=(parts.next(),parts.next(),parts.next())else{continue;};
            let(Ok(high),Ok(low))=(high.parse::<i64>(),low.parse::<i64>())else{continue;};
            if i128::from(high)-i128::from(low)<=0 {continue;}
            if matcher.as_ref().is_some_and(|matcher|!matcher.is_match(name)) {continue;}
            if name.len()>512||name.chars().any(char::is_control) {return Err(invalid("LIST group name exceeds capture admission"));}
            owned=owned.checked_add(name.len() as u64*2+256).ok_or_else(||invalid("LIST ownership overflow"))?;
            if owned>limits.bytes {return Err(invalid("LIST ownership exceeds capture admission"));}
            rows.push(Row {ordinal:i64::try_from(input_rows).map_err(|_|invalid("LIST ordinal overflow"))?,name:name.into(),high,low});
        }
        Ok(Capture {provider,pattern:pattern.map(str::to_owned),captured_at,input_rows,digest:digest.finalize().into(),rows,_admission:admission})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader};

    #[test]
    #[ignore="requires explicit owned loopback aggregate LIST ownership admission"]
    fn short_list_rows_admit_aggregate_owned_shape_before_retention_and_release_the_permit() {
        super::super::Runtime::new().unwrap().block_on(async {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
            let mut cfg=crate::config::UsenetServer::new("127.0.0.1","","",address.port());cfg.ssl=Some(false);cfg.compress=Some(false);cfg.connections=Some(1);
            let body="alt.binaries.short 40 10 y\r\n".repeat(256)+".\r\n";assert!(body.len()<8192);
            let task=tokio::spawn(async move {
                for _ in 0..2 {
                    let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                    socket.get_mut().write_all(b"200 fixture\r\n").await.unwrap();let mut line=String::new();socket.read_line(&mut line).await.unwrap();assert_eq!(line,"LIST ACTIVE\r\n");
                    let _=socket.get_mut().write_all(format!("215 follows\r\n{body}").as_bytes()).await;
                }
            });
            let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
            let rejected=pool.capture_active_groups(None,Limits {rows:1024,bytes:96*1024,deadline:Duration::from_secs(2),..Limits::default()}).await;
            assert!(matches!(rejected,Err(NntpError::Protocol(_))),"small wire input can still exceed aggregate retained Row/String/Vec ownership");
            let admitted=pool.capture_active_groups(None,Limits {deadline:Duration::from_secs(2),..Limits::default()}).await.unwrap();
            assert_eq!(admitted.input_rows(),256);assert_eq!(admitted.rows().len(),256);task.await.unwrap();
        });
    }

    async fn provider(body:&'static [u8],fallback:bool)->(crate::config::UsenetServer,tokio::task::JoinHandle<Vec<String>>) {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let mut cfg=crate::config::UsenetServer::new("127.0.0.1","capture-account","fixture-password",address.port());
        cfg.ssl=Some(false);cfg.compress=Some(false);
        let task=tokio::spawn(async move {
            let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
            socket.get_mut().write_all(b"200 fixture\r\n").await.unwrap();let mut commands=Vec::new();
            loop {
                let mut command=String::new();if socket.read_line(&mut command).await.unwrap()==0{break;}
                let command=command.trim_end().to_owned();commands.push(command.clone());
                if command.starts_with("AUTHINFO USER "){socket.get_mut().write_all(b"281 authenticated\r\n").await.unwrap();}
                else if fallback&&command.starts_with("LIST ACTIVE "){socket.get_mut().write_all(b"501 pattern unsupported\r\n").await.unwrap();}
                else if command.starts_with("LIST ACTIVE") {
                    socket.get_mut().write_all(b"215 active groups follow\r\n").await.unwrap();
                    socket.get_mut().write_all(body).await.unwrap();break;
                }else{panic!("unexpected fixed fixture command");}
            }commands
        });(cfg,task)
    }

    #[test]
    #[ignore="requires explicit owned loopback NNTP fixture admission"]
    fn completed_list_retains_provider_scope_raw_ordinals_and_signed_bounds() {
        super::super::Runtime::new().unwrap().block_on(async {
            let(cfg,task)=provider(b"alt.binaries.films.b 30 10 y\r\nmalformed\r\nalt.binaries.films.a 10 -10 y\r\nalt.binaries.empty 1 1 y\r\n.\r\n",false).await;
            let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
            let capture=pool.capture_active_groups(Some("*films*"),Limits::default()).await.unwrap();
            assert_eq!(capture.provider_binding(),&provider_binding(&cfg).unwrap());assert_eq!(capture.pattern(),Some("*films*"));
            assert_eq!(capture.input_rows(),4);assert_eq!(capture.rows().collect::<Vec<_>>(),vec![(1,"alt.binaries.films.b",30,10),(3,"alt.binaries.films.a",10,-10)]);
            assert_ne!(capture.source_digest(),&[0;32]);
            assert_eq!(task.await.unwrap(),vec!["AUTHINFO USER capture-account","LIST ACTIVE *films*"]);
            let mut another=cfg.clone();another.username="another-account".into();
            assert_ne!(provider_binding(&cfg).unwrap(),provider_binding(&another).unwrap());
        });
    }

    #[test]
    #[ignore="requires explicit owned loopback NNTP fixture admission"]
    fn pattern_refusal_captures_actual_full_fallback_without_filtering_its_inventory() {
        super::super::Runtime::new().unwrap().block_on(async {
            let(cfg,task)=provider(b"alt.binaries.games 30 10 y\r\nalt.binaries.films 50 10 y\r\n.\r\n",true).await;
            let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
            let capture=pool.capture_active_groups(Some("*films*"),Limits::default()).await.unwrap();
            assert_eq!(capture.pattern(),None);assert_eq!(capture.rows().count(),2);
            assert_eq!(task.await.unwrap(),vec!["AUTHINFO USER capture-account","LIST ACTIVE *films*","LIST ACTIVE"]);
            assert_ne!(scope_key(None),scope_key(Some("*films*")));
        });
    }

    #[test]
    #[ignore="requires explicit owned loopback NNTP fixture admission"]
    fn quiet_list_deadline_releases_the_provider_permit_and_closes_unconsumed_input() {
        super::super::Runtime::new().unwrap().block_on(async {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address=listener.local_addr().unwrap();
            let mut cfg=crate::config::UsenetServer::new("127.0.0.1","","",address.port());
            cfg.ssl=Some(false);cfg.compress=Some(false);cfg.connections=Some(1);
            let task=tokio::spawn(async move {
                for attempt in 0..2 {
                    let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                    socket.get_mut().write_all(b"200 fixture\r\n").await.unwrap();
                    let mut command=String::new();socket.read_line(&mut command).await.unwrap();assert_eq!(command,"LIST ACTIVE\r\n");
                    socket.get_mut().write_all(b"215 groups follow\r\n").await.unwrap();
                    if attempt==0 {command.clear();assert_eq!(socket.read_line(&mut command).await.unwrap(),0,"deadline must close the unconsumed connection");}
                    else{socket.get_mut().write_all(b"alt.binaries.test 30 10 y\r\n.\r\n").await.unwrap();}
                }
            });
            let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
            let error=pool.capture_active_groups(None,Limits {deadline:Duration::from_millis(250),..Limits::default()}).await.err().unwrap();
            assert!(error.is_timeout());
            // The same one-permit pool must reconnect and finish another LIST.
            let retry=pool.capture_active_groups(None,Limits {deadline:Duration::from_secs(2),..Limits::default()}).await.unwrap();
            assert_eq!(retry.rows().collect::<Vec<_>>(),vec![(1,"alt.binaries.test",30,10)]);task.await.unwrap();
        });
    }

    #[test]
    #[ignore="requires explicit owned loopback NNTP fixture admission"]
    fn incomplete_or_over_admission_list_cannot_become_a_completed_capture() {
        super::super::Runtime::new().unwrap().block_on(async {
            for (body,limit) in [(b"alt.binaries.films 30 10 y\r\n".as_slice(),Limits::default()),
                (b"alt.binaries.\xff 30 10 y\r\n.\r\n".as_slice(),Limits::default()),
                (b"alt.binaries.films 30 10 y\r\nalt.binaries.other 20 10 y\r\n.\r\n".as_slice(),Limits {rows:1,..Limits::default()}),
                (b"alt.binaries.films 30 10 y\r\n.\r\n".as_slice(),Limits {line_bytes:16,..Limits::default()})] {
                let(cfg,task)=provider(body,false).await;let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
                assert!(pool.capture_active_groups(None,limit).await.is_err());task.await.unwrap();
            }
        });
    }
    #[test]
    #[ignore="requires explicit owned loopback NNTP fixture admission"]
    fn completed_capture_holds_shared_memory_admission_across_different_pools() {
        super::super::Runtime::new().unwrap().block_on(async {
            let(first_cfg,first_task)=provider(b".\r\n",false).await;
            let first_pool=super::super::Pool::new(std::slice::from_ref(&first_cfg));
            let first=first_pool.capture_active_groups(None,Limits::default()).await.unwrap();first_task.await.unwrap();
            let(second_cfg,second_task)=provider(b"alt.binaries.test 30 10 y\r\n.\r\n",false).await;
            let second_pool=super::super::Pool::new(std::slice::from_ref(&second_cfg));
            let blocked=second_pool.capture_active_groups(None,Limits {deadline:Duration::from_millis(100),..Limits::default()}).await;
            assert!(blocked.err().is_some_and(|error|error.is_timeout()),"an unconsumed completed capture must retain the shared slot");
            drop(first);
            let second=second_pool.capture_active_groups(None,Limits {deadline:Duration::from_secs(2),..Limits::default()}).await.unwrap();
            assert_eq!(second.rows().collect::<Vec<_>>(),vec![(1,"alt.binaries.test",30,10)]);
            assert_eq!(second_task.await.unwrap(),vec!["AUTHINFO USER capture-account","LIST ACTIVE"]);
        });
    }

    #[test]
    #[ignore="requires explicit owned loopback NNTP status-boundary admission"]
    fn capture_refuses_oversized_greeting_auth_positive_and_negative_list_status() {
        super::super::Runtime::new().unwrap().block_on(async {
            for phase in 0..4 {
                let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
                let mut cfg=crate::config::UsenetServer::new("127.0.0.1","capture-account","fixture-password",address.port());cfg.ssl=Some(false);cfg.compress=Some(false);
                let task=tokio::spawn(async move {
                    let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                    let status=|code:u16,large:bool|format!("{code} {}\r\n",if large{"x".repeat(8192)}else{"fixture".into()});
                    if socket.get_mut().write_all(status(200,phase==0).as_bytes()).await.is_err(){return;}
                    loop {
                        let mut command=String::new();match socket.read_line(&mut command).await{Ok(0)|Err(_)=>break,Ok(_)=>{}}
                        let response=if command.starts_with("AUTHINFO USER "){status(281,phase==1)}
                            else if phase==3&&command.starts_with("LIST ACTIVE "){status(501,true)}
                            else if command.starts_with("LIST ACTIVE"){format!("{}alt.binaries.test 30 10 y\r\n.\r\n",status(215,phase==2))}
                            else{panic!("unexpected fixed fixture command");};
                        if socket.get_mut().write_all(response.as_bytes()).await.is_err(){break;}
                        if command.starts_with("LIST ACTIVE")&&!(phase==3&&command.starts_with("LIST ACTIVE ")){break;}
                    }
                });
                let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
                let result=pool.capture_active_groups(Some("*test*"),Limits {deadline:Duration::from_secs(2),..Limits::default()}).await;
                assert!(matches!(result,Err(NntpError::Protocol(_))),"oversized status cannot complete any capture phase");task.await.unwrap();
            }
        });
    }


    #[test]
    #[ignore="requires explicit owned loopback NNTP reused-connection admission"]
    fn reused_connections_apply_capture_bound_and_restore_normal_status_policy() {
        super::super::Runtime::new().unwrap().block_on(async {
            for capture_first in [false,true] {
                let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
                let mut cfg=crate::config::UsenetServer::new("127.0.0.1","","",address.port());cfg.ssl=Some(false);cfg.compress=Some(false);cfg.connections=Some(1);
                let task=tokio::spawn(async move {
                    let(socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);socket.get_mut().write_all(b"200 fixture\r\n").await.unwrap();
                    for pass in 0..2 {
                        let mut command=String::new();socket.read_line(&mut command).await.unwrap();assert_eq!(command,"LIST ACTIVE\r\n");
                        let response=format!("215 {}\r\nalt.binaries.test 30 10 y\r\n.\r\n",if pass==1{"x".repeat(8192)}else{"fixture".into()});
                        if socket.get_mut().write_all(response.as_bytes()).await.is_err(){break;}
                    }
                });
                let pool=super::super::Pool::new(std::slice::from_ref(&cfg));
                if capture_first {
                    let first=pool.capture_active_groups(None,Limits::default()).await.unwrap();assert_eq!(first.rows().count(),1);drop(first);
                    assert_eq!(pool.list_groups(None).await.unwrap(),vec![("alt.binaries.test".into(),20)]);
                }else{
                    assert_eq!(pool.list_groups(None).await.unwrap(),vec![("alt.binaries.test".into(),20)]);
                    assert!(matches!(pool.capture_active_groups(None,Limits::default()).await,Err(NntpError::Protocol(_))),"a reused legacy connection must receive the capture-only bound");
                }
                task.await.unwrap();
            }
        });
    }
}
