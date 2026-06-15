//! THE END-TO-END MILESTONE `[boot-e2e]`: a real app exercising the WHOLE leaf
//! stack — macros (`#[component]`/`register_component!`/`#[config_properties]`) →
//! linkme `Descriptor` rows → the `Descriptor`→`ProviderSeed` JOIN →
//! `seal_environment` → `route_conditions`/`run_autoconfig` → `seal()` →
//! `validate()` (Tier-2) → `Context::refresh()` (R0..R8: the auto-proxy
//! `after_init` install, the EventPublisher + listener bind, the eager
//! wave-instantiation) → `run()` (the runner in the readiness-gate window) →
//! `shutdown()` (the LIFO teardown drain).
//!
//! It proves, over a real multi-thread tokio runtime + the dev-dep `leaf-tokio`:
//!
//! 1. the bean GRAPH wired (a `#[component]` Service injects a `register_component!`
//!    Repository; a `#[config_properties]` AppProps is bound + resolvable);
//! 2. an advised call went through the INTERCEPTOR CHAIN (the R4 auto-proxy
//!    `after_init` install resolved an advisor's interceptor via `make_interceptor`
//!    and a call routed through it);
//! 3. the runner executed in the READY WINDOW (after `Started`, before
//!    `Ready`+`AcceptingTraffic`);
//! 4. `shutdown()` DRAINED cleanly (the container `TeardownLedger` LIFO).

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use leaf_boot::{
    AdvisorPairing, Application, InstalledProxies, RunOverlay, SealInputs,
};
use leaf_core::{
    AdviceError, AnnotationMetadata, Anything, Bean, BeanKey, BoxFuture, Call, Container,
    ContractId, CreatorPolicy, Descriptor, ErasedArgs, ErasedRet, FixedTarget, Interceptor,
    LeafError, MethodJoinPoint, MethodKey, Next, OrderKey, Origin, Provider, ProviderSeed,
    ProxyPlan, Published, Ref, ResolveCtx, Role, RunState, Runner, ScopeDef, Tail,
};
use leaf_macros::{component, config_properties, register_component};

// ─────────────────────────── the user's app beans ───────────────────────────

/// A leaf `@Component` repository constructed via `Repository::new()` (the
/// no-injected-collaborator `register_component!` form) — the dependency target.
#[derive(Debug)]
struct Repository {
    name: &'static str,
}

impl Repository {
    fn new() -> Self {
        Repository { name: "orders" }
    }
}
register_component!(Repository);

/// A leaf `@Component` service depending on the [`Repository`] (constructor
/// injection over the `Ref<Repository>` field) — the dependency graph edge.
#[component]
#[derive(Debug)]
struct OrderService {
    repo: Ref<Repository>,
}

impl OrderService {
    /// The constructor the `#[component]` provider calls with the resolved deps
    /// (`<OrderService>::new(repo)`).
    fn new(repo: Ref<Repository>) -> Self {
        OrderService { repo }
    }

    /// The "advised" business method — a call routes through the interceptor chain
    /// the R4 auto-proxy install builds. Reads the injected collaborator (proving
    /// the graph edge is live).
    fn place_order(&self, amount: i64) -> i64 {
        amount + self.repo.name.len() as i64
    }
}

/// A leaf `@ConfigurationProperties` type — derives `BindTarget` so the binder can
/// project `app.*` onto it; AUTO-REGISTERED + bound + resolvable.
///
/// `#[config_properties]` emits — beside the BindTarget — the PUBLIC C2 bind thunk
/// (`__leaf_config_bind_AppProps`, auto-collected into `CONFIG_BIND_PAIRINGS`), the
/// `impl ::leaf_core::Bean for AppProps {}` engine-resolvability marker, AND the bean's
/// own AUTO_CONFIGS `Descriptor` + seed (auto-collected into `AUTO_CONFIGS`/`SEED_PAIRINGS`
/// at `CandidateRole::FALLBACK`) — so the bean registers + binds + pre-materializes
/// purely from the slices, with NO hand-written provider/descriptor/seed.
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    title: String,
    workers: u16,
}

// ─────────────────────────────── the runner ─────────────────────────────────

static RUNNER_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A `Runner` that records it ran (proving the readiness-gate window) + reads the
/// shared `ApplicationArguments`.
struct StartupRunner;

impl Runner for StartupRunner {
    fn run<'a>(
        &'a self,
        args: &'a leaf_core::ApplicationArguments,
    ) -> BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            RUNNER_LOG.lock().unwrap().push("ran");
            let _ = args.source_args();
            Ok(())
        })
    }
}

// ─────────────────────────── the advisor (interceptor) ──────────────────────

