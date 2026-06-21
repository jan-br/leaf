//! The long-running lifecycle-component machinery (Spring's `SmartLifecycle`
//! analogue) — the backend-free CORE of the embedded-server lifecycle rework.
//!
//! A [`KeepAlive`] is a managed component that, once started, KEEPS RUNNING for
//! the life of the process (the embedded web server is the canonical example): it
//! binds/serves, latches readiness via [`LifecycleCtx::on_ready`], then parks on
//! the [`ShutdownSignal`] and DRAINS when shutdown is requested. Unlike the
//! one-shot [`Lifecycle`](crate::Lifecycle) `start`/`stop` pair, a `KeepAlive`'s
//! `start` future resolves only after the component has fully STOPPED.
//!
//! ## Why a backend-free shutdown signal
//!
//! leaf-boot drives this machinery but must NOT name a runtime. So the shutdown
//! handshake rides leaf's OWN `watch` cell ([`crate::watch_channel`]
//! — std-based, NO tokio, the same reactive primitive the `RunState`/availability
//! cells use): [`ShutdownSignal::quiesce`] `await`s a transition; the paired
//! [`ShutdownTriggerHandle::fire`] publishes it. The leaf-tokio
//! [`ShutdownTrigger`](crate::ShutdownTrigger) `arm`s `fire` onto SIGINT/SIGTERM;
//! a programmatic `RunningApp::shutdown` fires it too. Either way, every started
//! component observes the SAME reactive quiesce — no busy-poll, no global.

use std::time::Duration;

use crate::future::BoxFuture;
use crate::lifecycle::{watch_channel, WatchReceiver, WatchSender};
use crate::LeafError;

/// The shutdown phase the keep-alive handshake publishes through one leaf `watch`
/// cell. `Running` is the seed; `Quiescing` is the single terminal transition a
/// fire requests (idempotent — re-firing re-publishes the same terminal value).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ShutdownPhase {
    /// The seed: shutdown has not been requested.
    #[default]
    Running,
    /// Shutdown requested: every [`ShutdownSignal::quiesce`] resolves.
    Quiescing,
}

/// A backend-free, cloneable shutdown SIGNAL handed to every started
/// [`KeepAlive`] (the subscribing half of the handshake `watch` cell).
///
/// [`quiesce`](ShutdownSignal::quiesce) `await`s the shutdown request reactively
/// (parks on the leaf `watch` cell — NEVER a busy-poll); [`fired`](ShutdownSignal::fired)
/// is the cheap point read. Cloning is one `Arc` bump, so a component may hand a
/// clone to each of its own tasks.
#[derive(Clone)]
pub struct ShutdownSignal {
    rx: WatchReceiver<ShutdownPhase>,
}

impl ShutdownSignal {
    /// Resolve once shutdown is requested (a [`ShutdownTriggerHandle::fire`], a
    /// SIGTERM/SIGINT-armed trigger, or a programmatic `RunningApp::shutdown`).
    ///
    /// Reactive: parks on the leaf `watch` cell until the phase moves to
    /// `Quiescing`. Returns IMMEDIATELY if shutdown was already requested before
    /// this call (so a late-starting component never misses the edge).
    pub async fn quiesce(&self) {
        // A fresh receiver clone so this await is independent of any prior
        // observation (the predicate re-checks the current value first, so an
        // already-fired signal resolves at once).
        let mut rx = self.rx.clone();
        rx.wait_for(|p| *p == ShutdownPhase::Quiescing).await;
    }

    /// `true` iff shutdown has been requested (a cheap point read of the cell).
    #[must_use]
    pub fn fired(&self) -> bool {
        self.rx.borrow() == ShutdownPhase::Quiescing
    }

    /// A signal that NEVER fires — for components/tests that need a `LifecycleCtx`
    /// with no live trigger (the `await` parks forever).
    #[must_use]
    pub fn never() -> Self {
        let (_tx, rx) = watch_channel(ShutdownPhase::Running);
        // The sender is dropped: the cell can never transition, so `quiesce`
        // parks indefinitely (the intended "never shuts down" semantics).
        ShutdownSignal { rx }
    }
}

