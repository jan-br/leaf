//! THE leaf-cache AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[component]` + `#[advisable]` service with a `#[cacheable]`-described method is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure cache advisor leaf-cache ships AUTO-WIRES: a HIT short-circuits
//! the method body on the 2nd call (the body runs ONCE), and `cache_evict`
//! invalidates so the 3rd call recomputes.
//!
//! What is user code (annotations + one slice row):
//! - a `#[cacheable("users")]` free fn — its emitted `CacheOpMeta` const
//!   (`__leaf_cache_users_cache_spec_invoke`) is the per-method metadata the cache
//!   advisor reads (the macro emits the metadata; the auto-wire row + key fn are the
//!   binding site's, exactly like leaf-tx's `#[transactional]` staging);
//! - a `#[component]` `InMemoryCacheManagerBean` — a `CacheManager` bean wrapping
//!   leaf-cache's [`InMemoryCacheManager`] (a real backend would be its own bean; a
//!   local newtype is needed only because the orphan rule forbids `#[component]`-ing
//!   the foreign type);
//! - a `#[component]` + `#[advisable]` `UserService` whose `find` method counts its
//!   invocations + returns an `i64` — the ADVISED, cached bean;
//! - TWO const `ADVISOR_PAIRINGS` rows the binary submits (a `@Cacheable` advisor on
//!   `find` and a `@CacheEvict` advisor on `evict`), exactly like `#[aspect]` emits —
//!   so `Application::run` AUTO-COLLECTS the cache advisor with NO hand-assembled
//!   `.with_advisors`.
//!
//! Everything else (the proxy plan, the chain install at R4, the `make_interceptor`
//! resolving the manager through the container) is the run pipeline's auto-wiring.

use std::any::TypeId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{Cache, CacheManager, ContractId, ErasedArgs, Interceptor, LeafError, MethodKey};
use leaf_macros::{advisable, cacheable, register_component};
use leaf_cache::{
    cache_advisor_contract, resolve_manager, unit_key_fn, CacheInterceptor, CacheOp, CachePointcut,
    CacheRule, InMemoryCacheManager,
};

// ─────────────────── the #[cacheable] metadata source ────────────────────────

// The `#[cacheable]` macro emits a PUBLIC `CacheOpMeta` const the cache advisor
// reads. Applied to a marker fn here (the macro is free-fn-shaped); the emitted
// const `__leaf_cache_users_cache_spec_invoke` IS the per-method metadata the
// @Cacheable rule below points at — the proof the macro emits the metadata the
// advisor consumes.
#[cacheable("users")]
#[allow(dead_code)] // the macro-emitted CacheOpMeta const is what is consumed
fn users_cache_spec() {}

// The @CacheEvict op metadata for the write method: clear the whole `users` cache
// on a write (the real `all_entries` semantic — invalidates regardless of which
// method/key populated the entry). Emits `__leaf_cache_users_evict_spec_invoke`.
#[cacheable("users", all_entries = true)]
#[allow(dead_code)]
fn users_evict_spec() {}

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

