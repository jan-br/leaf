//! The [`ConcurrencyLimitInterceptor`] — around-advice that caps concurrent
//! entries to an advised method via the shared [`ConcurrencyGate`] (retry/resilience,
//! phase3/09 §`#[concurrency_limit(n)]`).
//!
//! The body, per phase3/09 ("holds an `Arc<dyn ConcurrencyGate>` sized from the
//! attribute (reusing async-context-model's gate — NOT a new limiter); its body:
//! `let _permit = gate.acquire().await; next.proceed().await`"):
//!
//! 1. `gate.acquire().await` — await a [`Permit`](leaf_core::Permit) (the gate
//!    parks if saturated; NEVER a busy-wait — it is a `Semaphore`-backed wait);
//! 2. `next.proceed(call).await` while HOLDING the permit;
//! 3. the [`Permit`](leaf_core::Permit)'s sync RAII `Drop` releases the slot on
//!    completion AND on cancellation (no leak, no async Drop).
//!
//! `n = 1` is the declarative instance lock (one body at a time). It sits at
//! [`CONCURRENCY_ORDER`](leaf_core::CONCURRENCY_ORDER) (550) — INNER of tx (so the
//! permit is held only for the actual work, INSIDE the tx demarcation) — and,
//! relative to retry (`RETRY_ORDER = 200`, OUTER), the permit is RE-ACQUIRED per
//! attempt (not held across backoff sleeps — the design's "permit per-attempt, not
//! across all sleeps" budget rule).
//!
//! The gate is the ONE async-context-model [`ConcurrencyGate`] (the
//! `ExecutionFacility`'s `Semaphore`-backed gate, or a dedicated small-limit gate
//! bean sized from the `#[concurrency_limit(n)]` attribute) — leaf-resilience mints
//! NO new limiter.

use std::sync::Arc;

use leaf_core::{AdviceError, BoxFuture, Call, ConcurrencyGate, ErasedRet, Interceptor, Next};

/// The around-advice [`Interceptor`] that bounds concurrent entries via a
/// [`ConcurrencyGate`]: acquire a permit, proceed, release on Drop.
pub struct ConcurrencyLimitInterceptor {
    gate: Arc<dyn ConcurrencyGate>,
}

impl ConcurrencyLimitInterceptor {
    /// Build an interceptor over a resolved [`ConcurrencyGate`] (the gate is sized
    /// elsewhere — the facility's gate, or a small-limit gate bean from the
    /// `#[concurrency_limit(n)]` attribute).
    #[must_use]
    pub fn new(gate: Arc<dyn ConcurrencyGate>) -> Self {
        ConcurrencyLimitInterceptor { gate }
    }
}

