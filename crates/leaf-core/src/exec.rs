//! The runtime-agnostic execution + scheduling ABI (ADR-07 5c/5f, phase3/10).
//!
//! NO runtime is named here — only the traits and the const data the macros
//! emit. The concrete impls (`spawn`/`spawn_blocking`/`Semaphore`, the reactive
//! timer-wheel) live in `leaf-tokio`/`leaf-smol`.
//!
//! - **task-execution** — the capability split: [`Spawner`] / [`BlockingOffload`]
//!   / [`ConcurrencyGate`], composed by the ONE [`ExecutionFacility`] supertrait
//!   (`Role::Infrastructure` bean). Consumers depend on EXACTLY the capability
//!   they need, so "this code may block" is a type-level property. Each boxes
//!   its future at the `dyn` seam ([`BoxFuture`]). [`SpawnHandle`] awaits to
//!   `Result<(), JoinError>`; DROP = abort, with [`DropPolicy::Detach`] for
//!   fire-and-forget. [`Permit`] is RAII: `Drop` releases even on cancel
//!   (limit-1 = an instance lock — the ONE bounded-concurrency primitive shared
//!   by retry + scheduling overlap). The [`SpawnableWork`] doctrine diagnostic
//!   enforces the uniform `Send + 'static`.
//! - **scheduling** — a TIME-AWARE capability over the SAME [`Spawner`], NOT a
//!   second executor: a SYNC [`Trigger`] SPI (kept sync to avoid boxing the hot
//!   timing path) with [`TriggerContext`] + built-in [`FixedRateTrigger`] /
//!   [`FixedDelayTrigger`] (the calendar [`TriggerSpec::Cron`] engine is
//!   `leaf-cron`), the [`SchedulerCore`] registration/quiesce seam (one reactive
//!   timer-wheel that spawns the due body onto the [`Spawner`] — the body NEVER
//!   runs on the driver), and the const [`ScheduledMethodDescriptor`] the thin
//!   `#[scheduled]` macro emits into the [`SCHEDULED`](crate::SCHEDULED) slice.

use std::time::{Duration, Instant};

use crate::discovery::{ScheduledRow, SCHEDULED};
use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::identity::ContractId;

// ════════════════════════════ task-execution ════════════════════════════════

/// Why a spawned task did not produce its value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JoinError {
    /// The task was cancelled (its [`SpawnHandle`] was dropped, or shutdown
    /// aborted it past the drain deadline).
    Cancelled,
    /// The task panicked (the runtime caught the unwind).
    Panicked,
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Cancelled => f.write_str("spawned task was cancelled"),
            JoinError::Panicked => f.write_str("spawned task panicked"),
        }
    }
}

impl std::error::Error for JoinError {}

/// What happens to a spawned task when its [`SpawnHandle`] is dropped.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum DropPolicy {
    /// Drop = abort (cancel at the next `.await`) — the structured default.
    #[default]
    Abort,
    /// Drop = detach (fire-and-forget): `@Async`, async-event, scheduled fire.
    /// Detached tasks are drained at shutdown then aborted past the deadline.
    Detach,
}

/// The runtime-supplied backing of a [`SpawnHandle`] — the join/abort seam.
///
/// The runtime (`leaf-tokio`'s `JoinHandle`, `leaf-smol`'s `Task`) impls this so
/// core never names a runtime. `poll_join` is the awaitable completion; `abort`
/// requests cancellation; `detach` consumes the handle's abort-on-drop behavior.
pub trait JoinSeam: Send {
    /// Poll the task to completion (the awaitable join).
    fn poll_join(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), JoinError>>;

    /// Request cancellation (cooperative; takes effect at the next `.await`).
    fn abort(&self);

    /// Detach: the task keeps running after the handle drops (fire-and-forget).
    fn detach(&self);
}

/// A handle to a spawned task. `.await` => `Result<(), JoinError>`.
///
/// DROP aborts the task ([`DropPolicy::Abort`], the structured default) unless
/// [`detach`](SpawnHandle::detach)ed first. The handle is runtime-agnostic: it
/// boxes a [`JoinSeam`] the runtime supplies.
#[must_use = "dropping a SpawnHandle aborts the task unless you .detach() it"]
pub struct SpawnHandle {
    seam: std::pin::Pin<Box<dyn JoinSeam>>,
    policy: DropPolicy,
    /// Set once `detach`/await consumed the seam, so `Drop` does not double-act.
    consumed: bool,
}

