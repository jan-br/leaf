//! The in-memory default [`Cache`] + [`CacheManager`] and the single-flight
//! in-flight map (caching, phase3/09).
//!
//! leaf-core pins the cache SEAMS — the object-safe [`Cache`] / [`CacheManager`]
//! traits + the typed [`CacheKey`] / [`StoredValue`]. This module owns the live
//! storage: one heterogeneous physical [`InMemoryCache`] keyed by the typed
//! [`CacheKey`] (`(MethodKey, payload)` — the `MethodKey` component prevents a
//! wrong-type read across two methods sharing one cache name+payload), holding the
//! cloneable [`CachedValue`] carrier, plus a per-cache
//! single-flight in-flight map (`InFlight`) so concurrent identical `sync=true`
//! keys await ONE computation. [`InMemoryCacheManager`] hands out named caches (the
//! `Arc<dyn CacheManager>` bean a real backend — Caffeine/Redis — replaces as an
//! ordinary bean).
//!
//! Cancellation (phase3/09 §caching): the computer OWNS the single-flight future
//! (the value lands on the [`Cache::put`] path, not a Drop); a cancelled WAITER
//! releases only its shared completion handle and never cancels the computation; a
//! computer cancelled mid-flight clears its slot via a sync-`Drop`
//! `FlightGuard` so the next caller is promoted to recompute — no async Drop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::{FutureExt, Shared};
use leaf_core::{BoxFuture, Cache, CacheKey, CacheManager, LeafError, StoredValue};

use crate::value::CachedValue;

/// A single-flight completion handle: a cheaply-cloneable shared future yielding
/// `Ok(())` once the owning computation has written the value into the cache (so a
/// joined waiter re-reads a HIT), or `Err` if the computer failed / was cancelled.
type Flight = Shared<BoxFuture<'static, Result<(), FlightError>>>;

/// The lightweight, cloneable signal a single-flight waiter observes when the
/// owning computation did not store a value (the computer failed or was
/// cancelled). The DETAILED [`LeafError`] travels back to the COMPUTER directly; a
/// waiter learns only "no value — recompute" (`Shared` requires a `Clone` output,
/// and a `LeafError` is not cheaply cloneable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightError {
    /// The owning computation returned an error / was dropped before storing a
    /// value: the next waiter is promoted to recompute.
    NoValue,
}

/// The per-cache single-flight in-flight map: `CacheKey` → an in-flight completion
/// [`Flight`] (caching `sync=true`).
#[derive(Default)]
struct InFlight {
    flights: Mutex<HashMap<CacheKey, Flight>>,
}

impl InFlight {
    /// Join the flight for `key` if one is in progress, else register `make_flight`
    /// as the new flight and return `None` (the caller is the computer).
    fn join_or_register(
        &self,
        key: &CacheKey,
        make_flight: impl FnOnce() -> Flight,
    ) -> Option<Flight> {
        let mut map = self.flights.lock().expect("in-flight map");
        if let Some(existing) = map.get(key) {
            return Some(existing.clone());
        }
        map.insert(key.clone(), make_flight());
        None
    }

    /// Clear the flight slot for `key` (the computer drops it on completion or
    /// cancellation so the next caller recomputes — promotion).
    fn clear(&self, key: &CacheKey) {
        self.flights.lock().expect("in-flight map").remove(key);
    }
}

/// The in-memory default [`Cache`]: ONE heterogeneous physical map keyed by the
/// typed [`CacheKey`], storing the cloneable [`CachedValue`] carrier + a per-cache
/// single-flight in-flight map.
///
/// This is the safe stand-in a bare engine / a test uses; a real backend
/// (Caffeine/Redis) is an ordinary `Arc<dyn Cache>` bean implementing the SAME
/// trait, with its own TTL/LRU/eviction.
pub struct InMemoryCache {
    name: String,
    store: Mutex<HashMap<CacheKey, CachedValue>>,
    in_flight: InFlight,
}

