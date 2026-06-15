//! [`SmolShutdownTrigger`] — the smol-backed [`ShutdownTrigger`].
//!
//! The mirror of `leaf-tokio`'s `TokioShutdownTrigger` (phase3/14). leaf NEVER
//! installs a global signal handler itself; this trigger spawns a listener task
//! on the smol executor that invokes the supplied `fire` callback EXACTLY ONCE on
//! the first signal, then stops listening. The container's
//! [`run_state`](leaf_core::watch_run_state) teardown is what `fire` drives.
//!
//! ## Honest deferral: the OS signal SOURCE
//!
//! `leaf-tokio` gets its signal stream from `tokio::signal` (Ctrl-C / `SIGTERM`).
//! smol's core crates (`smol = leaf-smol`'s only runtime dep, plus `futures`) do
//! NOT ship a signal abstraction — that lives in the separate `async-signal`
//! crate, which the crate's dependency budget (leaf-core + smol + futures)
//! deliberately excludes for now. So:
//!
//! - The once-only firing CORE (`arm_on`) is fully implemented and tested — it
//!   is generic over the signal future, so a smol-runtime binary (or a future
//!   `async-signal` integration) supplies the real source via
//!   [`arm_with`](SmolShutdownTrigger::arm_with).
//! - The default [`ShutdownTrigger::arm`] wires a source that NEVER resolves
//!   (`fire` is therefore never auto-invoked from an OS signal). This is a NOTE-d
//!   deferral, not a silent gap: a binary that wants signal-driven shutdown on
//!   smol either calls [`arm_with`](SmolShutdownTrigger::arm_with) with its own
//!   source, or drives teardown through the container's explicit
//!   [`watch_run_state`](leaf_core::watch_run_state) path (which does not depend
//!   on this trigger).
//!
//! A guard `AtomicBool` makes `fire` once-only even if two signals race.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use leaf_core::ShutdownTrigger;

/// The smol-backed [`ShutdownTrigger`].
///
/// See the module note: the default [`arm`](ShutdownTrigger::arm) wires a
/// never-resolving source (the OS signal source is a NOTE-d deferral pending the
/// `async-signal` dep); [`arm_with`](SmolShutdownTrigger::arm_with) accepts an
/// explicit signal future.
#[derive(Default, Clone)]
pub struct SmolShutdownTrigger {
    _priv: (),
}

impl SmolShutdownTrigger {
    /// Construct the trigger.
    #[must_use]
    pub fn new() -> Self {
        SmolShutdownTrigger { _priv: () }
    }

    /// Arm with an EXPLICIT signal source: spawn a listener that awaits `signal`,
    /// then invokes `fire` exactly once. The escape hatch a smol-runtime binary
    /// uses to supply its own (e.g. `async-signal`) source.
    pub fn arm_with<S>(&self, signal: S, fire: Box<dyn Fn() + Send + Sync>)
    where
        S: std::future::Future<Output = ()> + Send + 'static,
    {
        smol::spawn(arm_on(signal, fire)).detach();
    }
}

impl ShutdownTrigger for SmolShutdownTrigger {
    fn arm(&self, fire: Box<dyn Fn() + Send + Sync>) {
        // NOTE: no OS signal source within the leaf-core + smol + futures budget.
        // Wire a never-resolving source so the listener exists but only fires via
        // an explicit `arm_with` source. See the module note.
        self.arm_with(futures::future::pending::<()>(), fire);
    }
}

/// The once-only firing core, generic over the signal future so it is testable
/// without raising a real signal: await `signal`, then invoke `fire` at most once.
async fn arm_on<S: std::future::Future<Output = ()>>(
    signal: S,
    fire: Box<dyn Fn() + Send + Sync>,
) {
    let fired = Arc::new(AtomicBool::new(false));
    signal.await;
    // Once-only: a second racing signal must not double-fire teardown.
    if !fired.swap(true, Ordering::SeqCst) {
        fire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Duration;

    #[test]
    fn arm_returns_immediately_and_does_not_fire_without_a_signal() {
        smol::block_on(async {
            let trigger = SmolShutdownTrigger::new();
            let count = Arc::new(AtomicU32::new(0));
            let c = count.clone();
            // Arm must not block; the default source never resolves, so the
            // callback must not fire.
            trigger.arm(Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));
            smol::Timer::after(Duration::from_millis(30)).await;
            assert_eq!(
                count.load(Ordering::SeqCst),
                0,
                "must not fire without a signal"
            );
        });
    }

    #[test]
    fn fires_exactly_once_when_the_signal_arrives() {
        smol::block_on(async {
            // Drive `arm_on` with a controllable "signal" (a oneshot channel) so
            // we exercise the once-only firing logic without raising a real
            // signal.
            let count = Arc::new(AtomicU32::new(0));
            let c = count.clone();
            let (tx, rx) = smol::channel::bounded::<()>(1);
            let signal = async move {
                let _ = rx.recv().await;
            };
            let handle = smol::spawn(arm_on(
                signal,
                Box::new(move || {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            ));
            // Not fired yet.
            smol::Timer::after(Duration::from_millis(10)).await;
            assert_eq!(count.load(Ordering::SeqCst), 0);
            // "Signal" arrives.
            tx.send(()).await.unwrap();
            handle.await;
            assert_eq!(
                count.load(Ordering::SeqCst),
                1,
                "must fire exactly once on the signal"
            );
        });
    }

    #[test]
    fn arm_with_explicit_source_fires() {
        smol::block_on(async {
            let trigger = SmolShutdownTrigger::new();
            let count = Arc::new(AtomicU32::new(0));
            let c = count.clone();
            let (tx, rx) = smol::channel::bounded::<()>(1);
            trigger.arm_with(
                async move {
                    let _ = rx.recv().await;
                },
                Box::new(move || {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
            smol::Timer::after(Duration::from_millis(10)).await;
            assert_eq!(count.load(Ordering::SeqCst), 0);
            tx.send(()).await.unwrap();
            // Give the detached listener a moment to fire.
            for _ in 0..100 {
                if count.load(Ordering::SeqCst) == 1 {
                    break;
                }
                smol::Timer::after(Duration::from_millis(2)).await;
            }
            assert_eq!(count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn shutdown_trigger_is_object_safe() {
        let _t: Box<dyn ShutdownTrigger> = Box::new(SmolShutdownTrigger::new());
    }
}
