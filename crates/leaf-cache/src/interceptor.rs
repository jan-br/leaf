//! The [`CacheInterceptor`] — the around-advice that caches an advised method's
//! result (caching, phase3/09).
//!
//! The interceptor is PER-BEAN but PER-METHOD-AWARE: it holds one [`CacheRule`] per
//! cached method of the advised bean (keyed by [`MethodKey`]), so one auto-installed
//! chain entry handles a `@Cacheable` `find` AND a `@CacheEvict` `evict` on the same
//! bean, and any UN-cached method passes straight through to `next.proceed()`. Each
//! rule carries the resolved [`CacheOp`] lowered from the `#[cacheable]`/`#[cache_put]`/
//! `#[cache_evict]` [`CacheOpMeta`], the cache name(s), the
//! typed [`CacheKeyFn`], and the per-return-`T` hit-repack / miss-capture fns.
//!
//! The per-rule body, per phase3/09 §caching:
//!
//! - **`@Cacheable`** — build the [`CacheKey`] from the call
//!   args (the typed [`CacheKeyFn`]); `cache.get(key)` → on HIT re-pack the typed
//!   value and SKIP `next.proceed()` (the substrate short-circuit — the body never
//!   runs); on MISS `next.proceed()`, then `cache.put`. With `sync=true` a per-cache
//!   single-flight in-flight map ensures concurrent identical keys await ONE
//!   computation (a cancelled waiter never cancels the shared computation; a
//!   cancelled computer promotes the next waiter — sync-`Drop` cancel-safety).
//! - **`@CachePut`** — ALWAYS `next.proceed()` then `cache.put` (refresh, never
//!   short-circuit).
//! - **`@CacheEvict`** — evict one key (or `all_entries` → `clear`) either BEFORE
//!   the body (`before_invocation`) or after a successful return, then pass the
//!   result through.
//!
//! Value erasure is resolved by the cloneable [`CachedValue`] carrier riding the
//! `StoredValue` transport: each rule is monomorphized over its method's return
//! type `T`, so a HIT re-packs a typed `T` and a MISS captures the typed `T` — with
//! a `TypeId` guard so a cross-method wrong-type read is a loud
//! [`LeafError`], never a silent mis-cast.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::{FutureExt, Shared};
use leaf_core::{
    AdviceError, BoxFuture, Cache, CacheKey, CacheManager, CacheOpMeta, Call, ErasedRet, LeafError,
    MethodKey, Next,
};

use crate::value::CachedValue;

/// Builds a [`CacheKey`] payload from the call's erased args (caching).
///
/// The typed key fn the `#[cacheable(key = "#id")]` expression lowers to (a typed
/// closure over the real args, erasure-free) — until the macro threads that
/// expression, a binding site supplies one. It receives the [`Call`] (the erased
/// arg tuple + the method identity) and returns the key PAYLOAD bytes; the
/// interceptor prepends the [`MethodKey`] to form the full [`CacheKey`]. A
/// `None`-returning fn means "skip caching for this call" (a `condition` veto).
pub type CacheKeyFn = fn(&Call<'_>) -> Option<Box<[u8]>>;

/// Re-pack a [`CachedValue`] HIT into the typed [`ErasedRet`] (monomorphized over a
/// method's return `T`); errors loudly on a wrong-type read.
type PackHitFn = fn(&CachedValue, &MethodKey) -> Result<ErasedRet, LeafError>;

/// Capture the typed value from a MISS's [`ErasedRet`] into a [`CachedValue`]
/// (monomorphized over `T`); `None` on a non-capturable / mismatched return type.
type CaptureFn = fn(&ErasedRet) -> Option<CachedValue>;

/// The default key fn: a constant payload (every call shares ONE key under the
/// method). Suitable for a no-arg / single-entry cacheable method; a binding site
/// supplies a finer [`CacheKeyFn`] for keying on args.
#[must_use]
pub fn unit_key_fn() -> CacheKeyFn {
    |_call: &Call<'_>| Some(Box::from(&b"()"[..]))
}

/// The resolved cache operation (lowered from [`CacheOpMeta`]) a [`CacheRule`]
/// dispatches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheOp {
    /// `@Cacheable` — HIT short-circuits before proceeding; MISS computes + puts.
    Cacheable,
    /// `@CachePut` — always proceeds then puts (refresh, never short-circuit).
    CachePut,
    /// `@CacheEvict` — evict one key (or all) around the body.
    CacheEvict,
}

