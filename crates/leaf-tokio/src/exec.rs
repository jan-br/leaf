//! [`TokioExecutionFacility`] — the DEFAULT [`ExecutionFacility`](leaf_core::ExecutionFacility) over tokio.
//!
//! The capability split made concrete (phase3/10 `task-execution`, ADR-07 5c):
//!
//! - [`Spawner`] via [`tokio::spawn`] — the `where` for `Send + 'static` futures.
//! - [`BlockingOffload`] via [`tokio::task::spawn_blocking`] — the dedicated
//!   blocking pool (the type-level "this code may block").
//! - [`ConcurrencyGate`] via a [`tokio::sync::Semaphore`] — the ONE bounded
//!   permit primitive shared by retry + scheduling overlap.
//!
//! The runtime [`JoinHandle`] is wrapped in a
//! [`JoinSeam`] so the runtime-agnostic [`SpawnHandle`]/[`BlockingHandle`] never
//! name tokio; an owned semaphore permit is wrapped in a [`PermitSeam`] so the
//! RAII [`Permit`] is likewise runtime-agnostic (`Drop` releases the slot even on
//! cancel).
//!
//! This module registers the primary `applicationTaskExecutor` bean (the
//! `Role::Infrastructure` facility) through the THIN
//! `register_component!(TokioExecutionFacility, role = "infrastructure", name = "..")`
//! macro — the SAME maximal-magic channel a user bean uses: the macro emits the const
//! [`Descriptor`](leaf_core::Descriptor) into [`COMPONENTS`](leaf_core::COMPONENTS) +
//! force-links its [`ProviderSeed`](leaf_core::ProviderSeed) into
//! [`SEED_PAIRINGS`](leaf_core::SEED_PAIRINGS), so `Context::refresh()` auto-detects
//! and installs it before any application bean, and leaf-boot's `from_slices` JOINs it
//! with NO hand-written builtin pairing.

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

/// The DEFAULT [`ExecutionFacility`](leaf_core::ExecutionFacility) over tokio — the
/// primary `applicationTaskExecutor` bean.
///
/// Implements all three capabilities, so it IS an `ExecutionFacility` via the
/// core blanket impl. The [`ConcurrencyGate`] is bounded by a
/// [`Semaphore`]; [`with_limit`](TokioExecutionFacility::with_limit) sets the
/// permit count (limit-1 = an instance lock; the default is effectively
/// unbounded for `spawn`/`run_blocking`, the gate only caps `acquire`).
///
/// Registered through the THIN stereotype macro exactly like a user bean: the
/// `register_component!` form (the construct-via-`new()` shape — the facility holds no
/// injected collaborators, its runtime is the ambient tokio runtime, so its `gate`
/// field is internal state, NOT an injection point) carrying the
/// `role = "infrastructure", name = "applicationTaskExecutor"` provenance + Spring
/// bean-name. The macro emits the const `Role::Infrastructure`
/// [`Descriptor`](leaf_core::Descriptor) into [`COMPONENTS`](leaf_core::COMPONENTS),
/// the generated [`Provider`](leaf_core::Provider) (construct-and-publish via
/// `TokioExecutionFacility::new()`), and force-links the
/// [`ProviderSeed`](leaf_core::ProviderSeed) into
/// [`SEED_PAIRINGS`](leaf_core::SEED_PAIRINGS) — so leaf-boot's `from_slices` JOINs it
/// the same maximal-magic way as any annotated bean (NO hand-written const block, NO
/// hardcoded leaf-boot builtin pairing). See the `register_component!` invocation
/// below the impls.
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

