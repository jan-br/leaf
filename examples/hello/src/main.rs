//! THE COMPLETION PROOF — a real leaf application that depends on the `leaf`
//! UMBRELLA ALONE (phase3/03 TOPOLOGY "Starters & BOM": "the umbrella is the blessed
//! path"). EXACTLY ONE leaf dependency, `use leaf::prelude::*;`, and annotations — the
//! whole framework: dependency injection, `@ConfigurationProperties` binding,
//! declarative transactions + caching auto-advised through the auto-proxy, a startup
//! runner, the Redis auto-config force-linked via the umbrella's `redis` capability
//! feature (participating + backing off to the in-memory cache), and a clean shutdown.
//!
//! Run it:   `cargo run -p hello`        (the `#[leaf::main]` binary entry)
//! Test it:  `cargo test -p hello`       (the integration test below drives `run`)

// ── the umbrella-only facade-path aliases ──
//
// The annotation macros (`#[component]`/`#[config_properties]`/`#[runner]`/
// `#[transactional]`/`#[cacheable]`/…) emit ABSOLUTE crate-root paths
// (`::leaf_core::…`, `::leaf_cache::…`, `::leaf_tx::…`) — the single-kernel-invariant
// thin-macro rule. An absolute `::crate` path resolves against the EXTERN PRELUDE
// (direct Cargo deps), so an umbrella-only app aliases the ONE `leaf` dependency under
// each macro-referenced crate name. These are SOURCE aliases of the single `leaf`
// dependency (NOT new Cargo deps — `Cargo.toml` still names only `leaf`); the umbrella
// re-exports each crate's surface at its root so `::leaf_core::Descriptor` →
// `leaf::Descriptor`, etc. This is the umbrella-only maximal-magic DX.
extern crate leaf as leaf_core;
extern crate leaf as leaf_cache;
extern crate leaf as leaf_tx;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use leaf::prelude::*;

// ───────────────────────────── configuration ────────────────────────────────

/// `@ConfigurationProperties(prefix = "app")` — bound from `app.*` (CLI args / env /
/// config files) by the run pipeline's C2 pre-materialize pass, purely from the
/// macro-emitted bind thunk (no hand-written seed / descriptor).
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    /// `app.name` — the application's display name.
    name: String,
    /// `app.workers` — the configured worker count.
    workers: u16,
}

// ──────────────────────── the in-memory cache backend ────────────────────────

/// A [`CacheManager`] bean wrapping the framework-shipped in-memory cache
/// (`leaf_cache::InMemoryCacheManager`, reached through the umbrella's facade re-export
/// — the orphan rule forbids `#[component]`-ing the foreign type directly). It is a
/// `@Component` of a `CacheManager` (`CandidateRole::NORMAL`), so the force-linked
/// Redis `RedisCacheManager` (`CandidateRole::FALLBACK`, gated on `leaf.redis.enabled`)
/// transparently LOSES to it — the soft-override contract: the Redis auto-config
/// COEXISTS WITH this in-memory cache and backs off to it. Constructed via `::new()`
/// (`register_component!`, no injected collaborators).
#[derive(Debug)]
struct CacheManagerBean {
    inner: leaf_cache::InMemoryCacheManager,
}
register_component!(CacheManagerBean);

impl CacheManagerBean {
    fn new() -> Self {
        CacheManagerBean { inner: leaf_cache::InMemoryCacheManager::new() }
    }
}

impl CacheManager for CacheManagerBean {
    fn cache(&self, name: &str) -> Option<Arc<dyn leaf::core::Cache>> {
        self.inner.cache(name)
    }
}

// ──────────────────────── the transaction manager ────────────────────────────