impl SpawnHandle {
    /// Build a handle over a runtime [`JoinSeam`] with the default
    /// [`DropPolicy::Abort`].
    pub fn new(seam: Box<dyn JoinSeam>) -> Self {
        SpawnHandle {
            seam: Box::into_pin(seam),
            policy: DropPolicy::Abort,
            consumed: false,
        }
    }

    /// Set the [`DropPolicy`] (builder style).
    pub fn with_policy(mut self, policy: DropPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The current [`DropPolicy`].
    #[must_use]
    pub fn policy(&self) -> DropPolicy {
        self.policy
    }

    /// Request cancellation now (cooperative).
    pub fn abort(&self) {
        self.seam.abort();
    }

    /// Detach: keep the task running after this handle drops (fire-and-forget).
    pub fn detach(mut self) {
        self.seam.detach();
        self.consumed = true;
        // `self` drops here; `consumed` suppresses the abort-on-drop.
    }
}

impl std::future::Future for SpawnHandle {
    type Output = Result<(), JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let out = self.seam.as_mut().poll_join(cx);
        if out.is_ready() {
            self.consumed = true;
        }
        out
    }
}

impl Drop for SpawnHandle {
    fn drop(&mut self) {
        // Abort-on-drop unless detached, completed, or policy is Detach.
        if !self.consumed && self.policy == DropPolicy::Abort {
            self.seam.abort();
        } else if !self.consumed && self.policy == DropPolicy::Detach {
            self.seam.detach();
        }
    }
}

impl std::fmt::Debug for SpawnHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnHandle")
            .field("policy", &self.policy)
            .field("consumed", &self.consumed)
            .finish()
    }
}

/// A handle to an offloaded blocking job. `.await` => `Result<(), JoinError>`.
///
/// Mirrors [`SpawnHandle`] but for the dedicated blocking pool (Spring's
/// `spawn_blocking` analogue). A blocking job cannot be cooperatively cancelled
/// once running, so DROP detaches by default (the work runs to completion).
#[must_use = "await a BlockingHandle to observe its completion"]
pub struct BlockingHandle {
    seam: std::pin::Pin<Box<dyn JoinSeam>>,
    consumed: bool,
}

impl BlockingHandle {
    /// Build a handle over a runtime [`JoinSeam`].
    pub fn new(seam: Box<dyn JoinSeam>) -> Self {
        BlockingHandle {
            seam: Box::into_pin(seam),
            consumed: false,
        }
    }
}

impl std::future::Future for BlockingHandle {
    type Output = Result<(), JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let out = self.seam.as_mut().poll_join(cx);
        if out.is_ready() {
            self.consumed = true;
        }
        out
    }
}

impl Drop for BlockingHandle {
    fn drop(&mut self) {
        // Blocking work cannot be cancelled mid-run; detach so it completes.
        if !self.consumed {
            self.seam.detach();
        }
    }
}

/// The RAII release seam a [`Permit`] holds — the runtime's semaphore guard.
///
/// Dropping the [`Permit`] runs the runtime's release (returns the slot). The
/// seam is boxed so `Permit` is runtime-agnostic; a real runtime supplies the
/// owned semaphore guard.
pub trait PermitSeam: Send {}

/// A RAII concurrency permit (ADR-07 5c) — the ONE bounded-concurrency primitive.
///
/// `Drop` releases the slot EVEN on cancel (no leak across a backoff sleep). A
/// limit-1 gate makes [`Permit`] a declarative instance lock. Shared by
/// retry-resilience's `@ConcurrencyLimit` and scheduling's overlap ceiling.
#[must_use = "a Permit releases its slot when dropped; hold it for the guarded region"]
pub struct Permit {
    // The runtime guard; `Drop` of the box runs the runtime release. `Option`
    // so an explicit `release()` can drop it early without a double-release.
    seam: Option<Box<dyn PermitSeam>>,
}

impl Permit {
    /// Wrap a runtime release guard.
    pub fn new(seam: Box<dyn PermitSeam>) -> Self {
        Permit { seam: Some(seam) }
    }