static ADVICE_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A recording around-interceptor (the aspect bean the `make_interceptor` bridge
/// resolves at the R4 after_init install).
struct AuditInterceptor;

impl Interceptor for AuditInterceptor {
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        mut next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
        Box::pin(async move {
            ADVICE_LOG.lock().unwrap().push("before");
            let r = next.proceed(call).await;
            ADVICE_LOG.lock().unwrap().push("after");
            r
        })
    }
}

static ANY: Anything = Anything;

fn make_audit() -> leaf_core::MakeInterceptor {
    |_c: &dyn Container| Box::pin(async { Ok(Arc::new(AuditInterceptor) as Arc<dyn Interceptor>) })
}

// ────────────────────────────── the milestone ────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_whole_stack_wires_runs_advises_and_shuts_down_cleanly() {
    RUNNER_LOG.lock().unwrap().clear();
    ADVICE_LOG.lock().unwrap().clear();
    leaf_tokio::install_ambient_store().ok();

    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::OrderService"));

    // ── the execution facility (the force-linked tokio runtime) ──
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // ── drive the FULL run pipeline — every per-bean wiring channel AUTO-COLLECTS
    // from the macro-emitted linkme slices (seeds, injection plans, the AppProps
    // AUTO_CONFIGS Descriptor + seed, its C2 bind thunk). The only explicit input is
    // the embedder's spawner (a runtime handle, not a pairing table) + the explicit
    // `.with_runner` handle (the programmatic Runner escape hatch this test exercises
    // alongside the auto-collected wiring). ──
    let running = Application::new()
        .with_name("orders-app")
        .with_spawner(spawner)
        .with_runner(Arc::new(StartupRunner))
        .run(
            SealInputs::new().with_args(["--app.title=Orders", "--app.workers=4"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    // ── (1) the GRAPH wired: Service injected Repository; AppProps bound ──
    let service = running.context().get::<OrderService>().await.expect("OrderService resolves");
    assert_eq!(service.repo.name, "orders", "the Repository was injected into the Service");
    let props = running.context().get::<AppProps>().await.expect("AppProps resolves");
    assert_eq!(props.title, "Orders", "AppProps bound app.title from the env");
    assert_eq!(props.workers, 4, "AppProps bound app.workers from the env");

    // ── (3) the runner ran in the READY WINDOW (after Started, then readiness up) ──
    assert_eq!(*RUNNER_LOG.lock().unwrap(), vec!["ran"], "the runner executed once");
    assert_eq!(running.unit().run_state(), RunState::Running, "running after the run pipeline");
    assert_eq!(
        running.unit().availability().readiness(),
        leaf_core::ReadinessState::AcceptingTraffic,
        "readiness flipped to AcceptingTraffic at Ready (after the runner)"
    );

    // ── (2) an advised call goes through the INTERCEPTOR CHAIN ──
    // The R4 auto-proxy after_init install: build a ProxyPlan marking the service
    // advised, install the chain (resolving the advisor's interceptor via the bean
    // bridge), then route a call through it.
    let engine = running.context().engine();
    let svc_id = engine.registry().by_contract(service_contract).expect("service in registry");
    let advisor = AdvisorPairing::new(
        ContractId::of("e2e::AuditAdvisor"),
        OrderKey::implicit(),
        Role::Application,
        &ANY,
        make_audit(),
    )
    .into_descriptor();
    let method = MethodKey::of("OrderService::place_order");
    let methods = vec![MethodJoinPoint {
        method,
        arg_types: Default::default(),
        ret_type: TypeId::of::<i64>(),
    }];
    let mut jps = std::collections::HashMap::new();
    jps.insert(
        svc_id,
        leaf_core::BeanJoinPoints {
            bean_type: TypeId::of::<OrderService>(),
            markers: &AnnotationMetadata::EMPTY,
            methods: &methods,
        },
    );
    let plan = ProxyPlan::freeze(
        std::slice::from_ref(&advisor),
        engine.registry(),
        &CreatorPolicy::ALL,
        &jps,
    )
    .expect("freeze proxy plan");
    let installed = InstalledProxies::install(engine, &plan, &[advisor])
        .await
        .expect("auto-proxy after_init install");
    assert!(installed.is_advised(svc_id), "the service is advised");

    // Route a call to place_order(40) through the chain. The tail invokes the real
    // method over the FixedTarget (the published singleton).
    let chain = installed.chain_for(svc_id).expect("the installed chain");
    let target = FixedTarget::new(
        InstalledProxies::fixed_target_for(engine.registry(), svc_id).expect("published"),
    );
    let cx = ResolveCtx::for_engine(engine);
    let call = Call::new(
        method,
        BeanKey::ByType(TypeId::of::<OrderService>()),
        ErasedArgs::pack(40_i64),
        &target,
        &cx,
    );
    let tail: Box<Tail> = Box::new(|call: &Call<'_>| {
        Box::pin(async move {
            let bean = call.source.get(call.cx).await.map_err(AdviceError::TargetResolution)?;
            let svc = bean
                .downcast_ref::<OrderService>()
                .ok_or(AdviceError::DowncastMismatch { method: call.method })?;
            let amount = *call.args.0.downcast_ref::<i64>().unwrap();
            Ok(ErasedRet::pack(svc.place_order(amount)))
        })
    });
    let out = chain.invoke(&call, &*tail).await.expect("the advised call");
    // 40 + len("orders")=6 = 46.
    assert_eq!(out.unpack::<i64>().unwrap(), 46, "the real advised method ran (40 + 6)");
    assert_eq!(
        *ADVICE_LOG.lock().unwrap(),
        vec!["before", "after"],
        "the call routed through the interceptor chain (before → method → after)"
    );

    // ── (4) shutdown() DRAINS cleanly (the LIFO teardown) ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, RunState::Closed, "the context closed");
    assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
}

// ── a listener bound through the run pipeline fires on a lifecycle fact ──

static STARTED_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// The host bean for the `Started` listener (a singleton resolvable by contract).
#[derive(Debug)]
struct Watcher;
impl Bean for Watcher {}
struct WatcherProv(Descriptor);
impl Provider for WatcherProv {
    fn descriptor(&self) -> &Descriptor {
        &self.0
    }
    fn provide<'a>(&'a self, _cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
        Box::pin(async { Ok(Published::shared_value(Watcher)) })
    }
}
const fn watcher_seed() -> ProviderSeed {
    || {
        Arc::new(WatcherProv(Descriptor {
            contract: ContractId::of(concat!(module_path!(), "::Watcher")),
            self_type: TypeId::of::<Watcher>(),
            provides: &[],
            declared_name: Some("watcher"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("leaf-boot::e2e") },
        }))
    }
}

