//! Container lifecycle: the [`Lifecycle`]/[`Shutdown`] seams, the ONE phase-axis
//! [`RunState`] machine, and the runtime-agnostic [`watch_run_state`] watch cell.
//!
//! Realizes phase3/13-container-lifecycle (the fused leaf-boot template) and the
//! phase4 SEAMS seam #1 / ADR-07 decisions:
//!
//! - [`RunState`] is the **single** phase axis: `Created → Refreshing → Running →
//!   Stopping → Closing → Closed`, with `Failed` as the cancel-cascade terminal
//!   that *structurally* suppresses `Closed`. It is ORTHOGONAL to the two
//!   availability watch cells (liveness/readiness, owned by the events
//!   subsystem) — they share only this watch-cell *shape*, never a composite
//!   machine.
//! - `RunDown` is NOT a type: it is the `Stopping/Closing/Closed/Failed` tail of
//!   `RunState`. The keep-alive terminal predicate is `RunState ∈ {Closed,
//!   Failed}` (the informal "Terminated" superset).
//! - [`watch_run_state`] returns a [`RunStateReceiver`]; consumers `await` a
//!   transition (charter §2.4: reactive watch, NEVER an `is_running` poll loop).
//!   The watch is a small std-based cell — NO tokio, NO global lock (one
//!   `Mutex` *per cell*, fine-grained, holding only the current value + a waker
//!   list).
//! - [`Lifecycle`]/[`Shutdown`] box their futures at the `dyn` seam ([`BoxFuture`],
//!   AFIT/RPITIT not dyn-compatible). There is NO async `Drop`: teardown is the
//!   explicit container-driven ledger drain.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::error::LeafError;
use crate::future::BoxFuture;

// ───────────────────────────── RunState ─────────────────────────────────────

/// THE single container phase axis (SEAMS seam #1 (D), ADR-07).
///
/// Linear bring-up `Created → Refreshing → Running`, linear teardown `Running →
/// Stopping → Closing → Closed`, and the orthogonal cancel-cascade terminal
/// `* → Failed` (which structurally suppresses `Closed`). This is the ONLY
/// state machine modelling container *phase*; availability (liveness/readiness)
/// lives in two separate watch cells of the same shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum RunState {
    /// Constructed but `refresh()` not yet entered (the inert initial state).
    #[default]
    Created,
    /// Inside `Context::refresh().await` (the inert→live bring-up).
    Refreshing,
    /// `refresh()` completed; `start_all()` ran; the container is serving.
    Running,
    /// Inside `Context::shutdown().await`, before `stop_all()` (quiescing).
    Stopping,
    /// `stop_all()` done; draining the `TeardownLedger` LIFO.
    Closing,
    /// Cleanly closed (the normal terminal). Mutually exclusive with [`Failed`].
    ///
    /// [`Failed`]: RunState::Failed
    Closed,
    /// A bring-up step faulted: cancel-cascade ran, `stop_all` + `Closed` were
    /// SKIPPED. The cancel terminal; structurally suppresses [`Closed`].
    ///
    /// [`Closed`]: RunState::Closed
    Failed,
}

