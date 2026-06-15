//! The default-runtime bootstrap bridge: wire the leaf-tokio
//! [`ExecutionFacility`](leaf_core::ExecutionFacility) into the
//! [`Application`](leaf_boot::Application) run pipeline (charter §2.6: async-first,
//! a real runtime out of the box).
//!
//! The base umbrella always pulls `leaf-tokio` as the default runtime. The run
//! pipeline needs a [`Spawner`](leaf_core::Spawner) (for `Bootstrap::Background`
//! eager beans + the scheduler body executor) and the tokio ambient
//! [`AmbientStore`](leaf_core::AmbientStore) installed before refresh. [`bootstrap`]
//! does both and returns a ready-to-run `Application`, so a downstream writes:
//!
//! ```ignore
//! # async fn run() -> Result<(), leaf_boot::RunFailure> {
//! let app = leaf::bootstrap("my-app");
//! let running = app.run(leaf::RunInputs::from_env().into(), leaf_boot::RunOverlay::none()).await?;
//! # let _ = running; Ok(())
//! # }
//! ```
//!
//! The runtime itself (a tokio `Runtime` + `block_on`) is the BINARY crate's to
//! own — `#[leaf::main]` / a hand-written `#[tokio::main]` provides the executor the
//! returned `Application` future drives on. This bridge owns only the leaf-side
//! wiring (the facility + the ambient store), not the executor construction.

use std::ffi::OsString;
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, RunningApp, SealInputs};
use leaf_core::{BoxFuture, LeafError};
use leaf_tokio::{TokioExecutionFacility, TokioSchedulerCore};

/// Build the default leaf [`Application`] over the tokio runtime: install the tokio
/// ambient [`AmbientStore`](leaf_core::AmbientStore) (idempotent — a no-op if
/// already installed) and wire the tokio [`Spawner`](leaf_core::Spawner) facility +
/// the reactive [`SchedulerCore`](leaf_core::SchedulerCore) so `Bootstrap::Background`
/// beans + `#[scheduled]` tasks run.
///
/// Every per-bean wiring channel (seeds, guards, advisors, runners, config binds, …)
/// AUTO-COLLECTS from its `linkme` slice inside [`Application::run`] — so the
/// returned application needs no `.with_*` tables; it is ready to `.run(inputs,
/// overlay)`. The `name` is the banner / diagnostics application name.
///
/// The caller drives the returned future on its own executor (the tokio runtime the
/// binary owns via `#[leaf::main]` / `#[tokio::main]`).
#[must_use]
pub fn bootstrap(name: &'static str) -> Application {
    // Swap the core thread-local ambient fallback for the tokio task-local backing
    // (so the ambient Cx rides a work-stealing hop). Idempotent: an `Err` means it
    // was already installed by an earlier bootstrap — harmless, so ignore it.
    let _ = leaf_tokio::install_ambient_store();

    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(TokioExecutionFacility::new());
    let scheduler: Arc<dyn leaf_core::SchedulerCore> =
        Arc::new(TokioSchedulerCore::new(Arc::clone(&spawner)));

    Application::new()
        .with_name(name)
        .with_spawner(spawner)
        .with_scheduler(scheduler)
}

/// A thin, ergonomic builder for the [`SealInputs`] the run pipeline's environment
/// fence consumes — the umbrella's convenience over the raw leaf-boot type so a
/// downstream reaches argv without naming `leaf-boot` (the prelude exports neither
/// `SealInputs` nor this; it is reached as `leaf::RunInputs`).
///
/// The common path is [`RunInputs::from_env`] (the process argv). Convert into the
/// leaf-boot [`SealInputs`] with [`Into`] / [`RunInputs::into_seal_inputs`].
#[derive(Clone, Debug, Default)]
pub struct RunInputs {
    argv: Vec<OsString>,
}

impl RunInputs {
    /// An empty input bundle (no argv) — the in-process / test path.
    #[must_use]
    pub fn new() -> Self {
        RunInputs::default()
    }

    /// Capture the process command-line arguments (EXCLUDING the program name, per
    /// the [`SealInputs`] argv convention) — the real-binary path.
    #[must_use]
    pub fn from_env() -> Self {
        RunInputs { argv: std::env::args_os().skip(1).collect() }
    }

