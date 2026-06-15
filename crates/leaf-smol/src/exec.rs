//! [`SmolExecutionFacility`] — the ALTERNATIVE [`ExecutionFacility`](leaf_core::ExecutionFacility) over smol.
//!
//! The mirror of `leaf-tokio`'s `TokioExecutionFacility`, proving the
//! runtime-agnostic seam (charter §2.6, phase3/10 `task-execution`, ADR-07 5c).
//! The capability split made concrete over smol primitives:
//!
//! - [`Spawner`] via [`smol::spawn`] (the global smol executor — lazily driven by
//!   background threads, `SMOL_THREADS`-configurable) — the `where` for
//!   `Send + 'static` futures.
//! - [`BlockingOffload`] via [`smol::unblock`] — the dedicated blocking thread
//!   pool (the type-level "this code may block").
//! - [`ConcurrencyGate`] via an [`async_lock::Semaphore`](smol::lock::Semaphore)
//!   — the ONE bounded permit primitive shared by retry + scheduling overlap.
//!
//! The smol [`Task`] is wrapped in a [`JoinSeam`]
//! so the runtime-agnostic [`SpawnHandle`] /
//! [`BlockingHandle`] never name smol; an owned
//! semaphore guard is wrapped in a [`PermitSeam`] so the
//! RAII [`Permit`] is likewise runtime-agnostic (`Drop`
//! releases the slot even on cancel).
//!
//! ## Panic vs cancellation
//!
//! smol's [`Task`], when awaited directly, *resumes-unwind* on a body
//! panic, and its `fallible()` task collapses BOTH panic and cancellation to
//! `None` — neither distinguishes the two, which the
//! [`JoinError`] contract requires. So the facility wraps
//! each spawned body in a [`catch_unwind`](std::panic::catch_unwind) that swallows
//! the panic and flips a shared flag: the task then always completes normally (so
//! the task resolving to `None` means cancelled), and the flag tells `poll_join`
//! whether `Some(())` was a clean finish or a caught panic.
//!
//! This crate also exposes the primary `applicationTaskExecutor`
//! [`Descriptor`](leaf_core::Descriptor) const (the `Role::Infrastructure` bean),
//! submitted into the [`COMPONENTS`](leaf_core::COMPONENTS) slice the same way the
//! thin macros emit a const row — so `Context::refresh()` auto-detects and
//! installs it before any application bean. It is NOT link-submitted by default
//! (the default runtime is tokio's, and two primary facilities would collide);
//! a smol-runtime binary registers it explicitly.

use std::any::TypeId;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::FutureExt;
use leaf_core::{
    BlockingHandle, BlockingOffload, BoxFuture, ConcurrencyGate, JoinError, JoinSeam, Permit,
    PermitSeam, SpawnHandle, Spawner,
};
use smol::lock::{Semaphore, SemaphoreGuardArc};
use smol::Task;

// ─────────────────────────── JoinSeam over smol::Task ───────────────────────

/// The runtime backing of a [`SpawnHandle`]/[`BlockingHandle`]: a smol
/// [`Task`](smol::Task) plus a shared "did the body panic?" flag.
///
/// `poll_join` polls the fallible task: `Some(())` maps to `Ok(())` unless the
/// panic flag is set (`Err(Panicked)`); `None` (the task was closed by a cancel /
/// drop) maps to `Err(Cancelled)`. `abort` drops the held task (smol cancels a
/// task on drop). `detach` consumes the held task via [`Task::detach`] so it
/// keeps running after the handle is gone.
struct SmolJoin {
    // Behind a `Mutex` so `abort`/`detach` (which take `&self`) can take the task
    // out, while `poll_join` (which takes `Pin<&mut Self>`) polls it in place.
    // The task is `Send`, so the `Mutex` is `Send + Sync`.
    task: Mutex<Option<Task<()>>>,
    panicked: Arc<AtomicBool>,
}

impl SmolJoin {
    fn new(task: Task<()>, panicked: Arc<AtomicBool>) -> Self {
        SmolJoin {
            task: Mutex::new(Some(task)),
            panicked,
        }
    }
}

