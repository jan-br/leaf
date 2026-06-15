//! THE leaf-resilience AUTO-ADVISE PROOF-GATE: REAL annotated beans whose
//! `#[advisable]` services are AUTO-ADVISED end-to-end through
//! `Application::new().run()` — proving the TWO Infrastructure resilience advisors
//! leaf-resilience ships AUTO-WIRE:
//!
//! 1. a RETRY-advised method that FAILS TWICE then SUCCEEDS is re-invoked up to the
//!    policy (three attempts, the third wins) — the substrate's REPLAYABLE `Next`;
//! 2. a CONCURRENCY-LIMIT-advised method caps concurrent entries to the gate's
//!    limit (no more than N bodies hold a permit at once).
//!
//! What is user code (annotations + the slice rows):
//! - a `register_component!` `LimitGate` — a `ConcurrencyGate` bean wrapping
//!   leaf-tokio's limit-2 `TokioExecutionFacility` (a local newtype: the orphan rule
//!   forbids `#[component]`-ing the foreign type; `register_component!` constructs it
//!   via `::new()`, NOT field-injection);
//! - two `register_component!` + `#[advisable]` services with OWNED atomic state
//!   (the ADVISED beans — `register_component!` so their atomic fields are owned
//!   state, not injected deps);
//! - the const `ADVISOR_PAIRINGS` rows the binary submits, exactly like `#[aspect]`
//!   emits — so `Application::run` AUTO-COLLECTS the resilience advisors with NO
//!   hand-assembled `.with_advisors`.

use std::any::TypeId;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    AdvisorPairingRow, BoxFuture, ConcurrencyGate, ContractId, ErasedArgs, ErrorKind, Interceptor,
    LeafError, MethodKey, OrderKey, OrderSource, Permit, Role, CONCURRENCY_ORDER, RETRY_ORDER,
};
use leaf_macros::{advisable, register_component};
use leaf_resilience::{
    concurrency_advisor_contract, make_concurrency_interceptor, result_classifier, NoBackoff,
    ResiliencePointcut, ResilientRetry, RetryInterceptor, RetryPolicy, Sleeper,
};
use leaf_tokio::TokioExecutionFacility;

// ─────────────────────── a tokio-backed reactive Sleeper ─────────────────────

/// A [`Sleeper`] that parks on `tokio::time::sleep` — the reactive timer the retry
/// backoff awaits on (NO busy-poll).
struct TokioSleeper;
impl Sleeper for TokioSleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

// ───────────────────────── the concurrency-gate bean ────────────────────────

/// A [`ConcurrencyGate`] bean: a limit-2 [`TokioExecutionFacility`] (a local
/// newtype, since the orphan rule forbids `#[component]`-ing the foreign type).
/// Constructed via `::new()` (`register_component!`, no field-injection).
struct LimitGate {
    inner: TokioExecutionFacility,
}
register_component!(LimitGate);

impl LimitGate {
    fn new() -> Self {
        LimitGate { inner: TokioExecutionFacility::with_limit(2) }
    }
}

impl ConcurrencyGate for LimitGate {
    fn acquire(&self) -> BoxFuture<'static, Permit> {
        self.inner.acquire()
    }
}

// ───────────────────────── the RETRY-advised service ────────────────────────

/// A service whose `flaky` method FAILS TWICE then SUCCEEDS — the retry headline.
/// The attempt counter is OWNED bean state (`register_component!` so it is not an
/// injected dependency), shared across the retried method's three attempts.
struct FlakyService {
    attempts: AtomicU32,
}
register_component!(FlakyService);

#[advisable]
impl FlakyService {
    fn new() -> Self {
        FlakyService { attempts: AtomicU32::new(0) }
    }

    /// Fails on attempts 1 and 2 (a retryable `Cancelled`), succeeds on attempt 3.
    /// Takes NO args so the auto-installed boot tail can re-run it per retry attempt
    /// (the substrate's REPLAYABLE `Next`; an args-bearing method's re-run is the
    /// documented v1 limitation).
    fn flaky(&self) -> Result<i64, LeafError> {
        let n = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if n < 3 {
            Err(LeafError::new(ErrorKind::Cancelled))
        } else {
            Ok(100 + n as i64)
        }
    }

    /// How many times `flaky` actually ran (the proof it was re-invoked).
    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }
}

// ─────────────────────── the CONCURRENCY-advised service ─────────────────────

/// A service whose `guarded` method records the PEAK number of bodies live at once
/// (so the test asserts the gate capped it). It awaits a tiny tokio sleep so
/// concurrent invocations genuinely overlap inside the guarded region.
struct GuardedService {
    live: AtomicUsize,
    peak: AtomicUsize,
}
register_component!(GuardedService);

#[advisable]
impl GuardedService {
    fn new() -> Self {
        GuardedService { live: AtomicUsize::new(0), peak: AtomicUsize::new(0) }
    }