impl RunState {
    /// A short, stable slug (rendering / tests / `leaf doctor`).
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            RunState::Created => "created",
            RunState::Refreshing => "refreshing",
            RunState::Running => "running",
            RunState::Stopping => "stopping",
            RunState::Closing => "closing",
            RunState::Closed => "closed",
            RunState::Failed => "failed",
        }
    }

    /// `true` iff this is a terminal state — the keep-alive predicate.
    ///
    /// `RunState ∈ {Closed, Failed}` is the informal "Terminated" superset that
    /// `exit-code-shutdown`'s keep-alive awaits; no separate `Terminated`
    /// variant exists (SEAMS seam #1 (D)).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, RunState::Closed | RunState::Failed)
    }

    /// `true` iff this is part of the `RunDown` tail (`Stopping`/`Closing`/
    /// `Closed`/`Failed`). `RunDown` is NOT a separate type — it is this tail.
    #[must_use]
    pub const fn is_rundown(self) -> bool {
        matches!(
            self,
            RunState::Stopping | RunState::Closing | RunState::Closed | RunState::Failed
        )
    }

    /// `true` iff a direct `self → next` transition is structurally legal.
    ///
    /// Encodes the SEAMS seam #1 (D) template: the linear bring-up/teardown spine
    /// plus the always-available cancel jump to [`Failed`](RunState::Failed) from any non-terminal
    /// state. `Closed` and `Failed` are terminal (no outgoing edge); the
    /// cancel-vs-close fork is structural (you reach `Failed` XOR `Closed`,
    /// never both).
    #[must_use]
    pub const fn can_transition_to(self, next: RunState) -> bool {
        use RunState::{Closed, Closing, Created, Failed, Refreshing, Running, Stopping};
        match (self, next) {
            // Linear bring-up.
            (Created, Refreshing) => true,
            (Refreshing, Running) => true,
            // Linear teardown (valid only from Running, per the CAS close-once).
            (Running, Stopping) => true,
            (Stopping, Closing) => true,
            (Closing, Closed) => true,
            // Cancel-cascade: any non-terminal phase can fault to Failed.
            (Created | Refreshing | Running | Stopping | Closing, Failed) => true,
            // Terminal states have no outgoing edge; everything else illegal.
            _ => false,
        }
    }
}

// ─────────────────────── the watch-cell ABI (std-based) ─────────────────────

/// Shared inner state of one watch cell: the current value, a monotonic version
/// counter, and the parked receiver wakers.
///
/// The version counter is what makes "changed since I last saw it" cheap and
/// lost-wakeup-free: a receiver remembers the version it last observed, and a
/// send bumps the version then wakes everyone. There is exactly ONE `Mutex`
/// per cell (fine-grained, never a process-global lock); it is held only to
/// swap the value, bump the counter, and drain the waker list.
struct WatchInner<T> {
    state: Mutex<WatchState<T>>,
    version: AtomicU64,
}

struct WatchState<T> {
    value: T,
    wakers: VecDeque<Waker>,
}

/// The publishing half of a watch cell — held by the ONE [`RunState`] publisher
/// (the leaf-boot refresh/teardown template). Cloneable so the bring-up and
/// teardown drivers can both publish, but there is logically one owner.
pub struct WatchSender<T> {
    inner: Arc<WatchInner<T>>,
}

/// The subscribing half of a watch cell — held by every consumer that needs to
/// react to phase transitions. Cheap to [`Clone`] (one `Arc` bump).
pub struct WatchReceiver<T> {
    inner: Arc<WatchInner<T>>,
    /// The version this receiver has already observed; [`changed`] resolves once
    /// the cell's version moves past it.
    ///
    /// [`changed`]: WatchReceiver::changed
    seen: u64,
}

impl<T: Clone> WatchSender<T> {
    /// Publish a new value, bumping the version and waking all parked receivers.
    ///
    /// Idempotent in the sense that it always bumps the version and wakes — even
    /// if the new value equals the old — so a publisher need not diff; callers
    /// that want change-suppression compare before sending.
    pub fn send(&self, value: T) {
        let wakers = {
            let mut st = self.inner.state.lock().expect("watch mutex poisoned");
            st.value = value;
            // Bump AFTER the value is in place so a receiver that observes the
            // new version is guaranteed to read the new value.
            self.inner.version.fetch_add(1, Ordering::Release);
            std::mem::take(&mut st.wakers)
        };
        for w in wakers {
            w.wake();
        }
    }

    /// Read the current value (a clone-out snapshot).
    #[must_use]
    pub fn borrow(&self) -> T {
        self.inner.state.lock().expect("watch mutex poisoned").value.clone()
    }

    /// Mint a fresh receiver subscribed from the CURRENT version (so its first
    /// [`changed`](WatchReceiver::changed) awaits the *next* transition).
    #[must_use]
    pub fn subscribe(&self) -> WatchReceiver<T> {
        WatchReceiver {
            inner: Arc::clone(&self.inner),
            seen: self.inner.version.load(Ordering::Acquire),
        }
    }
}

impl<T: Clone> WatchReceiver<T> {
    /// Read the current value (a clone-out snapshot). Does NOT mark it observed.
    #[must_use]
    pub fn borrow(&self) -> T {
        self.inner.state.lock().expect("watch mutex poisoned").value.clone()
    }