/// A `@Component` service whose `find` method is CACHEABLE (advised by the cache
/// advisor). It counts its body invocations so the test can assert a HIT
/// short-circuits the body on the 2nd call. `evict` is a cache-evicting method.
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

    /// The CACHED read (NO args — see the key-fn NOTE below): increments the run
    /// counter and returns a value derived from it (so a recompute is observably
    /// different from a hit). A HIT returns the cached value WITHOUT running this
    /// body — `run_count` does not advance.
    fn find(&self) -> i64 {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        100 + n as i64
    }

    /// The EVICTING write: clears the whole `users` cache (the test asserts the
    /// next `find` recomputes).
    fn evict(&self) -> i64 {
        0
    }

    /// The body-run count (so the test can assert the HIT short-circuit).
    fn run_count(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

// ───────────────────── the AUTO-WIRED cache advisor rows ─────────────────────

// The cache advisor matches UserService by its concrete TypeId (the recursion-safe
// pointcut — it never advises the manager bean itself).
static ADVISED_TYPES: [TypeId; 1] = [const { TypeId::of::<UserService>() }];
static CACHE_POINTCUT: CachePointcut = CachePointcut::new(&ADVISED_TYPES, &[]);

// THE auto-wire cache row: ONE const `AdvisorPairingRow` in `ADVISOR_PAIRINGS` (the
// same channel `Application::run` collects `#[aspect]` rows from), binding the
// manager bean + per-method rules (`find` = @Cacheable, `evict` = @CacheEvict over
// the SAME `users` cache). No `.with_advisors` in the run call. The make_interceptor
// is a non-capturing closure literal (const-promoted to the bare fn-pointer exactly
// like the `#[aspect]` codegen emits).
#[leaf_core::linkme::distributed_slice(leaf_core::ADVISOR_PAIRINGS)]
#[linkme(crate = leaf_core::linkme)]
static CACHE_ADVISOR_ROW: leaf_core::AdvisorPairingRow = leaf_core::AdvisorPairingRow {
    contract: ContractId::of("leaf::cache::CacheAdvisor"),
    order: leaf_core::OrderKey {
        value: leaf_core::CACHE_ORDER,
        source: leaf_core::OrderSource::Interface,
    },
    role: leaf_core::Role::Infrastructure,
    pointcut: &CACHE_POINTCUT,
    make_interceptor: |c| Box::pin(build_user_cache(c)),
};

// The bean bridge: resolve the manager + build a CacheInterceptor with BOTH the
// @Cacheable `find` rule (return type i64, reading the #[cacheable]-emitted
// CacheOpMeta) and the @CacheEvict `evict` rule (clear-all on the SAME `users`
// cache). Both use the unit key fn — keying on a single per-method entry.
//
// NOTE (honest, the design's deferred ErasedArgs ABI risk, doc line ~172): keying
// the cache on a method ARGUMENT (`#[cacheable(key="#id")]`) is NOT exercised here
// because the auto-proxy invocation (`InstalledProxies::invoke`) leaves `Call.args`
// EMPTY and rides the real args through a take-once cell the tail thunk consumes —
// so an interceptor cannot read the args off `Call.args` to build a per-arg key
// through the current substrate. Arg-aware keying works when args ARE on `Call.args`
// (the leaf-cache unit tests pass them directly); threading args to the interceptor
// through the auto-proxy is a substrate follow-up. So the cached `find` is a no-arg
// method (one logical entry), which is the correct + faithful headline proof.
async fn build_user_cache(
    c: &dyn leaf_core::Container,
) -> Result<std::sync::Arc<dyn Interceptor>, LeafError> {
    let manager = resolve_manager::<InMemoryCacheManagerBean>(c).await?;
    let rules = vec![
        CacheRule::for_method::<i64>(
            MethodKey::of("UserService::find"),
            CacheOp::Cacheable,
            &__leaf_cache_users_cache_spec_invoke,
            unit_key_fn(),
        ),
        CacheRule::for_method::<i64>(
            MethodKey::of("UserService::evict"),
            CacheOp::CacheEvict,
            &__leaf_cache_users_evict_spec_invoke,
            unit_key_fn(),
        ),
    ];
    Ok(std::sync::Arc::new(CacheInterceptor::new(manager, rules)) as std::sync::Arc<dyn Interceptor>)
}

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cacheable_bean_auto_advises_through_run_and_a_hit_short_circuits() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::UserService"));

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but annotations + the one slice row.
    let running = Application::new()
        .with_name("cache-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // The cache advisor row auto-collected (the headline: it is in ADVISOR_PAIRINGS).
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == cache_advisor_contract()),
        "the cache Infrastructure advisor row auto-collected from ADVISOR_PAIRINGS"
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
        "the #[advisable] bean is AUTOMATICALLY advised by the auto-collected cache advisor"
    );

    let svc = running.context().get::<UserService>().await.expect("the service resolves");
    assert_eq!(svc.run_count(), 0, "no body run yet");

    // ── 1st call: MISS — the body runs, the value is cached ──
    let r1 = running
        .invoke_advised(svc_id, MethodKey::of("UserService::find"), ErasedArgs::pack(()))
        .await
        .expect("the advised call routes through the auto-installed cache chain");
    assert_eq!(r1.unpack::<i64>().expect("i64 return"), 100, "the real method ran (100 + run #0)");
    assert_eq!(svc.run_count(), 1, "the body ran on the MISS");

    // ── 2nd call: HIT — the body does NOT run; the cached value returns ──
    let r2 = running
        .invoke_advised(svc_id, MethodKey::of("UserService::find"), ErasedArgs::pack(()))
        .await
        .expect("the advised call routes through the cache chain");
    assert_eq!(r2.unpack::<i64>().expect("i64 return"), 100, "the CACHED value (100) is returned");
    assert_eq!(svc.run_count(), 1, "a HIT SHORT-CIRCUITED the body — it ran exactly ONCE");

    // ── 3rd call: still a HIT (the body STILL ran only once) ──
    let r3 = running
        .invoke_advised(svc_id, MethodKey::of("UserService::find"), ErasedArgs::pack(()))
        .await
        .expect("a third cached call");
    assert_eq!(r3.unpack::<i64>().expect("i64 return"), 100, "still the cached 100");
    assert_eq!(svc.run_count(), 1, "repeated hits never re-run the body");

    // ── cache_evict invalidates: the next find recomputes ──
    running
        .invoke_advised(svc_id, MethodKey::of("UserService::evict"), ErasedArgs::pack(()))
        .await
        .expect("the evicting call routes through the evict advisor");
    let r4 = running
        .invoke_advised(svc_id, MethodKey::of("UserService::find"), ErasedArgs::pack(()))
        .await
        .expect("the post-evict find");
    assert_eq!(r4.unpack::<i64>().expect("i64 return"), 101, "the recomputed value (100 + run #1)");
    assert_eq!(svc.run_count(), 2, "cache_evict INVALIDATED the entry — find recomputed");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}