/// One cached method's resolved rule (the per-method metadata the interceptor
/// dispatches on).
pub struct CacheRule {
    /// The advised method this rule applies to.
    method: MethodKey,
    op: CacheOp,
    cache_names: &'static [&'static str],
    all_entries: bool,
    before_invocation: bool,
    sync: bool,
    key_fn: CacheKeyFn,
    pack_hit: PackHitFn,
    capture: CaptureFn,
}

impl CacheRule {
    /// Build a rule for `method`/`op` over a method returning `T: Clone`, baking in
    /// the per-`T` hit-repack / miss-capture fns + the per-method `meta` + key fn.
    #[must_use]
    pub fn for_method<T: Clone + Send + Sync + 'static>(
        method: MethodKey,
        op: CacheOp,
        meta: &CacheOpMeta,
        key_fn: CacheKeyFn,
    ) -> Self {
        CacheRule {
            method,
            op,
            cache_names: meta.cache_names,
            all_entries: meta.all_entries,
            before_invocation: meta.before_invocation,
            sync: meta.sync,
            key_fn,
            pack_hit: |cv, m| cv.pack_ret_as::<T>(m),
            capture: |ret| ret.0.downcast_ref::<T>().map(|v| CachedValue::of(v.clone())),
        }
    }

    /// The method this rule advises.
    #[must_use]
    pub fn method(&self) -> MethodKey {
        self.method
    }

    /// The resolved op.
    #[must_use]
    pub fn op(&self) -> CacheOp {
        self.op
    }

    /// The first configured cache name (the primary target); `None` if unset.
    #[must_use]
    pub fn primary_cache_name(&self) -> Option<&'static str> {
        self.cache_names.first().copied()
    }
}

impl std::fmt::Debug for CacheRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheRule")
            .field("method", &self.method)
            .field("op", &self.op)
            .field("cache_names", &self.cache_names)
            .field("sync", &self.sync)
            .finish_non_exhaustive()
    }
}

/// A single-flight completion handle (the interceptor's per-cache in-flight map).
type Flight = Shared<BoxFuture<'static, Result<(), ()>>>;

/// The interceptor's per-(cache-name, key) single-flight coordination map — works
/// over ANY `dyn Cache` backend (the value STORE is the backend's; the in-flight
/// COORDINATION is the caching layer's, per phase3/09 §caching).
#[derive(Default)]
struct InFlight {
    flights: Mutex<HashMap<(String, CacheKey), Flight>>,
}

impl InFlight {
    fn join_or_register(
        &self,
        key: (String, CacheKey),
        make_flight: impl FnOnce() -> Flight,
    ) -> Option<Flight> {
        let mut map = self.flights.lock().expect("in-flight map");
        if let Some(existing) = map.get(&key) {
            return Some(existing.clone());
        }
        map.insert(key, make_flight());
        None
    }

    fn clear(&self, key: &(String, CacheKey)) {
        self.flights.lock().expect("in-flight map").remove(key);
    }
}

/// The around-advice [`Interceptor`](leaf_core::Interceptor) that caches an advised
/// bean's results.
///
/// Holds the resolved [`CacheManager`] (resolved by the advisor's `make_interceptor`
/// bean bridge through the container) + the per-method [`CacheRule`]s + the shared
/// single-flight map. On a call, it looks the rule up by the call's [`MethodKey`];
/// an unmatched method passes straight through.
pub struct CacheInterceptor {
    manager: Arc<dyn CacheManager>,
    rules: Vec<CacheRule>,
    in_flight: InFlight,
}