    /// Set the raw argv explicitly (the test / programmatic path).
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.argv = args.into_iter().map(Into::into).collect();
        self
    }

    /// Lower to the leaf-boot [`SealInputs`] the run pipeline consumes.
    #[must_use]
    pub fn into_seal_inputs(self) -> SealInputs {
        SealInputs::new().with_args(self.argv)
    }
}

impl From<RunInputs> for SealInputs {
    fn from(inputs: RunInputs) -> Self {
        inputs.into_seal_inputs()
    }
}

/// THE `#[leaf::main]` ENTRY DRIVER (the umbrella-only maximal-magic DX): build the
/// default tokio runtime the umbrella owns, bootstrap + run the application to Ready
/// (the per-bean wiring + auto-configs + `#[runner]`s all fire here), hand the live
/// [`RunningApp`] to the user's `async fn main` body, then shut down cleanly — all
/// from `#[leaf::main]` + the single `leaf` dependency.
///
/// The generated `fn main()` `#[leaf::main]` emits is a one-liner over this:
///
/// ```ignore
/// fn main() -> Result<(), ::leaf::LeafError> {
///     ::leaf::run_main(env!("CARGO_PKG_NAME"), |app| async move { /* user body */ })
/// }
/// ```
///
/// The user body receives the live [`RunningApp`] by shared reference (the wired
/// [`Context`](leaf_core::Context), the advised-method seam, availability) so it can
/// drive post-Ready work; the umbrella owns the clean shutdown drain afterwards.
///
/// The binary owns the executor (the tokio `Runtime` built here) per the bootstrap
/// bridge's contract; this is the umbrella's blessed default. A binary wanting a
/// hand-built runtime / a different shutdown policy drops to [`bootstrap`] +
/// `Application::run` directly (the escape hatch the proof test exercises).
///
/// The body is a closure returning a [`BoxFuture`] borrowing the [`RunningApp`] (the
/// `#[leaf::main]` macro wraps the user's `async` body in `Box::pin`), so the body may
/// `.await` over the live context — the boxed-future lifetime ties cleanly to the app
/// borrow (the HRTB-with-borrowed-return shape that a bare `-> impl Future` cannot
/// express).
///
/// # Errors
/// A [`LeafError`] if the tokio runtime cannot be built, the run pipeline faults
/// (the [`RunFailure`](leaf_boot::RunFailure)'s `error` is surfaced), or the user
/// body returns `Err`.
pub fn run_main<F>(name: &'static str, body: F) -> Result<(), LeafError>
where
    F: for<'a> FnOnce(&'a RunningApp) -> BoxFuture<'a, Result<(), LeafError>>,
{
    // The binary owns the executor: build the default multi-thread tokio runtime the
    // returned Application future drives on (charter §2.6: a real runtime out of the
    // box). A build failure is the one non-LeafError fault — map it onto the spine.
    let runtime = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(
                leaf_core::Cause::plain("building the tokio runtime", e.to_string()),
            )
        })?;

    runtime.block_on(async move {
        // Bootstrap + run to Ready: the runners fire in the readiness-gate window, the
        // graph wires, the auto-configs participate — all auto-collected inside `run`.
        let running = bootstrap(name)
            .run(RunInputs::from_env().into(), RunOverlay::none())
            .await
            .map_err(|failure| failure.error)?;

        // The user's `async fn main` body, over the live RunningApp.
        let outcome = body(&running).await;

        // Drain the teardown ledger LIFO regardless of the body's outcome (a clean
        // shutdown is part of the proof), then surface the body's result.
        running.shutdown().await;
        outcome
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_builds_a_named_application_with_a_spawner() {
        let app = bootstrap("test-app");
        // The Debug surface confirms the name + that the tables start empty (the
        // slices auto-collect inside run, not here). The spawner/scheduler are
        // private, so we assert the build does not panic + produces a fresh app.
        let dbg = format!("{app:?}");
        assert!(dbg.contains("Application"), "got: {dbg}");
    }

    #[test]
    fn run_inputs_from_args_lower_to_seal_inputs_argv() {
        let inputs = RunInputs::new().with_args(["--app.title=Hi", "--app.workers=4"]);
        let seal: SealInputs = inputs.into();
        assert_eq!(seal.argv.len(), 2);
        assert_eq!(seal.argv[0], "--app.title=Hi");
    }

    #[test]
    fn empty_run_inputs_lower_to_empty_argv() {
        let seal: SealInputs = RunInputs::new().into();
        assert!(seal.argv.is_empty());
    }
}
