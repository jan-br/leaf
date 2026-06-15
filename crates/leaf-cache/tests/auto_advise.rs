//! THE leaf-cache AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[advisable]` service with `#[cacheable(key="#0")]` / `#[cache_evict]` methods is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure cache advisor leaf-cache ships AUTO-WIRES from the NATURAL
//! annotation: a HIT short-circuits the method body (the body runs ONCE PER KEY), a
//! DIFFERENT arg is a different cache entry (per-arg keying), and `#[cache_evict]`
//! invalidates so the next call recomputes.
//!
//! What is user code (the NATURAL declarative annotations — NO `#[aspect]`, NO
//! hand-written `ADVISOR_PAIRINGS` row, NO marker-fn `CacheOpMeta`):
//! - a `register_component!` `InMemoryCacheManagerBean` — a `CacheManager` bean
//!   wrapping leaf-cache's [`InMemoryCacheManager`] (a real backend would be its own
//!   bean; a local newtype is needed only because the orphan rule forbids
//!   `#[component]`-ing the foreign type);
//! - a `#[advisable]` `UserService` whose `find(id: u64)` carries
//!   `#[cacheable("users", key = "#0", manager = InMemoryCacheManagerBean)]` (the cached
//!   read, keyed PER-ARG) and whose `evict` carries `#[cache_evict("users",
//!   all_entries, manager = InMemoryCacheManagerBean)]` (clear-all) — the ADVISED bean.
//!
//! Each natural annotation on a `#[advisable]`-impl method is what the impl-block macro
//! lowers to the per-method `CacheOpMeta` const plus the `ADVISOR_PAIRINGS` row (the
//! cache advisor keyed by the bean's `TypeId`, binding the manager, the method's
//! return-`T`, and the `key = "#0"` typed arg-key fn) — so `Application::run`
//! AUTO-COLLECTS the cache advisor with NO `.with_advisors`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{Cache, CacheManager, ContractId, ErasedArgs, MethodKey};
// `#[cacheable]` / `#[cache_evict]` are NOT imported: they are per-method MARKERS the
// `#[advisable]` impl macro STRIPS + lowers (the impl-block macro owns the rows), so —
// exactly like `#[bean]` inside `#[configuration] impl` — they need no import.
use leaf_cache::InMemoryCacheManager;
use leaf_macros::{advisable, register_component};

// ───────────────────────── the cache manager bean ────────────────────────────

/// A [`CacheManager`] bean: a thin local newtype delegating to leaf-cache's
/// [`InMemoryCacheManager`] (the orphan rule forbids `#[component]`-ing the foreign
/// type directly). Registered via `register_component!` (constructed via `::new()`,
/// no field-injection).
#[derive(Debug)]
struct InMemoryCacheManagerBean {
    inner: InMemoryCacheManager,
}
register_component!(InMemoryCacheManagerBean);

impl InMemoryCacheManagerBean {
    fn new() -> Self {
        InMemoryCacheManagerBean { inner: InMemoryCacheManager::new() }
    }
}

impl CacheManager for InMemoryCacheManagerBean {
    fn cache(&self, name: &str) -> Option<Arc<dyn Cache>> {
        self.inner.cache(name)
    }
}

// ───────────────────────── the advised service bean ──────────────────────────

/// A `@Component` service whose `find(id)` method is CACHEABLE PER-ARG (advised by the
/// cache advisor the `#[cacheable(key="#0")]` annotation auto-wires). It counts its body
/// invocations so the test can assert a HIT short-circuits the body for a SEEN key while
/// a NEW key recomputes. `evict` is a cache-evicting method (clear the whole `users`
/// cache).
#[derive(Debug)]
struct UserService {
    runs: Arc<AtomicUsize>,
}
register_component!(UserService);

#[advisable]
impl UserService {
    fn new() -> Self {
        UserService { runs: Arc::new(AtomicUsize::new(0)) }
    }

