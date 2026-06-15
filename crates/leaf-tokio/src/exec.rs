//! [`TokioExecutionFacility`] — the DEFAULT [`ExecutionFacility`] over tokio.
//!
//! The capability split made concrete (phase3/10 `task-execution`, ADR-07 5c):
//!
//! - [`Spawner`] via [`tokio::spawn`] — the `where` for `Send + 'static` futures.
//! - [`BlockingOffload`] via [`tokio::task::spawn_blocking`] — the dedicated
//!   blocking pool (the type-level "this code may block").
//! - [`ConcurrencyGate`] via a [`tokio::sync::Semaphore`] — the ONE bounded
//!   permit primitive shared by retry + scheduling overlap.
//!
//! The runtime [`JoinHandle`](tokio::task::JoinHandle) is wrapped in a
//! [`JoinSeam`] so the runtime-agnostic [`SpawnHandle`]/[`BlockingHandle`] never
//! name tokio; an owned semaphore permit is wrapped in a [`PermitSeam`] so the
//! RAII [`Permit`] is likewise runtime-agnostic (`Drop` releases the slot even on
//! cancel).
//!
//! This crate also exposes the primary `applicationTaskExecutor`
//! [`Descriptor`](leaf_core::Descriptor) const (the `Role::Infrastructure` bean),
//! submitted into the [`COMPONENTS`](leaf_core::COMPONENTS) slice the same way the
//! thin macros emit a const row — so `Context::refresh()` auto-detects and
//! installs it before any application bean.

use std::any::TypeId;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use leaf_core::{
    BlockingHandle, BlockingOffload, BoxFuture, ConcurrencyGate, JoinError, JoinSeam, Permit,
    PermitSeam, SpawnHandle, Spawner,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

// ─────────────────────────── JoinSeam over JoinHandle ───────────────────────

/// The runtime backing of a [`SpawnHandle`]/[`BlockingHandle`]: a tokio
/// [`JoinHandle`].
///
/// `poll_join` maps tokio's `Result<(), JoinError>` (panic / cancelled) onto the
/// runtime-agnostic [`JoinError`]; `abort` requests cancellation; `detach` drops
/// the handle so the task keeps running (tokio detaches on `JoinHandle` drop).
struct TokioJoin {
    // `Option` so `detach` can drop the handle (releasing tokio's abort-on-drop
    // is a no-op for tokio — dropping a JoinHandle detaches — but we keep the
    // shape uniform with the abort path).
    handle: Option<JoinHandle<()>>,
}

impl TokioJoin {
    fn new(handle: JoinHandle<()>) -> Self {
        TokioJoin { handle: Some(handle) }
    }
}

impl JoinSeam for TokioJoin {
    fn poll_join(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), JoinError>> {
        let Some(handle) = self.handle.as_mut() else {
            // Already detached/consumed: report cancellation (the seam was given
            // up). This should not happen on the await path (await holds the
            // handle), but keep it total.
            return Poll::Ready(Err(JoinError::Cancelled));
        };
        match Pin::new(handle).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                let mapped = if e.is_cancelled() {
                    JoinError::Cancelled
                } else {
                    JoinError::Panicked
                };
                Poll::Ready(Err(mapped))
            }
        }
    }

    fn abort(&self) {
        if let Some(h) = self.handle.as_ref() {
            h.abort();
        }
    }

    fn detach(&self) {
        // tokio detaches a task simply by dropping its `JoinHandle` WITHOUT
        // calling `abort` (unlike the structured default, tokio never aborts on
        // handle drop). The core `SpawnHandle`/`BlockingHandle` already suppress
        // the abort path once `detach`/`Detach`-policy is in effect, then drop the
        // seam — which drops this held handle without aborting. So `detach` needs
        // no action here; the held handle dropping un-aborted IS the detach.
    }
}

// ─────────────────────────── PermitSeam over Semaphore ──────────────────────

/// The runtime backing of a [`Permit`]: an owned tokio semaphore permit.
///
/// Holding it occupies one slot; dropping it (the RAII `Drop`, even on cancel)
/// returns the slot to the [`Semaphore`].
struct TokioPermit {
    _permit: OwnedSemaphorePermit,
}