/// The PAIRED sender that requests shutdown (the publishing half of the handshake
/// `watch` cell). [`fire`](ShutdownTriggerHandle::fire) is idempotent.
///
/// leaf-boot arms one of these onto the [`ShutdownTrigger`](crate::ShutdownTrigger)
/// (SIGINT/SIGTERM) AND fires it from a programmatic teardown, so a started
/// [`KeepAlive`] quiesces either way through the SAME signal.
#[derive(Clone)]
pub struct ShutdownTriggerHandle {
    tx: WatchSender<ShutdownPhase>,
}

impl ShutdownTriggerHandle {
    /// Request shutdown: publish `Quiescing` so every [`ShutdownSignal::quiesce`]
    /// resolves. Idempotent — a second `fire` re-publishes the terminal value with
    /// no additional effect (a started component already saw the first edge).
    pub fn fire(&self) {
        self.tx.send(ShutdownPhase::Quiescing);
    }
}

/// Construct a fresh shutdown handshake: the subscribing [`ShutdownSignal`] every
/// started [`KeepAlive`] receives + the publishing [`ShutdownTriggerHandle`]
/// leaf-boot arms/fires. Backed by ONE leaf `watch` cell (std-based, NO tokio).
#[must_use]
pub fn shutdown_channel() -> (ShutdownSignal, ShutdownTriggerHandle) {
    let (tx, rx) = watch_channel(ShutdownPhase::Running);
    (ShutdownSignal { rx }, ShutdownTriggerHandle { tx })
}

/// The bind OUTCOME a started [`KeepAlive`] reports back to the run pipeline, ridden
/// over ONE leaf `watch` cell (std-based, NO tokio — the same primitive the shutdown
/// handshake uses).
///
/// The pipeline AWAITS this outcome (bounded by a short timeout) BEFORE it declares the
/// app Ready: `Bound` flips availability to `AcceptingTraffic` (the FIRST/real readiness
/// transition — the cell is seeded `RefusingTraffic`); `Failed` makes boot FAIL (a taken
/// port is not a silent dead server, exactly like Spring failing to start). `Pending` is
/// the seed (the component has neither bound nor faulted yet).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
enum BindOutcome {
    /// The seed: the component has neither bound nor faulted yet.
    #[default]
    Pending,
    /// The component bound/serves — the `on_ready` latch fired (readiness may open).
    Bound,
    /// The component's `start` resolved `Err` before binding (e.g. a port-in-use bind
    /// failure) — boot must fail. The rendered error message rides along.
    Failed(String),
}

/// The bounded bind outcome leaf-boot observes per started [`KeepAlive`] (the result of
/// [`ReadySignal::settled_within`]): the component bound, faulted, or neither within the
/// timeout. Public so the run pipeline can match it; the inner wire vocabulary the report
/// rides (a private `watch`-cell enum) stays an implementation detail.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReadyOutcome {
    /// The component bound and serves — readiness may flip to `AcceptingTraffic`.
    Bound,
    /// The component's `start` faulted before binding (the rendered error) — boot fails.
    Failed(String),
    /// Neither outcome arrived within the bounded timeout — readiness stays closed.
    TimedOut,
}

/// The subscribing half of the bind-outcome report — leaf-boot AWAITS this per started
/// [`KeepAlive`] before flipping readiness, so the readiness transition is precise (it
/// happens only AFTER an actual bind) and a bind failure is SURFACED (not swallowed).
///
/// Cheap to clone (one `Arc` bump on the underlying leaf `watch` cell). Backend-free.
#[derive(Clone)]
pub struct ReadySignal {
    rx: WatchReceiver<BindOutcome>,
}

