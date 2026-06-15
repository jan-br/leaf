//! THE AUTO-WIRE PROOF-GATE `[boot-auto-wire]`: a REAL annotated app that
//! auto-wires end-to-end through `Application::new().run()` with NOTHING but
//! annotations + the `run()` call — ZERO `.with_*` pairing tables, ZERO
//! hand-written providers/descriptors/seeds, ZERO hand-built `ProxyPlan`.
//!
//! The USER CODE below is ONLY the annotations — `#[component]` structs (+ an
//! `#[advisable]` impl for the advised methods), an `#[aspect]` (the advisor: its
//! interceptor + pointcut auto-collect), a `#[runner]` bean, a `#[config_properties]`
//! bean — plus `Application::new().with_spawner(..).run(..)`. The spawner is the ONE
//! non-annotation input (the embedder's tokio runtime handle, not a pairing table).
//! EVERYTHING else is AUTO-COLLECTED from the macro-emitted `linkme` distributed-slice
//! pairing channels and JOINed by `ContractId`:
//!
//! 1. AUTO-PROXY — the `#[aspect]` advisor's interceptor + pointcut auto-collect
//!    from `ADVISOR_PAIRINGS`; the `#[advisable]` bean's join-points
//!    (`JOINPOINT_PAIRINGS`) + method-table (`METHOD_TABLE_PAIRINGS`) auto-collect;
//!    the run pipeline builds the ProxyPlan + installs the transparent proxy at R4.
//! 2. AUTO-RUNNER — the `#[runner]` upcast thunk auto-collects from `RUNNER_PAIRINGS`
//!    and the bean auto-runs in the readiness-gate window.
//! 3. AUTO-CONFIG — the `#[config_properties]` bean auto-registers (its
//!    `Descriptor`/seed auto-collect) + binds/validates at Tier-2 from its
//!    `CONFIG_BIND_PAIRINGS` thunk.
//! 4. AUTO-SEED/PLAN — every `#[component]` seed (`SEED_PAIRINGS`) + injection plan
//!    (`INJECTION_PLAN_PAIRINGS`) auto-collects, so the graph wires + the wave plan
//!    resolves the `OrderService → Repository` edge with no hand table.

use std::sync::{Arc, Mutex};

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    AdviceError, BoxFuture, Call, ErasedArgs, ErasedRet, Interceptor, LeafError, MethodKey, Next,
    Ref, ReadinessState, RunState, Runner,
};
use leaf_macros::{advisable, aspect, component, config_properties, register_component, runner};

// ─────────────────────────── the user's app beans ───────────────────────────

/// A `@Component` repository (the dependency target).
#[derive(Debug)]
struct Repository {
    name: &'static str,
}
impl Repository {
    fn new() -> Self {
        Repository { name: "order" }
    }
}
register_component!(Repository);

/// A `@Component` service depending on the [`Repository`] — the ADVISED bean.
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

/// A `@ConfigurationProperties` bean — auto-registers + binds/validates at Tier-2.
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    title: String,
    workers: u16,
}

// ─────────────────────────────── the runner bean ──────────────────────────────

static RUNNER_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// A `#[runner]` bean — auto-collects + auto-runs in the readiness-gate window.
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

// ─────────────────────────── the advisor (an #[aspect]) ──────────────────────

static ADVICE_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// An `#[aspect]` advisor bean. The aspect IS the interceptor: the run pipeline
/// auto-collects its `make_interceptor` (resolve-the-aspect-bean) + pointcut from
/// `ADVISOR_PAIRINGS`, so the advised call routes through it with NO `.with_advisors`.
#[aspect]
#[derive(Debug)]
struct AuditAspect;
impl AuditAspect {
    fn new() -> Self {
        AuditAspect
    }
}
impl Interceptor for AuditAspect {
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

// ────────────────────────────── the milestone ────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_annotated_app_auto_wires_from_the_slices_alone() {
    RUNNER_LOG.lock().unwrap().clear();
    ADVICE_LOG.lock().unwrap().clear();
    leaf_tokio::install_ambient_store().ok();