    /// Read the current value AND mark this receiver caught up, so the next
    /// [`changed`](WatchReceiver::changed) awaits a genuinely later transition.
    #[must_use]
    pub fn borrow_and_update(&mut self) -> T {
        self.seen = self.inner.version.load(Ordering::Acquire);
        self.inner.state.lock().expect("watch mutex poisoned").value.clone()
    }

    /// Resolve once the cell changes past the version this receiver last saw.
    ///
    /// Returns the new value and advances the receiver's observed version. This
    /// is the charter §2.4 reactive primitive: subscribe once and `await` —
    /// NEVER spin on `borrow()`.
    pub fn changed(&mut self) -> Changed<'_, T> {
        Changed { rx: self }
    }

    /// Resolve once the value satisfies `pred` (re-checked on every transition).
    ///
    /// The terminal-state await (`wait_for(|s| s.is_terminal())`) the keep-alive
    /// uses. Checks the current value first, so an already-satisfied predicate
    /// resolves immediately.
    pub fn wait_for<F>(&mut self, pred: F) -> WaitFor<'_, T, F>
    where
        F: FnMut(&T) -> bool,
    {
        WaitFor { rx: self, pred }
    }
}

impl<T> Clone for WatchReceiver<T> {
    fn clone(&self) -> Self {
        WatchReceiver {
            inner: Arc::clone(&self.inner),
            seen: self.seen,
        }
    }
}

impl<T> Clone for WatchSender<T> {
    fn clone(&self) -> Self {
        WatchSender {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// The future returned by [`WatchReceiver::changed`].
#[must_use = "futures do nothing unless awaited"]
pub struct Changed<'a, T> {
    rx: &'a mut WatchReceiver<T>,
}

impl<T: Clone> std::future::Future for Changed<'_, T> {
    type Output = T;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        let inner = &this.rx.inner;
        let current = inner.version.load(Ordering::Acquire);
        if current != this.rx.seen {
            this.rx.seen = current;
            return Poll::Ready(inner.state.lock().expect("watch mutex poisoned").value.clone());
        }
        // Register under the lock, then re-check the version to close the
        // lost-wakeup race (a send between our load and our park would have
        // taken an empty waker list).
        let mut st = inner.state.lock().expect("watch mutex poisoned");
        let recheck = inner.version.load(Ordering::Acquire);
        if recheck != this.rx.seen {
            this.rx.seen = recheck;
            return Poll::Ready(st.value.clone());
        }
        st.wakers.push_back(cx.waker().clone());
        Poll::Pending
    }
}

/// The future returned by [`WatchReceiver::wait_for`].
#[must_use = "futures do nothing unless awaited"]
pub struct WaitFor<'a, T, F> {
    rx: &'a mut WatchReceiver<T>,
    pred: F,
}

impl<T, F> std::future::Future for WaitFor<'_, T, F>
where
    T: Clone,
    F: FnMut(&T) -> bool + Unpin,
{
    type Output = T;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        let inner = &this.rx.inner;
        loop {
            let current = inner.version.load(Ordering::Acquire);
            {
                let st = inner.state.lock().expect("watch mutex poisoned");
                if (this.pred)(&st.value) {
                    this.rx.seen = current;
                    return Poll::Ready(st.value.clone());
                }
            }
            // Park, then re-check the version to avoid a lost wakeup.
            let mut st = inner.state.lock().expect("watch mutex poisoned");
            let recheck = inner.version.load(Ordering::Acquire);
            if recheck != current {
                // A send landed; loop to re-evaluate the predicate.
                continue;
            }
            st.wakers.push_back(cx.waker().clone());
            this.rx.seen = current;
            return Poll::Pending;
        }
    }
}

