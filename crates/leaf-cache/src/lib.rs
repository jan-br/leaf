//! `leaf-cache` — the caching cross-cutting concern crate (declarative-advice,
//! phase3/09): it SHIPS the runtime [`CacheInterceptor`] (around advice — a cache
//! HIT short-circuits BEFORE proceeding, a MISS computes then puts) AND the
//! Infrastructure cache advisor that auto-wires through the run pipeline.
//!
//! Caching sits at `CACHE_ORDER = 400` — OUTSIDE (before) `TX_ORDER = 500` so a
//! hit avoids opening a transaction (the canonical cache-outside-tx correctness
//! invariant, data-driven and inspectable, `CACHE_ORDER < TX_ORDER`).
//!
//! The pieces (all resting on the leaf-core ABI — nothing minted twice):
//!
//! - **[`CacheInterceptor`]** — the `Role::Infrastructure`, `CACHE_ORDER`
//!   around-advice. It builds the [`CacheKey`](leaf_core::CacheKey) from the call
//!   args (via a typed key fn), then per the resolved
//!   [`CacheOpMeta`](leaf_core::CacheOpMeta) op: `@Cacheable` `cache.get` → on HIT
//!   re-pack the typed return and SKIP `next.proceed()` (the substrate
//!   short-circuit), on MISS proceed then `cache.put`; `@CachePut` always proceeds
//!   then puts; `@CacheEvict` evicts (one key or all) before/after the body.
//! - **value** ([`CachedValue`]) — the cloneable, typed-rebuildable cache-value
//!   carrier that rides leaf-core's non-cloneable [`StoredValue`](leaf_core::StoredValue)
//!   transport (so a hit re-yields a fresh typed value through the erased boundary).
//! - **manager** ([`InMemoryCache`] / [`InMemoryCacheManager`]) — the in-memory
//!   default `Cache`/`CacheManager` (the `Arc<dyn CacheManager>` bean) keyed by the
//!   typed `(MethodKey, CacheKey)` store, plus the single-flight in-flight map
//!   (`sync=true`: concurrent identical keys await one computation, cancel-safe via
//!   sync `Drop`). A real backend (Caffeine/Redis) is a separate integration crate
//!   contributing an `Arc<dyn CacheManager>` bean.
//! - **advisor** ([`advisor`]) — the Infrastructure [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow)
//!   builders ([`cache_advisor_pairing`]/[`build_cache_interceptor`]) +
//!   [`CachePointcut`] that auto-wire the advisor through `Application::run`'s
//!   `ADVISOR_PAIRINGS` collection.
//! - **autoconfig** ([`autoconfig`]) — the DEFAULT cache-manager `#[auto_config]`
//!   ([`CacheAutoConfig`]): an `AUTO_CONFIGS` row at `FALLBACK` contributing the in-memory
//!   [`InMemoryCacheManager`] as the `"cacheManager"` bean, guarded by
//!   `OnMissingBean(InMemoryCacheManager)`, so `#[cacheable(manager = InMemoryCacheManager)]`
//!   resolves with no hand-written wrapper bean (the in-memory peer of the Redis-backed
//!   `leaf_redis::autoconfig::RedisAutoConfig`).
//!
//! ## Deferred (honest NOTEs)
//!
//! - The `#[cacheable]` macro emits the per-method
//!   [`CacheOpMeta`](leaf_core::CacheOpMeta) const + the `ADVISORS` identity row,
//!   but (like leaf-tx's `#[transactional]`) it does NOT yet emit the
//!   `ADVISOR_PAIRINGS` auto-wire row or the typed key fn — so the auto-wire row
//!   (built by [`cache_advisor_pairing`]) is supplied at the binding site, which
//!   passes the [`CacheOpMeta`](leaf_core::CacheOpMeta) + a typed key fn + the return type `T`. Until the
//!   macro threads a per-method key expression, the default key fn hashes the whole
//!   erased arg tuple's `TypeId` + a caller-supplied discriminator.
//! - `CacheErrorHandler` policy (swallow-and-fall-through vs fail-fast on a backend
//!   I/O error) is fixed to fail-fast here; the swallow knob is a NOTE for a later
//!   unit.
//! - TTL / LRU / size eviction are a backend's concern; the in-memory default never
//!   evicts on its own (only explicit `@CacheEvict` / `clear`).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod advisor;
pub mod autoconfig;
pub mod interceptor;
pub mod manager;
pub mod value;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag in
// the link-collected `SOURCES` slice so a binary that lists leaf-cache in its
// ExpectedManifest (the `web` capability bundle) can tell "linked-but-zero-rows"
// from "never-linked" — a loud `SourceVanished` rather than a silent missing
// concern. The package name (dashes) is the string the ExpectedManifest joins on.
leaf_core::declare_source!("leaf-cache");

pub use advisor::{
    build_cache_interceptor, build_cache_interceptor_view, cache_advisor_contract,
    cache_advisor_pairing, cache_order_key, enable_caching, resolve_manager, resolve_manager_view,
    CachePointcut,
};
pub use autoconfig::{
    cache_manager_descriptor, CacheAutoConfig, CACHE_AUTO_CONFIG_GUARD, CACHE_MANAGER_BEAN,
    CACHE_MANAGER_CONTRACT, CACHE_MANAGER_SEED,
};
pub use interceptor::{unit_key_fn, CacheInterceptor, CacheKeyFn, CacheOp, CacheRule};
pub use manager::{FlightError, InMemoryCache, InMemoryCacheManager};
pub use value::CachedValue;