// Register the facility as the primary `applicationTaskExecutor` through the THIN
// macro — the SAME maximal-magic channel a user bean uses. `register_component!` is
// the construct-via-`new()` form (the facility's `gate` field is internal state, NOT
// an injection point, so the `#[component]` struct-field path does not fit); the
// `role`/`name` args carry the `Role::Infrastructure` provenance + Spring bean-name.
//
// This single line emits everything the hand-written block used to: the const
// `Role::Infrastructure` `Descriptor` (-> COMPONENTS), the generated `Provider`
// (construct-and-publish via `TokioExecutionFacility::new()`), the `ProviderSeed`
// (force-linked into SEED_PAIRINGS — so leaf-boot's `from_slices` JOINs it with NO
// hand-written builtin pairing), the per-bean `InjectionPlan` (empty), and the
// `impl leaf_core::Bean for TokioExecutionFacility {}` engine-resolvability marker.
leaf_macros::register_component!(
    TokioExecutionFacility,
    role = "infrastructure",
    name = "applicationTaskExecutor"
);

/// The stable contract NAME of the primary execution-facility bean (Spring's
/// `applicationTaskExecutor` identity, the macro-emitted `declared_name`).
pub const APPLICATION_TASK_EXECUTOR: &str = "applicationTaskExecutor";

/// The stable cross-build contract PATH of the primary execution-facility bean — the
/// macro-derived `module_path!()::Ident` identity the `Descriptor.contract` carries
/// (the JOIN key leaf-boot's `from_slices` matches against). Kept as a pub const so
/// leaf-boot (and tests) name the facility's stable identity without hardcoding the
/// string.
pub const APPLICATION_TASK_EXECUTOR_CONTRACT: &str = "leaf_tokio::exec::TokioExecutionFacility";

/// The macro-emitted [`ProviderSeed`](leaf_core::ProviderSeed) for the
/// `applicationTaskExecutor` facility — a thin alias over the deterministic public
/// `__leaf_seed_TokioExecutionFacility` const the `register_component!` expansion
/// exposes (the construction recipe; mints the `Arc<dyn Provider>` once at
/// register/freeze). It force-links itself into
/// [`SEED_PAIRINGS`](leaf_core::SEED_PAIRINGS), so this alias exists only as a named
/// handle for the escape-hatch `.with_seeds` path / diagnostics.
#[allow(non_upper_case_globals)]
pub const APPLICATION_TASK_EXECUTOR_SEED: leaf_core::ProviderSeed = __leaf_seed_TokioExecutionFacility;

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{DropPolicy, ExecutionFacility};
    use std::any::TypeId;
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
        // The macro-emitted const Descriptor row lands in COMPONENTS carrying the
        // `Role::Infrastructure` provenance + the Spring `applicationTaskExecutor`
        // declared_name — the EXACT shape the old hand-written block carried, now
        // emitted by `register_component!(.., role = "infrastructure", name = "..")`.
        let contract = leaf_core::ContractId::of(APPLICATION_TASK_EXECUTOR_CONTRACT);
        let rows = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        let d = rows
            .iter()
            .find(|r| r.contract == contract)
            .expect("the applicationTaskExecutor Descriptor is in COMPONENTS");
        assert_eq!(d.declared_name, Some(APPLICATION_TASK_EXECUTOR));
        assert_eq!(d.role, leaf_core::Role::Infrastructure);
        assert_eq!(d.self_type, TypeId::of::<TokioExecutionFacility>());
    }

    #[test]
    fn application_task_executor_is_discoverable_in_components() {
        // The descriptor is link-submitted into COMPONENTS by the macro (the same
        // channel a user bean emits into), so a self-check / refresh auto-detect
        // finds it.
        let rows = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            rows.iter().any(|r| r.contract
                == leaf_core::ContractId::of(APPLICATION_TASK_EXECUTOR_CONTRACT)),
            "applicationTaskExecutor must be discoverable in COMPONENTS"
        );
    }

    #[test]
    fn application_task_executor_seed_force_links_into_seed_pairings() {
        // The proof the special case is GONE: the facility's seed force-links into the
        // SEED_PAIRINGS slice via the macro (NOT a hand-written const + a leaf-boot
        // builtin pairing), so leaf-boot's `from_slices` JOINs it like any user bean.
        let contract = leaf_core::ContractId::of(APPLICATION_TASK_EXECUTOR_CONTRACT);
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == contract),
            "the applicationTaskExecutor ProviderSeed must force-link into SEED_PAIRINGS"
        );
    }
}