/// Construct a fresh [`RunState`] watch cell, seeded at [`RunState::Created`].
///
/// Returns the publishing [`WatchSender`] (held by the ONE leaf-boot
/// refresh/teardown publisher) and an initial [`WatchReceiver`]. Per SEAMS seam
/// #1 (D): there is exactly ONE `watch<RunState>` cell; consumers `subscribe()`
/// more receivers off the sender. NO tokio, NO global lock.
#[must_use]
pub fn run_state_channel() -> (RunStateSender, RunStateReceiver) {
    let inner = Arc::new(WatchInner {
        state: Mutex::new(WatchState {
            value: RunState::Created,
            wakers: VecDeque::new(),
        }),
        version: AtomicU64::new(0),
    });
    let tx = WatchSender { inner: Arc::clone(&inner) };
    let rx = WatchReceiver { inner, seen: 0 };
    (tx, rx)
}

/// Construct a fresh generic watch cell seeded at `initial`.
///
/// The same std-based, single-`Mutex`, version-counter cell [`run_state_channel`]
/// builds for [`RunState`], but generic over any `T` — used by the availability
/// cells (`watch<LivenessState>` / `watch<ReadinessState>`, events phase3/12 5f)
/// and any other reactive-state axis. NO tokio, NO global lock; the cell is
/// owned by whoever holds the [`WatchSender`].
#[must_use]
pub fn watch_channel<T>(initial: T) -> (WatchSender<T>, WatchReceiver<T>) {
    let inner = Arc::new(WatchInner {
        state: Mutex::new(WatchState { value: initial, wakers: VecDeque::new() }),
        version: AtomicU64::new(0),
    });
    let tx = WatchSender { inner: Arc::clone(&inner) };
    let rx = WatchReceiver { inner, seen: 0 };
    (tx, rx)
}

/// The publishing half of the one `watch<RunState>` cell.
pub type RunStateSender = WatchSender<RunState>;

/// A subscribing half of the one `watch<RunState>` cell — what
/// [`watch_run_state`] hands a consumer.
pub type RunStateReceiver = WatchReceiver<RunState>;

/// The process-wide [`RunState`] watch sender, lazily created on first access.
///
/// SEAMS seam #1 (D) names ONE `watch<RunState>` cell; this is its ambient home
/// so [`watch_run_state`] can hand out receivers without threading the sender
/// through every API. leaf-boot's refresh/teardown template publishes through
/// [`run_state_sender`]; everyone else subscribes via [`watch_run_state`].
static RUN_STATE: once_cell::sync::Lazy<RunStateSender> =
    once_cell::sync::Lazy::new(|| run_state_channel().0);

/// The ONE process-wide [`RunState`] publisher (the leaf-boot template's handle).
///
/// Returns a clone of the ambient sender. There is logically a single publisher
/// (the refresh/teardown driver); this accessor exists so that driver — wherever
/// it is constructed — reaches the same cell every subscriber reads.
#[must_use]
pub fn run_state_sender() -> RunStateSender {
    RUN_STATE.clone()
}

/// Subscribe to the process-wide [`RunState`] cell (charter §2.4).
///
/// `await` a transition via [`RunStateReceiver::changed`] /
/// [`wait_for`](WatchReceiver::wait_for) — NEVER poll `is_running` in a loop.
/// The keep-alive awaits `wait_for(|s| s.is_terminal())`.
#[must_use]
pub fn watch_run_state() -> RunStateReceiver {
    RUN_STATE.subscribe()
}

// ─────────────────────── Lifecycle / Shutdown seams ─────────────────────────

/// A managed component with explicit, ordered start/stop (Spring's
/// `SmartLifecycle` analogue), driven by `start_all()`/`stop_all()` at refresh
/// R7 / teardown step 4.
///
/// Object-safe: futures box at the `dyn` seam ([`BoxFuture`]). There is NO async
/// `Drop` — graceful stop is this explicit awaited call, NOT a destructor.
/// Ordering across participants is the integer-`Phase` axis (ASC on start, DESC
/// on stop), the orthogonal non-`RoleTier` axis sorted by
/// [`cmp_order`](crate::cmp_order).
pub trait Lifecycle: Send + Sync {
    /// Start this component (refresh R7, `start_all()` ASC by phase).
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the component cannot start; the bring-up
    /// driver routes it into the cancel-cascade.
    fn start(&self) -> BoxFuture<'_, Result<(), LeafError>>;