    /// The CACHED, PER-ARG read: `key = "#0"` keys on the `id` argument, so a HIT for a
    /// SEEN id short-circuits the body (the counter does not advance) while a NEW id
    /// recomputes. Returns a value derived from the run count + the id so a recompute is
    /// observably distinct from a hit.
    #[cacheable("users", key = "#0", manager = InMemoryCacheManagerBean)]
    fn find(&self, id: u64) -> i64 {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        1000 + (n as i64) * 100 + id as i64
    }

    /// The EVICTING write: clears the whole `users` cache (the test asserts the next
    /// `find` for any id recomputes).
    #[cache_evict("users", all_entries, manager = InMemoryCacheManagerBean)]
    fn evict(&self) -> i64 {
        0
    }

    /// The body-run count (so the test can assert the HIT short-circuit).
    fn run_count(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cacheable_bean_auto_advises_through_run_and_caches_per_arg() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::UserService"));

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but the natural annotations.
    let running = Application::new()
        .with_name("cache-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // The cache advisor rows auto-collected from the `#[cacheable]`/`#[cache_evict]`
    // annotations: each is a per-method-unique row whose contract is the cache family
    // base @ the module-qualified `Bean::method` (so two cache methods do not collide).
    let find_contract = ContractId::of(&format!(
        "leaf::cache::CacheAdvisor@{module}::UserService::find"
    ));
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == find_contract),
        "the cache advisor row for `find` auto-collected from the #[cacheable] annotation"
    );

    // The service is AUTOMATICALLY advised (the proxy installed at R4).
    let svc_id = running
        .context()
        .engine()
        .registry()
        .by_contract(service_contract)
        .expect("UserService in registry");
    assert!(
        running.is_advised(svc_id),
        "the #[cacheable] #[advisable] bean is AUTOMATICALLY advised by the auto-wired cache advisor"
    );

    let svc = running.context().get::<UserService>().await.expect("the service resolves");
    assert_eq!(svc.run_count(), 0, "no body run yet");

    // Helper: invoke `find(id)` through the auto-installed cache chain.
    let find = |id: u64| {
        running.invoke_advised(svc_id, MethodKey::of("UserService::find"), ErasedArgs::pack((id,)))
    };

    // ── find(7): MISS — the body runs, the value cached under key #7 ──
    let r1 = find(7).await.expect("the advised call routes through the auto-installed cache chain");
    assert_eq!(r1.unpack::<i64>().expect("i64"), 1007, "the real method ran (1000 + run#0*100 + 7)");
    assert_eq!(svc.run_count(), 1, "the body ran on the MISS for id 7");

    // ── find(7) again: HIT — the body does NOT run; the cached value returns ──
    let r2 = find(7).await.expect("a cached call");
    assert_eq!(r2.unpack::<i64>().expect("i64"), 1007, "the CACHED value (1007) for id 7 returns");
    assert_eq!(svc.run_count(), 1, "a HIT for the SEEN key short-circuited the body");

    // ── find(9): a DIFFERENT arg is a DIFFERENT cache entry — the body recomputes ──
    let r3 = find(9).await.expect("a fresh-key call");
    assert_eq!(r3.unpack::<i64>().expect("i64"), 1109, "a NEW key recomputed (1000 + run#1*100 + 9)");
    assert_eq!(svc.run_count(), 2, "per-arg keying: a different arg is a different entry");

    // ── find(9) again: HIT for the now-seen key #9 ──
    let r4 = find(9).await.expect("a cached call for id 9");
    assert_eq!(r4.unpack::<i64>().expect("i64"), 1109, "the CACHED value (1109) for id 9 returns");
    assert_eq!(svc.run_count(), 2, "id 9 is now a HIT — the body ran exactly once per key");

    // ── cache_evict invalidates the whole cache: the next find recomputes ──
    running
        .invoke_advised(svc_id, MethodKey::of("UserService::evict"), ErasedArgs::pack(()))
        .await
        .expect("the evicting call routes through the evict advisor");
    let r5 = find(7).await.expect("the post-evict find for id 7");
    assert_eq!(r5.unpack::<i64>().expect("i64"), 1207, "evict cleared id 7 → recompute (run#2*100 + 7)");
    assert_eq!(svc.run_count(), 3, "cache_evict INVALIDATED the cache — find recomputed");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}