    /// A permit that owns no runtime slot (a limit-∞ / test gate).
    pub fn unbounded() -> Self {
        Permit { seam: None }
    }

    /// Release the slot now (idempotent; `Drop` is the usual path).
    pub fn release(&mut self) {
        self.seam = None;
    }
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit")
            .field("held", &self.seam.is_some())
            .finish()
    }
}

/// Spawn work on the executor (the `where`). Object-safe; boxes its future.
pub trait Spawner: Send + Sync {
    /// Spawn a `Send + 'static` future; returns its [`SpawnHandle`].
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> SpawnHandle;
}

/// Offload synchronous blocking work to a dedicated pool (Spring's
/// `spawn_blocking`). Depending on this is the type-level "this code may block".
pub trait BlockingOffload: Send + Sync {
    /// Run a `Send + 'static` blocking closure on the blocking pool.
    fn run_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BlockingHandle;
}

/// Acquire a bounded [`Permit`] (the ONE shared bounded-concurrency primitive).
pub trait ConcurrencyGate: Send + Sync {
    /// Acquire a permit, awaiting if the gate is saturated.
    fn acquire(&self) -> BoxFuture<'static, Permit>;
}

/// THE composing execution facility (`Role::Infrastructure`, ADR-07 5c).
///
/// One bean implementing all three capabilities over one runtime (the
/// `applicationTaskExecutor` identity). Consumers usually depend on the narrowest
/// capability (`Ref<dyn Spawner>` etc.); this supertrait is the whole-facility
/// handle and the auto-detect target at refresh R2 (a missing facility HARD-FAILS
/// the self-check). The concrete `ExecutionFacility` STRUCT lives in
/// `leaf-tokio`/`leaf-smol`; core defines only this runtime-agnostic supertrait.
pub trait ExecutionFacility: Spawner + BlockingOffload + ConcurrencyGate {}

/// Blanket impl: anything that supplies all three capabilities IS a facility.
impl<T: Spawner + BlockingOffload + ConcurrencyGate> ExecutionFacility for T {}

/// The doctrine bound for spawnable work, with the migration-grade diagnostic.
///
/// Any future/closure run on leaf's executor must be `Send + 'static` (ADR-07 5e,
/// the uniform discipline riding the [`Bean`](crate::Bean)/
/// [`ErasedBean`](crate::ErasedBean) bound). The `#[diagnostic::on_unimplemented]`
/// turns the cryptic auto-trait error into the actionable hint.
#[diagnostic::on_unimplemented(
    message = "async work `{Self}` must be `Send + 'static` to run on leaf's executor",
    note = "move blocking work to `BlockingOffload`, or make per-interaction state prototype/request-scoped"
)]
pub trait SpawnableWork: Send + 'static {}

impl<T: Send + 'static> SpawnableWork for T {}

/// The ONE container-level sink for detached-task panics + failed scheduled
/// fires (ADR-07 5d). Routes into the [`LeafError`] chain via the open
/// `Integration { ContractId }` arm. Object-safe; boxes its future.
pub trait AsyncUncaughtFailureHandler: Send + Sync {
    /// Handle an uncaught failure from a detached task / failed scheduled fire.
    fn handle(&self, error: LeafError) -> BoxFuture<'_, ()>;
}

// ════════════════════════════════ scheduling ════════════════════════════════

/// The feedback a [`Trigger`] computes its next fire from (Spring's
/// `TriggerContext`).
///
/// `last_scheduled` (when the previous fire was *meant* to run) drives
/// fixed-rate + cron; `last_completion` (when the previous body *finished*)
/// drives fixed-delay (getting this wrong silently turns fixedDelay into
/// fixedRate). `last_actual_fire` is when it actually started.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TriggerContext {
    /// When the previous fire was scheduled to run, if any.
    pub last_scheduled: Option<Instant>,
    /// When the previous body completed, if any (fixedDelay feedback).
    pub last_completion: Option<Instant>,
    /// When the previous fire actually started, if any.
    pub last_actual_fire: Option<Instant>,
}

impl TriggerContext {
    /// The initial context (no prior fire).
    #[must_use]
    pub const fn initial() -> Self {
        TriggerContext {
            last_scheduled: None,
            last_completion: None,
            last_actual_fire: None,
        }
    }
}

