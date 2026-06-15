//! The [`RetryInterceptor`] — around-advice that re-invokes the call on a
//! retryable error up to the policy (retry/resilience, phase3/09).
//!
//! The body, per phase3/09 §retry-resilience ("RETRY ADVISOR wraps
//! `RetryTemplate::execute(|| next.proceed())` — relying on the substrate's
//! REPLAYABLE `Next`"):
//!
//! 1. `next.proceed(call).await` (attempt N — a FRESH future each time, the
//!    substrate's REPLAYABLE `Next` guarantee);
//! 2. on `Ok` → return it;
//! 3. on a framework `AdviceError` whose [`LeafError`] is retryable → consult the
//!    [`ResilientRetry`](crate::ResilientRetry) decision, AWAIT the backoff on the
//!    reactive [`Sleeper`](crate::Sleeper) (NEVER a busy-wait), then re-proceed;
//! 4. exhaust into the LAST error.
//!
//! It sits at [`RETRY_ORDER`](leaf_core::RETRY_ORDER) (200) — INSIDE validation
//! (reject bad args once, before any retry) and OUTSIDE tx (each attempt is its
//! own fresh transaction, since the tx advisor at `TX_ORDER = 500` is INNER).
//!
//! ## Error classification scope (a NOTE)
//!
//! `proceed` yields `Err(AdviceError)` only for a FRAMEWORK fault; a method
//! returning `Result<T, LeafError>` reports a business failure THROUGH its
//! `Ok(ErasedRet)` (the chain packs the whole `Result`). This interceptor retries
//! the framework-error path by default. A method whose business `Result::Err`
//! should be retried installs a [`ReturnClassifier`] (the symmetric design to the
//! tx advisor's), which peeks inside the typed return; until the `#[retryable]`
//! macro emits the per-method classifier a binding site supplies one via
//! [`RetryInterceptor::with_return_classifier`].

use std::sync::Arc;

use leaf_core::{
    AdviceError, BoxFuture, Call, ErasedRet, Interceptor, LeafError, Next, RetryPolicy,
};

use crate::backoff::{BackoffPolicy, Sleeper};
use crate::template::ResilientRetry;

/// Classifies an advised method's ERASED `Ok` return as a retryable business
/// failure: `Some(err)` iff the typed return is a `Result::Err` (the symmetric
/// twin of the tx advisor's return classifier). [`result_classifier`] builds one
/// per concrete `T`.
pub type ReturnClassifier = fn(&ErasedRet) -> Option<LeafError>;

/// A [`ReturnClassifier`] for a method returning `Result<T, LeafError>`: downcasts
/// the erased return and clones the `Err` payload (the business failure the retry
/// policy classifies), or `None` on `Ok` / a type mismatch (degrades to "treat as
/// success", never a panic).
#[must_use]
pub fn result_classifier<T: std::any::Any + Send>() -> ReturnClassifier {
    |ret: &ErasedRet| -> Option<LeafError> {
        ret.0.downcast_ref::<Result<T, LeafError>>().and_then(|r| r.as_ref().err().cloned())
    }
}

/// The around-advice [`Interceptor`] that retries the call on a retryable error.
///
/// Holds the [`ResilientRetry`](crate::ResilientRetry) (policy + backoff + the
/// reactive sleeper) + an optional [`ReturnClassifier`] for the business-`Result::Err`
/// path. Each attempt is a fresh `next.proceed(call)` (the substrate's REPLAYABLE
/// `Next`).
pub struct RetryInterceptor {
    retry: ResilientRetry,
    classify_return: Option<ReturnClassifier>,
}

impl RetryInterceptor {
    /// Build an interceptor over a pre-assembled [`ResilientRetry`].
    #[must_use]
    pub fn new(retry: ResilientRetry) -> Self {
        RetryInterceptor { retry, classify_return: None }
    }

    /// Build from a [`RetryPolicy`] + a boxed [`BackoffPolicy`] (immediate sleeper;
    /// bind a runtime sleeper via [`with_sleeper`](Self::with_sleeper)).
    #[must_use]
    pub fn from_policy(policy: RetryPolicy, backoff: Arc<dyn BackoffPolicy>) -> Self {
        RetryInterceptor::new(ResilientRetry::new(policy, backoff))
    }

    /// Bind the reactive [`Sleeper`](crate::Sleeper) the backoff awaits on
    /// (builder style).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.retry = self.retry.with_sleeper(sleeper);
        self
    }

    /// Install a [`ReturnClassifier`] so a `Result::Err` business return (rather
    /// than only a framework `AdviceError`) is also retried (builder style).
    #[must_use]
    pub fn with_return_classifier(mut self, classify: ReturnClassifier) -> Self {
        self.classify_return = Some(classify);
        self
    }

    /// The underlying [`ResilientRetry`].
    #[must_use]
    pub fn retry(&self) -> &ResilientRetry {
        &self.retry
    }
}