    let module = module_path!();
    let service_contract = leaf_core::ContractId::of(&format!("{module}::OrderService"));

    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // ── drive the FULL run pipeline with NOTHING but annotations + the spawner ──
    let running = Application::new()
        .with_name("order-app")
        .with_spawner(spawner)
        .run(
            SealInputs::new().with_args(["--app.title=Orders", "--app.workers=4"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app auto-wires and runs to Ready from the slices alone");

    // ── (3) the config bound automatically (auto-registered + auto-bound) ──
    let props = running.context().get::<AppProps>().await.expect("AppProps resolves");
    assert_eq!(props.title, "Orders", "AppProps bound app.title");
    assert_eq!(props.workers, 4, "AppProps bound app.workers");

    // ── the graph wired: OrderService injected its Repository ──
    let service = running.context().get::<OrderService>().await.expect("OrderService resolves");
    assert_eq!(service.repo.name, "order", "the Repository was auto-injected into the Service");

    // ── (2) the runner ran automatically (auto-collected) ──
    assert_eq!(*RUNNER_LOG.lock().unwrap(), vec!["migrated"], "the runner auto-ran once");
    assert_eq!(running.unit().run_state(), RunState::Running);
    assert_eq!(
        running.unit().availability().readiness(),
        ReadinessState::AcceptingTraffic,
        "readiness flipped at Ready (after the runner)"
    );

    // ── (1) the advised call AUTO-ROUTED through the interceptor chain ──
    let svc_id = running
        .context()
        .engine()
        .registry()
        .by_contract(service_contract)
        .expect("service in registry");
    assert!(
        running.is_advised(svc_id),
        "the #[component] is AUTOMATICALLY advised by the auto-collected #[aspect] (proxy at R4)"
    );
    let out = running
        .invoke_advised(svc_id, MethodKey::of("OrderService::place_order"), ErasedArgs::pack((40_i64,)))
        .await
        .expect("the advised call routes through the auto-installed chain");
    assert_eq!(out.unpack::<i64>().unwrap(), 45, "the real method ran (40 + len(\"order\"))");
    assert_eq!(
        *ADVICE_LOG.lock().unwrap(),
        vec!["before", "after"],
        "the call routed through the auto-collected interceptor chain"
    );

    // ── (4) shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, RunState::Closed, "the context closed");
    assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
}

// ─────────────────────── the NEGATIVE-CHECK: drop = gone ──────────────────────

/// A PLAIN struct with NO annotation — it must contribute NOTHING to any pairing
/// slice (no seed, no injection plan, no advisor, no runner, no config bind). This is
/// the negative half of "discovery IS the annotation": removing the annotation removes
/// the bean/advice/runner from the auto-collected set.
#[derive(Debug)]
#[allow(dead_code)]
struct Unannotated;

#[test]
fn dropping_an_annotation_removes_its_slice_contribution() {
    use leaf_core::{
        collect_slice, ContractId, ADVISOR_PAIRINGS, CONFIG_BIND_PAIRINGS, RUNNER_PAIRINGS,
        SEED_PAIRINGS,
    };
    let module = module_path!();
    let unannotated = ContractId::of(&format!("{module}::Unannotated"));

    // No #[component] => no seed row => the bean is never registered/constructed.
    assert!(
        !collect_slice(&SEED_PAIRINGS).iter().any(|r| r.contract == unannotated),
        "a struct with NO #[component] contributes no SEED_PAIRINGS row"
    );
    // No #[runner] => not in RUNNER_PAIRINGS.
    assert!(
        !collect_slice(&RUNNER_PAIRINGS).iter().any(|r| r.contract == unannotated),
        "a struct with NO #[runner] contributes no RUNNER_PAIRINGS row"
    );
    // No #[aspect] => not in ADVISOR_PAIRINGS.
    assert!(
        !collect_slice(&ADVISOR_PAIRINGS).iter().any(|r| r.contract == unannotated),
        "a struct with NO #[aspect] contributes no ADVISOR_PAIRINGS row"
    );
    // No #[config_properties] => not in CONFIG_BIND_PAIRINGS.
    assert!(
        !collect_slice(&CONFIG_BIND_PAIRINGS).iter().any(|r| r.contract == unannotated),
        "a struct with NO #[config_properties] contributes no CONFIG_BIND_PAIRINGS row"
    );

    // CONVERSELY: the ANNOTATED beans above DO contribute (the positive sanity twin).
    let aspect = ContractId::of(&format!("{module}::AuditAspect"));
    let runner = ContractId::of(&format!("{module}::MigrateRunner"));
    assert!(
        collect_slice(&ADVISOR_PAIRINGS).iter().any(|r| r.contract == aspect),
        "the #[aspect] DOES contribute an ADVISOR_PAIRINGS row (the annotation IS the wiring)"
    );
    assert!(
        collect_slice(&RUNNER_PAIRINGS).iter().any(|r| r.contract == runner),
        "the #[runner] DOES contribute a RUNNER_PAIRINGS row"
    );
}
