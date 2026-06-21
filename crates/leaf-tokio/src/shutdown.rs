//! [`TokioShutdownTrigger`] — the `tokio::signal`-based [`ShutdownTrigger`].
//!
//! Realizes the runtime half of bootstrap-diagnostics' shutdown-trigger seam
//! (phase3/14): leaf NEVER installs a global signal handler itself; this trigger
//! arms a `tokio::signal` listener (Ctrl-C / `SIGINT`, plus `SIGTERM` on Unix)
//! and invokes the supplied `fire` callback EXACTLY ONCE on the first signal, then
//! stops listening. The container's [`run_state`](leaf_core::watch_run_state)
//! teardown is what `fire` drives.
//!
//! The listener runs on a spawned task (so arming returns immediately) and fires
//! `fire` exactly once: the signal future resolves on the FIRST signal and the task
//! ends, so a single `arm_on` await fires at most once by construction.

use leaf_core::ShutdownTrigger;

/// The tokio-backed [`ShutdownTrigger`]: fires once on `SIGINT`/Ctrl-C (and
/// `SIGTERM` on Unix).
#[derive(Default, Clone)]
pub struct TokioShutdownTrigger {
    _priv: (),
}

impl TokioShutdownTrigger {
    /// Construct the trigger.
    #[must_use]
    pub fn new() -> Self {
        TokioShutdownTrigger { _priv: () }
    }
}

impl ShutdownTrigger for TokioShutdownTrigger {
    fn arm(&self, fire: Box<dyn Fn() + Send + Sync>) {
        tokio::spawn(arm_on(wait_for_signal(), fire));
    }
}

/// The firing core, generic over the signal future so it is testable without raising a
/// real signal at the test process: await the FIRST signal, then invoke `fire` once.
///
/// Once-only is structural: `wait_for_signal` resolves on the first SIGINT/SIGTERM and
/// this future completes (the spawned task ends), so a racing second signal has no live
/// `arm_on` left to fire again — no guard flag needed.
async fn arm_on<S: std::future::Future<Output = ()>>(signal: S, fire: Box<dyn Fn() + Send + Sync>) {
    signal.await;
    fire();
}

/// Await the first shutdown signal: Ctrl-C / `SIGINT` everywhere, plus `SIGTERM`
/// on Unix (the orchestrator's graceful-stop signal).
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // If a stream cannot be installed (e.g. inside a restricted sandbox),
        // fall back to Ctrl-C only rather than panicking.
        let mut term = signal(SignalKind::terminate()).ok();
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        match term.as_mut() {
            Some(term) => {
                tokio::select! {
                    _ = &mut ctrl_c => {}
                    _ = term.recv() => {}
                }
            }
            None => {
                let _ = ctrl_c.await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn arm_returns_immediately_and_does_not_fire_without_a_signal_present() {
        let trigger = TokioShutdownTrigger::new();
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();
        // Arm must not block; the callback must not fire absent a signal.
        trigger.arm(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0, "must not fire without a signal");
    }

    #[tokio::test]
    async fn fires_exactly_once_when_the_signal_arrives() {
        // Drive `arm_on` with a controllable "signal" (a oneshot) so we exercise
        // the once-only firing logic WITHOUT raising a real signal at the test
        // process (which would be hostile to the parallel test harness).
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let signal = async move {
            let _ = rx.await;
        };
        let handle = tokio::spawn(arm_on(
            signal,
            Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        // Not fired yet.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // "Signal" arrives.
        tx.send(()).unwrap();
        handle.await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1, "must fire exactly once on the signal");
    }

    #[test]
    fn shutdown_trigger_is_object_safe() {
        let _t: Box<dyn ShutdownTrigger> = Box::new(TokioShutdownTrigger::new());
    }
}