impl InMemoryCache {
    /// A fresh, empty cache named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        InMemoryCache {
            name: name.into(),
            store: Mutex::new(HashMap::new()),
            in_flight: InFlight::default(),
        }
    }

    /// This cache's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The number of live entries (diagnostics / tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.store.lock().expect("cache store").len()
    }

    /// `true` iff the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The synchronous typed read (the hot path; the async [`Cache::get`] wraps
    /// it). `None` on a miss.
    #[must_use]
    pub fn read(&self, k: &CacheKey) -> Option<CachedValue> {
        self.store.lock().expect("cache store").get(k).cloned()
    }

    /// The synchronous typed write (the hot path; the async [`Cache::put`] wraps
    /// it).
    pub fn write(&self, k: CacheKey, v: CachedValue) {
        self.store.lock().expect("cache store").insert(k, v);
    }

    /// Single-flight a value computation for `key` (`sync=true`): if no flight is
    /// in progress, run `compute` (this caller is the COMPUTER), store the value,
    /// complete the flight, and return it; otherwise JOIN the in-flight
    /// computation, await it, and re-read the cache for the freshly-stored hit.
    ///
    /// A cancelled WAITER never cancels the computation; a cancelled COMPUTER
    /// clears its slot so the next caller recomputes (promotion).
    ///
    /// # Errors
    /// The [`LeafError`] from the computation (for the computer / the promoted
    /// recomputing waiter).
    pub async fn get_or_compute_sync(
        self: &Arc<Self>,
        key: CacheKey,
        compute: impl std::future::Future<Output = Result<CachedValue, LeafError>>,
    ) -> Result<CachedValue, LeafError> {
        // Fast path: an existing hit short-circuits without touching the flight map.
        if let Some(hit) = self.read(&key) {
            return Ok(hit);
        }

        // A oneshot completes the flight; the waiters await the shared rx.
        let (tx, rx) = futures::channel::oneshot::channel::<Result<(), FlightError>>();
        let rx_fut: BoxFuture<'static, Result<(), FlightError>> = Box::pin(async move {
            // A dropped sender (computer cancelled mid-flight) => promote-recompute.
            rx.await.unwrap_or(Err(FlightError::NoValue))
        });
        let flight: Flight = rx_fut.shared();

        match self.in_flight.join_or_register(&key, || flight.clone()) {
            // We JOINED an existing flight: await it, then re-read the cache.
            Some(existing) => {
                drop(tx); // a waiter never completes the flight.
                match existing.await {
                    Ok(()) => match self.read(&key) {
                        Some(hit) => Ok(hit),
                        // The value was evicted between completion and re-read, or
                        // the computer failed: recompute (promotion).
                        None => self.compute_and_store(key, compute).await,
                    },
                    Err(FlightError::NoValue) => self.compute_and_store(key, compute).await,
                }
            }
            // We are the COMPUTER: run, store, complete the flight, clear the slot
            // (all via the sync-Drop FlightGuard — no async finalization).
            None => {
                let mut guard = FlightGuard {
                    cache: Arc::clone(self),
                    key: key.clone(),
                    stored: false,
                    tx: Some(tx),
                };
                let computed = Box::pin(compute).await;
                if let Ok(v) = &computed {
                    self.write(key.clone(), v.clone());
                    guard.stored = true;
                }
                computed
            }
        }
    }

    /// A promoted waiter's direct recompute-and-store (no flight registration; the
    /// previous computer already vacated its slot).
    async fn compute_and_store(
        self: &Arc<Self>,
        key: CacheKey,
        compute: impl std::future::Future<Output = Result<CachedValue, LeafError>>,
    ) -> Result<CachedValue, LeafError> {
        let v = Box::pin(compute).await?;
        self.write(key, v.clone());
        Ok(v)
    }
}

/// The computer's RAII guard: on `Drop` it completes the flight (Ok if the value
/// was stored, else [`FlightError::NoValue`]) and clears the in-flight slot so the
/// next caller recomputes — the promotion contract via sync `Drop`, no async
/// finalization.
struct FlightGuard {
    cache: Arc<InMemoryCache>,
    key: CacheKey,
    stored: bool,
    tx: Option<futures::channel::oneshot::Sender<Result<(), FlightError>>>,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(if self.stored { Ok(()) } else { Err(FlightError::NoValue) });
        }
        self.cache.in_flight.clear(&self.key);
    }
}

impl Cache for InMemoryCache {
    fn get<'a>(
        &'a self,
        k: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, LeafError>> {
        let hit = self.read(k).map(CachedValue::into_stored);
        Box::pin(async move { Ok(hit) })
    }

