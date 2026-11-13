//! Provider clients capture their storage backend when the real service is
//! constructed. A native client cannot fall back to a process-global SQLite
//! cache after its owner ends or its destination becomes unavailable.

use super::resolve::CacheFailure;
use std::cell::RefCell;
use std::sync::Arc;

thread_local! {static CONSTRUCT:RefCell<Option<Arc<crate::pg::service_cache::Cache>>>=const {RefCell::new(None)};}

pub(crate) fn native<T>(cache: Arc<crate::pg::service_cache::Cache>, construct: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<crate::pg::service_cache::Cache>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONSTRUCT.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let previous = CONSTRUCT.with(|slot| slot.replace(Some(cache)));
    let _restore = Restore(previous);
    construct()
}

#[derive(Clone)]
pub(crate) struct Backend(Option<Arc<crate::pg::service_cache::Cache>>);
impl Backend {
    pub(crate) fn selected() -> Self {
        Self(CONSTRUCT.with(|slot| slot.borrow().clone()))
    }

    fn owned(&self) -> Result<&crate::pg::service_cache::Cache, CacheFailure> {
        self.0
            .as_deref()
            .ok_or_else(|| CacheFailure::new(anyhow::anyhow!("native provider requires its owned cache backend")))
    }
    pub(crate) fn check(&self) -> Result<(), CacheFailure> {
        self.owned()?.check().map_err(CacheFailure::new)
    }
    pub(crate) fn get(&self, source: &str, url: &str) -> Result<Option<(u16, String)>, CacheFailure> {
        self.owned()?.http(source, url).map_err(CacheFailure::new)
    }
    pub(crate) fn put(&self, source: &str, url: &str, status: u16, body: &str) -> Result<(), CacheFailure> {
        self.owned()?.put_http(source, url, status, body).map_err(CacheFailure::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unbound_provider_cache_refuses_reads_writes_and_requests() {
        let cache = Backend::selected();
        assert!(cache.check().is_err());
        assert!(cache.get("fixture", "https://example.invalid").is_err());
        assert!(cache.put("fixture", "https://example.invalid", 200, "{}").is_err());
    }
}