/// The publishing half of the bind-outcome report, handed (as the `on_ready` latch +
/// the failure path) to a started [`KeepAlive`]. leaf-boot builds the `on_ready` closure
/// in [`LifecycleCtx`] over [`report_bound`](ReadyReporter::report_bound), and reports a
/// `start` `Err` over [`report_failed`](ReadyReporter::report_failed) from the spawn
/// wrapper. Idempotent — the FIRST outcome wins (a later re-publish is ignored).
#[derive(Clone)]
pub struct ReadyReporter {
    tx: WatchSender<BindOutcome>,
}

impl ReadyReporter {
    /// Report that the component BOUND and is serving (the `on_ready` latch fired). The
    /// first outcome wins: a failure already reported is not overwritten.
    pub fn report_bound(&self) {
        if self.tx.borrow() == BindOutcome::Pending {
            self.tx.send(BindOutcome::Bound);
        }
    }

    /// Report that the component's `start` FAILED before binding (the rendered error).
    /// The first outcome wins.
    pub fn report_failed(&self, message: String) {
        if self.tx.borrow() == BindOutcome::Pending {
            self.tx.send(BindOutcome::Failed(message));
        }
    }
}

impl ReadySignal {
    /// AWAIT the bind outcome reactively (parks on the leaf `watch` cell — NEVER a
    /// busy-poll): resolves `Ok(())` once the component reports `Bound`, or
    /// `Err(message)` once it reports `Failed`. Returns immediately if the outcome was
    /// already reported before this call.
    ///
    /// # Errors
    /// The rendered start-failure message iff the component reported `Failed`.
    pub async fn settled(&self) -> Result<(), String> {
        let mut rx = self.rx.clone();
        let outcome = rx.wait_for(|o| *o != BindOutcome::Pending).await;
        match outcome {
            BindOutcome::Bound => Ok(()),
            BindOutcome::Failed(message) => Err(message),
            // `wait_for` only resolves on a non-Pending value, so this is unreachable;
            // treat a defensive Pending as "not yet bound" (the timeout caller decides).
            BindOutcome::Pending => Ok(()),
        }
    }

    /// AWAIT the bind outcome bounded by an optional `deadline` (a runtime-supplied
    /// reactive sleep future — the SAME `drain_sleeper` primitive the teardown drain
    /// uses; the ONLY runtime-named input, so core stays runtime-agnostic). The race is
    /// REACTIVE (`select` over the settle and the deadline) — never a busy-poll. `None`
    /// ⇒ the unbounded await (a cooperative component reports promptly).
    ///
    /// Returns [`ReadyOutcome::Bound`]/[`ReadyOutcome::Failed`] once the component
    /// reports, or [`ReadyOutcome::TimedOut`] if the deadline wins first.
    pub async fn settled_within(&self, deadline: Option<BoxFuture<'static, ()>>) -> ReadyOutcome {
        let settled = self.settled();
        let Some(deadline) = deadline else {
            return match settled.await {
                Ok(()) => ReadyOutcome::Bound,
                Err(message) => ReadyOutcome::Failed(message),
            };
        };
        futures::pin_mut!(settled);
        match futures::future::select(settled, deadline).await {
            futures::future::Either::Left((Ok(()), _)) => ReadyOutcome::Bound,
            futures::future::Either::Left((Err(message), _)) => ReadyOutcome::Failed(message),
            futures::future::Either::Right(((), _)) => ReadyOutcome::TimedOut,
        }
    }

    /// `true` iff the component has reported `Bound` (a cheap point read).
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.rx.borrow() == BindOutcome::Bound
    }
}

/// Construct a fresh bind-outcome report: the [`ReadyReporter`] a started [`KeepAlive`]
/// publishes through (its `on_ready` latch + a `start`-failure path) + the [`ReadySignal`]
/// leaf-boot awaits before flipping readiness. Backed by ONE leaf `watch` cell.
#[must_use]
pub fn ready_report() -> (ReadySignal, ReadyReporter) {
    let (tx, rx) = watch_channel(BindOutcome::Pending);
    (ReadySignal { rx }, ReadyReporter { tx })
}

