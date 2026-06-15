//! THE AUTO-WIRE PROOF-GATE `[boot-auto-wire]`: a REAL annotated app that
//! auto-wires end-to-end through `Application::new().run()` with NO hand-mirrored
//! ProxyPlan / runner list / config plan.
//!
//! Unlike `end_to_end.rs` (which hand-builds the ProxyPlan + install + Call/Tail
//! AFTER the run, and registers the runner via `with_runner`), this test proves the
//! macro→run-pipeline auto-wiring the task asks for:
//!
//! 1. AUTO-PROXY — a `#[component]` advised by an advisor (`with_advisors` +
//!    the macro-emitted join-point pairing `with_join_points` + the macro-emitted
//!    method-table pairing `with_method_tables`) is AUTOMATICALLY advised: the run
//!    pipeline builds the ProxyPlan at refresh R4 and installs the transparent
//!    proxy over the published bean, so an advised call routes through the
//!    interceptor chain via `RunningApp::invoke_advised` — no manual
//!    ProxyPlan/install in user code.
//! 2. AUTO-RUNNER — a runner bean wired via `with_runner_beans` (the macro-emitted
//!    `RunnerPairing` upcast thunk) is AUTO-COLLECTED from the live Context and run
//!    in the readiness-gate window — no explicit `with_runner` handle.
//! 3. AUTO-CONFIG — a `#[config_properties]` bean wired via `with_config_properties`
//!    is materialized/validated at Tier-2 automatically.
//!
//! Assert: the advised call went through the interceptor chain (auto-installed),
//! the runner ran, the config bound, shutdown clean.

#![allow(non_upper_case_globals)]

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use leaf_boot::{
    AdvisorPairing, Application, ConfigPairing, JoinPointPairing, MethodTablePairing, RunOverlay,
    RunnerPairing, SealInputs, SeedPairing,
};
use leaf_core::{
    AdviceError, AnnotationMetadata, Anything, BoxFuture, Call, Container, ContractId, Descriptor,
    ErasedArgs, ErasedRet, Interceptor, LeafError, MethodKey, Next, Origin, Provider, ProviderSeed,
    Published, Ref, ResolveCtx, Role, RunState, Runner, ScopeDef,
};
use leaf_macros::{advisable, component, config_properties, register_component, runner};

// ─────────────────────────── the user's app beans ───────────────────────────

/// A `@Component` repository (the dependency target).
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

/// A `@Component` service depending on the [`Repository`] — the ADVISED bean.
///
/// `#[component]` on the struct emits the `Descriptor`/`ProviderSeed`/`InjectionPlan`;
/// `#[advisable]` on the impl block emits the per-bean PROXY METADATA the auto-proxy
/// pipeline JOINs — the `__leaf_joinpoints_OrderService` join-point spec (the
/// `ProxyPlan` pointcut input) + the `__leaf_methods_OrderService` method table (the
/// transparent downcast-thunk index) — with NO hand-written `MethodTable`/`MethodEntry`.
#[component]
#[derive(Debug)]
struct OrderService {
    repo: Ref<Repository>,
}
#[advisable]
impl OrderService {
    fn new(repo: Ref<Repository>) -> Self {
        OrderService { repo }
    }

    /// The advised business method (auto-routed through the interceptor chain).
    fn place_order(&self, amount: i64) -> i64 {
        amount + self.repo.name.len() as i64
    }
}

/// A `@ConfigurationProperties` bean (bound + validated at Tier-2 automatically).
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    title: String,
    workers: u16,
}

// ─── AppProps fallback provider (the C2 thunk pre-binds the real value at validate) ───

struct AppPropsProvider {
    descriptor: Descriptor,
}
impl Provider for AppPropsProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn provide<'a>(&'a self, _cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
        Box::pin(async move { Ok(Published::shared_value(AppProps::default())) })
    }
}

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
        origin: Origin::Native { crate_name: Some("leaf-boot::auto-wire") },
    }
}
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
                origin: Origin::Native { crate_name: Some("leaf-boot::auto-wire") },
            },
        })
    }
}

// ─────────────────────────────── the runner bean ──────────────────────────────

static RUNNER_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A `#[runner]` bean — a `@Component` that ALSO declares the `dyn Runner` upcast view.
///
/// `#[runner]` emits the `Descriptor`/`ProviderSeed` (the `dyn Runner` `provides[]`
/// upcast) PLUS the per-runner upcast thunk `__leaf_runner_upcast_MigrateRunner`
/// (`ErasedBean → Option<Arc<dyn Runner>>`) the run pipeline pairs by `ContractId` as
/// a `RunnerPairing` — so the runner auto-collects from the live Context with NO
/// hand-written `RunnerUpcast`. It auto-runs in the readiness-gate window.
#[runner]
#[derive(Debug)]
struct MigrateRunner;
impl MigrateRunner {
    fn new() -> Self {
        MigrateRunner
    }
}
impl Runner for MigrateRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf_core::ApplicationArguments,
    ) -> BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            RUNNER_LOG.lock().unwrap().push("migrated");
            Ok(())
        })
    }
}