/// The SYNC next-fire SPI (ADR-07 5c) — kept sync to avoid boxing the hot timing
/// path. `None` means "no further fire" (a one-shot that has fired).
pub trait Trigger: Send + Sync {
    /// Compute the next fire `Instant` from `now` + the feedback `ctx`.
    fn next_fire(&self, now: Instant, ctx: TriggerContext) -> Option<Instant>;
}

/// Fixed-RATE: fire every `period` measured from the previous SCHEDULED time
/// (so a slow body does not push the cadence — it can overlap, bounded by the
/// [`ConcurrencyGate`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FixedRateTrigger {
    /// The cadence period.
    pub period: Duration,
    /// An optional one-time initial delay before the first fire.
    pub initial_delay: Duration,
}

impl FixedRateTrigger {
    /// A fixed-rate trigger with no initial delay.
    #[must_use]
    pub const fn new(period: Duration) -> Self {
        FixedRateTrigger {
            period,
            initial_delay: Duration::ZERO,
        }
    }

    /// Set the initial delay (builder style).
    #[must_use]
    pub const fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }
}

impl Trigger for FixedRateTrigger {
    fn next_fire(&self, now: Instant, ctx: TriggerContext) -> Option<Instant> {
        Some(match ctx.last_scheduled {
            // Subsequent fires: previous SCHEDULED + period (rate, not delay).
            Some(prev) => prev + self.period,
            // First fire: now + initial delay.
            None => now + self.initial_delay,
        })
    }
}

/// Fixed-DELAY: fire `delay` after the previous body COMPLETED (so a slow body
/// pushes the next fire — the completion-feedback contract).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FixedDelayTrigger {
    /// The delay measured from the previous completion.
    pub delay: Duration,
    /// An optional one-time initial delay before the first fire.
    pub initial_delay: Duration,
}

impl FixedDelayTrigger {
    /// A fixed-delay trigger with no initial delay.
    #[must_use]
    pub const fn new(delay: Duration) -> Self {
        FixedDelayTrigger {
            delay,
            initial_delay: Duration::ZERO,
        }
    }

    /// Set the initial delay (builder style).
    #[must_use]
    pub const fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }
}

impl Trigger for FixedDelayTrigger {
    fn next_fire(&self, now: Instant, ctx: TriggerContext) -> Option<Instant> {
        Some(match ctx.last_completion {
            // Subsequent fires: previous COMPLETION + delay (the contract).
            Some(done) => done + self.delay,
            // First fire: now + initial delay.
            None => now + self.initial_delay,
        })
    }
}

/// What to do when a fire is due while the previous body is still running.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum OverlapPolicy {
    /// Skip this fire (async has no implicit ceiling — the inverted gotcha).
    /// The DEFAULT (data-safe).
    #[default]
    SkipIfRunning,
    /// Queue the fire to run after the current body completes.
    Queue,
    /// Allow concurrent bodies, bounded by the shared [`ConcurrencyGate`].
    AllowConcurrent,
}

/// The const trigger spec the `#[scheduled(...)]` macro emits (data only).
///
/// `Cron` carries the unparsed expression — the 6/7-field calendar engine
/// (`leaf-cron`) parses it at the Tier-2 startup pass; core never parses cron.
/// `${...}` placeholders in any spec are resolved by value-injection before the
/// startup pass (config-bound specs are ordinary placeholders, not a separate
/// schedule table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TriggerSpec {
    /// A 6/7-field cron expression (parsed by `leaf-cron` at startup).
    Cron(&'static str),
    /// Fire every N (fixed-rate from the previous scheduled time).
    FixedRate {
        /// Period.
        period: Duration,
        /// One-time initial delay.
        initial_delay: Duration,
    },
    /// Fire N after the previous completion (fixed-delay).
    FixedDelay {
        /// Delay from completion.
        delay: Duration,
        /// One-time initial delay.
        initial_delay: Duration,
    },
}

/// Identity of a scheduled method on its bean (the `#[scheduled]` target).
///
/// A stable [`ContractId`] over the `bean::method` canonical path (the same one
/// hash, never a `TypeId`), so a dropped descriptor is detectable by the
/// anti-DCE expected-vs-found self-check.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MethodKey(pub ContractId);