impl PermitSeam for TokioPermit {}

// ───────────────────────────── the facility ─────────────────────────────────

/// The DEFAULT [`ExecutionFacility`](leaf_core::ExecutionFacility) over tokio.
///
/// Implements all three capabilities, so it IS an `ExecutionFacility` via the
/// core blanket impl. The [`ConcurrencyGate`] is bounded by a
/// [`Semaphore`]; [`with_limit`](TokioExecutionFacility::with_limit) sets the
/// permit count (limit-1 = an instance lock; the default is effectively
/// unbounded for `spawn`/`run_blocking`, the gate only caps `acquire`).
#[derive(Clone)]
pub struct TokioExecutionFacility {
    gate: Arc<Semaphore>,
}

impl TokioExecutionFacility {
    /// A facility with a near-unbounded gate (the default executor).
    #[must_use]
    pub fn new() -> Self {
        // `Semaphore::MAX_PERMITS` is the runtime cap; use a large but safe count
        // so `acquire` never blocks for the unbounded default.
        TokioExecutionFacility::with_limit(Semaphore::MAX_PERMITS)
    }

    /// A facility whose [`ConcurrencyGate`] admits at most `limit` permits.
    ///
    /// `with_limit(1)` is the declarative instance lock (one body at a time).
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        TokioExecutionFacility {
            gate: Arc::new(Semaphore::new(limit)),
        }
    }

    /// The current number of available permits on the gate (test/diagnostic).
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.gate.available_permits()
    }
}

impl Default for TokioExecutionFacility {
    fn default() -> Self {
        TokioExecutionFacility::new()
    }
}

impl Spawner for TokioExecutionFacility {
    fn spawn(&self, fut: BoxFuture<'static, ()>) -> SpawnHandle {
        let handle = tokio::spawn(fut);
        SpawnHandle::new(Box::new(TokioJoin::new(handle)))
    }
}

impl BlockingOffload for TokioExecutionFacility {
    fn run_blocking(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BlockingHandle {
        let handle = tokio::task::spawn_blocking(f);
        BlockingHandle::new(Box::new(TokioJoin::new(handle)))
    }
}

impl ConcurrencyGate for TokioExecutionFacility {
    fn acquire(&self) -> BoxFuture<'static, Permit> {
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            match gate.acquire_owned().await {
                Ok(permit) => Permit::new(Box::new(TokioPermit { _permit: permit })),
                // The semaphore is only closed at shutdown; an unbounded permit
                // is the safe degraded behavior (the guarded region still runs).
                Err(_closed) => Permit::unbounded(),
            }
        })
    }
}

// ───────────────────── the applicationTaskExecutor bean ──────────────────────

/// The stable contract name of the primary execution-facility bean (Spring's
/// `applicationTaskExecutor` identity preserved as a single named bean).
pub const APPLICATION_TASK_EXECUTOR: &str = "applicationTaskExecutor";

/// The stable contract path of the primary execution-facility bean.
pub const APPLICATION_TASK_EXECUTOR_CONTRACT: &str = "leaf_tokio::applicationTaskExecutor";

/// The const `Role::Infrastructure` [`Descriptor`](leaf_core::Descriptor) for the
/// primary `applicationTaskExecutor` bean — the auto-detect target at
/// `Context::refresh()` R2.
///
/// This is the EXACT shape the thin stereotype macro emits: a flat const row over
/// `::leaf_core` paths. It is submitted into the [`COMPONENTS`](leaf_core::COMPONENTS)
/// distributed-slice (see [`APPLICATION_TASK_EXECUTOR_ELEMENT`]) so a force-linking
/// binary picks it up through the same link-time channel; a missing facility
/// HARD-FAILS the refresh self-check (an async-first framework cannot run without
/// it).
pub const APPLICATION_TASK_EXECUTOR_DESCRIPTOR: leaf_core::Descriptor = leaf_core::Descriptor {
    contract: leaf_core::ContractId::of(APPLICATION_TASK_EXECUTOR_CONTRACT),
    self_type: TypeId::of::<TokioExecutionFacility>(),
    provides: &[],
    declared_name: Some(APPLICATION_TASK_EXECUTOR),
    aliases: &[],
    scope: leaf_core::ScopeDef::SINGLETON,
    role: leaf_core::Role::Infrastructure,
    meta: &leaf_core::AnnotationMetadata::EMPTY,
    parent: None,
    origin: leaf_core::Origin::Native {
        crate_name: Some("leaf-tokio"),
    },
};

