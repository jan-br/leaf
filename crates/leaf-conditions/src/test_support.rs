//! Shared test helpers (an `Env` + a stub `ConditionCtx`).
//!
//! Compiled only under `cfg(test)`. Kept in one module so each condition's tests
//! build their context the same way.

#![cfg(test)]

use leaf_core::{ConditionCtx, Env, EnvBuilder, MapPropertySource, NoopReportSink};
use std::sync::Arc;

/// Build a sealed `Env` from `(key, value)` pairs.
#[must_use]
pub fn env_with(pairs: &[(&str, &str)]) -> Env {
    let src = MapPropertySource::from_pairs(
        "test",
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
    );
    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(src));
    b.seal_env()
}

/// A leaked no-op report sink (lives for the test's duration) so the returned
/// `ConditionCtx` borrows a `'static`-ish sink without lifetime gymnastics.
fn leaked_sink() -> &'static NoopReportSink {
    Box::leak(Box::new(NoopReportSink))
}

/// Build a `ConditionCtx` over `env`. Returns the ctx plus the sink reference so
/// the caller keeps it alive (`let (ctx, _s) = ctx_over(&env);`).
#[must_use]
pub fn ctx_over(env: &Env) -> (ConditionCtx<'_>, &'static NoopReportSink) {
    let sink = leaked_sink();
    (ConditionCtx::new(env, sink), sink)
}
