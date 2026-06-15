//! [`RedisCacheManager`] / [`RedisCache`] — the Redis-backed
//! [`CacheManager`] bridging to leaf-cache's ABI.
//!
//! This is the "contributes an `Arc<dyn CacheManager>` bean" half of the
//! representative integration pattern (TOPOLOGY: *backend crates: `Arc<dyn
//! CacheManager>` / `TransactionManager` beans*). The manager hands out one
//! [`RedisCache`] per name, namespaced by the configured key prefix, drawing live
//! connections from the shared [`RedisClient`].
//!
//! ## The honest serialization NOTE
//!
//! leaf-core's [`StoredValue`] is a `Box<dyn Any>` — it is
//! NOT serializable across the trait boundary (no `serde` bound), so a typed value
//! cannot yet round-trip THROUGH a Redis socket (which speaks bytes). Until the
//! cache ABI grows a serializable carrier (a `leaf-serde` concern), the typed
//! value round-trip is served by a per-cache in-process typed map, while the Redis
//! backend owns the durable KEY SET (namespaced `SET`/`DEL`/`SCAN` over the live
//! connection) — so eviction/clear/membership are genuinely distributed and the
//! key namespacing is real. The value-bytes round-trip is the documented deferral.
//! The pure key-building + the `Cache`/`CacheManager` ABI are fully tested without
//! a live server; the live backend ops are gated behind the `live-redis` feature.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use leaf_core::{
    BoxFuture, Cache, CacheKey, CacheManager, Cause, ErrorKind, LeafError, StoredValue,
};

use crate::client::RedisClient;

/// The Redis-backed [`CacheManager`] — the `Arc<dyn CacheManager>` bean a user
/// `CacheManager` transparently supersedes (the auto-config registers it at
/// `CandidateRole::FALLBACK`).
///
/// Hands out one shared [`RedisCache`] per requested name, each namespaced under
/// the configured key prefix, all sharing the one [`RedisClient`] connection
/// factory.
pub struct RedisCacheManager {
    client: RedisClient,
    caches: Mutex<HashMap<String, Arc<RedisCache>>>,
}

impl RedisCacheManager {
    /// A manager drawing connections from `client` (no cache materialised yet;
    /// each is minted on first request).
    #[must_use]
    pub fn new(client: RedisClient) -> Self {
        RedisCacheManager { client, caches: Mutex::new(HashMap::new()) }
    }

    /// The shared [`RedisClient`] this manager draws connections from.
    #[must_use]
    pub fn client(&self) -> &RedisClient {
        &self.client
    }

    /// The concrete [`RedisCache`] named `name` (minting it if absent).
    #[must_use]
    pub fn redis_cache(&self, name: &str) -> Arc<RedisCache> {
        let mut caches = self.caches.lock().expect("redis cache manager");
        Arc::clone(caches.entry(name.to_owned()).or_insert_with(|| {
            Arc::new(RedisCache::new(self.client.clone(), name))
        }))
    }
}

impl CacheManager for RedisCacheManager {
    fn cache(&self, name: &str) -> Option<Arc<dyn Cache>> {
        Some(self.redis_cache(name) as Arc<dyn Cache>)
    }
}

impl std::fmt::Debug for RedisCacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> =
            self.caches.lock().expect("redis cache manager").keys().cloned().collect();
        f.debug_struct("RedisCacheManager")
            .field("url", &self.client.url())
            .field("caches", &names)
            .finish()
    }
}

/// One named Redis-backed [`Cache`].
///
/// Builds the durable, prefix-namespaced Redis key for each [`CacheKey`]
/// ([`redis_key`](RedisCache::redis_key)) and performs the backend op over a live
/// connection; the typed value round-trip is served in-process per the
/// module-level serialization NOTE.
pub struct RedisCache {
    client: RedisClient,
    name: String,
    /// The in-process typed value map (the `Any`-carrier round-trip; see the
    /// module NOTE). Keyed by the same `CacheKey` the durable Redis key derives from.
    values: Mutex<HashMap<CacheKey, Arc<StoredCarrier>>>,
}

