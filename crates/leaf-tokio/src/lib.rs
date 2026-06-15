//! `leaf-tokio` — the DEFAULT runtime integration: concrete impls of leaf-core's
//! execution / context / lifecycle SPIs over [tokio](https://tokio.rs).
//!
//! This crate is the runtime half of the async spine the kernel (`leaf-core`)
//! defines abstractly (execution-context phase3/10 + container-lifecycle
//! phase3/13, ADR-07). leaf-core names NO runtime; every "the current X" /
//! "spawn here" / "sleep until" seam is a trait it declares and THIS crate
//! implements over tokio primitives. `leaf-boot` force-links this crate and
//! HARD-FAILS the refresh self-check if no primary `ExecutionFacility` is present.
//!
//! ## What this provides (each a concrete impl of a leaf-core SPI)
//!
//! - [`TokioExecutionFacility`] — the primary `applicationTaskExecutor`
//!   [`ExecutionFacility`](leaf_core::ExecutionFacility): [`Spawner`](leaf_core::Spawner)
//!   via [`tokio::spawn`], [`BlockingOffload`](leaf_core::BlockingOffload) via
//!   `spawn_blocking`, [`ConcurrencyGate`](leaf_core::ConcurrencyGate) via a
//!   [`Semaphore`](tokio::sync::Semaphore). Submitted into
//!   [`COMPONENTS`](leaf_core::COMPONENTS) as a const `Role::Infrastructure`
//!   [`Descriptor`](leaf_core::Descriptor) via the same link-time channel the
//!   macros emit into (see [`exec::APPLICATION_TASK_EXECUTOR_DESCRIPTOR`]).
//! - [`TokioAmbient`] — the [`AmbientStore`](leaf_core::AmbientStore) backing the
//!   ambient [`Cx`](leaf_core::Cx) over `tokio::task_local!` (the per-poll
//!   re-install that rides a work-stealing hop, where the core thread-local
//!   fallback would lose context).
//! - [`TokioSchedulerCore`] — the ONE reactive timer-wheel backing
//!   [`SchedulerCore`](leaf_core::SchedulerCore) (`sleep_until` the global
//!   earliest, spawn the due body onto the [`Spawner`](leaf_core::Spawner); NO
//!   busy-poll), also the cold-path timer retry/backoff reuse.
//! - [`FileResourceProvider`] — the `file:` [`ResourceProvider`](leaf_core::ResourceProvider),
//!   with an async [`Resource`](leaf_core::Resource) /
//!   [`ResourceReader`](leaf_core::ResourceReader) over `tokio::fs`.
//! - [`AsyncDispatchInterceptor`] — the async-dispatch concern as a
//!   [`DispatchInterceptor`](leaf_core::DispatchInterceptor) entry (captures +
//!   re-installs the ambient `Cx` around the listener fan-out).
//! - [`availability`] — the process-wide availability watch-cell home.
//! - [`TokioShutdownTrigger`] — the `tokio::signal`-based
//!   [`ShutdownTrigger`](leaf_core::ShutdownTrigger).
//!
//! ## Boot wiring
//!
//! [`install_ambient_store`] swaps the core thread-local fallback for the tokio
//! task-local backing; call it ONCE at boot before refresh. The other impls are
//! ordinary beans resolved through the engine (the facility) or seams handed to
//! the bootstrap template (the scheduler / shutdown trigger).

#![deny(unsafe_code)]

pub mod ambient;
pub mod availability;
pub mod dispatch;
pub mod exec;
pub mod resource;
pub mod scheduler;
pub mod shutdown;
pub mod sleeper;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag in
// the link-collected `SOURCES` slice so the binary's expected-vs-found self-check
// can tell "linked-but-zero-rows" from "never-linked". A force-linked-but-zero-
// contributing leaf-tokio (a real DCE drop) becomes a loud `SourceVanished` naming
// it rather than a silent missing ExecutionFacility. The package name (dashes) is
// the author-stable string the ExpectedManifest joins on.
leaf_core::declare_source!("leaf-tokio");

// ── curated re-exports: the flat runtime surface leaf-boot wires ──

pub use ambient::TokioAmbient;
pub use availability::availability;
pub use dispatch::{AsyncDispatchInterceptor, CaptureCurrentCx};
pub use exec::{
    TokioExecutionFacility, TokioExecutionFacilityProvider, APPLICATION_TASK_EXECUTOR,
    APPLICATION_TASK_EXECUTOR_CONTRACT, APPLICATION_TASK_EXECUTOR_DESCRIPTOR,
    APPLICATION_TASK_EXECUTOR_SEED,
};
pub use resource::{FileResource, FileResourceProvider};
pub use scheduler::TokioSchedulerCore;
pub use shutdown::TokioShutdownTrigger;
pub use sleeper::{install_tokio_sleeper, TokioSleeper};

use std::sync::Arc;

/// Install the tokio [`AmbientStore`](leaf_core::AmbientStore) backing
/// process-wide (the runtime install at boot, before refresh).
///
/// After this, [`Cx::current`](leaf_core::Cx::current) /
/// [`Scoped`](leaf_core::Scoped) read the `tokio::task_local!` backing rather than
/// the degraded core thread-local fallback — so ambient context rides a
/// work-stealing task migration correctly.
///
/// # Errors
/// Returns the store back as `Err` if a backing was already installed (one
/// backing per process; a second install is a programming error the caller
/// surfaces as the appropriate `AssemblyError`).
pub fn install_ambient_store() -> Result<(), Arc<dyn leaf_core::AmbientStore>> {
    leaf_core::install_ambient_store(TokioAmbient::shared())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{CxFutureExt, CxKey, Propagation, Spawner};

    struct ReqKey;
    impl CxKey for ReqKey {
        type Value = String;
        const NAME: &'static str = "request.id";
        const POLICY: Propagation = Propagation::Inherit;
    }

    // The end-to-end propagation property the design names as the keystone:
    // ambient Cx survives an `.await` across a spawned tokio task, when the body
    // is `.scoped(Cx::current())` (the propagation hop) and the tokio backing is
    // installed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ambient_cx_propagates_across_a_spawn_hop() {
        let _ = install_ambient_store();
        let facility = TokioExecutionFacility::new();

        let cx = leaf_core::Cx::empty().with::<ReqKey>("req-42".to_string());
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Spawn the child body scoped to the captured Cx (the hop).
        let body = async move {
            // Force re-polls (possibly on another worker thread).
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            let seen = leaf_core::Cx::current().and_then(|c| c.get::<ReqKey>().cloned());
            let _ = tx.send(seen);
        }
        .scoped(cx);

        let h = facility.spawn(Box::pin(body));
        h.await.unwrap();
        assert_eq!(rx.await.unwrap().as_deref(), Some("req-42"));
    }

    #[test]
    fn install_ambient_store_is_idempotent_after_first() {
        // First install may succeed or already be done by another test; a second
        // install must return Err (one backing per process).
        let _ = install_ambient_store();
        assert!(
            install_ambient_store().is_err(),
            "a second ambient-store install must be rejected"
        );
    }
}
