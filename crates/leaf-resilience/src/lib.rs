//! `leaf-resilience` — the retry + concurrency-limit cross-cutting concern crate
//! (declarative-advice, phase3/09): it SHIPS the runtime [`RetryInterceptor`] +
//! [`ConcurrencyLimitInterceptor`] (both `Role::Infrastructure` around-advice) AND
//! the TWO Infrastructure resilience advisors that auto-wire through the run
//! pipeline.
//!
//! The pieces (all resting on the leaf-core ABI — nothing minted twice):
//!
//! - **the imperative primitive** ([`ResilientRetry`]) — the core
//!   [`RetryTemplate`](leaf_core::RetryTemplate)`{policy, backoff}` + a reactive
//!   [`Sleeper`] with the awaiting [`execute`](ResilientRetry::execute) loop (on a
//!   retryable error it AWAITs the backoff on the reactive timer — NEVER a
//!   busy-wait — and re-attempts, exhausting into the last error). Container-free
//!   (the self-invocation escape hatch).
//! - **backoff** ([`backoff`]) — [`FixedBackoff`](leaf_core::FixedBackoff) (reused
//!   from core), [`ExponentialBackoff`]`{base, mult, max, jitter}`, [`NoBackoff`],
//!   plus the reactive [`Sleeper`] seam ([`ImmediateSleeper`] is the runtime-free
//!   default; a runtime crate supplies a timer-backed sleeper).
//! - **[`RetryInterceptor`]** ([`retry`]) — the `RETRY_ORDER = 200` around-advice
//!   (OUTSIDE tx, INSIDE validation) that re-invokes the call on a retryable error
//!   up to the policy — each attempt a FRESH `next.proceed()` (the substrate's
//!   REPLAYABLE `Next`).
//! - **[`ConcurrencyLimitInterceptor`]** ([`concurrency`]) — the
//!   `CONCURRENCY_ORDER = 550` around-advice (INSIDE tx) that caps concurrent
//!   entries via the shared [`ConcurrencyGate`](leaf_core::ConcurrencyGate) (NOT a
//!   new limiter): `acquire().await` a [`Permit`](leaf_core::Permit), proceed, the
//!   permit's sync RAII `Drop` releases on completion AND cancellation. `n = 1` is
//!   the declarative instance lock.
//! - **advisor** ([`advisor`]) — the two Infrastructure [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow)
//!   builders + [`ResiliencePointcut`] that auto-wire through `Application::run`'s
//!   `ADVISOR_PAIRINGS` collection, plus [`enable_resilient_methods`] (the mandatory
//!   two-advisor self-check naming BOTH advisor identities).
//!
//! ## Deferred (honest NOTEs)
//!
//! - The `#[retryable(max=…, backoff=…)]` / `#[concurrency_limit(n)]` attribute
//!   macros (which would emit the per-method [`RetryPolicy`](leaf_core::RetryPolicy)
//!   /gate-size + the resilience markers the pointcut keys on) are NOT in
//!   leaf-macros; until they land the auto-wire row keys on a leaf-resilience-owned
//!   marker (or a concrete `TypeId` via [`ResiliencePointcut`]) and a binding site
//!   supplies the policy via a [`RetrySpec`] / a closure-literal row.
//! - The reactive [`Sleeper`] is a leaf-resilience seam (leaf-core ships no
//!   runtime-agnostic async-sleep trait). The runtime-free [`ImmediateSleeper`] is
//!   the default; a binding site that needs real timed backoff hands a
//!   timer-backed sleeper (e.g. over `tokio::time::sleep`) through
//!   [`RetryInterceptor::with_sleeper`]. The interceptor body NEVER busy-polls — it
//!   `.await`s the sleeper's future.
//! - Jitter is a deterministic per-attempt hash (reproducible, dependency-free);
//!   true random full-jitter would need an RNG bean (deferred).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod advisor;
pub mod backoff;
pub mod concurrency;
pub mod retry;
pub mod template;

pub use advisor::{
    concurrency_advisor_contract, concurrency_advisor_pairing, concurrency_limit_marker,
    concurrency_order_key, enable_resilient_methods, make_concurrency_interceptor,
    make_retry_interceptor, retry_advisor_contract, retry_advisor_pairing, retry_order_key,
    retryable_marker, ResiliencePointcut, RetrySpec, CONCURRENCY_LIMIT_MARKER_POINTCUT,
    RETRYABLE_MARKER_POINTCUT,
};
pub use backoff::{
    immediate_sleeper, BackoffPolicy, ExponentialBackoff, FixedBackoff, ImmediateSleeper, NoBackoff,
    Sleeper,
};
pub use concurrency::ConcurrencyLimitInterceptor;
pub use retry::{result_classifier, RetryInterceptor, ReturnClassifier};
pub use template::ResilientRetry;

// Re-export the core decision primitives so a consumer reaches the whole retry
// vocabulary through leaf-resilience (one import surface).
pub use leaf_core::{RetryPolicy, RetryTemplate};