    fn put(&self, k: CacheKey, v: StoredValue) -> BoxFuture<'_, Result<(), LeafError>> {
        // The interceptor always puts through the CachedValue carrier; a foreign
        // raw StoredValue (no carrier) is dropped with a structured error rather
        // than corrupting the typed read path.
        let stored = CachedValue::from_stored(&v);
        Box::pin(async move {
            match stored {
                Some(cv) => {
                    self.write(k, cv);
                    Ok(())
                }
                None => Err(non_carrier_put(&k)),
            }
        })
    }

    fn evict<'a>(&'a self, k: &'a CacheKey) -> BoxFuture<'a, Result<(), LeafError>> {
        self.store.lock().expect("cache store").remove(k);
        Box::pin(async move { Ok(()) })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), LeafError>> {
        self.store.lock().expect("cache store").clear();
        Box::pin(async move { Ok(()) })
    }
}

/// The error a raw (non-[`CachedValue`]) [`StoredValue`] `put` surfaces — the
/// in-memory cache only stores values minted through the typed carrier.
fn non_carrier_put(key: &CacheKey) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "in-memory cache put",
        format!("a StoredValue without the leaf-cache CachedValue carrier was put for {key:?}"),
    ))
}

impl std::fmt::Debug for InMemoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryCache")
            .field("name", &self.name)
            .field("entries", &self.len())
            .finish_non_exhaustive()
    }
}

/// The in-memory default [`CacheManager`]: lazily mints one [`InMemoryCache`] per
/// requested name and shares it across calls (the `Arc<dyn CacheManager>` bean).
///
/// A real backend's manager (Caffeine/Redis) is an ordinary bean implementing the
/// SAME trait; this is the safe stand-in for the bare engine + tests.
pub struct InMemoryCacheManager {
    caches: Mutex<HashMap<String, Arc<InMemoryCache>>>,
}

impl InMemoryCacheManager {
    /// A fresh manager with no caches yet (each is minted on first request).
    #[must_use]
    pub fn new() -> Self {
        InMemoryCacheManager { caches: Mutex::new(HashMap::new()) }
    }

    /// The concrete in-memory cache named `name` (minting it if absent) — the
    /// single-flight-aware `Arc<InMemoryCache>` the interceptor uses (the `dyn
    /// Cache` face is [`CacheManager::cache`]).
    #[must_use]
    pub fn in_memory_cache(&self, name: &str) -> Arc<InMemoryCache> {
        let mut caches = self.caches.lock().expect("cache manager");
        Arc::clone(
            caches
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(InMemoryCache::new(name))),
        )
    }
}

impl Default for InMemoryCacheManager {
    fn default() -> Self {
        InMemoryCacheManager::new()
    }
}

impl CacheManager for InMemoryCacheManager {
    fn cache(&self, name: &str) -> Option<Arc<dyn Cache>> {
        Some(self.in_memory_cache(name) as Arc<dyn Cache>)
    }
}