impl MethodKey {
    /// Mint a method key from a canonical `bean::method` path (const).
    #[must_use]
    pub const fn of(canonical_path: &str) -> Self {
        MethodKey(ContractId::of(canonical_path))
    }
}

/// The const descriptor the thin `#[scheduled(...)]` macro emits into the
/// [`SCHEDULED`](crate::SCHEDULED) slice (data only, charter 2.10).
///
/// An instance-tier post-processor at `after_init` binds each descriptor to the
/// live bean `Ref`, resolves its [`Trigger`] from the [`spec`], and registers
/// `(Trigger, body)` into the [`SchedulerCore`] — arming only after the
/// SmartInitializing all-singletons barrier (refresh R6) so a fire never hits a
/// half-built graph.
///
/// [`spec`]: ScheduledMethodDescriptor::spec
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduledMethodDescriptor {
    /// The owning bean's stable identity.
    pub bean: ContractId,
    /// The scheduled method's identity on that bean.
    pub method: MethodKey,
    /// The const trigger spec (cron / fixed-rate / fixed-delay).
    pub spec: TriggerSpec,
    /// The overlap policy (default skip-if-running).
    pub overlap: OverlapPolicy,
    /// An optional facility qualifier (which executor to spawn the body onto).
    pub qualifier: Option<&'static str>,
}

impl ScheduledMethodDescriptor {
    /// A minimal descriptor: bean + method + spec, default overlap, no qualifier.
    #[must_use]
    pub const fn new(bean: ContractId, method: MethodKey, spec: TriggerSpec) -> Self {
        ScheduledMethodDescriptor {
            bean,
            method,
            spec,
            overlap: OverlapPolicy::SkipIfRunning,
            qualifier: None,
        }
    }

    /// Set the overlap policy (builder style).
    #[must_use]
    pub const fn with_overlap(mut self, overlap: OverlapPolicy) -> Self {
        self.overlap = overlap;
        self
    }

    /// Set the facility qualifier (builder style).
    #[must_use]
    pub const fn with_qualifier(mut self, qualifier: &'static str) -> Self {
        self.qualifier = Some(qualifier);
        self
    }

    /// Bridge to the link-time anti-DCE identity row in the
    /// [`SCHEDULED`](crate::SCHEDULED) slice (mirrors
    /// [`group_to_row`](crate::group_to_row) → `ConfigMetadataRow`).
    ///
    /// The slice carries the cheap identity row (for the expected-vs-found
    /// self-check: a dropped descriptor is a task that silently never fires);
    /// the full descriptor data rides alongside, materialized by the after_init
    /// post-processor.
    #[must_use]
    pub const fn to_row(&self) -> ScheduledRow {
        ScheduledRow { contract: self.bean }
    }
}

/// Collect the link-discovered [`SCHEDULED`](crate::SCHEDULED) identity rows
/// (the anti-DCE expected-vs-found input). One `Vec` read idiom, like
/// [`collect_config_metadata`](crate::collect_config_metadata).
#[must_use]
pub fn collect_scheduled() -> Vec<ScheduledRow> {
    crate::discovery::collect_slice(&SCHEDULED)
}

/// The container-owned scheduler seam (`Role::Infrastructure`, internal — NOT
/// user-injectable). The ONE reactive timer-wheel that `sleep_until`s the global
/// earliest fire and, on wake, SPAWNS the due body onto the injected [`Spawner`]
/// (the body NEVER runs on the driver — the structural fix for Spring's
/// single-thread serialization gotcha). The timer is runtime-backed
/// (`leaf-tokio`/`leaf-smol`), so this is a seam, not a concrete struct in core.
///
/// Boxes its futures at the `dyn` seam. Participates in `shutdown().await`:
/// [`disarm`](SchedulerCore::disarm) is called early (teardown step 1) to stop
/// arming; in-flight bodies drain via the facility.
pub trait SchedulerCore: Send + Sync {
    /// Register a scheduled task: its descriptor + a sync [`Trigger`] + a
    /// fire-the-body factory. The body factory is called per fire (so each fire
    /// gets a fresh future); the returned [`SpawnHandle`] is the one the wheel
    /// awaits for fixed-delay completion feedback.
    fn register(
        &self,
        descriptor: ScheduledMethodDescriptor,
        trigger: Box<dyn Trigger>,
        body: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    ) -> Result<(), LeafError>;

