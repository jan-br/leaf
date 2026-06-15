//! `leaf-smol` — an ALTERNATIVE runtime integration: concrete impls of
//! leaf-core's execution / context / scheduling / lifecycle SPIs over
//! [smol](https://docs.rs/smol).
//!
//! This crate is the smol mirror of [`leaf-tokio`](https://docs.rs/leaf-tokio).
//! Its reason for existing is to PROVE the runtime-agnostic seam (charter §2.6,
//! TOPOLOGY.md): leaf-core names NO runtime, every "the current X" / "spawn here"
//! / "sleep until" / "stop on a signal" concern is a trait it declares, and this
//! crate implements the exact same set of traits over smol primitives instead of
//! tokio's. Anything that depends on `dyn Spawner` / `dyn AmbientStore` /
//! `dyn SchedulerCore` / `dyn ShutdownTrigger` works unchanged on either runtime.
//!
//! ## What this provides (each a concrete impl of a leaf-core SPI)
//!
//! - [`SmolExecutionFacility`] — the primary `applicationTaskExecutor`
//!   [`ExecutionFacility`](leaf_core::ExecutionFacility): [`Spawner`](leaf_core::Spawner)
//!   via [`smol::spawn`], [`BlockingOffload`](leaf_core::BlockingOffload) via
//!   [`smol::unblock`], [`ConcurrencyGate`](leaf_core::ConcurrencyGate) via an
//!   [`async_lock::Semaphore`](smol::lock::Semaphore). It coerces to
//!   `dyn ExecutionFacility` via the core blanket impl and runs on a smol
//!   executor. Its const `Role::Infrastructure`
//!   [`Descriptor`](leaf_core::Descriptor) + [`ProviderSeed`](leaf_core::ProviderSeed)
//!   are exposed for a smol-runtime binary to register (NOT link-submitted by
//!   default — that would collide with tokio's primary facility).
//! - [`SmolAmbient`] — the [`AmbientStore`](leaf_core::AmbientStore) backing the
//!   ambient [`Cx`](leaf_core::Cx). smol has no task-local, but the per-poll
//!   re-install model is a synchronous region, so a thread-local is correct even
//!   across a work-stealing hop (see [`ambient`]).
//! - [`SmolSchedulerCore`] — the ONE reactive timer-wheel backing
//!   [`SchedulerCore`](leaf_core::SchedulerCore) (a [`smol::Timer`] sleep to the
//!   global earliest, spawn the due body onto the [`Spawner`](leaf_core::Spawner);
//!   NO busy-poll), with the fixed-delay completion-feedback contract.
//! - [`SmolShutdownTrigger`] — the [`ShutdownTrigger`](leaf_core::ShutdownTrigger);
//!   its once-only firing core is implemented + tested, with the OS signal source
//!   a NOTE-d deferral pending the `async-signal` dep (see [`shutdown`]).
//!
//! ## Boot wiring
//!
//! [`install_ambient_store`] swaps the core thread-local fallback for the smol
//! backing; call it ONCE at boot before refresh. The other impls are ordinary
//! beans / seams handed to the bootstrap template.

#![deny(unsafe_code)]

pub mod ambient;
pub mod exec;
pub mod scheduler;
pub mod shutdown;

mod notify;

// ── curated re-exports: the flat runtime surface a smol boot wires ──

pub use ambient::SmolAmbient;
pub use exec::{
    SmolExecutionFacility, SmolExecutionFacilityProvider, APPLICATION_TASK_EXECUTOR,
    APPLICATION_TASK_EXECUTOR_CONTRACT, APPLICATION_TASK_EXECUTOR_DESCRIPTOR,
    APPLICATION_TASK_EXECUTOR_SEED,
};
pub use scheduler::SmolSchedulerCore;
pub use shutdown::SmolShutdownTrigger;

use std::sync::Arc;

/// Install the smol [`AmbientStore`](leaf_core::AmbientStore) backing
/// process-wide (the runtime install at boot, before refresh).
///
/// After this, [`Cx::current`](leaf_core::Cx::current) /
/// [`Scoped`](leaf_core::Scoped) read the smol backing rather than the degraded
/// core thread-local fallback.
///
/// # Errors
/// Returns the store back as `Err` if a backing was already installed (one
/// backing per process; a second install is a programming error the caller
/// surfaces as the appropriate `AssemblyError`).
pub fn install_ambient_store() -> Result<(), Arc<dyn leaf_core::AmbientStore>> {
    leaf_core::install_ambient_store(SmolAmbient::shared())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{CxFutureExt, CxKey, ExecutionFacility, Propagation, Spawner};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ReqKey;
    impl CxKey for ReqKey {
        type Value = String;
        const NAME: &'static str = "request.id";
        const POLICY: Propagation = Propagation::Inherit;
    }

    // The end-to-end propagation property the design names as the keystone:
    // ambient Cx survives an `.await` across a spawned smol task, when the body is
    // `.scoped_in(cx, store)` (the propagation hop) over the smol ambient backing.
    //
    // We thread the store explicitly via `scoped_in` rather than relying on the
    // process-wide install, because the global `install_ambient_store` is a
    // once-per-process cell that other crates' tests may also touch; `scoped_in`
    // exercises the SAME SmolAmbient backing deterministically.
    #[test]
    fn ambient_cx_propagates_across_a_smol_spawn_hop() {
        smol::block_on(async {
            let facility = SmolExecutionFacility::new();
            let store = SmolAmbient::shared();

            let cx = leaf_core::Cx::empty().with::<ReqKey>("req-42".to_string());
            let (tx, rx) = smol::channel::bounded::<Option<String>>(1);

            // Spawn the child body scoped to the captured Cx over the smol store.
            let s = store.clone();
            let body = async move {
                // Force several re-polls (possibly on another worker thread).
                for _ in 0..8 {
                    smol::future::yield_now().await;
                }
                let seen = s.current().and_then(|c| c.get::<ReqKey>().cloned());
                let _ = tx.send(seen).await;
            }
            .scoped_in(cx, store.clone());

            let h = facility.spawn(Box::pin(body));
            h.await.unwrap();
            assert_eq!(rx.recv().await.unwrap().as_deref(), Some("req-42"));
        });
    }

    // The facility coerces to `dyn ExecutionFacility` and runs on a smol executor
    // (the headline "coerces + runs" property).
    #[test]
    fn facility_coerces_to_dyn_and_runs_on_smol() {
        smol::block_on(async {
            let f: Arc<dyn ExecutionFacility> = Arc::new(SmolExecutionFacility::new());
            let n = Arc::new(AtomicU32::new(0));
            let c = n.clone();
            let h = f.spawn(Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
            }));
            h.await.unwrap();
            assert_eq!(n.load(Ordering::SeqCst), 1);
        });
    }
}