impl CacheInterceptor {
    /// Build an interceptor over a resolved manager + the per-method rules.
    #[must_use]
    pub fn new(manager: Arc<dyn CacheManager>, rules: Vec<CacheRule>) -> Self {
        CacheInterceptor { manager, rules, in_flight: InFlight::default() }
    }

    /// A single-rule interceptor for one cached `method` returning `T: Clone` (the
    /// common binding shape + the unit-test constructor).
    #[must_use]
    pub fn single<T: Clone + Send + Sync + 'static>(
        manager: Arc<dyn CacheManager>,
        method: MethodKey,
        op: CacheOp,
        meta: &CacheOpMeta,
        key_fn: CacheKeyFn,
    ) -> Self {
        Self::new(manager, vec![CacheRule::for_method::<T>(method, op, meta, key_fn)])
    }

    /// The rule for `method`, if this bean caches it.
    #[must_use]
    pub fn rule_for(&self, method: MethodKey) -> Option<&CacheRule> {
        self.rules.iter().find(|r| r.method == method)
    }

    /// Resolve the named [`Cache`] from the manager (the rule's first name).
    fn resolve_cache(&self, rule: &CacheRule) -> Result<(String, Arc<dyn Cache>), LeafError> {
        let name = rule.primary_cache_name().ok_or_else(no_cache_name)?;
        let cache = self.manager.cache(name).ok_or_else(|| no_such_cache(name))?;
        Ok((name.to_owned(), cache))
    }

    /// Build the full [`CacheKey`] for the call (the typed key fn payload prefixed
    /// by the method identity), or `None` if the key fn vetoes caching.
    fn build_key(rule: &CacheRule, call: &Call<'_>) -> Option<CacheKey> {
        let payload = (rule.key_fn)(call)?;
        Some(CacheKey::new(call.method, payload))
    }
}

// A local alias keeps the trait signature readable.
type ErasedRetAlias = leaf_core::ErasedRet;

#[leaf_macros::async_impl]
impl leaf_core::Interceptor for CacheInterceptor {
    async fn intercept(
        &self,
        call: &Call<'_>,
        next: Next<'_>,
    ) -> Result<ErasedRetAlias, AdviceError> {
        // An un-cached method on the advised bean passes straight through.
        let Some(rule) = self.rule_for(call.method) else {
            return proceed(next, call).await;
        };
        match rule.op {
            CacheOp::Cacheable => self.run_cacheable(rule, call, next).await,
            CacheOp::CachePut => self.run_cache_put(rule, call, next).await,
            CacheOp::CacheEvict => self.run_cache_evict(rule, call, next).await,
        }
    }
}

impl CacheInterceptor {
    /// `@Cacheable`: HIT short-circuits before proceeding; MISS computes + puts
    /// (single-flight when `sync`).
    async fn run_cacheable<'a>(
        &'a self,
        rule: &'a CacheRule,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> Result<ErasedRetAlias, AdviceError> {
        let Some(key) = Self::build_key(rule, call) else {
            // The condition/key fn vetoed caching: run the body uncached.
            return proceed(next, call).await;
        };
        let (name, cache) = self.resolve_cache(rule).map_err(AdviceError::AroundBody)?;

        // Look for an existing hit.
        if let Some(stored) = cache.get(&key).await.map_err(AdviceError::AroundBody)? {
            let cv = CachedValue::from_stored(&stored)
                .ok_or_else(|| AdviceError::AroundBody(foreign_value(&call.method)))?;
            let ret = (rule.pack_hit)(&cv, &call.method).map_err(AdviceError::AroundBody)?;
            return Ok(ret); // SKIP proceed — the substrate short-circuit.
        }