    /// Enter (bump live + record peak), HOLD across a real async suspension (a tokio
    /// sleep, so concurrently-driven invocations genuinely overlap inside the guarded
    /// region), exit. The interceptor holds the gate permit across this whole body,
    /// so `peak` can never exceed the gate's limit.
    async fn guarded(&self, _ignore: i64) -> i64 {
        let cur = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(cur, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        cur as i64
    }

    /// The peak concurrent body count observed.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

// ───────────────────── the AUTO-WIRED resilience advisor rows ────────────────

// The advisors match each service by its concrete TypeId (the const TypeId-of seam
// mints the 'static slice exactly as a `#[retryable]`/`#[concurrency_limit]` macro
// would emit the marker pointcut).
static RETRY_TYPES: [TypeId; 1] = [const { TypeId::of::<FlakyService>() }];
static RETRY_POINTCUT: ResiliencePointcut = ResiliencePointcut::new(&RETRY_TYPES, &[]);

static LIMIT_TYPES: [TypeId; 1] = [const { TypeId::of::<GuardedService>() }];
static LIMIT_POINTCUT: ResiliencePointcut = ResiliencePointcut::new(&LIMIT_TYPES, &[]);

// THE retry auto-wire row: builds a RetryInterceptor (max_attempts = 3, NoBackoff)
// with the i64-return classifier so the business `Result::Err` drives the retry,
// and a tokio reactive sleeper. A non-capturing closure literal (const-promoted to
// the bare fn-pointer exactly like the `#[aspect]` codegen emits).
#[leaf_core::linkme::distributed_slice(leaf_core::ADVISOR_PAIRINGS)]
#[linkme(crate = leaf_core::linkme)]
static RETRY_ADVISOR_ROW: AdvisorPairingRow = AdvisorPairingRow {
    contract: ContractId::of("leaf::resilience::RetryAdvisor"),
    order: OrderKey { value: RETRY_ORDER, source: OrderSource::Interface },
    role: Role::Infrastructure,
    pointcut: &RETRY_POINTCUT,
    make_interceptor: |_container| {
        Box::pin(async move {
            let retry = ResilientRetry::new(RetryPolicy::new(3), Arc::new(NoBackoff))
                .with_sleeper(Arc::new(TokioSleeper));
            let interceptor =
                RetryInterceptor::new(retry).with_return_classifier(result_classifier::<i64>());
            Ok(Arc::new(interceptor) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, leaf_core::ResolveError>>
    },
};

// THE concurrency-limit auto-wire row: resolves the LimitGate bean through the
// container and wraps it in a ConcurrencyLimitInterceptor.
#[leaf_core::linkme::distributed_slice(leaf_core::ADVISOR_PAIRINGS)]
#[linkme(crate = leaf_core::linkme)]
static LIMIT_ADVISOR_ROW: AdvisorPairingRow = AdvisorPairingRow {
    contract: ContractId::of("leaf::resilience::ConcurrencyLimitAdvisor"),
    order: OrderKey { value: CONCURRENCY_ORDER, source: OrderSource::Interface },
    role: Role::Infrastructure,
    pointcut: &LIMIT_POINTCUT,
    make_interceptor: |c| make_concurrency_interceptor::<LimitGate>()(c),
};

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resilience_advisors_auto_advise_through_run() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let flaky_contract = ContractId::of(&format!("{module}::FlakyService"));
    let guarded_contract = ContractId::of(&format!("{module}::GuardedService"));

    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but annotations + the slice rows.
    let running = Application::new()
        .with_name("resilience-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // BOTH resilience advisor rows auto-collected (the headline two-advisor check).
    let collected = leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS);
    assert!(
        collected.iter().any(|r| r.contract == ContractId::of("leaf::resilience::RetryAdvisor")),
        "the retry Infrastructure advisor row auto-collected"
    );
    assert!(
        collected.iter().any(|r| r.contract == concurrency_advisor_contract()),
        "the concurrency-limit Infrastructure advisor row auto-collected"
    );

    // ── RETRY: the flaky bean is AUTOMATICALLY advised + retried ──
    let flaky_id = running
        .context()
        .engine()
        .registry()
        .by_contract(flaky_contract)
        .expect("FlakyService in registry");
    assert!(running.is_advised(flaky_id), "FlakyService is auto-advised by the retry advisor");

    let out = running
        .invoke_advised(flaky_id, MethodKey::of("FlakyService::flaky"), ErasedArgs::none())
        .await
        .expect("the advised call routes through the auto-installed retry chain");
    let ret: Result<i64, LeafError> = out.unpack().expect("the Result<i64,_> return");
    // The third attempt succeeds: 100 + attempt number 3 = 103.
    assert_eq!(ret.expect("Ok after retries"), 103, "the third attempt won (failed twice, then Ok)");

    let flaky = running.context().get::<FlakyService>().await.expect("FlakyService resolves");
    assert_eq!(flaky.attempts(), 3, "the method was RE-INVOKED twice (three attempts total)");

    // ── CONCURRENCY-LIMIT: the guarded bean is advised + capped ──
    let guarded_id = running
        .context()
        .engine()
        .registry()
        .by_contract(guarded_contract)
        .expect("GuardedService in registry");
    assert!(
        running.is_advised(guarded_id),
        "GuardedService is auto-advised by the concurrency-limit advisor"
    );

    // Fire MANY overlapping advised invocations; the gate (limit 2) must cap the
    // number of bodies running at once. join_all drives them concurrently on the
    // multi-thread runtime (each holds the gate permit across its body's sleep).
    let calls = (0..12_i64).map(|i| {
        running.invoke_advised(
            guarded_id,
            MethodKey::of("GuardedService::guarded"),
            ErasedArgs::pack((i,)),
        )
    });
    let results = join_all(calls).await;
    for r in results {
        r.expect("the guarded call routes through the auto-installed gate chain");
    }

    let guarded = running.context().get::<GuardedService>().await.expect("GuardedService resolves");
    let peak = guarded.peak();
    assert!(peak >= 2, "the bodies genuinely overlapped (peak {peak}) so the cap is meaningful");
    assert!(peak <= 2, "the concurrency-limit gate (limit 2) capped concurrent entries (peak {peak})");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}