/// The context handed to a [`KeepAlive::start`]ed component: the shutdown signal it
/// parks on, the readiness latch it calls ONCE when bound/serving, and the optional
/// graceful-drain budget.
///
/// `on_ready` is the "I am now bound/serving" callback — leaf-boot supplies a closure
/// that REPORTS the bind so the run pipeline can flip availability to `AcceptingTraffic`
/// (the availability cell is seeded `RefusingTraffic`, so this is the FIRST/real readiness
/// transition — readiness reaches the K8s gate only after the component actually binds,
/// not merely when it is spawned). The component calls it exactly once; the closure is
/// `FnOnce`.
pub struct LifecycleCtx {
    /// The reactive shutdown signal the component parks on (then drains).
    pub shutdown: ShutdownSignal,
    /// The readiness latch the component calls ONCE when bound/serving.
    pub on_ready: Box<dyn FnOnce() + Send>,
    /// The graceful-drain budget (the `ShutdownSettings.grace`), if bounded.
    pub grace: Option<Duration>,
}

impl std::fmt::Debug for LifecycleCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleCtx")
            .field("fired", &self.shutdown.fired())
            .field("grace", &self.grace)
            .finish_non_exhaustive()
    }
}

/// A long-running lifecycle COMPONENT (Spring's `SmartLifecycle` analogue): once
/// [`start`](KeepAlive::start)ed it keeps running for the life of the process
/// (binding/serving, then draining on the shutdown signal).
///
/// `start` returns a `'static` [`BoxFuture`] (object-safe across the `dyn` seam)
/// that resolves ONLY after the component has FULLY STOPPED — so leaf-boot can
/// spawn it, latch readiness via [`LifecycleCtx::on_ready`], and join the spawned
/// handle (bounded by the grace budget) at teardown. There is NO async `Drop`;
/// graceful stop is the awaited drain inside this future.
///
/// `dyn KeepAlive` is an injectable VIEW (the [`impl_resolve_view!`](crate::impl_resolve_view)
/// seam below), so leaf-boot collects every provider as `Vec<Ref<dyn KeepAlive>>`
/// through the SAME by-trait/collection path `dyn Route`/`dyn WebFilter` use.
pub trait KeepAlive: Send + Sync {
    /// Start this component. The returned `'static` future binds/serves, calls
    /// [`ctx.on_ready`](LifecycleCtx::on_ready) once it is serving, parks on
    /// [`ctx.shutdown`](LifecycleCtx::shutdown), then DRAINS and resolves `Ok`
    /// once fully stopped.
    ///
    /// # Errors
    /// A [`LeafError`] if the component cannot start/serve (e.g. a bind failure);
    /// leaf-boot surfaces it from the spawned handle.
    fn start(&self, ctx: LifecycleCtx) -> BoxFuture<'static, Result<(), LeafError>>;
}