impl std::fmt::Debug for InMemoryCacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> =
            self.caches.lock().expect("cache manager").keys().cloned().collect();
        f.debug_struct("InMemoryCacheManager").field("caches", &names).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::MethodKey;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    fn key(payload: &[u8]) -> CacheKey {
        CacheKey::new(MethodKey::of("svc::find"), payload.to_vec())
    }

    fn val(v: u64) -> CachedValue {
        CachedValue::of(v)
    }

    fn read_u64(cache: &InMemoryCache, k: &CacheKey) -> Option<u64> {
        block(cache.get(k))
            .unwrap()
            .and_then(|sv| CachedValue::from_stored(&sv))
            .and_then(|cv| cv.peek::<u64>().copied())
    }

    #[test]
    fn put_then_get_round_trips_a_typed_value() {
        let cache = InMemoryCache::new("users");
        let k = key(b"1");
        block(cache.put(k.clone(), val(42).into_stored())).unwrap();
        assert_eq!(read_u64(&cache, &k), Some(42));
    }

    #[test]
    fn a_miss_is_none() {
        let cache = InMemoryCache::new("users");
        assert!(block(cache.get(&key(b"absent"))).unwrap().is_none());
    }

    #[test]
    fn evict_removes_one_entry() {
        let cache = InMemoryCache::new("users");
        let k = key(b"1");
        block(cache.put(k.clone(), val(1).into_stored())).unwrap();
        block(cache.evict(&k)).unwrap();
        assert!(block(cache.get(&k)).unwrap().is_none(), "evicted");
    }

    #[test]
    fn clear_drops_all_entries() {
        let cache = InMemoryCache::new("users");
        block(cache.put(key(b"1"), val(1).into_stored())).unwrap();
        block(cache.put(key(b"2"), val(2).into_stored())).unwrap();
        assert_eq!(cache.len(), 2);
        block(cache.clear()).unwrap();
        assert!(cache.is_empty());
    }

    #[test]
    fn the_method_key_partitions_two_methods_sharing_a_payload() {
        // Two methods with the SAME payload bytes are DISTINCT entries.
        let cache = InMemoryCache::new("shared");
        let a = CacheKey::new(MethodKey::of("svc::find_user"), b"7".to_vec());
        let b = CacheKey::new(MethodKey::of("svc::find_order"), b"7".to_vec());
        block(cache.put(a.clone(), CachedValue::of(1_u64).into_stored())).unwrap();
        block(cache.put(b.clone(), CachedValue::of("order".to_string()).into_stored())).unwrap();
        let ga = block(cache.get(&a)).unwrap().unwrap();
        let gb = block(cache.get(&b)).unwrap().unwrap();
        assert_eq!(CachedValue::from_stored(&ga).unwrap().peek::<u64>().copied(), Some(1));
        assert_eq!(
            CachedValue::from_stored(&gb).unwrap().peek::<String>().map(String::as_str),
            Some("order")
        );
    }

    #[test]
    fn manager_shares_one_cache_per_name() {
        let mgr = InMemoryCacheManager::new();
        let a = mgr.in_memory_cache("users");
        a.write(key(b"1"), val(9));
        let b = mgr.cache("users").expect("same named cache");
        // The second handle sees the first handle's write (the SAME physical map).
        let got = block(b.get(&key(b"1"))).unwrap().unwrap();
        assert_eq!(CachedValue::from_stored(&got).unwrap().peek::<u64>().copied(), Some(9));
        // A different name is a different, empty cache.
        assert!(mgr.in_memory_cache("orders").is_empty());
    }

    #[test]
    fn a_raw_non_carrier_put_is_a_loud_error() {
        // A StoredValue not minted through CachedValue is rejected (never corrupts
        // the typed read path).
        let cache = InMemoryCache::new("users");
        let raw = StoredValue::new(99_u64);
        assert!(block(cache.put(key(b"x"), raw)).is_err());
    }

    #[test]
    fn single_flight_computes_once_for_sequential_callers() {
        let cache = Arc::new(InMemoryCache::new("sync"));
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let k = key(b"sf");
        for _ in 0..2 {
            let runs = Arc::clone(&runs);
            let v = block(cache.get_or_compute_sync(k.clone(), async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(val(5))
            }))
            .unwrap();
            assert_eq!(v.peek::<u64>().copied(), Some(5));
        }
        // The SECOND sequential call hit the cache; the body ran exactly once.
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_flight_concurrent_callers_share_one_computation() {
        // N concurrent identical-key sync callers await ONE computation; the body
        // runs once and every caller observes the same value. Driven on a real
        // (tokio) executor with proper wakers — the computer yields a few times so
        // the waiters register on the flight before it completes.
        let cache = Arc::new(InMemoryCache::new("sync"));
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let k = key(b"sf");
        let futs: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let runs = Arc::clone(&runs);
                let k = k.clone();
                async move {
                    cache
                        .get_or_compute_sync(k, async move {
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            // Yield (waker-driven) so the other callers register on
                            // the flight before the computer completes.
                            for _ in 0..4 {
                                tokio::task::yield_now().await;
                            }
                            Ok(val(11))
                        })
                        .await
                        .unwrap()
                }
            })
            .collect();
        let results = futures::future::join_all(futs).await;
        for r in &results {
            assert_eq!(r.peek::<u64>().copied(), Some(11));
        }
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "concurrent identical sync keys share ONE computation"
        );
    }
}
