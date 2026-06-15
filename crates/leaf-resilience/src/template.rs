//! The imperative retry primitive's awaiting `execute` loop (retry/resilience,
//! phase3/09).
//!
//! leaf-core ships the data + the PURE decision
//! ([`RetryTemplate`](leaf_core::RetryTemplate)`{policy, backoff}` +
//! [`should_retry`](leaf_core::RetryTemplate::should_retry)). The design pins that
//! "the awaiting `execute` loop with real sleeps lives in leaf-resilience over the
//! ExecutionFacility" — so this module adds [`ResilientRetry`]: the core template
//! PLUS a [`Sleeper`](crate::Sleeper), with the async [`execute`](ResilientRetry::execute)
//! loop the RETRY advisor AND container-free user code both drive.
//!
//! The loop, per phase3/09 §retry-resilience:
//!
//! 1. run `op()` (attempt N, 1-based) — a fresh future each call (the substrate's
//!    REPLAYABLE `Next::proceed`);
//! 2. on `Ok` → return it;
//! 3. on `Err` → consult [`should_retry`](leaf_core::RetryTemplate::should_retry)
//!    (max-attempts + the typed retryability predicate): if it yields a delay, AWAIT
//!    the [`Sleeper`](crate::Sleeper) (parked on the reactive timer — NEVER a
//!    busy-wait), then loop; else EXHAUST into the last error.
//!
//! It is container-free (the self-invocation escape hatch): a user constructs a
//! [`ResilientRetry`] and calls `execute` directly off the AOP path.

use std::sync::Arc;
use std::time::Duration;

use leaf_core::{BoxFuture, LeafError, RetryPolicy, RetryTemplate};

use crate::backoff::{immediate_sleeper, BackoffPolicy, Sleeper};

/// The imperative retry primitive WITH its reactive sleeper: the core
/// [`RetryTemplate`] (policy + backoff) plus the [`Sleeper`](crate::Sleeper) the
/// awaiting [`execute`](ResilientRetry::execute) loop parks on.
#[derive(Clone)]
pub struct ResilientRetry {
    template: RetryTemplate,
    sleeper: Arc<dyn Sleeper>,
}

impl ResilientRetry {
    /// Build from a [`RetryPolicy`] + a boxed [`BackoffPolicy`], using the
    /// runtime-free [`ImmediateSleeper`](crate::ImmediateSleeper) (zero real
    /// sleeps). Bind a runtime-backed sleeper via [`with_sleeper`](Self::with_sleeper)
    /// for real timed backoff.
    #[must_use]
    pub fn new(policy: RetryPolicy, backoff: Arc<dyn BackoffPolicy>) -> Self {
        ResilientRetry {
            template: RetryTemplate::new(policy, backoff),
            sleeper: immediate_sleeper(),
        }
    }

    /// Build from a pre-assembled core [`RetryTemplate`] (immediate sleeper).
    #[must_use]
    pub fn from_template(template: RetryTemplate) -> Self {
        ResilientRetry { template, sleeper: immediate_sleeper() }
    }

    /// Bind the reactive [`Sleeper`](crate::Sleeper) the backoff awaits on
    /// (builder style) — a runtime crate's timer-backed sleeper for real delays.
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// The underlying core [`RetryTemplate`] (policy + backoff).
    #[must_use]
    pub fn template(&self) -> &RetryTemplate {
        &self.template
    }

    /// The pure per-attempt delay decision (delegates to the core template): the
    /// delay before re-attempting after `attempt` failures with `err`, or `None`
    /// to exhaust.
    #[must_use]
    pub fn should_retry(&self, attempt: u32, err: &LeafError) -> Option<Duration> {
        self.template.should_retry(attempt, err)
    }

