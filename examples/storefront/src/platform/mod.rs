//! Infrastructure beans: config binding and startup.
//!
//! Both the transaction manager and the cache manager are the framework's
//! auto-configured defaults — `leaf_tx::TxAutoConfig` contributes `InMemoryTransactionManager`
//! as the `transactionManager` bean and `leaf_cache::CacheAutoConfig` contributes
//! `InMemoryCacheManager` as the `cacheManager` bean, both at FALLBACK — so the app
//! hand-writes neither wrapper.
pub mod app_properties;
pub mod startup_runner;