    /// Arm the wheel (refresh R6, after the SmartInitializing barrier).
    fn arm(&self) -> BoxFuture<'_, Result<(), LeafError>>;

    /// Disarm (stop arming new fires) — teardown step 1, before the drain.
    fn disarm(&self) -> BoxFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    // ── a hand-rolled runtime-free JoinSeam for handle tests ──
    struct FakeJoin {
        aborted: Arc<AtomicBool>,
        detached: Arc<AtomicBool>,
        ready: bool,
        result: Result<(), JoinError>,
    }
    impl JoinSeam for FakeJoin {
        fn poll_join(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), JoinError>> {
            if self.ready {
                std::task::Poll::Ready(self.result)
            } else {
                std::task::Poll::Pending
            }
        }
        fn abort(&self) {
            self.aborted.store(true, Ordering::SeqCst);
        }
        fn detach(&self) {
            self.detached.store(true, Ordering::SeqCst);
        }
    }

    fn fake(ready: bool, result: Result<(), JoinError>) -> (SpawnHandle, Arc<AtomicBool>, Arc<AtomicBool>) {
        let aborted = Arc::new(AtomicBool::new(false));
        let detached = Arc::new(AtomicBool::new(false));
        let seam = FakeJoin {
            aborted: aborted.clone(),
            detached: detached.clone(),
            ready,
            result,
        };
        (SpawnHandle::new(Box::new(seam)), aborted, detached)
    }

    // ── SpawnHandle ──

    #[test]
    fn spawn_handle_awaits_to_result() {
        let (h, _ab, _de) = fake(true, Ok(()));
        assert_eq!(block_on(h), Ok(()));
    }

    #[test]
    fn spawn_handle_propagates_join_error() {
        let (h, _ab, _de) = fake(true, Err(JoinError::Panicked));
        assert_eq!(block_on(h), Err(JoinError::Panicked));
    }

    #[test]
    fn dropping_pending_handle_aborts_by_default() {
        let (h, aborted, detached) = fake(false, Ok(()));
        assert_eq!(h.policy(), DropPolicy::Abort);
        drop(h);
        assert!(aborted.load(Ordering::SeqCst), "drop must abort");
        assert!(!detached.load(Ordering::SeqCst));
    }

    #[test]
    fn detach_suppresses_abort_on_drop() {
        let (h, aborted, detached) = fake(false, Ok(()));
        h.detach();
        assert!(detached.load(Ordering::SeqCst), "detach must run");
        assert!(!aborted.load(Ordering::SeqCst), "detached handle must not abort");
    }

    #[test]
    fn detach_policy_detaches_on_drop() {
        let (h, aborted, detached) = fake(false, Ok(()));
        let h = h.with_policy(DropPolicy::Detach);
        assert_eq!(h.policy(), DropPolicy::Detach);
        drop(h);
        assert!(detached.load(Ordering::SeqCst), "Detach policy detaches on drop");
        assert!(!aborted.load(Ordering::SeqCst));
    }

    #[test]
    fn completed_handle_does_not_abort_on_drop() {
        let (h, aborted, _de) = fake(true, Ok(()));
        let mut h = Box::pin(h);
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(matches!(
            h.as_mut().poll(&mut cx),
            std::task::Poll::Ready(Ok(()))
        ));
        drop(h);
        assert!(!aborted.load(Ordering::SeqCst), "completed handle must not abort");
    }

    // ── Permit RAII ──

    struct RelSeam(Arc<AtomicU32>);
    impl PermitSeam for RelSeam {}
    impl Drop for RelSeam {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn permit_releases_slot_on_drop() {
        let released = Arc::new(AtomicU32::new(0));
        let p = Permit::new(Box::new(RelSeam(released.clone())));
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(p);
        assert_eq!(released.load(Ordering::SeqCst), 1, "drop releases the slot");
    }

