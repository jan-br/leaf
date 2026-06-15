use std::sync::atomic::{AtomicUsize, Ordering};

use leaf::prelude::*;

/// Set once when the runner fires (a process-global so the test can assert it ran).
pub static RUNNER_FIRED: AtomicUsize = AtomicUsize::new(0);

/// A `#[runner]` that runs once at startup, in the readiness-gate window (the
/// migration/warmup hook).
#[runner]
pub struct StartupRunner;

impl StartupRunner {
    fn new() -> Self {
        StartupRunner
    }
}

impl Runner for StartupRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf::core::ApplicationArguments,
    ) -> leaf::core::BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            RUNNER_FIRED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