    /// Gracefully stop this component (teardown step 4, `stop_all()` DESC).
    ///
    /// # Errors
    /// Returns a [`LeafError`] aggregated into the `ShutdownReport`; a stop
    /// fault is reported, never re-thrown to abort the rest of teardown.
    fn stop_graceful(&self) -> BoxFuture<'_, Result<(), LeafError>>;

    /// `true` iff the component is currently running. This is a CHEAP local
    /// query for the driver's bookkeeping — consumers reacting to container
    /// phase use [`watch_run_state`], never an `is_running` spin.
    fn is_running(&self) -> bool;

    /// The integer lifecycle phase (Spring's `getPhase()`); lower starts first,
    /// stops last. Defaults to `0`. Sorted by [`cmp_order`](crate::cmp_order).
    fn phase(&self) -> i32 {
        0
    }
}

/// An explicit teardown participant drained from the `TeardownLedger` LIFO at
/// teardown step 5 (the destroy-method analogue) — distinct from [`Lifecycle`]
/// (which is the runtime stop, step 4).
///
/// Object-safe; boxes its future. NO async `Drop`: this awaited call IS the
/// teardown path. The `Arc` strong-count governs the *real* memory free; this
/// is the *logical* close (flushing a pool, closing a connection).
pub trait Shutdown: Send + Sync {
    /// Logically close this resource (teardown step 5, ledger LIFO drain).
    ///
    /// # Errors
    /// Returns a [`LeafError`] aggregated into the `ShutdownReport`; a fault is
    /// reported, never aborts the rest of the drain.
    fn shutdown(&self) -> BoxFuture<'_, Result<(), LeafError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as O};

    // ── RunState transition validity ──

    #[test]
    fn happy_path_transitions_are_legal() {
        use RunState::*;
        assert!(Created.can_transition_to(Refreshing));
        assert!(Refreshing.can_transition_to(Running));
        assert!(Running.can_transition_to(Stopping));
        assert!(Stopping.can_transition_to(Closing));
        assert!(Closing.can_transition_to(Closed));
    }

    #[test]
    fn cancel_cascade_to_failed_from_any_nonterminal() {
        use RunState::*;
        for s in [Created, Refreshing, Running, Stopping, Closing] {
            assert!(s.can_transition_to(Failed), "{s:?} -> Failed must be legal");
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_edge() {
        use RunState::*;
        for next in [Created, Refreshing, Running, Stopping, Closing, Closed, Failed] {
            assert!(!Closed.can_transition_to(next), "Closed is terminal");
            assert!(!Failed.can_transition_to(next), "Failed is terminal");
        }
    }

    #[test]
    fn illegal_skips_are_rejected() {
        use RunState::*;
        // Cannot skip Refreshing, cannot teardown from non-Running, cannot
        // resurrect, cannot go Failed -> Closed (the structural fork).
        assert!(!Created.can_transition_to(Running));
        assert!(!Created.can_transition_to(Stopping));
        assert!(!Refreshing.can_transition_to(Stopping));
        assert!(!Closing.can_transition_to(Running));
        assert!(!Failed.can_transition_to(Closed));
        assert!(!Closed.can_transition_to(Failed));
    }

    #[test]
    fn rundown_and_terminal_predicates() {
        use RunState::*;
        assert!(!Created.is_rundown() && !Refreshing.is_rundown() && !Running.is_rundown());
        for s in [Stopping, Closing, Closed, Failed] {
            assert!(s.is_rundown());
        }
        assert!(Closed.is_terminal() && Failed.is_terminal());
        assert!(!Running.is_terminal() && !Stopping.is_terminal());
    }

    // ── watch cell ──

    #[test]
    fn watch_seeds_at_created_and_reflects_sends() {
        let (tx, rx) = run_state_channel();
        assert_eq!(rx.borrow(), RunState::Created);
        tx.send(RunState::Refreshing);
        assert_eq!(rx.borrow(), RunState::Refreshing);
        assert_eq!(tx.borrow(), RunState::Refreshing);
    }

    #[test]
    fn changed_resolves_on_transition() {
        let (tx, mut rx) = run_state_channel();
        tx.send(RunState::Refreshing);
        let got = block_on(rx.changed());
        assert_eq!(got, RunState::Refreshing);
    }

    #[test]
    fn changed_does_not_resolve_without_a_send() {
        // Hand-drive the future: it must be Pending until a send bumps the cell.
        let (tx, mut rx) = run_state_channel();
        let mut fut = Box::pin(rx.changed());
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        tx.send(RunState::Refreshing);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(RunState::Refreshing)));
    }