// Make `dyn KeepAlive` an injectable VIEW (the by-trait-injection seam, emitted
// ONCE — orphan-rule-OK since `dyn KeepAlive` is local to this crate). Every
// KeepAlive bean (any crate's `#[bean(provides = "dyn ::leaf_core::KeepAlive")]`)
// is collected by leaf-boot as `Vec<Ref<dyn KeepAlive>>` (collection injection),
// the SAME seam as `dyn Route`/`dyn WebFilter`.
crate::impl_resolve_view!(dyn KeepAlive);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn fresh_signal_is_not_fired() {
        let (signal, _handle) = shutdown_channel();
        assert!(!signal.fired(), "a fresh signal has not been requested");
    }

    #[test]
    fn fire_flips_fired_and_resolves_quiesce() {
        let (signal, handle) = shutdown_channel();
        handle.fire();
        assert!(signal.fired(), "fire() flips the point read");
        // quiesce resolves immediately on an already-fired signal.
        block(signal.quiesce());
    }

    #[test]
    fn quiesce_parks_until_fire() {
        // Drive quiesce + a deferred fire on one executor: the await must resolve
        // only AFTER the fire publishes.
        let (signal, handle) = shutdown_channel();
        let observed = Arc::new(AtomicBool::new(false));
        let obs = observed.clone();
        block(async move {
            let waited = {
                let signal = signal.clone();
                async move {
                    signal.quiesce().await;
                    obs.store(true, Ordering::SeqCst);
                }
            };
            futures::pin_mut!(waited);
            // Poll once: not fired yet, so the await must be pending.
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            assert!(
                std::future::Future::poll(waited.as_mut(), &mut cx).is_pending(),
                "quiesce parks while Running"
            );
            // Now fire and drive to completion.
            handle.fire();
            waited.await;
        });
        assert!(observed.load(Ordering::SeqCst), "quiesce resolved after fire");
    }

    #[test]
    fn fire_is_idempotent() {
        let (signal, handle) = shutdown_channel();
        handle.fire();
        handle.fire(); // second fire is a harmless re-publish.
        assert!(signal.fired());
        block(signal.quiesce());
    }

    #[test]
    fn never_signal_does_not_fire() {
        let signal = ShutdownSignal::never();
        assert!(!signal.fired(), "never() is never fired");
        // quiesce on a never() signal parks forever — assert it stays pending.
        let fut = signal.quiesce();
        futures::pin_mut!(fut);
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(
            std::future::Future::poll(fut.as_mut(), &mut cx).is_pending(),
            "never() never resolves quiesce"
        );
    }

    #[test]
    fn signal_clone_observes_the_same_fire() {
        let (signal, handle) = shutdown_channel();
        let clone = signal.clone();
        handle.fire();
        assert!(signal.fired() && clone.fired(), "a clone observes the same cell");
    }

    #[test]
    fn ready_report_settles_bound() {
        let (signal, reporter) = ready_report();
        assert!(!signal.is_bound(), "a fresh report is Pending, not bound");
        reporter.report_bound();
        assert!(signal.is_bound(), "report_bound flips the point read");
        assert_eq!(block(signal.settled()), Ok(()), "settled resolves Ok on Bound");
    }

    #[test]
    fn ready_report_settles_failed_with_the_message() {
        let (signal, reporter) = ready_report();
        reporter.report_failed("port in use".to_string());
        assert!(!signal.is_bound());
        assert_eq!(
            block(signal.settled()),
            Err("port in use".to_string()),
            "settled surfaces the failure message"
        );
    }

    #[test]
    fn ready_report_first_outcome_wins() {
        // A bind failure already reported is NOT overwritten by a later spurious Bound.
        let (signal, reporter) = ready_report();
        reporter.report_failed("bind failed".to_string());
        reporter.report_bound(); // ignored — Failed already won.
        assert!(!signal.is_bound());
        assert_eq!(block(signal.settled()), Err("bind failed".to_string()));
    }

    #[test]
    fn ready_report_settled_parks_until_an_outcome() {
        // settled must PARK while the outcome is Pending, then resolve once reported.
        let (signal, reporter) = ready_report();
        let fut = signal.settled();
        futures::pin_mut!(fut);
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(
            std::future::Future::poll(fut.as_mut(), &mut cx).is_pending(),
            "settled parks while Pending"
        );
        reporter.report_bound();
        assert_eq!(block(fut), Ok(()), "settled resolves after the outcome is reported");
    }

    #[test]
    fn lifecycle_ctx_carries_the_signal_and_grace() {
        let (signal, handle) = shutdown_channel();
        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let ctx = LifecycleCtx {
            shutdown: signal.clone(),
            on_ready: Box::new(move || r.store(true, Ordering::SeqCst)),
            grace: Some(Duration::from_millis(50)),
        };
        assert_eq!(ctx.grace, Some(Duration::from_millis(50)));
        (ctx.on_ready)();
        assert!(ran.load(Ordering::SeqCst), "on_ready latch fired");
        handle.fire();
        assert!(signal.fired());
    }
}