/// A [`TransactionManager`] bean wrapping the framework-shipped no-op manager
/// (`leaf_tx::InMemoryTransactionManager`) — what the `#[transactional]` method's
/// auto-installed `TransactionInterceptor` demarcates against (begin/commit/rollback
/// bookkeeping; a real datastore manager is its own ordinary bean). It exposes the
/// counts so the test can assert the interceptor demarcated. Constructed via `::new()`
/// (`register_component!`).
#[derive(Debug)]
struct TxManagerBean {
    inner: leaf_tx::InMemoryTransactionManager,
}
register_component!(TxManagerBean);

impl TxManagerBean {
    fn new() -> Self {
        TxManagerBean { inner: leaf_tx::InMemoryTransactionManager::new() }
    }
    // The demarcation counters the test asserts against (the begin/commit/rollback the
    // auto-installed interceptor drove). Test-only — the running binary never reads them.
    #[cfg(test)]
    fn begins(&self) -> usize {
        self.inner.begins()
    }
    #[cfg(test)]
    fn commits(&self) -> usize {
        self.inner.commits()
    }
    #[cfg(test)]
    fn rollbacks(&self) -> usize {
        self.inner.rollbacks()
    }
}

impl TransactionManager for TxManagerBean {
    fn begin<'a>(
        &'a self,
        def: &'a leaf::core::TxDefinition,
        cx: &'a leaf::core::ResolveCtx<'a>,
    ) -> leaf::core::BoxFuture<'a, Result<leaf::core::TxState, LeafError>> {
        self.inner.begin(def, cx)
    }

    fn commit(
        &self,
        st: leaf::core::TxState,
    ) -> leaf::core::BoxFuture<'_, Result<(), LeafError>> {
        self.inner.commit(st)
    }

    fn rollback(
        &self,
        st: leaf::core::TxState,
    ) -> leaf::core::BoxFuture<'_, Result<(), LeafError>> {
        self.inner.rollback(st)
    }

    fn synchronizations<'a>(
        &'a self,
        st: &'a leaf::core::TxState,
    ) -> &'a leaf::core::TxSyncRegistry {
        self.inner.synchronizations(st)
    }
}

// ───────────────────────────── the domain beans ──────────────────────────────

/// A `@Component` repository (the dependency target) — constructed via `::new()`,
/// no injected collaborators (`register_component!`).
#[derive(Debug)]
struct Repo {
    label: &'static str,
}
register_component!(Repo);

impl Repo {
    fn new() -> Self {
        Repo { label: "orders" }
    }
}

/// The body-run count of `OrderService::place_order` — a process-global so a cache
/// HIT (which short-circuits the method body) is observable WITHOUT making the counter
/// an injected `#[component]` field (every `#[component]` field is a constructor
/// injection point, so service state lives outside the struct).
static PLACE_ORDER_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A `@Component` service that INJECTS [`Repo`] (constructor injection over the
/// `Ref<Repo>` field — the live dependency-graph edge) and whose `place_order` method
/// is BOTH `#[transactional]` (commit on `Ok`, rollback on `Err`) AND
/// `#[cacheable(key = "#0")]` (a hit for a seen id short-circuits the body) — the two
/// natural declarative concerns, auto-advised together through the auto-proxy.
#[component]
#[derive(Debug)]
struct OrderService {
    repo: Ref<Repo>,
}

#[advisable]
impl OrderService {
    /// The `#[component]` provider calls this with the resolved deps.
    fn new(repo: Ref<Repo>) -> Self {
        OrderService { repo }
    }

    /// Place an order for `id`. `#[transactional]` demarcates a tx (commit on `Ok`);
    /// `#[cacheable(key = "#0")]` caches per-`id` so a repeat call short-circuits the
    /// body (the run counter does not advance). Returns `Result<i64, LeafError>` so the
    /// tx return-classifier can read the business outcome.
    #[transactional(manager = TxManagerBean)]
    #[cacheable("orders", key = "#0", manager = CacheManagerBean)]
    fn place_order(&self, id: i64) -> Result<i64, LeafError> {
        let n = PLACE_ORDER_RUNS.fetch_add(1, Ordering::SeqCst);
        let _ = self.repo.label;
        Ok(1000 + (n as i64) * 100 + id)
    }
}

// ──────────────────────────────── the runner ─────────────────────────────────

/// A `#[runner]` that runs ONCE at startup, in the readiness-gate window (after the
/// context is Ready, before traffic is accepted) — the migration/warmup hook. It sets
/// a process-global flag so the test can assert it fired.
static RUNNER_FIRED: AtomicUsize = AtomicUsize::new(0);

#[runner]
struct StartupRunner;

impl StartupRunner {
    fn new() -> Self {
        StartupRunner
    }
}

impl Runner for StartupRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf::core::ApplicationArguments,
    ) -> leaf::core::BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            RUNNER_FIRED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

