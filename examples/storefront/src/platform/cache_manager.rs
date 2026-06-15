use std::sync::Arc;

use leaf::prelude::*;

/// A `@Component` [`CacheManager`] (`CandidateRole::NORMAL`) wrapping the framework's
/// in-memory cache. The force-linked Redis `RedisCacheManager` is a `FALLBACK`, so it
/// transparently backs off to this one — they coexist.
#[derive(Debug)]
pub struct InMemoryCache {
    inner: leaf_cache::InMemoryCacheManager,
}
register_component!(InMemoryCache);

impl InMemoryCache {
    fn new() -> Self {
        InMemoryCache { inner: leaf_cache::InMemoryCacheManager::new() }
    }
}

impl CacheManager for InMemoryCache {
    fn cache(&self, name: &str) -> Option<Arc<dyn leaf::core::Cache>> {
        self.inner.cache(name)
    }
}