        if rule.sync {
            self.run_cacheable_sync(rule, call, next, name, &*cache, key).await
        } else {
            // Plain miss: proceed, capture (sync), put.
            let ret = proceed(next, call).await?;
            let captured = (rule.capture)(&ret);
            put_captured(&*cache, key, captured).await?;
            Ok(ret)
        }
    }

    /// The `sync=true` single-flight miss path: the COMPUTER runs the body + puts;
    /// concurrent WAITERS join the flight, await it, then re-read the cache.
    async fn run_cacheable_sync<'a>(
        &'a self,
        rule: &'a CacheRule,
        call: &'a Call<'a>,
        next: Next<'a>,
        name: String,
        cache: &dyn Cache,
        key: CacheKey,
    ) -> Result<ErasedRetAlias, AdviceError> {
        let flight_key = (name, key.clone());
        let (tx, rx) = futures::channel::oneshot::channel::<Result<(), ()>>();
        let rx_fut: BoxFuture<'static, Result<(), ()>> =
            Box::pin(async move { rx.await.unwrap_or(Err(())) });
        let flight: Flight = rx_fut.shared();

        match self.in_flight.join_or_register(flight_key.clone(), || flight.clone()) {
            // WAITER: await the computer, then re-read the freshly-stored hit.
            Some(existing) => {
                drop(tx);
                let completed = existing.await;
                if completed.is_ok()
                    && let Some(stored) = cache.get(&key).await.map_err(AdviceError::AroundBody)?
                    && let Some(cv) = CachedValue::from_stored(&stored)
                {
                    return (rule.pack_hit)(&cv, &call.method).map_err(AdviceError::AroundBody);
                }
                // The computer failed / value vanished: this waiter is promoted to
                // compute directly (no flight registration — the slot is vacated).
                let ret = proceed(next, call).await?;
                let captured = (rule.capture)(&ret);
                put_captured(cache, key, captured).await?;
                Ok(ret)
            }
            // COMPUTER: run + put, releasing the slot on the sync-Drop guard.
            None => {
                let mut guard = FlightGuard {
                    in_flight: &self.in_flight,
                    key: flight_key,
                    tx: Some(tx),
                    stored: false,
                };
                let ret = proceed(next, call).await?;
                let captured = (rule.capture)(&ret);
                put_captured(cache, key, captured).await?;
                guard.stored = true;
                Ok(ret)
            }
        }
    }

    /// `@CachePut`: ALWAYS proceed then put (refresh, never short-circuit).
    async fn run_cache_put<'a>(
        &'a self,
        rule: &'a CacheRule,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> Result<ErasedRetAlias, AdviceError> {
        let ret = proceed(next, call).await?;
        if let Some(key) = Self::build_key(rule, call) {
            let (_name, cache) = self.resolve_cache(rule).map_err(AdviceError::AroundBody)?;
            let captured = (rule.capture)(&ret);
            put_captured(&*cache, key, captured).await?;
        }
        Ok(ret)
    }

    /// `@CacheEvict`: evict one key (or `all_entries` → `clear`) before/after the
    /// body, then pass the result through.
    async fn run_cache_evict<'a>(
        &'a self,
        rule: &'a CacheRule,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> Result<ErasedRetAlias, AdviceError> {
        if rule.before_invocation {
            self.do_evict(rule, call).await?;
            return proceed(next, call).await;
        }
        let ret = proceed(next, call).await?;
        self.do_evict(rule, call).await?;
        Ok(ret)
    }

    /// Run the eviction (one key or the whole cache).
    async fn do_evict(&self, rule: &CacheRule, call: &Call<'_>) -> Result<(), AdviceError> {
        let (_name, cache) = self.resolve_cache(rule).map_err(AdviceError::AroundBody)?;
        if rule.all_entries {
            cache.clear().await.map_err(AdviceError::AroundBody)?;
        } else if let Some(key) = Self::build_key(rule, call) {
            cache.evict(&key).await.map_err(AdviceError::AroundBody)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for CacheInterceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheInterceptor").field("rules", &self.rules).finish_non_exhaustive()
    }
}

/// `cache.put` the already-captured cloneable [`CachedValue`] carrier (the
/// non-`Sync` [`ErasedRet`] is captured SYNCHRONOUSLY before any `.await` so it
/// never crosses one). A non-capturable return type yields `None` (left uncached).
async fn put_captured(
    cache: &dyn Cache,
    key: CacheKey,
    captured: Option<CachedValue>,
) -> Result<(), AdviceError> {
    if let Some(cv) = captured {
        cache.put(key, cv.into_stored()).await.map_err(AdviceError::AroundBody)?;
    }
    Ok(())
}