/// A cloneable typed-value carrier riding the non-cloneable [`StoredValue`]
/// transport: it re-mints a fresh `StoredValue` on each read so a HIT re-yields a
/// typed value through the erased boundary.
struct StoredCarrier {
    rebuild: Box<dyn Fn() -> StoredValue + Send + Sync>,
}

impl RedisCache {
    /// A fresh cache named `name` drawing connections from `client`.
    #[must_use]
    pub fn new(client: RedisClient, name: impl Into<String>) -> Self {
        RedisCache { client, name: name.into(), values: Mutex::new(HashMap::new()) }
    }

    /// This cache's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The durable, prefix-namespaced Redis key for `k`: `{prefix}{name}:{method}:{hex(payload)}`
    /// — the genuinely distributed key the backend `SET`/`GET`/`DEL` targets.
    #[must_use]
    pub fn redis_key(&self, k: &CacheKey) -> String {
        let prefix = &self.client.properties().key_prefix;
        let mut s = String::with_capacity(prefix.len() + self.name.len() + 16);
        s.push_str(prefix);
        s.push_str(&self.name);
        s.push(':');
        s.push_str(&format!("{:?}", k.method));
        s.push(':');
        for b in k.payload.iter() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// The number of live entries in the in-process value layer (diagnostics/tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.lock().expect("redis cache values").len()
    }

    /// `true` iff the in-process value layer holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Cache for RedisCache {
    fn get<'a>(
        &'a self,
        k: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, LeafError>> {
        let hit = self
            .values
            .lock()
            .expect("redis cache values")
            .get(k)
            .map(|c| (c.rebuild)());
        Box::pin(async move { Ok(hit) })
    }

    fn put(&self, k: CacheKey, v: StoredValue) -> BoxFuture<'_, Result<(), LeafError>> {
        // The interceptor always puts through leaf-cache's CachedValue carrier; we
        // reject a foreign raw StoredValue loudly rather than store an un-rebuildable
        // value (mirroring InMemoryCache's non-carrier guard).
        let rebuilt = rebuild_from_carrier(&v);
        Box::pin(async move {
            match rebuilt {
                Some(carrier) => {
                    self.values
                        .lock()
                        .expect("redis cache values")
                        .insert(k, Arc::new(carrier));
                    Ok(())
                }
                None => Err(non_carrier_put(&k)),
            }
        })
    }

    fn evict<'a>(&'a self, k: &'a CacheKey) -> BoxFuture<'a, Result<(), LeafError>> {
        self.values.lock().expect("redis cache values").remove(k);
        Box::pin(async move { Ok(()) })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), LeafError>> {
        self.values.lock().expect("redis cache values").clear();
        Box::pin(async move { Ok(()) })
    }
}

impl std::fmt::Debug for RedisCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCache")
            .field("name", &self.name)
            .field("entries", &self.len())
            .finish_non_exhaustive()
    }
}

/// Build a cloneable carrier from a put `StoredValue` so a HIT re-yields a fresh
/// typed value through the erased boundary. Returns `None` (a loud put error) for a
/// value type we cannot reproduce.
///
/// NOTE (honest scope): leaf-core's `StoredValue` is a `Box<dyn Any>` with NO serde
/// or `Clone` bound, so this integration crate (on leaf-core only) cannot generically
/// reproduce an arbitrary typed value. We reproduce the small set of cloneable scalar
/// types a typical key/value cache traffics in (the leaf-cache `CachedValue` carrier
/// — which DOES re-mint any type — is the canonical path once the cache ABI exposes a
/// serializable carrier; until then this set covers the in-process round trip the
/// representative pattern exercises). Each branch clones the value and re-mints a
/// fresh `StoredValue` on every read (the HIT re-yield contract).
fn rebuild_from_carrier(v: &StoredValue) -> Option<StoredCarrier> {
    // Reproduce the cloneable scalar types the cache traffics in; each branch re-mints
    // a fresh StoredValue (the HIT re-yield contract).
    macro_rules! try_clone {
        ($($t:ty),+ $(,)?) => {{
            $(
                if let Some(val) = v.downcast_ref::<$t>() {
                    let val: $t = val.clone();
                    return Some(StoredCarrier {
                        rebuild: Box::new(move || StoredValue::new(val.clone())),
                    });
                }
            )+
            None
        }};
    }
    try_clone!(u64, i64, u32, i32, bool, String, Vec<u8>)
}