/// The erased adapter for `fn on(&self, _e: &Started)`.
fn started_adapter<'a>(
    _host: leaf_core::ErasedBean,
    _event: &'a (dyn std::any::Any + Send + Sync),
) -> BoxFuture<'a, Result<leaf_core::ListenerOutcome, LeafError>> {
    Box::pin(async move {
        STARTED_LOG.lock().unwrap().push("started-observed");
        Ok(leaf_core::ListenerOutcome::None)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_run_pipeline_fires_the_started_lifecycle_fact_to_a_bound_listener() {
    STARTED_LOG.lock().unwrap().clear();

    let module = module_path!();
    let watcher_contract = ContractId::of(&format!("{module}::Watcher"));
    // The auto-config candidate registers the Watcher host bean (a register_component
    // equivalent without a slice row — proves the auto-config lane registers a host).
    let watcher_desc = Descriptor {
        contract: watcher_contract,
        self_type: TypeId::of::<Watcher>(),
        provides: &[],
        declared_name: Some("watcher"),
        aliases: &[],
        scope: ScopeDef::SINGLETON,
        role: Role::Application,
        meta: &AnnotationMetadata::EMPTY,
        parent: None,
        origin: Origin::Native { crate_name: Some("leaf-boot::e2e") },
    };

    // The macro-emitted ListenerDescriptor: a listener on the built-in `Started`
    // lifecycle fact, hosted by the Watcher bean.
    let listener = leaf_core::ListenerDescriptor {
        host: watcher_contract,
        event_type: TypeId::of::<leaf_core::Started>(),
        supports: None,
        order: OrderKey::implicit(),
        condition: None,
        chains: false,
        adapter: started_adapter,
    };

    // The other test's `#[component]`s (OrderService/Repository) + the AppProps
    // config bean AUTO-COLLECT their seeds + injection plans from the macro-emitted
    // slices — no hand seed/plan table here. The Watcher host is registered via the
    // `.with_autoconfig` ESCAPE HATCH (an autoconfig candidate with no slice row),
    // proving the escape hatch still ADDS to the slice-collected set; the listener
    // rides the `.with_listeners` escape hatch.
    let running = Application::new()
        .with_name("watcher-app")
        .with_autoconfig(vec![leaf_boot::AutoConfigCandidate::new(
            watcher_desc,
            watcher_seed(),
            None,
        )])
        .with_listeners(vec![listener])
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app runs to Ready");

    // The run pipeline published `Started` through the live EventPublisher at R8 —
    // the bound listener observed it (the R3 multicaster install + listener bind +
    // the R8 milestone publish, end-to-end through the run pipeline).
    assert_eq!(
        *STARTED_LOG.lock().unwrap(),
        vec!["started-observed"],
        "the Started lifecycle fact fired to the bound listener during refresh"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, RunState::Closed);
}
