//! Real local NNTP responses must retain the actual answering account.
use super::*;
#[test]
fn native_provider_binding_is_account_exact_and_does_not_expose_or_depend_on_password() {
    let mut a=UsenetServer::new("EXAMPLE.INVALID","account-a","first",119);a.ssl=Some(false);
    let mut b=a.clone();b.password="second".into();
    let pool=Pool::new(&[a.clone(),b.clone()]);assert_eq!(pool.provider_binding_on(0).unwrap(),pool.provider_binding_on(1).unwrap());
    b.username="account-b".into();let pool=Pool::new(&[a,b]);assert_ne!(pool.provider_binding_on(0).unwrap(),pool.provider_binding_on(1).unwrap());assert!(pool.provider_binding_on(2).is_err());
}
#[test]
fn native_background_head_retains_the_actual_reply_source_and_preserves_all_provider_missing_rules() {
    let rt=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mode=Arc::new(std::sync::atomic::AtomicU8::new(0));let mut configurations=Vec::new();let mut servers=Vec::new();let seen=Arc::new(Mutex::new(Vec::new()));
        for index in 0..2 {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
            let mut config=UsenetServer::new("127.0.0.1","","",port);config.ssl=Some(false);config.compress=Some(false);config.connections=Some(1);configurations.push(config);
            let mode=mode.clone();let seen=seen.clone();
            servers.push(tokio::spawn(async move {
                loop {
                    let (socket,_)=listener.accept().await.unwrap();let (read,mut write)=socket.into_split();let mut lines=BufReader::new(read);write.write_all(b"200 owned fixture\r\n").await.unwrap();
                    let mut command=String::new();while lines.read_line(&mut command).await.unwrap_or(0)>0 {
                        assert_eq!(command,"HEAD <part3of4.own@example.invalid>\r\n","HEAD never borrows a GROUP/article number or fetches BODY");seen.lock().unwrap().push(index);
                        let phase=mode.load(Ordering::Acquire);let response:&[u8]=if phase==2&&index==0 {b"403 cannot answer\r\n"}else if index==1&&phase==0 {
                            b"221 headers follow\r\nSubject: Own - \"own.bin\" yEnc (3/4)\r\nMessage-ID: <part3of4.own@example.invalid>\r\n.\r\n"
                        }else {b"430 not here\r\n"};
                        if write.write_all(response).await.is_err(){break;}command.clear();
                    }
                }
            }));
        }
        let pool=Pool::new(&configurations).with_body_tries(Duration::from_secs(1),0);
        let HeadAnswer::Found(reply)=pool.background_head("<part3of4.own@example.invalid>").await.unwrap() else {panic!("actual second-provider HEAD required")};
        assert_eq!(reply.provider_binding(),&pool.provider_binding_on(1).unwrap());assert!(reply.bytes().windows(10).any(|b|b==b"Message-ID"));assert_eq!(*seen.lock().unwrap(),vec![0,1]);drop(pool);
        mode.store(1,Ordering::Release);seen.lock().unwrap().clear();let pool=Pool::new(&configurations).with_body_tries(Duration::from_secs(1),0);
        assert!(matches!(pool.background_head("<part3of4.own@example.invalid>").await.unwrap(),HeadAnswer::Missing));assert_eq!(*seen.lock().unwrap(),vec![0,1]);drop(pool);
        mode.store(2,Ordering::Release);seen.lock().unwrap().clear();let pool=Pool::new(&configurations).with_body_tries(Duration::from_secs(1),0);
        assert!(pool.background_head("<part3of4.own@example.invalid>").await.is_err(),"one unanswerable account must not become an all-provider negative");
        for server in servers {server.abort();let _=server.await;}
    });
}