    /// Await `delay` on the bound reactive [`Sleeper`](crate::Sleeper) (parked on
    /// the timer — NEVER a busy-wait). The [`RetryInterceptor`](crate::RetryInterceptor)
    /// drives this between attempts (it owns the `Next` borrow, so it cannot route
    /// through [`execute`](Self::execute)'s `FnMut` closure).
    pub fn delay(&self, delay: Duration) -> BoxFuture<'_, ()> {
        self.sleeper.sleep(delay)
    }

    /// Run `op` with retry: each call is a FRESH attempt (a fresh future — the
    /// substrate's REPLAYABLE `Next::proceed`). On a retryable error it AWAITs the
    /// backoff delay on the reactive [`Sleeper`](crate::Sleeper) (never busy-waits),
    /// then re-attempts; it exhausts into the LAST error.
    ///
    /// `op` is `FnMut` so it can be re-invoked; it yields a `BoxFuture` so the
    /// loop owns no borrow across attempts (each attempt is independent — e.g. a
    /// fresh tx, since the RETRY advisor sits OUTSIDE tx).
    ///
    /// # Errors
    /// The last attempt's [`LeafError`] once retries are exhausted (max-attempts
    /// reached or the error is not retryable).
    pub fn execute<'a, T, F>(&'a self, mut op: F) -> BoxFuture<'a, Result<T, LeafError>>
    where
        F: FnMut() -> BoxFuture<'a, Result<T, LeafError>> + Send + 'a,
        T: Send + 'a,
    {
        Box::pin(async move {
            let mut attempt: u32 = 1;
            loop {
                match op().await {
                    Ok(value) => return Ok(value),
                    Err(err) => match self.should_retry(attempt, &err) {
                        Some(delay) => {
                            // Park on the reactive timer (cold-path, NEVER a spin).
                            self.sleeper.sleep(delay).await;
                            attempt += 1;
                        }
                        // Exhausted (max attempts / not retryable): the last error.
                        None => return Err(err),
                    },
                }
            }
        })
    }
}

impl std::fmt::Debug for ResilientRetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientRetry").field("template", &self.template).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use leaf_core::ErrorKind;

    use crate::backoff::{ExponentialBackoff, NoBackoff, Sleeper};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    /// A sleeper that RECORDS every delay it was asked to wait (so a test asserts
    /// the backoff was consulted) without actually sleeping.
    #[derive(Default)]
    struct RecordingSleeper {
        delays: std::sync::Mutex<Vec<Duration>>,
    }
    impl Sleeper for RecordingSleeper {
        fn sleep(&self, delay: Duration) -> BoxFuture<'static, ()> {
            self.delays.lock().unwrap().push(delay);
            Box::pin(std::future::ready(()))
        }
    }

    #[test]
    fn succeeds_on_first_attempt_runs_op_once() {
        let calls = AtomicU32::new(0);
        let retry = ResilientRetry::new(RetryPolicy::new(3), Arc::new(NoBackoff));
        let out: Result<i64, _> = block(retry.execute(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(7) })
        }));
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry needed");
    }

    #[test]
    fn fails_twice_then_succeeds_retries_up_to_the_policy() {
        // THE headline behavior: fail twice, succeed on the third attempt.
        let calls = AtomicU32::new(0);
        let retry = ResilientRetry::new(RetryPolicy::new(3), Arc::new(NoBackoff));
        let out: Result<i64, _> = block(retry.execute(|| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if n < 3 {
                    Err(LeafError::new(ErrorKind::Cancelled))
                } else {
                    Ok(99)
                }
            })
        }));
        assert_eq!(out.unwrap(), 99, "the third attempt succeeded");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "exactly three attempts (a fresh op each)");
    }

    #[test]
    fn exhausts_into_the_last_error_at_max_attempts() {
        let calls = AtomicU32::new(0);
        let retry = ResilientRetry::new(RetryPolicy::new(2), Arc::new(NoBackoff));
        let out: Result<i64, _> = block(retry.execute(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(LeafError::new(ErrorKind::Cancelled)) })
        }));
        assert!(out.is_err(), "exhausted");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "stopped at max_attempts = 2");
    }

    #[test]
    fn a_non_retryable_error_stops_immediately() {
        let calls = AtomicU32::new(0);
        // Only Cancelled is retryable; the op returns ValidationError.
        let policy = RetryPolicy { max_attempts: 5, is_retryable: |e| e.kind == ErrorKind::Cancelled };
        let retry = ResilientRetry::new(policy, Arc::new(NoBackoff));
        let out: Result<i64, _> = block(retry.execute(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(LeafError::new(ErrorKind::ValidationError)) })
        }));
        assert!(out.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a non-retryable error is not retried");
    }

    #[test]
    fn the_backoff_delay_is_awaited_on_the_sleeper() {
        // The loop consults the backoff and awaits the sleeper between attempts.
        let sleeper = Arc::new(RecordingSleeper::default());
        let retry = ResilientRetry::new(
            RetryPolicy::new(3),
            Arc::new(ExponentialBackoff::new(Duration::from_millis(10), 2.0)),
        )
        .with_sleeper(sleeper.clone() as Arc<dyn Sleeper>);
        let _: Result<i64, _> = block(retry.execute(|| {
            Box::pin(async { Err(LeafError::new(ErrorKind::Cancelled)) })
        }));
        let delays = sleeper.delays.lock().unwrap().clone();
        // Two waits between three attempts: 10ms then 20ms (exponential).
        assert_eq!(delays, vec![Duration::from_millis(10), Duration::from_millis(20)]);
    }
}
