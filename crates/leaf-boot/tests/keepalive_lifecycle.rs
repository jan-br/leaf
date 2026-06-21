//! Integration test `[boot-keepalive-lifecycle]` — STAGE 1 of the embedded-server
//! lifecycle rework: the CORE `KeepAlive` machinery wired through the real
//! `Application::run` pipeline, proven with a FAKE `KeepAlive` bean (no leaf-web).
//!
//! The audit gap this closes: the embedded web server was a BLOCKING `#[runner]`, so
//! `app.run()` never returned for a web app (readiness never reached
//! `AcceptingTraffic`, later runners starved, no graceful shutdown — the
//! `ShutdownTrigger::arm` seam was NEVER called). Stage 1 replaces the blocking model
//! with a spawned `KeepAlive` lifecycle component + a backend-free `ShutdownSignal`,
//! all in leaf-core (leaf-boot names NO leaf-web type). This proves, end to end:
//!
//! 1. **`run()` RETURNS for a KeepAlive app** (it is spawned, not blocking): readiness
//!    reaches `AcceptingTraffic`, the fake's `start` ran on a spawned task, AND a
//!    second `#[runner]` in the same app still ran (no starvation).
//! 2. **`park_until_shutdown()` PARKS until shutdown is fired**, then returns; the
//!    bounded grace-join lives in the RunUnit unit tests (lifecycle.rs).
//! 3. **A non-web app (no KeepAlive, just a runner) returns IMMEDIATELY** from
//!    `park_until_shutdown()` and runs to completion promptly (a timeout proves it).
//! 4. **The `ShutdownTrigger::arm` seam is now called exactly once** — a fake trigger
//!    captures the fire closure; invoking it quiesces the signal.
//!
//! All test beans are gated on a property so each `#[tokio::test]` selects exactly
//! which beans its boot sees (the global linkme slices are shared across the binary).

#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{BoxFuture, KeepAlive, LeafError, LifecycleCtx, ReadinessState, ShutdownTrigger};
use leaf_macros::{auto_config, component, conditional, runner};

// ─────────────────────────── shared observation state ────────────────────────────
//
// THREAD-LOCAL is wrong here (the spawned KeepAlive task runs on a different runtime
// thread than the test), so the fakes record into process-wide atomics. Each test
// gates which beans boot, so the counters are read after a single boot.

static COOP_STARTED: AtomicBool = AtomicBool::new(false);
static COOP_READY_CALLED: AtomicBool = AtomicBool::new(false);
static SECOND_RUNNER_RAN: AtomicU32 = AtomicU32::new(0);

// ───────────────────────────── the fake KeepAlive bean ───────────────────────────
//
// A COOPERATIVE KeepAlive: `start` records it ran, calls `ctx.on_ready()` (the "I am
// serving" latch that flips readiness), then parks on `ctx.shutdown.quiesce()` and
// returns Ok — the canonical embedded-server shape (Stage 2 = the real web server).

struct CooperativeKeepAlive;

impl KeepAlive for CooperativeKeepAlive {
    fn start(&self, ctx: LifecycleCtx) -> BoxFuture<'static, Result<(), LeafError>> {
        Box::pin(async move {
            COOP_STARTED.store(true, Ordering::SeqCst);
            // Latch readiness (the embedded server's "bound + serving" signal).
            (ctx.on_ready)();
            COOP_READY_CALLED.store(true, Ordering::SeqCst);
            // Park until shutdown is requested, then drain + resolve.
            ctx.shutdown.quiesce().await;
            Ok(())
        })
    }
}

/// The holder publishing the fake as the `dyn KeepAlive` view — leaf's idiom for a
/// concrete bean that publishes a `dyn` view (the SAME shape the leaf-web `dyn Route`/
/// `dyn WebFilter` beans use). `#[auto_config]` (not plain `#[configuration]`) so the
/// per-method `#[conditional]` GATE is lowered into a guard — GATED on
/// `test.keepalive=coop`.
#[component]
struct KeepAliveBeans;

#[auto_config]
impl KeepAliveBeans {
    #[bean(name = "coopKeepAlive", provides = "dyn ::leaf_core::KeepAlive")]
    #[conditional(on_property("test.keepalive", having_value = "coop"))]
    fn coop_keep_alive(&self) -> CooperativeKeepAlive {
        CooperativeKeepAlive
    }
}