impl Interceptor for ConcurrencyLimitInterceptor {
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        mut next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
        Box::pin(async move {
            // Acquire a permit (parks if saturated — Semaphore-backed, NOT a spin).
            let _permit = self.gate.acquire().await;
            // Proceed while HOLDING the permit; `_permit`'s sync RAII Drop releases
            // the slot on completion AND on cancellation (no leak, no async Drop).
            next.proceed(call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use futures::future::join_all;
    use leaf_core::{
        AdviceChain, BeanKey, ContractId, ErasedArgs, FixedTarget, MethodKey, Permit, PermitSeam,
        ResolveCtx, Tail,
    };

    #[derive(Debug)]
    struct Nop;
    impl leaf_core::Bean for Nop {}
    fn nop_target() -> FixedTarget {
        FixedTarget::new(Arc::new(Nop))
    }

    /// A future that returns `Pending` on the first poll and `Ready(())` on the
    /// second — a cooperative single-tick yield (no CPU spin), so interleaved
    /// futures on one executor make progress in turn.
    fn yield_once() -> impl std::future::Future<Output = ()> {
        let mut yielded = false;
        std::future::poll_fn(move |cx| {
            if yielded {
                std::task::Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        })
    }

    // ── a runtime-free counting gate (limit-N) for the unit test ──

    /// A guard whose `Drop` decrements the live count (the RAII release the real
    /// Semaphore permit performs).
    struct CountingPermit {
        live: Arc<AtomicUsize>,
    }
    impl PermitSeam for CountingPermit {}
    impl Drop for CountingPermit {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A `ConcurrencyGate` that admits up to `limit` permits, recording the PEAK
    /// concurrent live permits. Runtime-free: `acquire` resolves immediately while
    /// under the limit, else yields once and re-checks (a tiny cooperative wait, no
    /// CPU spin) — sufficient for a single-threaded block_on test.
    struct CountingGate {
        limit: usize,
        live: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }
    impl ConcurrencyGate for CountingGate {
        fn acquire(&self) -> BoxFuture<'static, Permit> {
            let limit = self.limit;
            let live = Arc::clone(&self.live);
            let peak = Arc::clone(&self.peak);
            Box::pin(async move {
                loop {
                    let cur = live.load(Ordering::SeqCst);
                    if cur < limit
                        && live
                            .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        peak.fetch_max(cur + 1, Ordering::SeqCst);
                        return Permit::new(Box::new(CountingPermit { live }));
                    }
                    // Saturated: yield to let a holder finish (cooperative, not a spin).
                    yield_once().await;
                }
            })
        }
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn a_single_call_proceeds_under_the_limit() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let gate: Arc<dyn ConcurrencyGate> =
            Arc::new(CountingGate { limit: 2, live: Arc::clone(&live), peak: Arc::clone(&peak) });
        let interceptor: Arc<dyn Interceptor> = Arc::new(ConcurrencyLimitInterceptor::new(gate));
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let call = Call::new(
            MethodKey::of("svc::guarded"),
            BeanKey::ByContract(ContractId::of("svc::Svc")),
            ErasedArgs::none(),
            &target,
            &cx,
        );
        let tail: Box<Tail> = Box::new(|_c: &Call<'_>| {
            Box::pin(async { Ok(ErasedRet::pack(1_i64)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 1, "the guarded method ran");
        assert_eq!(live.load(Ordering::SeqCst), 0, "the permit was released after the body");
    }

    #[test]
    fn the_permit_is_released_on_drop_even_after_an_error() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let gate: Arc<dyn ConcurrencyGate> =
            Arc::new(CountingGate { limit: 1, live: Arc::clone(&live), peak });
        let interceptor: Arc<dyn Interceptor> = Arc::new(ConcurrencyLimitInterceptor::new(gate));
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let call = Call::new(
            MethodKey::of("svc::guarded"),
            BeanKey::ByContract(ContractId::of("svc::Svc")),
            ErasedArgs::none(),
            &target,
            &cx,
        );
        let tail: Box<Tail> = Box::new(|_c: &Call<'_>| {
            Box::pin(async {
                Err(AdviceError::AroundBody(leaf_core::LeafError::new(
                    leaf_core::ErrorKind::Cancelled,
                )))
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail));
        assert!(out.is_err(), "the error propagates");
        assert_eq!(live.load(Ordering::SeqCst), 0, "the permit released on the error path too");
    }

    #[test]
    fn concurrent_entries_are_capped_at_the_limit() {
        // Drive MANY interleaved guarded calls on one executor; assert the gate
        // never let more than `limit` bodies hold a permit at once (the peak).
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        const LIMIT: usize = 3;
        let gate: Arc<dyn ConcurrencyGate> = Arc::new(CountingGate {
            limit: LIMIT,
            live: Arc::clone(&live),
            peak: Arc::clone(&peak),
        });
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(ConcurrencyLimitInterceptor::new(Arc::clone(&gate)));
        let chain = Arc::new(AdviceChain::new(Box::new([interceptor])));

        // Record the peak SEEN INSIDE the bodies too (how many were live when each ran).
        let inside_peak = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));

        let fut = async {
            let mut tasks = Vec::new();
            for _ in 0..20 {
                let chain = Arc::clone(&chain);
                let running = Arc::clone(&running);
                let inside_peak = Arc::clone(&inside_peak);
                tasks.push(async move {
                    let target = nop_target();
                    let cx = ResolveCtx::root();
                    let call = Call::new(
                        MethodKey::of("svc::guarded"),
                        BeanKey::ByContract(ContractId::of("svc::Svc")),
                        ErasedArgs::none(),
                        &target,
                        &cx,
                    );
                    let running_in = Arc::clone(&running);
                    let inside_in = Arc::clone(&inside_peak);
                    let body = Mutex::new(Some((running_in, inside_in)));
                    let tail: Box<Tail> = Box::new(move |_c: &Call<'_>| {
                        let (running_in, inside_in) = body.lock().unwrap().take().unwrap();
                        let cur = running_in.fetch_add(1, Ordering::SeqCst) + 1;
                        inside_in.fetch_max(cur, Ordering::SeqCst);
                        Box::pin(async move {
                            // Yield several times so bodies interleave under the gate.
                            for _ in 0..5 {
                                yield_once().await;
                            }
                            running_in.fetch_sub(1, Ordering::SeqCst);
                            Ok(ErasedRet::pack(1_i64))
                        })
                            as BoxFuture<'_, Result<ErasedRet, AdviceError>>
                    });
                    chain.invoke(&call, &*tail).await.expect("ok");
                });
            }
            join_all(tasks).await;
        };
        block(fut);

        assert!(
            inside_peak.load(Ordering::SeqCst) <= LIMIT,
            "no more than LIMIT={LIMIT} bodies ran concurrently (peak was {})",
            inside_peak.load(Ordering::SeqCst)
        );
        assert_eq!(live.load(Ordering::SeqCst), 0, "all permits released at the end");
        let _ = Duration::from_secs(0);
    }
}