    #[test]
    fn permit_explicit_release_is_not_double_released() {
        let released = Arc::new(AtomicU32::new(0));
        let mut p = Permit::new(Box::new(RelSeam(released.clone())));
        p.release();
        assert_eq!(released.load(Ordering::SeqCst), 1);
        drop(p);
        assert_eq!(released.load(Ordering::SeqCst), 1, "no double release");
    }

    #[test]
    fn unbounded_permit_holds_no_slot() {
        let p = Permit::unbounded();
        drop(p); // no panic, no release
    }

    // ── Trigger semantics ──

    #[test]
    fn fixed_rate_first_fire_uses_initial_delay() {
        let now = Instant::now();
        let t = FixedRateTrigger::new(Duration::from_secs(5))
            .with_initial_delay(Duration::from_secs(2));
        let next = t.next_fire(now, TriggerContext::initial()).unwrap();
        assert_eq!(next, now + Duration::from_secs(2));
    }

    #[test]
    fn fixed_rate_subsequent_fire_is_from_previous_scheduled() {
        let now = Instant::now();
        let scheduled = now - Duration::from_secs(1);
        let t = FixedRateTrigger::new(Duration::from_secs(5));
        let ctx = TriggerContext {
            last_scheduled: Some(scheduled),
            // A LATE completion must NOT push a fixed-RATE cadence.
            last_completion: Some(now + Duration::from_secs(100)),
            last_actual_fire: Some(scheduled),
        };
        let next = t.next_fire(now, ctx).unwrap();
        assert_eq!(next, scheduled + Duration::from_secs(5));
    }

    #[test]
    fn fixed_delay_subsequent_fire_is_from_previous_completion() {
        // The completion-feedback contract: fixedDelay measures from completion,
        // NOT from the scheduled time (getting this wrong = silent fixedRate).
        let now = Instant::now();
        let completion = now + Duration::from_secs(10);
        let t = FixedDelayTrigger::new(Duration::from_secs(3));
        let ctx = TriggerContext {
            last_scheduled: Some(now - Duration::from_secs(50)),
            last_completion: Some(completion),
            last_actual_fire: Some(now - Duration::from_secs(50)),
        };
        let next = t.next_fire(now, ctx).unwrap();
        assert_eq!(next, completion + Duration::from_secs(3));
    }

    #[test]
    fn fixed_delay_first_fire_uses_initial_delay() {
        let now = Instant::now();
        let t = FixedDelayTrigger::new(Duration::from_secs(3))
            .with_initial_delay(Duration::from_secs(1));
        let next = t.next_fire(now, TriggerContext::initial()).unwrap();
        assert_eq!(next, now + Duration::from_secs(1));
    }

    #[test]
    fn trigger_is_object_safe() {
        let t: Box<dyn Trigger> = Box::new(FixedRateTrigger::new(Duration::from_secs(1)));
        assert!(t.next_fire(Instant::now(), TriggerContext::initial()).is_some());
    }

    // ── descriptor identity ──

    #[test]
    fn method_key_is_stable_over_canonical_path() {
        assert_eq!(MethodKey::of("svc::cleanup"), MethodKey::of("svc::cleanup"));
        assert_ne!(MethodKey::of("svc::cleanup"), MethodKey::of("svc::flush"));
    }

    #[test]
    fn scheduled_descriptor_builder() {
        const D: ScheduledMethodDescriptor = ScheduledMethodDescriptor::new(
            ContractId::of("app::Cleaner"),
            MethodKey::of("app::Cleaner::sweep"),
            TriggerSpec::Cron("0 0 * * * *"),
        )
        .with_overlap(OverlapPolicy::Queue)
        .with_qualifier("ioExecutor");
        assert_eq!(D.bean, ContractId::of("app::Cleaner"));
        assert_eq!(D.overlap, OverlapPolicy::Queue);
        assert_eq!(D.qualifier, Some("ioExecutor"));
        assert!(matches!(D.spec, TriggerSpec::Cron("0 0 * * * *")));
    }

    #[test]
    fn descriptor_bridges_to_identity_row() {
        const D: ScheduledMethodDescriptor = ScheduledMethodDescriptor::new(
            ContractId::of("app::Cleaner"),
            MethodKey::of("app::Cleaner::sweep"),
            TriggerSpec::FixedDelay {
                delay: Duration::from_secs(2),
                initial_delay: Duration::ZERO,
            },
        );
        let row = D.to_row();
        assert_eq!(row.contract, ContractId::of("app::Cleaner"));
    }