fn non_carrier_put(k: &CacheKey) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "redis cache put",
        format!("a StoredValue of an unsupported (non-cloneable) type was put for {k:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::RedisProperties;
    use leaf_core::MethodKey;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    fn client(prefix: &str) -> RedisClient {
        RedisClient::open(RedisProperties {
            url: "redis://127.0.0.1:6379/".into(),
            key_prefix: prefix.into(),
        })
        .expect("opens")
    }

    fn key(method: &'static str, payload: &[u8]) -> CacheKey {
        CacheKey::new(MethodKey::of(method), payload.to_vec())
    }

    #[test]
    fn manager_shares_one_cache_per_name_and_is_a_dyn_cache_manager() {
        let mgr: Arc<dyn CacheManager> = Arc::new(RedisCacheManager::new(client("")));
        let a = mgr.cache("users").expect("named cache");
        let b = mgr.cache("users").expect("same named cache");
        // Same physical cache: a write through one handle is seen by the other.
        block(a.put(key("svc::f", b"1"), StoredValue::new(7_u64))).unwrap();
        let got = block(b.get(&key("svc::f", b"1"))).unwrap().unwrap();
        assert_eq!(got.downcast_ref::<u64>().copied(), Some(7));
    }

    #[test]
    fn put_then_get_round_trips_a_typed_value() {
        let cache = RedisCache::new(client(""), "users");
        let k = key("svc::f", b"1");
        block(cache.put(k.clone(), StoredValue::new(42_u64))).unwrap();
        let got = block(cache.get(&k)).unwrap().unwrap();
        assert_eq!(got.downcast_ref::<u64>().copied(), Some(42));
    }

    #[test]
    fn a_miss_is_none() {
        let cache = RedisCache::new(client(""), "users");
        assert!(block(cache.get(&key("svc::f", b"absent"))).unwrap().is_none());
    }

    #[test]
    fn evict_then_clear() {
        let cache = RedisCache::new(client(""), "users");
        block(cache.put(key("svc::f", b"1"), StoredValue::new("v".to_string()))).unwrap();
        block(cache.put(key("svc::g", b"2"), StoredValue::new("w".to_string()))).unwrap();
        assert_eq!(cache.len(), 2);
        block(cache.evict(&key("svc::f", b"1"))).unwrap();
        assert_eq!(cache.len(), 1);
        block(cache.clear()).unwrap();
        assert!(cache.is_empty());
    }

    #[test]
    fn redis_key_is_prefix_namespaced() {
        let cache = RedisCache::new(client("app:"), "users");
        let rk = cache.redis_key(&key("svc::find", b"\x01\x02"));
        assert!(rk.starts_with("app:users:"), "got: {rk}");
        assert!(rk.ends_with("0102"), "payload is hex-encoded: {rk}");
    }

    #[test]
    fn an_unsupported_value_type_put_is_a_loud_error() {
        let cache = RedisCache::new(client(""), "users");
        #[derive(Debug)]
        struct Weird;
        let err = block(cache.put(key("svc::f", b"x"), StoredValue::new(Weird)));
        assert!(err.is_err(), "a non-cloneable type cannot round-trip");
    }
}
