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
//! What is user code (the NATURAL `#[retryable]` / `#[concurrency_limit]` annotations
//! — NO `#[aspect]`, NO hand-written `ADVISOR_PAIRINGS` rows):
//! - a `register_component!` `LimitGate` — a `ConcurrencyGate` bean wrapping
//!   leaf-tokio's limit-2 `TokioExecutionFacility` (a local newtype: the orphan rule
//!   forbids `#[component]`-ing the foreign type; `register_component!` constructs it
//!   via `::new()`, NOT field-injection);
//! - two `register_component!` + `#[advisable]` services with OWNED atomic state
//!   whose methods carry `#[retryable(max = 3)]` / `#[concurrency_limit(2, gate = ..)]`
//!   (the ADVISED beans).
//!
//! Each natural annotation on a `#[advisable]`-impl method is what the impl-block macro
//! lowers to the const `ADVISOR_PAIRINGS` row (the resilience advisors keyed by the
//! bean's `TypeId`, `#[retryable]` binding the `Result<i64,_>` retry classifier and
//! `#[concurrency_limit]` binding the named gate) — so `Application::run` AUTO-COLLECTS
//! the resilience advisors with NO `.with_advisors`.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{BoxFuture, ConcurrencyGate, ContractId, ErasedArgs, ErrorKind, LeafError, MethodKey, Permit};
// `#[retryable]` / `#[concurrency_limit]` are NOT imported: they are per-method MARKERS
// the `#[advisable]` impl macro STRIPS + lowers (the impl-block macro owns the rows),
// so — exactly like `#[bean]` inside `#[configuration] impl` — they need no import.
use leaf_macros::{advisable, register_component};
use leaf_tokio::TokioExecutionFacility;

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
    /// The arg value seen on the LAST attempt — proof the args-bearing method was
    /// re-proceeded WITH its args (a fresh clone re-supplied each attempt).
    last_arg: AtomicU32,
}
register_component!(FlakyService);

#[advisable]
impl FlakyService {
    fn new() -> Self {
        FlakyService { attempts: AtomicU32::new(0), last_arg: AtomicU32::new(0) }
    }

    /// Fails on attempts 1 and 2 (a retryable `Cancelled`), succeeds on attempt 3.
    /// The `#[retryable(max = 3)]` annotation auto-wires the retry advisor (binding the
    /// `Result<i64,_>` retry classifier, keyed by the bean's `TypeId`). Takes an `i64`
    /// ARG — the auto-installed boot tail re-supplies a FRESH clone of the args off
    /// `Call.args` per retry attempt (the substrate's REPLAYABLE `Next` over the
    /// cloneable advised-arg ABI), so the args-bearing method is genuinely re-proceeded
    /// with its args every attempt.
    #[retryable(max = 3)]
    fn flaky(&self, base: i64) -> Result<i64, LeafError> {
        self.last_arg.store(base as u32, Ordering::SeqCst);
        let n = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if n < 3 {
            Err(LeafError::new(ErrorKind::Cancelled))
        } else {
            Ok(base + n as i64)
        }
    }

    /// How many times `flaky` actually ran (the proof it was re-invoked).
    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }

    /// The arg the last attempt saw (proof each replay carried the args).
    fn last_arg(&self) -> u32 {
        self.last_arg.load(Ordering::SeqCst)
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
    /// region), exit. The `#[concurrency_limit(2, gate = LimitGate)]` annotation
    /// auto-wires the concurrency-limit advisor (resolving the `LimitGate` bean, keyed
    /// by this bean's `TypeId`); the interceptor holds the gate permit across this whole
    /// body, so `peak` can never exceed the gate's limit.
    #[concurrency_limit(2, gate = LimitGate)]
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

    // BOTH resilience advisor rows auto-collected from the natural annotations (the
    // headline two-advisor check). Each is a per-method-unique row whose contract is
    // the resilience family base @ the module-qualified `Bean::method`.
    let collected = leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS);
    let retry_contract = ContractId::of(&format!(
        "leaf::resilience::RetryAdvisor@{module}::FlakyService::flaky"
    ));
    let limit_contract = ContractId::of(&format!(
        "leaf::resilience::ConcurrencyLimitAdvisor@{module}::GuardedService::guarded"
    ));
    assert!(
        collected.iter().any(|r| r.contract == retry_contract),
        "the retry Infrastructure advisor row auto-collected from #[retryable]"
    );
    assert!(
        collected.iter().any(|r| r.contract == limit_contract),
        "the concurrency-limit Infrastructure advisor row auto-collected from #[concurrency_limit]"
    );

    // ── RETRY: the flaky bean is AUTOMATICALLY advised + retried ──
    let flaky_id = running
        .context()
        .engine()
        .registry()
        .by_contract(flaky_contract)
        .expect("FlakyService in registry");
    assert!(running.is_advised(flaky_id), "FlakyService is auto-advised by the retry advisor");

    // Drive the ARGS-BEARING method through the retry chain: each of the three
    // attempts re-supplies a fresh clone of the `(100,)` args off `Call.args`.
    let out = running
        .invoke_advised(flaky_id, MethodKey::of("FlakyService::flaky"), ErasedArgs::pack((100_i64,)))
        .await
        .expect("the advised call routes through the auto-installed retry chain");
    let ret: Result<i64, LeafError> = out.unpack().expect("the Result<i64,_> return");
    // The third attempt succeeds: base 100 + attempt number 3 = 103.
    assert_eq!(ret.expect("Ok after retries"), 103, "the third attempt won (failed twice, then Ok)");

    let flaky = running.context().get::<FlakyService>().await.expect("FlakyService resolves");
    assert_eq!(flaky.attempts(), 3, "the method was RE-INVOKED twice (three attempts total)");
    assert_eq!(flaky.last_arg(), 100, "every replayed attempt carried the args (a fresh clone)");

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
