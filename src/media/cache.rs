//! Provider clients capture their storage backend when the real service is
//! constructed. A native client cannot fall back to a process-global SQLite
//! cache after its owner ends or its destination becomes unavailable.

use std::cell::RefCell;
use std::sync::Arc;
use super::resolve::CacheFailure;

thread_local! {static CONSTRUCT:RefCell<Option<Arc<crate::pg::service_cache::Cache>>>=const {RefCell::new(None)};}

pub(crate) fn native<T>(cache:Arc<crate::pg::service_cache::Cache>,construct:impl FnOnce()->T)->T {
    struct Restore(Option<Arc<crate::pg::service_cache::Cache>>);
    impl Drop for Restore {fn drop(&mut self){CONSTRUCT.with(|slot|{slot.replace(self.0.take());});}}
    let previous=CONSTRUCT.with(|slot|slot.replace(Some(cache)));let _restore=Restore(previous);construct()
}

#[derive(Clone)]
pub(crate) enum Backend {Legacy,Native(Arc<crate::pg::service_cache::Cache>)}
impl Backend {
    pub(crate) fn selected()->Self {CONSTRUCT.with(|slot|slot.borrow().as_ref().map_or(Self::Legacy,|cache|Self::Native(cache.clone())))}
    pub(crate) fn is_native(&self)->bool {matches!(self,Self::Native(_))}
    pub(crate) fn check(&self)->Result<(),CacheFailure> {
        match self {
            Self::Native(cache)=>cache.check().map_err(CacheFailure::new),
            Self::Legacy=>{
                let storage=crate::config::checked_storage_config().map_err(|_|CacheFailure::new(anyhow::anyhow!("provider cache configuration unavailable")))?;
                if storage.is_postgres() {return Err(CacheFailure::new(anyhow::anyhow!("native provider requires its owned cache backend")))}
                Ok(())
            }
        }
    }
    pub(crate) fn get(&self,source:&str,url:&str)->Result<Option<(u16,String)>,CacheFailure> {
        match self {
            Self::Native(cache)=>cache.http(source,url).map_err(CacheFailure::new),
            Self::Legacy=>{self.check()?;Ok(crate::raw_store::get_http_cache(source,url))}
        }
    }
    pub(crate) fn put(&self,source:&str,url:&str,status:u16,body:&str)->Result<(),CacheFailure> {
        match self {
            Self::Native(cache)=>cache.put_http(source,url,status,body).map_err(CacheFailure::new),
            Self::Legacy=>{self.check()?;crate::raw_store::put_http_cache(source,url,status,body);Ok(())}
        }
    }
}