/// The computer's single-flight RAII guard: on `Drop` it completes the flight
/// (Ok if the value was stored, else Err — promote the next waiter) and clears the
/// slot. Sync `Drop`, no async finalization.
struct FlightGuard<'a> {
    in_flight: &'a InFlight,
    key: (String, CacheKey),
    stored: bool,
    tx: Option<futures::channel::oneshot::Sender<Result<(), ()>>>,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(if self.stored { Ok(()) } else { Err(()) });
        }
        self.in_flight.clear(&self.key);
    }
}

/// `next.proceed(call)` as an owned future.
fn proceed<'a>(
    mut next: Next<'a>,
    call: &'a Call<'a>,
) -> BoxFuture<'a, Result<ErasedRetAlias, AdviceError>> {
    next.proceed(call)
}

fn no_cache_name() -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "cache interceptor",
        "no cache name configured on the cache op",
    ))
}

fn no_such_cache(name: &str) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::NoSuchBean).caused_by(leaf_core::Cause::plain(
        "cache interceptor",
        format!("the cache manager has no cache named {name:?}"),
    ))
}

fn foreign_value(method: &MethodKey) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "cache hit",
        format!("a cached value without the leaf-cache carrier was found for {method:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use leaf_core::{
        AdviceChain, BeanKey, ContractId, ErasedArgs, FixedTarget, Interceptor, ResolveCtx, Tail,
    };

    use crate::manager::InMemoryCacheManager;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[derive(Debug)]
    struct Nop;
    impl leaf_core::Bean for Nop {}

    fn nop_target() -> FixedTarget {
        let bean: leaf_core::ErasedBean = std::sync::Arc::new(Nop);
        FixedTarget::new(bean)
    }

    fn method() -> MethodKey {
        MethodKey::of("svc::find")
    }

    fn bean() -> BeanKey {
        BeanKey::ByContract(ContractId::of("svc::Svc"))
    }

    static USERS: CacheOpMeta = CacheOpMeta {
        cache_names: &["users"],
        all_entries: false,
        before_invocation: false,
        sync: false,
    };
    static USERS_ALL: CacheOpMeta = CacheOpMeta {
        cache_names: &["users"],
        all_entries: true,
        before_invocation: false,
        sync: false,
    };

    fn cacheable_i(mgr: Arc<dyn CacheManager>) -> Arc<dyn Interceptor> {
        Arc::new(CacheInterceptor::single::<u64>(mgr, method(), CacheOp::Cacheable, &USERS, unit_key_fn()))
    }

    /// Drive a one-interceptor chain whose tail computes `value` (counting runs).
    fn drive(
        interceptor: Arc<dyn Interceptor>,
        runs: Arc<AtomicUsize>,
        value: u64,
    ) -> Result<u64, AdviceError> {
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let call = Call::new(method(), bean(), ErasedArgs::pack((7_i64,)), &target, &cx);
        let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
            runs.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(ErasedRet::pack(value)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        block(chain.invoke(&call, &*tail)).map(|r| r.unpack::<u64>().unwrap())
    }

    #[test]
    fn a_hit_short_circuits_the_body_on_the_second_call() {
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));

        // First call: MISS -> body runs -> value cached.
        let v1 = drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 42).unwrap();
        assert_eq!(v1, 42);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the body ran on the miss");

        // Second call (a FRESH interceptor over the SAME manager/cache): HIT.
        let v2 = drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 999).unwrap();
        assert_eq!(v2, 42, "the cached value (42) is returned, NOT the fresh 999");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "a HIT short-circuited the body");
    }

    #[test]
    fn cache_put_always_runs_the_body_and_refreshes() {
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));
        // @Cacheable seeds 42.
        drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 42).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // @CachePut: the body RUNS (no short-circuit) and refreshes the entry to 7.
        let put: Arc<dyn Interceptor> = Arc::new(CacheInterceptor::single::<u64>(
            mgr.clone(),
            method(),
            CacheOp::CachePut,
            &USERS,
            unit_key_fn(),
        ));
        let v = drive(put, Arc::clone(&runs), 7).unwrap();
        assert_eq!(v, 7, "cache_put returns the fresh value");
        assert_eq!(runs.load(Ordering::SeqCst), 2, "cache_put ALWAYS runs the body");

        // A subsequent @Cacheable now HITs the refreshed 7.
        let v2 = drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 0).unwrap();
        assert_eq!(v2, 7, "cache_put refreshed the cached value");
        assert_eq!(runs.load(Ordering::SeqCst), 2, "the refreshed value is a hit");
    }

    #[test]
    fn cache_evict_invalidates_the_entry() {
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));
        // Seed 42.
        drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 42).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        // @CacheEvict (after invocation) removes the entry.
        let evict: Arc<dyn Interceptor> = Arc::new(CacheInterceptor::single::<u64>(
            mgr.clone(),
            method(),
            CacheOp::CacheEvict,
            &USERS,
            unit_key_fn(),
        ));
        drive(evict, Arc::clone(&runs), 0).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2, "the evicting method's body ran");

        // The next @Cacheable MISSes (the entry was invalidated) -> body runs again.
        let v = drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 100).unwrap();
        assert_eq!(v, 100, "after eviction the body recomputes (100), not the old 42");
        assert_eq!(runs.load(Ordering::SeqCst), 3, "eviction forced a recompute");
    }

    #[test]
    fn cache_evict_all_clears_the_whole_cache() {
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));
        drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 42).unwrap();

        let evict: Arc<dyn Interceptor> = Arc::new(CacheInterceptor::single::<u64>(
            mgr.clone(),
            method(),
            CacheOp::CacheEvict,
            &USERS_ALL,
            unit_key_fn(),
        ));
        drive(evict, Arc::clone(&runs), 0).unwrap();

        drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 5).unwrap();
        // seed(1) + evict-body(1) + recompute(1) = 3.
        assert_eq!(runs.load(Ordering::SeqCst), 3, "evict-all cleared, forcing a recompute");
    }

    #[test]
    fn an_uncached_method_passes_straight_through() {
        // The interceptor caches `svc::find`; a call to a DIFFERENT method on the
        // same bean is a pass-through (the body always runs, nothing cached).
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let i: Arc<dyn Interceptor> = cacheable_i(mgr);
        let chain = AdviceChain::new(Box::new([i]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        // A call to `svc::other` (NOT the cached `svc::find`).
        let call = Call::new(MethodKey::of("svc::other"), bean(), ErasedArgs::none(), &target, &cx);
        for _ in 0..2 {
            let runs = Arc::clone(&runs);
            let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
                runs.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Ok(ErasedRet::pack(1_u64)) })
                    as BoxFuture<'_, Result<ErasedRet, AdviceError>>
            });
            block(chain.invoke(&call, &*tail)).unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2, "an uncached method is never short-circuited");
    }

    #[test]
    fn a_wrong_return_type_hit_read_is_a_loud_error() {
        // Seed as u64, then read with a rule expecting String — a loud error.
        let mgr: Arc<dyn CacheManager> = Arc::new(InMemoryCacheManager::new());
        let runs = Arc::new(AtomicUsize::new(0));
        drive(cacheable_i(mgr.clone()), Arc::clone(&runs), 42).unwrap();

        let bad: Arc<dyn Interceptor> = Arc::new(CacheInterceptor::single::<String>(
            mgr.clone(),
            method(),
            CacheOp::Cacheable,
            &USERS,
            unit_key_fn(),
        ));
        let chain = AdviceChain::new(Box::new([bad]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let call = Call::new(method(), bean(), ErasedArgs::pack((7_i64,)), &target, &cx);
        let tail: Box<Tail> = Box::new(|_call: &Call<'_>| {
            Box::pin(async move { Ok(ErasedRet::pack("x".to_string())) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        assert!(block(chain.invoke(&call, &*tail)).is_err(), "a wrong-type hit read is loud");
    }
}
