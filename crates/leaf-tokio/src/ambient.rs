//! [`TokioAmbient`] — the [`AmbientStore`] backing over `tokio::task_local!`.
//!
//! This is the runtime half of the ONE ambient substrate (phase3/10
//! `context-propagation`, ADR-07 5a): the [`Cx`] bundle re-installed per poll by
//! the core [`Scoped`](leaf_core::Scoped) combinator reads/writes THIS store. On
//! a multi-threaded work-stealing runtime, a `std::thread_local` would lose the
//! bundle across a task migration; a `tokio::task_local!` is scoped to the *task*
//! and rides the migration correctly — that is the whole reason this backing
//! exists and the core [`ThreadLocalAmbientStore`](leaf_core::ThreadLocalAmbientStore)
//! is only a degraded sync-bridge fallback.
//!
//! The seam contract is exact: [`with_installed`](AmbientStore::with_installed)
//! installs `cx` for the duration of the synchronous closure `f` (one poll of the
//! inner future), then restores the previous binding on return (RAII, cancel-safe
//! because there is no `.await` between the install and the restore — the slot is
//! pristine between polls).

use std::cell::RefCell;
use std::sync::Arc;

use leaf_core::{AmbientStore, Cx};

tokio::task_local! {
    /// The per-task ambient [`Cx`] slot. A `RefCell` so a nested
    /// `with_installed` (e.g. `Cx::enter` inside a `Scoped` poll) can shadow and
    /// restore the binding WITHIN the same task scope without a second
    /// `task_local!::scope` (which `tokio` only lets us enter at task spawn / a
    /// fresh `.scope(..).await`, not synchronously mid-poll).
    static TASK_CX: RefCell<Option<Cx>>;
}

/// The tokio-backed [`AmbientStore`]: a `tokio::task_local!` holding the current
/// [`Cx`] bundle.
///
/// Install it process-wide at boot via
/// [`leaf_core::install_ambient_store`]`(Arc::new(TokioAmbient::new()))` so
/// `Cx::current`/`Scoped` read the task-local rather than the thread-local
/// fallback.
#[derive(Default, Clone)]
pub struct TokioAmbient {
    _priv: (),
}

impl TokioAmbient {
    /// Construct the tokio ambient store.
    #[must_use]
    pub fn new() -> Self {
        TokioAmbient { _priv: () }
    }

    /// An `Arc<dyn AmbientStore>` ready for [`leaf_core::install_ambient_store`].
    #[must_use]
    pub fn shared() -> Arc<dyn AmbientStore> {
        Arc::new(TokioAmbient::new())
    }
}

impl AmbientStore for TokioAmbient {
    fn with_installed(&self, cx: &Cx, f: &mut dyn FnMut()) {
        // Two cases. If the task-local slot is already established (we are inside
        // a task that entered the `scope`), swap the value in place + restore on
        // exit (cheap, no re-entry of `scope`). If it is NOT established (the
        // very first install on this task — i.e. the outermost `Scoped` poll or a
        // bare `Cx::enter` on a task that never scoped), enter the task-local
        // `scope` for exactly the duration of `f`.
        let cx = cx.clone();
        let established = TASK_CX.try_with(|_| ()).is_ok();
        if established {
            // Swap-in-place; a guard restores the previous binding on scope exit
            // even if `f` panics (cancel-safe sync restore).
            let prev = TASK_CX.with(|slot| slot.borrow_mut().replace(cx));
            struct Restore(Option<Cx>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    // The slot is still established here (we are inside the same
                    // task scope), so `with` cannot fail.
                    let _ = TASK_CX.try_with(|slot| *slot.borrow_mut() = self.0.take());
                }
            }
            let _restore = Restore(prev);
            f();
        } else {
            // Establish the task-local for this region. `sync_scope` runs the
            // closure with the slot present and tears it down on return. A
            // `&mut dyn FnMut()` is itself callable as the required `FnOnce`.
            TASK_CX.sync_scope(RefCell::new(Some(cx)), f);
        }
    }

    fn current(&self) -> Option<Cx> {
        TASK_CX.try_with(|slot| slot.borrow().clone()).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{CxFutureExt, CxKey, Propagation};

    struct LocaleKey;
    impl CxKey for LocaleKey {
        type Value = String;
        const NAME: &'static str = "locale";
        const POLICY: Propagation = Propagation::Inherit;
    }

    struct TxKey;
    impl CxKey for TxKey {
        type Value = u64;
        const NAME: &'static str = "tx.resource";
        const POLICY: Propagation = Propagation::Isolate;
    }

    #[tokio::test]
    async fn with_installed_round_trips_a_bundle() {
        let store = TokioAmbient::new();
        let cx = Cx::empty().with::<LocaleKey>("de-DE".to_string());
        let mut seen = None;
        store.with_installed(&cx, &mut || {
            seen = store.current().and_then(|c| c.get::<LocaleKey>().cloned());
        });
        assert_eq!(seen.as_deref(), Some("de-DE"));
        // Restored to empty after the region.
        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn nested_install_shadows_then_restores() {
        let store = TokioAmbient::new();
        let outer = Cx::empty().with::<TxKey>(1);
        let inner = Cx::empty().with::<TxKey>(2);
        store.with_installed(&outer, &mut || {
            assert_eq!(store.current().unwrap().get::<TxKey>().copied(), Some(1));
            store.with_installed(&inner, &mut || {
                assert_eq!(store.current().unwrap().get::<TxKey>().copied(), Some(2));
            });
            // Outer restored after the inner region.
            assert_eq!(store.current().unwrap().get::<TxKey>().copied(), Some(1));
        });
        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn current_is_none_without_an_install() {
        let store = TokioAmbient::new();
        assert!(store.current().is_none());
    }

    // The headline property: the bundle survives an `.await` re-installed over a
    // tokio task via the core `Scoped` combinator threading THIS store.
    #[tokio::test]
    async fn scoped_propagates_across_an_await_on_this_store() {
        let store: Arc<dyn AmbientStore> = TokioAmbient::shared();
        let cx = Cx::empty().with::<LocaleKey>("ja-JP".to_string());

        let got = async {
            // Yield to force a re-poll (possibly on a different worker thread).
            tokio::task::yield_now().await;
            // After the await, the bundle must still be installed (re-installed
            // on every poll by Scoped).
            store.current().and_then(|c| c.get::<LocaleKey>().cloned())
        }
        .scoped_in(cx, store.clone());

        assert_eq!(got.await.as_deref(), Some("ja-JP"));
        // Outside the scoped future the ambient is clean.
        assert!(store.current().is_none());
    }

    // The same property when the store is the process-wide installed backing
    // (so plain `Cx::current()`/`fut.scoped(..)` work without threading a store).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_propagates_across_a_worker_hop_via_installed_store() {
        // Best-effort install (other tests in the binary may have installed it).
        let _ = leaf_core::install_ambient_store(TokioAmbient::shared());
        let cx = Cx::empty().with::<LocaleKey>("fr-FR".to_string());
        let got = async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            Cx::current().and_then(|c| c.get::<LocaleKey>().cloned())
        }
        .scoped(cx);
        assert_eq!(got.await.as_deref(), Some("fr-FR"));
    }
}