    #[test]
    fn changed_wakes_the_registered_waker() {
        struct Flag(Arc<AtomicBool>);
        impl std::task::Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, O::SeqCst);
            }
        }
        let (tx, mut rx) = run_state_channel();
        let flag = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(Flag(flag.clone())));
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(rx.changed());
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        assert!(!flag.load(O::SeqCst));
        tx.send(RunState::Refreshing);
        assert!(flag.load(O::SeqCst), "send must wake the parked receiver");
    }

    #[test]
    fn wait_for_terminal_resolves_after_full_lifecycle() {
        let (tx, mut rx) = run_state_channel();
        // Drive the cell through to Closed on another step, then await terminal.
        tx.send(RunState::Refreshing);
        tx.send(RunState::Running);
        tx.send(RunState::Stopping);
        tx.send(RunState::Closing);
        tx.send(RunState::Closed);
        let got = block_on(rx.wait_for(|s| s.is_terminal()));
        assert_eq!(got, RunState::Closed);
    }

    #[test]
    fn wait_for_already_satisfied_is_immediate() {
        let (tx, mut rx) = run_state_channel();
        tx.send(RunState::Failed);
        // Already terminal: must resolve on the first poll, no further send.
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(rx.wait_for(|s| s.is_terminal()));
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(RunState::Failed)));
    }

    #[test]
    fn process_wide_watch_run_state_sees_publisher_sends() {
        // The ambient cell: a subscriber must observe a transition published by
        // the ambient sender. (Seeded value may be non-Created if other tests
        // already published, so assert on the transition, not the initial value.)
        let mut rx = watch_run_state();
        let tx = run_state_sender();
        let before = rx.borrow_and_update();
        // Send a value guaranteed different from `before` to force a transition.
        let next = if before == RunState::Running {
            RunState::Stopping
        } else {
            RunState::Running
        };
        tx.send(next);
        let got = block_on(rx.changed());
        assert_eq!(got, next);
    }

    #[test]
    fn subscribe_starts_from_current_version() {
        let (tx, _rx0) = run_state_channel();
        tx.send(RunState::Refreshing);
        let mut late = tx.subscribe();
        // The late subscriber sees the current value but its `changed` awaits
        // the NEXT transition, not the historical one.
        assert_eq!(late.borrow(), RunState::Refreshing);
        let mut fut = Box::pin(late.changed());
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        tx.send(RunState::Running);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(RunState::Running)));
    }

    // ── Lifecycle / Shutdown dyn-compatibility smoke ──

    struct Comp {
        running: AtomicBool,
        starts: AtomicU32,
    }
    impl Lifecycle for Comp {
        fn start(&self) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async move {
                self.running.store(true, O::SeqCst);
                self.starts.fetch_add(1, O::SeqCst);
                Ok(())
            })
        }
        fn stop_graceful(&self) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async move {
                self.running.store(false, O::SeqCst);
                Ok(())
            })
        }
        fn is_running(&self) -> bool {
            self.running.load(O::SeqCst)
        }
        fn phase(&self) -> i32 {
            7
        }
    }

    #[test]
    fn lifecycle_is_object_safe_and_drives() {
        let c = Comp {
            running: AtomicBool::new(false),
            starts: AtomicU32::new(0),
        };
        let dynamic: &dyn Lifecycle = &c;
        assert!(!dynamic.is_running());
        assert_eq!(dynamic.phase(), 7);
        block_on(dynamic.start()).unwrap();
        assert!(dynamic.is_running());
        block_on(dynamic.stop_graceful()).unwrap();
        assert!(!dynamic.is_running());
        assert_eq!(c.starts.load(O::SeqCst), 1);
    }

    struct Pool;
    impl Shutdown for Pool {
        fn shutdown(&self) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn shutdown_is_object_safe() {
        let p = Pool;
        let dynamic: &dyn Shutdown = &p;
        block_on(dynamic.shutdown()).unwrap();
    }
}