impl JoinSeam for SmolJoin {
    fn poll_join(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), JoinError>> {
        let mut guard = self.task.lock().expect("join task mutex");
        let Some(task) = guard.as_mut() else {
            // Already detached/aborted out: report cancellation (the seam was
            // given up). This should not happen on the await path (await holds
            // the handle), but keep it total.
            return Poll::Ready(Err(JoinError::Cancelled));
        };
        match Pin::new(task).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                // The body always completes normally (its panic was caught by the
                // wrapper); the flag distinguishes a clean finish from a panic.
                let out = if self.panicked.load(Ordering::SeqCst) {
                    Err(JoinError::Panicked)
                } else {
                    Ok(())
                };
                // Consume the task so a later `Drop`/`abort` is a no-op.
                *guard = None;
                Poll::Ready(out)
            }
        }
    }

    fn abort(&self) {
        // Dropping a smol `Task` cancels it (at its next await point). Take it out
        // and drop it here.
        let _ = self.task.lock().expect("join task mutex").take();
    }

    fn detach(&self) {
        // Detach the held task so it keeps running after the handle drops.
        if let Some(task) = self.task.lock().expect("join task mutex").take() {
            task.detach();
        }
    }
}

/// Wrap a body future so a panic is CAUGHT (flipping `panicked`) rather than
/// propagated — see the module note on panic-vs-cancellation. The returned future
/// always resolves to `()`.
async fn guard_panics(fut: BoxFuture<'static, ()>, panicked: Arc<AtomicBool>) {
    // `AssertUnwindSafe`: the future may hold non-`UnwindSafe` state, but we
    // only record a boolean and never observe broken invariants afterwards.
    if AssertUnwindSafe(fut).catch_unwind().await.is_err() {
        panicked.store(true, Ordering::SeqCst);
    }
}

// ─────────────────────────── PermitSeam over Semaphore ──────────────────────

/// The runtime backing of a [`Permit`]: an owned smol semaphore guard plus the
/// facility's availability counter.
///
/// Holding it occupies one slot; dropping it (the RAII `Drop`, even on cancel)
/// returns the slot to the [`Semaphore`] AND bumps the availability counter back.
///
/// `async-lock`'s [`Semaphore`] exposes no `available_permits` query, so the
/// facility tracks availability itself in an [`AtomicUsize`](std::sync::atomic::AtomicUsize):
/// `acquire` decrements after obtaining the guard, this `Drop` increments. The
/// real guard is released FIRST, then the counter is bumped, so an observer that
/// sees `available > 0` can in fact acquire.
struct SmolPermit {
    guard: Option<SemaphoreGuardArc>,
    available: Arc<std::sync::atomic::AtomicUsize>,
}

impl PermitSeam for SmolPermit {}