// ──────────────────────────────── the entry ──────────────────────────────────

// The Layer-0 anti-DCE force-link shim from the BINARY crate (belt-and-suspenders:
// the umbrella's `redis` feature already force-links leaf-redis, but invoking this in
// `main`'s module makes the binary itself the originating link unit). With the `redis`
// feature on it path-references the Redis integration crate so its `AUTO_CONFIGS`
// rows survive link-time DCE.
leaf::force_link!();

/// `#[leaf::main]` — the umbrella-only entry. It builds the tokio runtime the umbrella
/// owns, bootstraps + runs the application to Ready (the graph wires, `AppProps`
/// binds, the `#[transactional]`+`#[cacheable]` method is auto-advised, the
/// `#[runner]` fires, the Redis auto-config participates + backs off), hands us the
/// live app, then drains the clean shutdown.
#[leaf::main]
async fn main(app: &leaf::boot::RunningApp) -> Result<(), leaf::LeafError> {
    let props = app.context().get::<AppProps>().await?;
    println!(
        "hello from leaf: app.name={:?} app.workers={} (runner fired {} time(s))",
        props.name,
        props.workers,
        RUNNER_FIRED.load(Ordering::SeqCst),
    );
    Ok(())
}

// ─────────────────────────────────── tests ───────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use leaf::core::{ErasedArgs, MethodKey, ReadinessState, RunState};

    /// THE END-TO-END PROOF: drive the full run pipeline from the umbrella alone and
    /// assert every claim — the graph wires, config binds, the transactional+cacheable
    /// method is AUTO-ADVISED (a cache hit short-circuits the body; the tx commits),
    /// the runner fired, and shutdown drains cleanly. The tokio runtime is built via
    /// the umbrella's re-export (`leaf::tokio`) — the umbrella provides the runtime, so
    /// the test names no `tokio` dependency.
    #[test]
    fn the_umbrella_only_app_wires_advises_runs_and_shuts_down() {
        let runtime = leaf::tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a tokio runtime");
        runtime.block_on(drive_the_app());
    }

    async fn drive_the_app() {
        RUNNER_FIRED.store(0, Ordering::SeqCst);
        PLACE_ORDER_RUNS.store(0, Ordering::SeqCst);

        // Bootstrap + run with NOTHING but the annotations (the blessed path). The
        // per-bean wiring + advisors + runners + auto-configs auto-collect inside `run`.
        let running = leaf::bootstrap("hello")
            .run(
                leaf::RunInputs::new()
                    .with_args(["--app.name=Orders", "--app.workers=4"])
                    .into(),
                leaf::boot::RunOverlay::none(),
            )
            .await
            .expect("the umbrella-only app runs to Ready");

        // (1) the GRAPH wired: the Service injected the Repo.
        let service =
            running.context().get::<OrderService>().await.expect("OrderService resolves");
        assert_eq!(service.repo.label, "orders", "the Repo was injected into the Service");
        assert_eq!(PLACE_ORDER_RUNS.load(Ordering::SeqCst), 0, "no body run yet");

        // (2) the @ConfigurationProperties bean bound from the CLI args.
        let props = running.context().get::<AppProps>().await.expect("AppProps resolves");
        assert_eq!(props.name, "Orders", "AppProps bound app.name");
        assert_eq!(props.workers, 4, "AppProps bound app.workers");

        // (3) the #[runner] fired exactly once in the readiness window.
        assert_eq!(RUNNER_FIRED.load(Ordering::SeqCst), 1, "the #[runner] ran at startup");

        // (4) the #[transactional] + #[cacheable] method was AUTO-ADVISED. Resolve the
        // bean's id by its concrete TypeId (robust to the module path the contract is
        // minted at — OrderService is defined at the crate root, not in `tests`).
        let registry = running.context().engine().registry();
        let svc_id = *registry
            .candidates(std::any::TypeId::of::<OrderService>())
            .first()
            .expect("OrderService in registry");
        assert!(running.is_advised(svc_id), "the concern-annotated bean is auto-advised");

        // The redis cache advisor coexists; OUR cache advisor is the one keyed to
        // place_order. Drive the advised method through the auto-installed chain.
        let place = |id: i64| {
            running.invoke_advised(
                svc_id,
                MethodKey::of("OrderService::place_order"),
                ErasedArgs::pack((id,)),
            )
        };

        // place_order(7): MISS — the body runs (tx begins + commits), the value caches.
        let r1: Result<i64, LeafError> =
            place(7).await.expect("the advised call routes through the chain").unpack().unwrap();
        assert_eq!(r1.expect("Ok"), 1007, "the real method ran (1000 + run#0*100 + 7)");
        assert_eq!(PLACE_ORDER_RUNS.load(Ordering::SeqCst), 1, "the body ran on the cache MISS");

        // place_order(7) again: HIT — the cached value returns; the body does NOT run.
        let r2: Result<i64, LeafError> =
            place(7).await.expect("a cached call").unpack().unwrap();
        assert_eq!(r2.expect("Ok"), 1007, "the CACHED value returns");
        assert_eq!(PLACE_ORDER_RUNS.load(Ordering::SeqCst), 1, "a cache HIT short-circuited the body");

        // The tx manager demarcated each non-cached invocation (begin + commit on Ok).
        let tx = running.context().get::<TxManagerBean>().await.expect("tx manager");
        assert!(tx.begins() >= 1, "a tx was begun for the write");
        assert!(tx.commits() >= 1, "the Ok path committed");
        assert_eq!(tx.rollbacks(), 0, "no rollback on the Ok path");

        // (5) the app reached Running + flipped readiness at Ready.
        assert_eq!(running.unit().run_state(), RunState::Running);
        assert_eq!(
            running.unit().availability().readiness(),
            ReadinessState::AcceptingTraffic,
        );

        // (6) shutdown drains cleanly (the LIFO teardown ledger).
        let report = running.shutdown().await;
        assert_eq!(report.run_state, RunState::Closed, "the context closed");
        assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
    }

    /// The Redis auto-config PARTICIPATES (force-linked via the umbrella's `redis`
    /// feature): its `AUTO_CONFIGS` row + force-link references survive DCE, and the
    /// umbrella's ExpectedManifest names leaf-redis. It COEXISTS WITH the in-memory
    /// cache (FALLBACK loses to our NORMAL `InMemoryCacheManager`).
    #[test]
    fn the_redis_capability_participates_via_the_umbrella_force_link() {
        let crates = leaf::forcelink::participating_crates();
        assert!(crates.contains(&"leaf-redis"), "the redis capability participates: {crates:?}");
        assert!(crates.contains(&"leaf-tokio"), "its runtime peer participates: {crates:?}");
        let manifest = leaf::forcelink::expected_manifest();
        assert!(
            manifest.iter().any(|t| t.0 == "leaf-redis"),
            "the ExpectedManifest names leaf-redis: {manifest:?}"
        );
    }
}
