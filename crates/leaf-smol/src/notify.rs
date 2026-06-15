//! A reactive, coalescing wake primitive for the smol scheduler — the analogue
//! of `tokio::sync::Notify`.
//!
//! smol has no built-in `Notify`. The scheduler driver needs ONE thing: park
//! until "something changed" (a new earlier fire was registered, or a disarm),
//! reactively (no spin). A [`smol::channel::bounded`] of capacity 1 gives exactly
//! that: [`notify`](Notify::notify) does a non-blocking `try_send(())` that is a
//! no-op when the slot is already full (so N notifies coalesce into at most one
//! pending wake), and [`notified`](Notify::notified) awaits a `recv` (reactive —
//! the task parks until a send arrives or one is already buffered).
//!
//! This is intentionally minimal: it serves the single-consumer scheduler driver,
//! not a general multi-waiter broadcast.

use smol::channel::{bounded, Receiver, Sender};

/// A coalescing single-slot wake channel.
#[derive(Clone)]
pub(crate) struct Notify {
    tx: Sender<()>,
    rx: Receiver<()>,
}

impl Notify {
    /// Create a fresh, un-notified handle.
    pub(crate) fn new() -> Self {
        let (tx, rx) = bounded(1);
        Notify { tx, rx }
    }

    /// Wake the [`notified`](Notify::notified) waiter (coalescing: a no-op if a
    /// wake is already pending). Never blocks, never fails meaningfully — a full
    /// slot already means "there is a pending wake".
    pub(crate) fn notify(&self) {
        // `try_send` errors on Full (already a pending wake — fine) or Closed
        // (we hold both ends, so impossible). Either way, nothing to do.
        let _ = self.tx.try_send(());
    }

    /// Park until a [`notify`](Notify::notify) arrives (or one is already
    /// buffered). Reactive — no spin.
    pub(crate) async fn notified(&self) {
        // `recv` errors only when all senders are dropped; we hold `tx`, so it
        // cannot. Treat an error as "woke" to stay total.
        let _ = self.rx.recv().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn notify_then_notified_returns_immediately() {
        smol::block_on(async {
            let n = Notify::new();
            n.notify();
            // A buffered notify is consumed without parking.
            n.notified().await;
        });
    }

    #[test]
    fn multiple_notifies_coalesce() {
        smol::block_on(async {
            let n = Notify::new();
            // Several notifies before any wait collapse to a single pending wake.
            n.notify();
            n.notify();
            n.notify();
            n.notified().await; // consumes the one buffered wake
                                // A second wait would now park; assert it does by
                                // racing against a short timer.
            let timer = smol::Timer::after(Duration::from_millis(20));
            futures::pin_mut!(timer);
            let again = n.notified();
            futures::pin_mut!(again);
            let parked = matches!(
                futures::future::select(again, timer).await,
                futures::future::Either::Right(_)
            );
            assert!(parked, "coalesced notifies must leave only ONE pending wake");
        });
    }

    #[test]
    fn notified_wakes_on_a_later_notify() {
        smol::block_on(async {
            let n = Notify::new();
            let n2 = n.clone();
            // Fire the notify shortly after we start waiting.
            let waker = smol::spawn(async move {
                smol::Timer::after(Duration::from_millis(20)).await;
                n2.notify();
            });
            n.notified().await; // must return once the spawned task notifies
            waker.await;
        });
    }
}