// ─────────────────────────── the advisor (interceptor) ──────────────────────

static ADVICE_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

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
async fn a_real_annotated_app_auto_wires_proxy_runner_and_config() {
    RUNNER_LOG.lock().unwrap().clear();
    ADVICE_LOG.lock().unwrap().clear();
    leaf_tokio::install_ambient_store().ok();

    let module = module_path!();
    let repo_contract = ContractId::of(&format!("{module}::Repository"));
    let service_contract = ContractId::of(&format!("{module}::OrderService"));
    let props_contract = ContractId::of(&format!("{module}::AppProps"));
    let runner_contract = ContractId::of(&format!("{module}::MigrateRunner"));

    // The seed JOIN (the #[component]s + the #[runner] — all macro-emitted seeds).
    let seeds = vec![
        SeedPairing::new(repo_contract, __leaf_seed_Repository),
        SeedPairing::new(service_contract, __leaf_seed_OrderService),
        SeedPairing::new(runner_contract, __leaf_seed_MigrateRunner),
    ];

    // The config-properties bean registers via the auto-config ladder.
    let autoconfig =
        vec![leaf_boot::AutoConfigCandidate::new(app_props_descriptor(), app_props_seed(), None)];

    // The per-bean injection plans (OrderService's Repository dep).
    let probe = leaf_boot::App::<leaf_boot::Define>::from_slices(&seeds)
        .expect("lift")
        .into_builder()
        .freeze()
        .expect("freeze probe");
    let service_id = probe.by_contract(service_contract);
    let inj = move |id: leaf_core::BeanId| -> leaf_core::InjectionPlan {
        if Some(id) == service_id {
            __LEAF_PLAN_OrderService
        } else {
            leaf_core::InjectionPlan::EMPTY
        }
    };

    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // ── drive the FULL run pipeline with the macro-emitted JOIN tables ──
    let running = Application::new()
        .with_name("orders-app")
        .with_seeds(seeds)
        .with_autoconfig(autoconfig)
        .with_injection_plans(inj)
        .with_config_properties(vec![ConfigPairing::new(props_contract, __leaf_config_bind_AppProps)])
        // The advisor + the per-bean join-points + the per-bean method table — the
        // proxy auto-installs at refresh R4 over the published bean.
        .with_advisors(vec![AdvisorPairing::new(
            ContractId::of("auto_wire::AuditAdvisor"),
            leaf_core::OrderKey::implicit(),
            Role::Application,
            &ANY,
            make_audit(),
        )])
        // The macro-emitted per-bean join-points (`__leaf_joinpoints_OrderService`) +
        // method table (`__leaf_methods_OrderService`) — NO hand-written consts.
        .with_join_points(vec![JoinPointPairing::new(service_contract, &__leaf_joinpoints_OrderService)])
        .with_method_tables(vec![MethodTablePairing::new(service_contract, __leaf_methods_OrderService)])
        // The runner bean is AUTO-COLLECTED from the live Context (no with_runner), via
        // the macro-emitted upcast thunk `__leaf_runner_upcast_MigrateRunner`.
        .with_runner_beans(vec![RunnerPairing::new(runner_contract, __leaf_runner_upcast_MigrateRunner)])
        .with_spawner(spawner)
        .run(
            SealInputs::new().with_args(["--app.title=Orders", "--app.workers=4"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app auto-wires and runs to Ready");

    // ── (3) the config bound automatically ──
    let props = running.context().get::<AppProps>().await.expect("AppProps resolves");
    assert_eq!(props.title, "Orders", "AppProps bound app.title");
    assert_eq!(props.workers, 4, "AppProps bound app.workers");

    // ── (2) the runner ran automatically (auto-collected) ──
    assert_eq!(*RUNNER_LOG.lock().unwrap(), vec!["migrated"], "the runner auto-ran once");
    assert_eq!(running.unit().run_state(), RunState::Running);
    assert_eq!(
        running.unit().availability().readiness(),
        leaf_core::ReadinessState::AcceptingTraffic,
        "readiness flipped at Ready (after the runner)"
    );

    // ── (1) the advised call AUTO-ROUTED through the interceptor chain ──
    // The proxy was auto-installed at R4. Routing a call by MethodKey goes through
    // the auto-installed chain (no hand-built ProxyPlan/install/Tail).
    let svc_id = running
        .context()
        .engine()
        .registry()
        .by_contract(service_contract)
        .expect("service in registry");
    assert!(
        running.is_advised(svc_id),
        "the #[component] is AUTOMATICALLY advised (the proxy auto-installed at R4)"
    );
    let out = running
        .invoke_advised(svc_id, MethodKey::of("OrderService::place_order"), ErasedArgs::pack((40_i64,)))
        .await
        .expect("the advised call routes through the auto-installed chain");
    assert_eq!(out.unpack::<i64>().unwrap(), 46, "the real method ran (40 + len(\"orders\"))");
    assert_eq!(
        *ADVICE_LOG.lock().unwrap(),
        vec!["before", "after"],
        "the call routed through the auto-installed interceptor chain"
    );

    // ── (4) shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, RunState::Closed, "the context closed");
    assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
}
