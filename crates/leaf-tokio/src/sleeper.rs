//! [`TokioSleeper`] — the timer-backed [`Sleeper`](leaf_resilience::Sleeper) over
//! [`tokio::time::sleep`], the runtime half of leaf-resilience's reactive backoff
//! seam (retry/resilience, phase3/09).
//!
//! leaf-resilience declares the runtime-agnostic [`Sleeper`](leaf_resilience::Sleeper)
//! one-method seam (a future that completes after a delay, parked on a reactive
//! timer — NEVER a busy-poll); leaf-core names no runtime sleep. THIS crate is the
//! runtime: [`TokioSleeper`] awaits `tokio::time::sleep(dur)`, parking on the tokio
//! timer-wheel (the same wheel [`TokioSchedulerCore`](crate::TokioSchedulerCore)
//! drives — the design's "ONE shared reactive timer-wheel … the cold-path timer
//! retry/backoff reuse").
//!
//! ## The install seam (no ABI change)
//!
//! Unlike the tx advisor (whose concrete manager `M` is named at the macro site and
//! resolved by `TypeId` from the container), the auto-wired RETRY advisor's emitted
//! interceptor has NO concrete sleeper type to resolve — leaf-codegen must not know
//! a runtime crate, and a container resolve yields only an exact-`TypeId` downcast
//! (no trait-object upcast at the seam). So leaf-resilience exposes a process-wide
//! default-sleeper slot
//! ([`install_default_sleeper`](leaf_resilience::install_default_sleeper) /
//! [`default_sleeper`](leaf_resilience::default_sleeper)), the
//! [`install_ambient_store`](crate::install_ambient_store) analogue. This crate's
//! [`install_tokio_sleeper`] fills it ONCE at boot so `#[retryable(backoff =
//! fixed(n))]` performs a REAL timed backoff; the emitted interceptor consults the
//! default and degrades to the runtime-free
//! [`ImmediateSleeper`](leaf_resilience::ImmediateSleeper) (zero delay, never a
//! panic) when no runtime is installed.

use std::sync::Arc;
use std::time::Duration;

use leaf_core::BoxFuture;
use leaf_resilience::Sleeper;

/// The timer-backed [`Sleeper`](leaf_resilience::Sleeper): a `sleep(delay)` future
/// that parks on the tokio timer-wheel (`tokio::time::sleep`) — NEVER a busy-poll.
///
/// Zero-sized: it carries no state (the timer is the ambient tokio runtime). A
/// production binding installs it as the process default
/// ([`install_tokio_sleeper`]) so the auto-wired retry advisor's backoff is real.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioSleeper;

impl TokioSleeper {
    /// A new timer-backed sleeper (stateless).
    #[must_use]
    pub fn new() -> Self {
        TokioSleeper
    }
}

impl Sleeper for TokioSleeper {
    fn sleep(&self, delay: Duration) -> BoxFuture<'static, ()> {
        // Park on the tokio timer-wheel (cold-path; the future is Pending until the
        // timer fires — NOT a spin). A zero delay completes promptly.
        Box::pin(tokio::time::sleep(delay))
    }
}

/// Install [`TokioSleeper`] as the process-wide default reactive
/// [`Sleeper`](leaf_resilience::Sleeper) the auto-wired retry advisor consults (the
/// runtime install at boot, before refresh — the
/// [`install_ambient_store`](crate::install_ambient_store) analogue for the backoff
/// timer).
///
/// At-most-once (delegates to
/// [`install_default_sleeper`](leaf_resilience::install_default_sleeper)): returns
/// `true` if THIS call installed it, `false` if a default was already installed
/// (one timer per process; a second call is a safe no-op, NOT a panic — re-running
/// the same boot path in a test harness is fine). After this,
/// `#[retryable(backoff = fixed(n))]` performs a REAL timed backoff.
pub fn install_tokio_sleeper() -> bool {
    leaf_resilience::install_default_sleeper(Arc::new(TokioSleeper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn tokio_sleeper_awaits_a_real_elapsed_delay() {
        // The timer-backed sleeper genuinely parks for the delay (a REAL wait, not
        // the zero-delay ImmediateSleeper). 30ms is enough to be unambiguous yet
        // fast.
        let s: Arc<dyn Sleeper> = Arc::new(TokioSleeper::new());
        let start = Instant::now();
        s.sleep(Duration::from_millis(30)).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(25),
            "the timer-backed sleeper parked for ~30ms (elapsed {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn install_tokio_sleeper_makes_it_the_process_default() {
        // After install, leaf-resilience's process-default sleeper IS a timer-backed
        // sleeper: awaiting `default_sleeper()` for a real delay actually elapses.
        // (This test owns the global install in this test binary.)
        let installed = install_tokio_sleeper();
        assert!(installed, "the first install in this process succeeds");

        let start = Instant::now();
        leaf_resilience::default_sleeper().sleep(Duration::from_millis(30)).await;
        assert!(
            start.elapsed() >= Duration::from_millis(25),
            "the installed process default is the timer-backed sleeper (a real delay elapsed)"
        );

        // A second install is rejected (one default per process).
        assert!(!install_tokio_sleeper(), "a second install is a no-op (at-most-once)");
    }
}