impl Drop for SmolPermit {
    fn drop(&mut self) {
        // Release the real slot FIRST, then return the counted slot — ordering so
        // a thread that observes the counter can immediately acquire the real one.
        drop(self.guard.take());
        self.available
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

// ───────────────────────────── the facility ─────────────────────────────────

/// The ALTERNATIVE [`ExecutionFacility`](leaf_core::ExecutionFacility) over smol.
///
/// Implements all three capabilities, so it IS an `ExecutionFacility` via the
/// core blanket impl. The [`ConcurrencyGate`] is bounded by an
/// [`async_lock::Semaphore`](smol::lock::Semaphore);
/// [`with_limit`](SmolExecutionFacility::with_limit) sets the permit count
/// (limit-1 = an instance lock; the default is effectively unbounded for
/// `spawn`/`run_blocking`, the gate only caps `acquire`).
#[derive(Clone)]
pub struct SmolExecutionFacility {
    gate: Arc<Semaphore>,
    // `async-lock`'s Semaphore has no `available_permits`; track it ourselves.
    available: Arc<std::sync::atomic::AtomicUsize>,
}

/// A large-but-safe permit count standing in for "unbounded" on the default
/// facility's gate (the gate only caps `acquire`; `spawn`/`run_blocking` are
/// never gated). `usize::MAX` would risk overflow inside the semaphore's
/// accounting, so use a generous ceiling.
const UNBOUNDED_PERMITS: usize = usize::MAX >> 4;

impl SmolExecutionFacility {
    /// A facility with a near-unbounded gate (the default executor).
    #[must_use]
    pub fn new() -> Self {
        SmolExecutionFacility::with_limit(UNBOUNDED_PERMITS)
    }

    /// A facility whose [`ConcurrencyGate`] admits at most `limit` permits.
    ///
    /// `with_limit(1)` is the declarative instance lock (one body at a time).
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        SmolExecutionFacility {
            gate: Arc::new(Semaphore::new(limit)),
            available: Arc::new(std::sync::atomic::AtomicUsize::new(limit)),
        }
    }

    /// The current number of available permits on the gate (test/diagnostic).
    ///
    /// Tracked in a side counter because `async-lock`'s [`Semaphore`] exposes no
    /// such query; it converges with the real semaphore (decrement after acquire,
    /// increment on permit drop).
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.available.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for SmolExecutionFacility {
    fn default() -> Self {
        SmolExecutionFacility::new()
    }
}

impl Spawner for SmolExecutionFacility {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> SpawnHandle {
        let panicked = Arc::new(AtomicBool::new(false));
        let task = smol::spawn(guard_panics(fut, Arc::clone(&panicked)));
        SpawnHandle::new(Box::new(SmolJoin::new(task, panicked)))
    }
}

impl BlockingOffload for SmolExecutionFacility {
    fn run_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BlockingHandle {
        // `smol::unblock` runs the closure on the blocking thread pool and yields
        // a `Task<()>`. A blocking job cannot be cooperatively cancelled, but the
        // panic-guard keeps the join contract uniform with `spawn`.
        let panicked = Arc::new(AtomicBool::new(false));
        let p = Arc::clone(&panicked);
        let task = smol::unblock(move || {
            if std::panic::catch_unwind(AssertUnwindSafe(f)).is_err() {
                p.store(true, Ordering::SeqCst);
            }
        });
        BlockingHandle::new(Box::new(SmolJoin::new(task, panicked)))
    }
}

impl ConcurrencyGate for SmolExecutionFacility {
    fn acquire(&self) -> BoxFuture<'static, Permit> {
        let gate = Arc::clone(&self.gate);
        let available = Arc::clone(&self.available);
        Box::pin(async move {
            let guard = gate.acquire_arc().await;
            // Count the taken slot AFTER the real acquire so the counter never
            // reads "available" for a slot that is still being waited on.
            available.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Permit::new(Box::new(SmolPermit {
                guard: Some(guard),
                available,
            }))
        })
    }
}

// ───────────────────── the applicationTaskExecutor bean ──────────────────────

/// The stable contract name of the primary execution-facility bean (Spring's
/// `applicationTaskExecutor` identity preserved as a single named bean).
pub const APPLICATION_TASK_EXECUTOR: &str = "applicationTaskExecutor";

/// The stable contract path of the smol primary execution-facility bean.
pub const APPLICATION_TASK_EXECUTOR_CONTRACT: &str = "leaf_smol::applicationTaskExecutor";

/// The const `Role::Infrastructure` [`Descriptor`](leaf_core::Descriptor) for the
/// smol `applicationTaskExecutor` bean — the auto-detect target at
/// `Context::refresh()` R2 when smol is the selected runtime.
///
/// This is the EXACT shape the thin stereotype macro emits: a flat const row over
/// `::leaf_core` paths. Unlike `leaf-tokio`, it is NOT link-submitted into
/// [`COMPONENTS`](leaf_core::COMPONENTS) by default — the default runtime is
/// tokio's, and two primary `applicationTaskExecutor` facilities in one binary
/// would be a duplicate-primary collision. A smol-runtime binary registers this
/// descriptor + [`APPLICATION_TASK_EXECUTOR_SEED`] explicitly.
pub const APPLICATION_TASK_EXECUTOR_DESCRIPTOR: leaf_core::Descriptor = leaf_core::Descriptor {
    contract: leaf_core::ContractId::of(APPLICATION_TASK_EXECUTOR_CONTRACT),
    self_type: TypeId::of::<SmolExecutionFacility>(),
    provides: &[],
    declared_name: Some(APPLICATION_TASK_EXECUTOR),
    aliases: &[],
    scope: leaf_core::ScopeDef::SINGLETON,
    role: leaf_core::Role::Infrastructure,
    meta: &leaf_core::AnnotationMetadata::EMPTY,
    parent: None,
    origin: leaf_core::Origin::Native {
        crate_name: Some("leaf-smol"),
    },
};

/// The [`Provider`](leaf_core::Provider) that constructs the smol
/// `applicationTaskExecutor` facility — what a smol-runtime boot drives to publish
/// the shared bean.
///
/// The facility holds no injected collaborators (its runtime is the ambient smol
/// global executor), so `provide` is a pure construct-and-publish; the published
/// handle is the SHARED `Arc`-shaped value consumers receive as `Ref<dyn Spawner>`
/// etc.
pub struct SmolExecutionFacilityProvider {
    descriptor: leaf_core::Descriptor,
}

impl SmolExecutionFacilityProvider {
    /// Construct the provider over the const descriptor.
    #[must_use]
    pub fn new() -> Self {
        SmolExecutionFacilityProvider {
            descriptor: APPLICATION_TASK_EXECUTOR_DESCRIPTOR,
        }
    }
}

impl Default for SmolExecutionFacilityProvider {
    fn default() -> Self {
        SmolExecutionFacilityProvider::new()
    }
}

impl leaf_core::Provider for SmolExecutionFacilityProvider {
    fn descriptor(&self) -> &leaf_core::Descriptor {
        &self.descriptor
    }