// ─────────────────────────── a SECOND #[runner] (no starvation) ───────────────────
//
// Proves a runner co-located with a KeepAlive still RUNS — the KeepAlive is spawned
// (not blocking), so the readiness-gate runner window is not starved. GATED on
// `test.keepalive=coop` so it boots alongside the KeepAlive.

#[runner]
#[conditional(on_property("test.keepalive", having_value = "coop"))]
struct SecondRunner;

#[leaf_macros::async_impl]
impl leaf_core::Runner for SecondRunner {
    async fn run(&self, _args: &leaf_core::ApplicationArguments) -> Result<(), LeafError> {
        SECOND_RUNNER_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ─────────────────────── a #[runner] for the NON-WEB app proof ────────────────────
//
// GATED on `test.keepalive=none` so the non-web test boots a runner but NO KeepAlive
// (keep_alive_count == 0 → park returns immediately).

static NONWEB_RUNNER_RAN: AtomicU32 = AtomicU32::new(0);

#[runner]
#[conditional(on_property("test.keepalive", having_value = "none"))]
struct NonWebRunner;

#[leaf_macros::async_impl]
impl leaf_core::Runner for NonWebRunner {
    async fn run(&self, _args: &leaf_core::ApplicationArguments) -> Result<(), LeafError> {
        NONWEB_RUNNER_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ──────────────────────────── a fake ShutdownTrigger ─────────────────────────────
//
// Captures the `fire` closure the run pipeline `arm`s onto it (instead of listening
// for a real SIGINT/SIGTERM, which would be hostile to the test harness). The test
// invokes the captured closure to simulate a signal.

/// The captured `fire` closure the run pipeline `arm`s onto the trigger.
type FireSlot = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

#[derive(Clone, Default)]
struct CapturingTrigger {
    arm_count: Arc<AtomicU32>,
    fire: FireSlot,
}

impl ShutdownTrigger for CapturingTrigger {
    fn arm(&self, fire: Box<dyn Fn() + Send + Sync>) {
        self.arm_count.fetch_add(1, Ordering::SeqCst);
        *self.fire.lock().unwrap() = Some(fire);
    }
}

impl CapturingTrigger {
    /// Simulate a signal arriving: invoke the captured `fire` closure.
    fn signal(&self) {
        if let Some(f) = self.fire.lock().unwrap().as_ref() {
            f();
        }
    }
}

fn spawner() -> Arc<dyn leaf_core::Spawner> {
    Arc::new(leaf_tokio::TokioExecutionFacility::new())
}

// ─────────────────────────────────── the proofs ──────────────────────────────────

// (1) A KeepAlive app: run() RETURNS (does not block), readiness reaches
// AcceptingTraffic, the fake's start ran on a spawned task, and the second runner
// still ran (no starvation).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keepalive_app_runs_without_blocking_and_does_not_starve_a_runner() {
    leaf_tokio::install_ambient_store().ok();
    COOP_STARTED.store(false, Ordering::SeqCst);
    COOP_READY_CALLED.store(false, Ordering::SeqCst);
    SECOND_RUNNER_RAN.store(0, Ordering::SeqCst);

    // run() must RETURN (the KeepAlive is spawned, not blocking). A timeout proves it.
    let running = tokio::time::timeout(
        Duration::from_secs(10),
        Application::new()
            .with_name("keepalive-app")
            .with_spawner(spawner())
            .run(SealInputs::new().with_args(["--test.keepalive=coop"]), RunOverlay::none()),
    )
    .await
    .expect("run() returned (did not block on the KeepAlive)")
    .expect("the app runs to Ready");

    // The KeepAlive was started on a spawned task; poll briefly for it.
    for _ in 0..100 {
        if COOP_STARTED.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(COOP_STARTED.load(Ordering::SeqCst), "the fake KeepAlive's start ran on a spawned task");
    assert!(COOP_READY_CALLED.load(Ordering::SeqCst), "the on_ready readiness latch was called");

    assert_eq!(running.unit().keep_alive_count(), 1, "one KeepAlive was collected + started");
    assert_eq!(
        running.unit().availability().readiness(),
        ReadinessState::AcceptingTraffic,
        "readiness reached AcceptingTraffic"
    );
    // No starvation: the co-located #[runner] still ran in the readiness-gate window.
    // (A `>=` check: this integration binary's tests share the process-wide counter
    // and may run in parallel, so an exact count is not deterministic — but the runner
    // having run AT ALL alongside a spawned KeepAlive is the no-starvation claim.)
    assert!(
        SECOND_RUNNER_RAN.load(Ordering::SeqCst) >= 1,
        "the second runner ran (no starvation): {}",
        SECOND_RUNNER_RAN.load(Ordering::SeqCst)
    );

    running.shutdown().await;
}

// (2) park_until_shutdown() PARKS until shutdown is fired, then returns; shutdown()
// then drains cleanly (the cooperative KeepAlive quiesces on the fired signal).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn park_until_shutdown_parks_then_returns_when_fired() {
    leaf_tokio::install_ambient_store().ok();
    COOP_STARTED.store(false, Ordering::SeqCst);

    let running = Arc::new(
        Application::new()
            .with_name("keepalive-app")
            .with_spawner(spawner())
            .run(SealInputs::new().with_args(["--test.keepalive=coop"]), RunOverlay::none())
            .await
            .expect("the app runs to Ready"),
    );
    assert_eq!(running.unit().keep_alive_count(), 1);

    // park_until_shutdown must NOT return while no shutdown is requested.
    let parked = {
        let running = Arc::clone(&running);
        tokio::spawn(async move { running.park_until_shutdown().await })
    };
    // Give it a moment; it must still be parked.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!parked.is_finished(), "park_until_shutdown stays parked until shutdown is requested");

    // Fire shutdown (the programmatic path fires the SAME signal park is waiting on).
    let r = Arc::clone(&running);
    tokio::spawn(async move {
        r.shutdown().await;
    });

    // park must now return promptly.
    tokio::time::timeout(Duration::from_secs(5), parked)
        .await
        .expect("park_until_shutdown returned after shutdown was fired")
        .expect("the parked task did not panic");
}

// (3) A NON-WEB app (no KeepAlive bean, just a #[runner]): keep_alive_count == 0, so
// park_until_shutdown() returns IMMEDIATELY and the whole run completes promptly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_web_app_parks_immediately_and_completes_promptly() {
    leaf_tokio::install_ambient_store().ok();
    NONWEB_RUNNER_RAN.store(0, Ordering::SeqCst);

    // The WHOLE thing (run + park + shutdown) must complete promptly — a timeout
    // proves park_until_shutdown did not hang on a non-web app.
    let elapsed = tokio::time::timeout(Duration::from_secs(10), async {
        let start = std::time::Instant::now();
        let running = Application::new()
            .with_name("non-web-app")
            .with_spawner(spawner())
            .run(SealInputs::new().with_args(["--test.keepalive=none"]), RunOverlay::none())
            .await
            .expect("the app runs to Ready");

        assert_eq!(running.unit().keep_alive_count(), 0, "a non-web app has zero KeepAlive components");
        assert_eq!(NONWEB_RUNNER_RAN.load(Ordering::SeqCst), 1, "the non-web runner ran");

        // The deterministic gate: returns immediately (no KeepAlive to park on).
        running.park_until_shutdown().await;
        running.shutdown().await;
        start.elapsed()
    })
    .await
    .expect("the non-web run completed promptly (park returned immediately)");

    assert!(
        elapsed < Duration::from_secs(5),
        "park_until_shutdown returned immediately for a non-web app (elapsed {elapsed:?})"
    );
}

// (4) The ShutdownTrigger::arm seam is now CALLED exactly once (the previously-dormant
// arm() is wired): a fake trigger captures the fire closure; invoking it quiesces the
// unit's shutdown signal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shutdown_trigger_arm_seam_is_called_once_and_fires_the_signal() {
    leaf_tokio::install_ambient_store().ok();
    let trigger = CapturingTrigger::default();

    let running = Application::new()
        .with_name("armed-app")
        .with_spawner(spawner())
        .with_shutdown_trigger(Arc::new(trigger.clone()))
        .run(SealInputs::new().with_args(["--test.keepalive=coop"]), RunOverlay::none())
        .await
        .expect("the app runs to Ready");

    // arm() was called EXACTLY once by the run pipeline (the closed audit gap).
    assert_eq!(trigger.arm_count.load(Ordering::SeqCst), 1, "arm() was called exactly once");

    let signal = running.unit().shutdown_signal();
    assert!(!signal.fired(), "the signal is not fired before the trigger signals");

    // Simulate a SIGINT/SIGTERM by invoking the captured fire closure.
    trigger.signal();
    assert!(signal.fired(), "the armed fire closure quiesced the shutdown signal");
    // And quiesce resolves (the parked KeepAlive observes the same edge).
    tokio::time::timeout(Duration::from_secs(5), signal.quiesce())
        .await
        .expect("quiesce resolved after the armed trigger fired");

    running.shutdown().await;
}