    #[test]
    fn collect_scheduled_reads_the_slice() {
        // The discovery unit registers a TEST_SCHEDULED row; collect_scheduled
        // must see it (proves the read idiom over the existing frozen slice).
        let rows = collect_scheduled();
        assert!(rows
            .iter()
            .any(|r| r.contract == ContractId::of("leaf_core::discovery::tests::Cleanup")));
    }

    #[test]
    fn overlap_default_is_skip_if_running() {
        assert_eq!(OverlapPolicy::default(), OverlapPolicy::SkipIfRunning);
        const D: ScheduledMethodDescriptor = ScheduledMethodDescriptor::new(
            ContractId::of("b"),
            MethodKey::of("b::m"),
            TriggerSpec::FixedRate {
                period: Duration::from_secs(1),
                initial_delay: Duration::ZERO,
            },
        );
        assert_eq!(D.overlap, OverlapPolicy::SkipIfRunning);
    }

    // ── object-safety smoke for the capability traits + facility ──

    struct NoopFacility {
        gate_calls: AtomicU32,
    }
    impl Spawner for NoopFacility {
        fn spawn(&self, fut: BoxFuture<'static, ()>) -> SpawnHandle {
            // Run inline (no runtime) and return a ready handle.
            block_on(fut);
            let seam = FakeJoin {
                aborted: Arc::new(AtomicBool::new(false)),
                detached: Arc::new(AtomicBool::new(false)),
                ready: true,
                result: Ok(()),
            };
            SpawnHandle::new(Box::new(seam))
        }
    }
    impl BlockingOffload for NoopFacility {
        fn run_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BlockingHandle {
            f();
            let seam = FakeJoin {
                aborted: Arc::new(AtomicBool::new(false)),
                detached: Arc::new(AtomicBool::new(false)),
                ready: true,
                result: Ok(()),
            };
            BlockingHandle::new(Box::new(seam))
        }
    }
    impl ConcurrencyGate for NoopFacility {
        fn acquire(&self) -> BoxFuture<'static, Permit> {
            Box::pin(async { Permit::unbounded() })
        }
    }

    #[test]
    fn facility_blanket_impl_composes_all_three() {
        let f = Arc::new(NoopFacility {
            gate_calls: AtomicU32::new(0),
        });
        // It IS an ExecutionFacility via the blanket impl, and each capability.
        let _ef: Arc<dyn ExecutionFacility> = f.clone();
        let sp: &dyn Spawner = &*f;
        let bl: &dyn BlockingOffload = &*f;
        let cg: &dyn ConcurrencyGate = &*f;

        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let _h = sp.spawn(Box::pin(async move {
            r.store(true, Ordering::SeqCst);
        }));
        assert!(ran.load(Ordering::SeqCst));

        let bran = Arc::new(AtomicBool::new(false));
        let b = bran.clone();
        let _bh = bl.run_blocking(Box::new(move || b.store(true, Ordering::SeqCst)));
        assert!(bran.load(Ordering::SeqCst));

        let _permit = block_on(cg.acquire());
        f.gate_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(f.gate_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn async_uncaught_failure_handler_is_object_safe() {
        struct Sink(Arc<AtomicU32>);
        impl AsyncUncaughtFailureHandler for Sink {
            fn handle(&self, _error: LeafError) -> BoxFuture<'_, ()> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {})
            }
        }
        let n = Arc::new(AtomicU32::new(0));
        let s = Sink(n.clone());
        let dynamic: &dyn AsyncUncaughtFailureHandler = &s;
        block_on(dynamic.handle(LeafError::new(crate::ErrorKind::Cancelled)));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    // SpawnableWork doctrine: a Send+'static type implements it; assert at type
    // level (a non-Send type would fail to compile a bound, which we cannot
    // negatively test here without a trybuild harness — deferred).
    #[test]
    fn spawnable_work_blanket_holds_for_send_static() {
        fn assert_spawnable<T: SpawnableWork>() {}
        assert_spawnable::<u32>();
        assert_spawnable::<String>();
        assert_spawnable::<BoxFuture<'static, ()>>();
    }
}