impl Interceptor for RetryInterceptor {
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        mut next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
        Box::pin(async move {
            let mut attempt: u32 = 1;
            loop {
                // A FRESH proceed each attempt (REPLAYABLE Next). Each attempt sees
                // a fresh inner chain (its own tx, etc.) since retry is OUTERMOST of
                // those concerns.
                match next.proceed(call).await {
                    Ok(ret) => {
                        // An Ok carrying a business Result::Err is a retry candidate
                        // IFF a return classifier is installed and detects it.
                        match self.classify_return.and_then(|c| c(&ret)) {
                            Some(err) if self.retry.should_retry(attempt, &err).is_some() => {
                                let delay = self.retry.should_retry(attempt, &err).unwrap();
                                self.retry.delay(delay).await;
                                attempt += 1;
                            }
                            // A clean success, no classifier, or an exhausted
                            // business failure: return the Ok (the value passes through).
                            _ => return Ok(ret),
                        }
                    }
                    Err(advice_err) => {
                        // A framework AdviceError IS a retryable candidate.
                        let leaf = advice_err.into_leaf_error();
                        match self.retry.should_retry(attempt, &leaf) {
                            Some(delay) => {
                                // Park on the reactive timer (NEVER a busy-wait).
                                self.retry.delay(delay).await;
                                attempt += 1;
                            }
                            // Exhausted (max attempts / not retryable): the last error.
                            None => return Err(AdviceError::AroundBody(leaf)),
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use leaf_core::{
        AdviceChain, BeanKey, ContractId, ErasedArgs, ErrorKind, FixedTarget, MethodKey, ResolveCtx,
        Tail,
    };

    use crate::backoff::NoBackoff;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[derive(Debug)]
    struct Nop;
    impl leaf_core::Bean for Nop {}
    fn nop_target() -> FixedTarget {
        FixedTarget::new(Arc::new(Nop))
    }

    fn call_key() -> (MethodKey, BeanKey) {
        (MethodKey::of("svc::flaky"), BeanKey::ByContract(ContractId::of("svc::Svc")))
    }

    /// Drive a one-interceptor (retry) chain over a tail that returns the next
    /// scripted result each call, counting attempts.
    fn drive(
        interceptor: RetryInterceptor,
        scripted: Vec<Result<i64, ErrorKind>>,
    ) -> (Result<ErasedRet, AdviceError>, u32) {
        let interceptor: Arc<dyn Interceptor> = Arc::new(interceptor);
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = Arc::clone(&calls);
        let script = Mutex::new(scripted.into_iter());
        let tail: Box<Tail> = Box::new(move |_c: &Call<'_>| {
            calls_in.fetch_add(1, Ordering::SeqCst);
            let r = script.lock().unwrap().next().expect("an attempt was scripted");
            Box::pin(async move {
                match r {
                    Ok(v) => Ok(ErasedRet::pack(v)),
                    Err(k) => Err(AdviceError::AroundBody(LeafError::new(k))),
                }
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail));
        (out, calls.load(Ordering::SeqCst))
    }

    #[test]
    fn fails_twice_then_succeeds_is_retried() {
        // THE headline: an error twice, then Ok on the third attempt.
        let r = RetryInterceptor::from_policy(RetryPolicy::new(3), Arc::new(NoBackoff));
        let (out, calls) = drive(
            r,
            vec![Err(ErrorKind::Cancelled), Err(ErrorKind::Cancelled), Ok(42)],
        );
        assert_eq!(out.expect("ok").unpack::<i64>().unwrap(), 42, "the third attempt won");
        assert_eq!(calls, 3, "the call was re-invoked twice (three attempts total)");
    }

    #[test]
    fn exhausts_after_max_attempts() {
        let r = RetryInterceptor::from_policy(RetryPolicy::new(2), Arc::new(NoBackoff));
        let (out, calls) = drive(r, vec![Err(ErrorKind::Cancelled), Err(ErrorKind::Cancelled)]);
        assert!(out.is_err(), "exhausted into the last error");
        assert_eq!(calls, 2, "stopped at max_attempts = 2");
    }

    #[test]
    fn a_clean_success_runs_once() {
        let r = RetryInterceptor::from_policy(RetryPolicy::new(3), Arc::new(NoBackoff));
        let (out, calls) = drive(r, vec![Ok(1)]);
        assert_eq!(out.expect("ok").unpack::<i64>().unwrap(), 1);
        assert_eq!(calls, 1, "no retry on a clean success");
    }

    #[test]
    fn a_non_retryable_error_is_not_retried() {
        let policy = RetryPolicy { max_attempts: 5, is_retryable: |e| e.kind == ErrorKind::Cancelled };
        let r = RetryInterceptor::from_policy(policy, Arc::new(NoBackoff));
        let (out, calls) = drive(r, vec![Err(ErrorKind::ValidationError)]);
        assert!(out.is_err());
        assert_eq!(calls, 1, "a non-retryable error stops immediately");
    }

    #[test]
    fn a_business_result_err_is_retried_via_the_classifier() {
        // A method returning Result<i64, LeafError> reports failure THROUGH Ok(ErasedRet);
        // the classifier detects it and retries.
        let r = RetryInterceptor::from_policy(RetryPolicy::new(3), Arc::new(NoBackoff))
            .with_return_classifier(result_classifier::<i64>());
        let interceptor: Arc<dyn Interceptor> = Arc::new(r);
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = Arc::clone(&calls);
        let tail: Box<Tail> = Box::new(move |_c: &Call<'_>| {
            let n = calls_in.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                let r: Result<i64, LeafError> = if n < 2 {
                    Err(LeafError::new(ErrorKind::Cancelled))
                } else {
                    Ok(5)
                };
                Ok(ErasedRet::pack(r))
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail)).expect("the chain succeeds");
        let returned: Result<i64, LeafError> = out.unpack().unwrap();
        assert_eq!(returned.unwrap(), 5, "the second attempt's Ok value");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the business Result::Err was retried once");
    }
}