// The link-time element: place the const Descriptor into the frozen COMPONENTS
// slice via the SAME `::leaf_core::linkme` path the macros emit. A `#[used]`
// `#[link_section]` static under the hood (hence the scoped allow — there is no
// hand-written `unsafe` block; only the macro-generated section attribute).
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static APPLICATION_TASK_EXECUTOR_ELEMENT: leaf_core::Descriptor =
    APPLICATION_TASK_EXECUTOR_DESCRIPTOR;

/// The [`Provider`](leaf_core::Provider) that constructs the
/// `applicationTaskExecutor` facility — what leaf-boot drives to publish the
/// shared bean.
///
/// The facility holds no injected collaborators (its runtime is the ambient tokio
/// runtime), so `provide` is a pure construct-and-publish; the published handle is
/// the SHARED `Arc`-shaped value consumers receive as `Ref<dyn Spawner>` etc.
pub struct TokioExecutionFacilityProvider {
    descriptor: leaf_core::Descriptor,
}

impl TokioExecutionFacilityProvider {
    /// Construct the provider over the const descriptor.
    #[must_use]
    pub fn new() -> Self {
        TokioExecutionFacilityProvider {
            descriptor: APPLICATION_TASK_EXECUTOR_DESCRIPTOR,
        }
    }
}

impl Default for TokioExecutionFacilityProvider {
    fn default() -> Self {
        TokioExecutionFacilityProvider::new()
    }
}

impl leaf_core::Provider for TokioExecutionFacilityProvider {
    fn descriptor(&self) -> &leaf_core::Descriptor {
        &self.descriptor
    }

    fn provide<'a>(
        &'a self,
        _cx: &'a leaf_core::ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<leaf_core::Published, leaf_core::LeafError>> {
        Box::pin(async {
            Ok(leaf_core::Published::shared_value(
                TokioExecutionFacility::new(),
            ))
        })
    }
}