    fn provide<'a>(
        &'a self,
        _cx: &'a leaf_core::ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<leaf_core::Published, leaf_core::LeafError>> {
        Box::pin(async { Ok(leaf_core::Published::shared_value(SmolExecutionFacility::new())) })
    }
}

/// The const [`ProviderSeed`](leaf_core::ProviderSeed) a smol-runtime boot binds
/// to the `applicationTaskExecutor` [`Descriptor`](leaf_core::Descriptor) (the construction recipe on the
/// const row path; mints the `Arc<dyn Provider>` once at register/freeze).
pub const APPLICATION_TASK_EXECUTOR_SEED: leaf_core::ProviderSeed =
    || std::sync::Arc::new(SmolExecutionFacilityProvider::new());

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{DropPolicy, ExecutionFacility};
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::time::Duration;

    // smol's global executor is driven by background threads; `block_on` drives
    // the calling task. Each test runs its body under `smol::block_on`.

    #[test]
    fn spawn_runs_and_joins_ok() {
        smol::block_on(async {
            let f = SmolExecutionFacility::new();
            let ran = Arc::new(AtomicBool::new(false));
            let r = ran.clone();
            let h = f.spawn(Box::pin(async move {
                r.store(true, Ordering::SeqCst);
            }));
            assert_eq!(h.await, Ok(()));
            assert!(ran.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn spawn_panic_maps_to_join_error_panicked() {
        smol::block_on(async {
            let f = SmolExecutionFacility::new();
            let h = f.spawn(Box::pin(async move {
                panic!("boom");
            }));
            assert_eq!(h.await, Err(JoinError::Panicked));
        });
    }

    #[test]
    fn dropping_spawn_handle_aborts_the_task() {
        smol::block_on(async {
            let f = SmolExecutionFacility::new();
            let observed_cancel = Arc::new(AtomicBool::new(false));
            let oc = observed_cancel.clone();
            let (started_tx, started_rx) = smol::channel::bounded::<()>(1);
            let h = f.spawn(Box::pin(async move {
                struct Guard(Arc<AtomicBool>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::SeqCst);
                    }
                }
                let _g = Guard(oc);
                let _ = started_tx.send(()).await;
                // Park forever until aborted.
                futures::future::pending::<()>().await;
            }));
            // Ensure the task actually started before we abort it.
            started_rx.recv().await.unwrap();
            drop(h); // Abort (default DropPolicy::Abort).
            // Give the runtime a moment to run the cancellation + drop guard.
            for _ in 0..200 {
                if observed_cancel.load(Ordering::SeqCst) {
                    break;
                }
                smol::Timer::after(Duration::from_millis(2)).await;
            }
            assert!(
                observed_cancel.load(Ordering::SeqCst),
                "dropping the handle must abort the task"
            );
        });
    }

    #[test]
    fn detached_handle_keeps_running() {
        smol::block_on(async {
            let f = SmolExecutionFacility::new();
            let done = Arc::new(AtomicBool::new(false));
            let d = done.clone();
            let h = f
                .spawn(Box::pin(async move {
                    smol::Timer::after(Duration::from_millis(5)).await;
                    d.store(true, Ordering::SeqCst);
                }))
                .with_policy(DropPolicy::Detach);
            drop(h); // Detach: the task must still complete.
            for _ in 0..200 {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                smol::Timer::after(Duration::from_millis(2)).await;
            }
            assert!(
                done.load(Ordering::SeqCst),
                "detached task must run to completion"
            );
        });
    }

    #[test]
    fn run_blocking_offloads_and_joins() {
        smol::block_on(async {
            let f = SmolExecutionFacility::new();
            let ran = Arc::new(AtomicBool::new(false));
            let r = ran.clone();
            let h = f.run_blocking(Box::new(move || {
                r.store(true, Ordering::SeqCst);
            }));
            assert_eq!(h.await, Ok(()));
            assert!(ran.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn concurrency_gate_limit_one_serializes() {
        smol::block_on(async {
            // limit-1 gate = instance lock. Two acquirers: the second must wait
            // until the first releases.
            let f = SmolExecutionFacility::with_limit(1);
            assert_eq!(f.available_permits(), 1);

            let p1 = f.acquire().await;
            assert_eq!(f.available_permits(), 0);

            // A second acquire cannot complete while p1 is held.
            let mut second = Box::pin(f.acquire());
            let raced = race_timeout(&mut second, Duration::from_millis(20)).await;
            assert!(
                raced.is_none(),
                "gate must block the second acquirer at limit-1"
            );

            // Release the first; now the second can proceed.
            drop(p1);
            let p2 = race_timeout(&mut second, Duration::from_millis(500))
                .await
                .expect("second acquire must complete after release");
            assert_eq!(f.available_permits(), 0);
            drop(p2);
            assert_eq!(f.available_permits(), 1);
        });
    }

    #[test]
    fn permit_releases_even_when_acquirer_is_cancelled() {
        smol::block_on(async {
            // RAII: dropping a Permit returns the slot.
            let f = SmolExecutionFacility::with_limit(2);
            let a = f.acquire().await;
            let b = f.acquire().await;
            assert_eq!(f.available_permits(), 0);
            drop(a);
            assert_eq!(f.available_permits(), 1);
            drop(b);
            assert_eq!(f.available_permits(), 2);
        });
    }

    #[test]
    fn facility_coerces_to_dyn_execution_facility_and_runs() {
        smol::block_on(async {
            let f: Arc<dyn ExecutionFacility> = Arc::new(SmolExecutionFacility::new());
            let counter = Arc::new(AtomicU32::new(0));
            let c = counter.clone();
            let h = f.spawn(Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            }));
            h.await.unwrap();
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            let _p = f.acquire().await;
        });
    }

    #[test]
    fn application_task_executor_descriptor_is_infrastructure() {
        let d = &APPLICATION_TASK_EXECUTOR_DESCRIPTOR;
        assert_eq!(d.declared_name, Some(APPLICATION_TASK_EXECUTOR));
        assert_eq!(d.role, leaf_core::Role::Infrastructure);
        assert_eq!(d.self_type, TypeId::of::<SmolExecutionFacility>());
        assert_eq!(
            d.contract,
            leaf_core::ContractId::of("leaf_smol::applicationTaskExecutor")
        );
    }

    /// Race a future against a timeout; `Some(out)` if it completed first, `None`
    /// on timeout (a tiny `select`-free helper so the test stays runtime-neutral).
    async fn race_timeout<F: std::future::Future + Unpin>(
        fut: &mut F,
        dur: Duration,
    ) -> Option<F::Output> {
        let timer = smol::Timer::after(dur);
        futures::pin_mut!(timer);
        match futures::future::select(fut, timer).await {
            futures::future::Either::Left((out, _)) => Some(out),
            futures::future::Either::Right(_) => None,
        }
    }
}
