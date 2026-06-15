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

#![allow(non_upper_case_globals)]

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use leaf_boot::{
    AdvisorPairing, Application, InstalledProxies, RunOverlay, SealInputs, SeedPairing,
};
use leaf_core::{
    AdviceError, AnnotationMetadata, Anything, Bean, BeanKey, Binder, BoxFuture, Call,
    CanonicalName, Container, ContractId, ConversionService, CreatorPolicy, Descriptor, ErasedArgs,
    ErasedRet, FixedTarget, InjectionPlan, Interceptor, LeafError, MethodJoinPoint, MethodKey, Next,
    NoopBindHandler, OrderKey, Origin, Provider, ProviderSeed, ProxyPlan, Published, Ref,
    ResolveCtx, Role, RunState, Runner, ScopeDef, StackCps, Tail,
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
/// project `app.*` onto it; bound + registered as a resolvable bean.
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    title: String,
    workers: u16,
}

impl Bean for AppProps {}

// ───────────────────────── the config-properties provider ───────────────────

/// A hand-written provider that BINDS `AppProps` from the sealed `Env` via the
/// derived `BindTarget` (the bind a config-properties seed / the C2 validate pass
/// performs). The sealed env is captured into `APP_ENV` before refresh.
struct AppPropsProvider {
    descriptor: Descriptor,
}

impl Provider for AppPropsProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    fn provide<'a>(&'a self, _cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
        Box::pin(async move {
            let env = APP_ENV.lock().unwrap().clone().expect("env captured before refresh");
            let cps = StackCps::new(env);
            let conv = ConversionService::new();
            let handler = NoopBindHandler;
            let binder = Binder::new(&cps, &conv, &handler);
            let prefix = CanonicalName::parse("app").expect("a valid prefix");
            let props = binder.bind::<AppProps>(&prefix).bound().unwrap_or_default();
            Ok(Published::shared_value(props))
        })
    }
}

static APP_ENV: Mutex<Option<leaf_core::Env>> = Mutex::new(None);

/// The AppProps bean descriptor (a config-properties bean is registered via the
/// auto-config ladder — `#[config_properties]` emits only the BindTarget + metadata,
/// not a COMPONENTS row, so the bean itself is registered as an auto-config default).
fn app_props_descriptor() -> Descriptor {
    Descriptor {
        contract: ContractId::of(concat!(module_path!(), "::AppProps")),
        self_type: TypeId::of::<AppProps>(),
        provides: &[],
        declared_name: Some("appProps"),
        aliases: &[],
        scope: ScopeDef::SINGLETON,
        role: Role::Application,
        meta: &AnnotationMetadata::EMPTY,
        parent: None,
        origin: Origin::Native { crate_name: Some("leaf-boot::e2e") },
    }
}

/// The AppProps config-properties seed (the const fn-ptr that mints the binding
/// provider — the construction recipe a config-properties bean's seed carries).
const fn app_props_seed() -> ProviderSeed {
    || {
        Arc::new(AppPropsProvider {
            descriptor: Descriptor {
                contract: ContractId::of(concat!(module_path!(), "::AppProps")),
                self_type: TypeId::of::<AppProps>(),
                provides: &[],
                declared_name: Some("appProps"),
                aliases: &[],
                scope: ScopeDef::SINGLETON,
                role: Role::Application,
                meta: &AnnotationMetadata::EMPTY,
                parent: None,
                origin: Origin::Native { crate_name: Some("leaf-boot::e2e") },
            },
        })
    }
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
    let repo_contract = ContractId::of(&format!("{module}::Repository"));
    let service_contract = ContractId::of(&format!("{module}::OrderService"));
    let props_contract = ContractId::of(&format!("{module}::AppProps"));

    // ── capture the sealed env for the AppProps provider (the binder reads it) ──
    let sealed = leaf_boot::seal_environment(
        SealInputs::new().with_args(["--app.title=Orders", "--app.workers=4"]),
    )
    .await
    .expect("seal");
    *APP_ENV.lock().unwrap() = Some(sealed.env.clone());

    // ── the macro→runtime JOIN tables `#[leaf::main]` would emit ──
    // The seed JOIN: the macro-emitted `__leaf_seed_<Ident>` consts (pub) for the
    // two #[component]s. AppProps is registered via the auto-config ladder below.
    let _ = props_contract;
    let seeds = vec![
        SeedPairing::new(repo_contract, __leaf_seed_Repository),
        SeedPairing::new(service_contract, __leaf_seed_OrderService),
    ];

    // The auto-config candidate for the config-properties bean (registered at
    // Fallback by the run_autoconfig ladder — the config-properties default lane).
    let autoconfig = vec![leaf_boot::AutoConfigCandidate::new(
        app_props_descriptor(),
        app_props_seed(),
        None,
    )];

    // The per-bean injection-plan table (the macro-emitted `__LEAF_PLAN_<Ident>`
    // consts), keyed by BeanId via the frozen registry's by_contract lookup. The
    // run lifts the SAME seeds (plus the framework executor) so by_contract over
    // the stable ContractId yields the matching BeanId.
    let probe_registry = leaf_boot::App::<leaf_boot::Define>::from_slices(&seeds)
        .expect("lift")
        .into_builder()
        .freeze()
        .expect("freeze probe");
    let repo_id = probe_registry.by_contract(repo_contract);
    let service_id = probe_registry.by_contract(service_contract);
    let inj = move |id: leaf_core::BeanId| -> InjectionPlan {
        if Some(id) == service_id {
            __LEAF_PLAN_OrderService
        } else if Some(id) == repo_id {
            __LEAF_PLAN_Repository
        } else {
            InjectionPlan::EMPTY
        }
    };

    // ── the execution facility (the force-linked tokio runtime) ──
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // ── drive the FULL run pipeline ──
    let running = Application::new()
        .with_name("orders-app")
        .with_seeds(seeds)
        .with_autoconfig(autoconfig)
        .with_injection_plans(inj)
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

    let _ = watcher_seed; // (the seed const is used via the candidate below)
    // ALL #[component]s in the test crate are link-collected into COMPONENTS, so the
    // seed JOIN table must cover the OTHER test's components too (the anti-DCE JOIN:
    // a COMPONENTS row with no matching SeedPairing is a loud AntiDce error).
    let repo_contract = ContractId::of(&format!("{module}::Repository"));
    let service_contract = ContractId::of(&format!("{module}::OrderService"));
    let seeds = vec![
        SeedPairing::new(repo_contract, __leaf_seed_Repository),
        SeedPairing::new(service_contract, __leaf_seed_OrderService),
    ];
    // The OrderService injection plan (its Repository dep) must be supplied so the
    // wave plan + the eager construction resolve the edge.
    let probe = leaf_boot::App::<leaf_boot::Define>::from_slices(&seeds)
        .unwrap()
        .into_builder()
        .freeze()
        .unwrap();
    let svc_id = probe.by_contract(service_contract);
    let inj = move |id: leaf_core::BeanId| -> InjectionPlan {
        if Some(id) == svc_id {
            __LEAF_PLAN_OrderService
        } else {
            InjectionPlan::EMPTY
        }
    };

    let running = Application::new()
        .with_name("watcher-app")
        .with_seeds(seeds)
        .with_injection_plans(inj)
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