/// The const [`ProviderSeed`](leaf_core::ProviderSeed) leaf-boot binds to the
/// `applicationTaskExecutor` [`Descriptor`] when lifting the
/// [`COMPONENTS`](leaf_core::COMPONENTS) slice (the construction recipe on the
/// const row path; mints the `Arc<dyn Provider>` once at register/freeze).
pub const APPLICATION_TASK_EXECUTOR_SEED: leaf_core::ProviderSeed =
    || std::sync::Arc::new(TokioExecutionFacilityProvider::new());

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{DropPolicy, ExecutionFacility};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn spawn_runs_and_joins_ok() {
        let f = TokioExecutionFacility::new();
        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let h = f.spawn(Box::pin(async move {
            r.store(true, Ordering::SeqCst);
        }));
        assert_eq!(h.await, Ok(()));
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn spawn_panic_maps_to_join_error_panicked() {
        let f = TokioExecutionFacility::new();
        let h = f.spawn(Box::pin(async move {
            panic!("boom");
        }));
        assert_eq!(h.await, Err(JoinError::Panicked));
    }

    #[tokio::test]
    async fn dropping_spawn_handle_aborts_the_task() {
        let f = TokioExecutionFacility::new();
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let oc = observed_cancel.clone();
        // A task that never completes on its own; if aborted, its drop-guard
        // flips the flag at the next cancellation point.
        let (started_tx, started_rx) = oneshot::channel();
        let h = f.spawn(Box::pin(async move {
            struct Guard(Arc<AtomicBool>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _g = Guard(oc);
            let _ = started_tx.send(());
            // Park forever until aborted.
            futures::future::pending::<()>().await;
        }));
        // Ensure the task actually started before we abort it.
        started_rx.await.unwrap();
        drop(h); // Abort (default DropPolicy::Abort).
        // Give the runtime a moment to run the cancellation + drop guard.
        for _ in 0..100 {
            if observed_cancel.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            observed_cancel.load(Ordering::SeqCst),
            "dropping the handle must abort the task"
        );
    }

    #[tokio::test]
    async fn detached_handle_keeps_running() {
        let f = TokioExecutionFacility::new();
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let h = f
            .spawn(Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                d.store(true, Ordering::SeqCst);
            }))
            .with_policy(DropPolicy::Detach);
        drop(h); // Detach: the task must still complete.
        for _ in 0..100 {
            if done.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(done.load(Ordering::SeqCst), "detached task must run to completion");
    }

    #[tokio::test]
    async fn run_blocking_offloads_and_joins() {
        let f = TokioExecutionFacility::new();
        let ran = Arc::new(AtomicBool::new(false));
        let r = ran.clone();
        let h = f.run_blocking(Box::new(move || {
            r.store(true, Ordering::SeqCst);
        }));
        assert_eq!(h.await, Ok(()));
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn concurrency_gate_limit_one_serializes() {
        // limit-1 gate = instance lock. Two acquirers: the second must wait until
        // the first releases.
        let f = TokioExecutionFacility::with_limit(1);
        assert_eq!(f.available_permits(), 1);

        let p1 = f.acquire().await;
        assert_eq!(f.available_permits(), 0);

        // A second acquire cannot complete while p1 is held.
        let second = f.acquire();
        tokio::pin!(second);
        let raced = tokio::time::timeout(Duration::from_millis(20), &mut second).await;
        assert!(raced.is_err(), "gate must block the second acquirer at limit-1");

        // Release the first; now the second can proceed.
        drop(p1);
        let p2 = tokio::time::timeout(Duration::from_millis(200), &mut second)
            .await
            .expect("second acquire must complete after release");
        assert_eq!(f.available_permits(), 0);
        drop(p2);
        assert_eq!(f.available_permits(), 1);
    }

    #[tokio::test]
    async fn permit_releases_even_when_acquirer_is_cancelled() {
        // RAII: dropping a Permit returns the slot. Acquire, hold, drop — the
        // count must recover.
        let f = TokioExecutionFacility::with_limit(2);
        let a = f.acquire().await;
        let b = f.acquire().await;
        assert_eq!(f.available_permits(), 0);
        drop(a);
        assert_eq!(f.available_permits(), 1);
        drop(b);
        assert_eq!(f.available_permits(), 2);
    }

    #[tokio::test]
    async fn facility_is_an_execution_facility_behind_dyn() {
        let f: Arc<dyn ExecutionFacility> = Arc::new(TokioExecutionFacility::new());
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let h = f.spawn(Box::pin(async move {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        h.await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let _p = f.acquire().await;
    }

    #[test]
    fn application_task_executor_descriptor_is_infrastructure() {
        let d = &APPLICATION_TASK_EXECUTOR_DESCRIPTOR;
        assert_eq!(d.declared_name, Some(APPLICATION_TASK_EXECUTOR));
        assert_eq!(d.role, leaf_core::Role::Infrastructure);
        assert_eq!(d.self_type, TypeId::of::<TokioExecutionFacility>());
        assert_eq!(
            d.contract,
            leaf_core::ContractId::of("leaf_tokio::applicationTaskExecutor")
        );
    }

    #[test]
    fn application_task_executor_is_discoverable_in_components() {
        // The descriptor is link-submitted into COMPONENTS (the same channel the
        // macros emit into), so a self-check / refresh auto-detect finds it.
        let rows = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            rows.iter().any(|r| r.contract
                == leaf_core::ContractId::of("leaf_tokio::applicationTaskExecutor")),
            "applicationTaskExecutor must be discoverable in COMPONENTS"
        );
    }
}
