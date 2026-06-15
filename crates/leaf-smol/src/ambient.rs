//! [`SmolAmbient`] — the [`AmbientStore`] backing for the smol runtime.
//!
//! This is the runtime half of the ONE ambient substrate (phase3/10
//! `context-propagation`, ADR-07 5a) for smol: the [`Cx`] bundle re-installed per
//! poll by the core [`Scoped`](leaf_core::Scoped) combinator reads/writes THIS
//! store.
//!
//! ## Why a thread-local is correct here (and the smol-vs-tokio difference)
//!
//! `leaf-tokio` backs the store with `tokio::task_local!` because that is the
//! natural per-task slot on tokio. smol has NO task-local primitive. But the
//! [`Scoped`](leaf_core::Scoped) model does NOT actually require one: `Scoped`
//! installs the bundle, runs ONE synchronous `inner.poll(..)`, then restores —
//! there is no `.await` between install and restore, so the whole window is one
//! synchronous region on whatever thread is currently polling. A thread-local is
//! therefore correct EVEN across a work-stealing hop, because each poll
//! re-installs on its own thread before doing any work and tears down before
//! yielding. (This is the same reason leaf-core's
//! [`ThreadLocalAmbientStore`](leaf_core::ThreadLocalAmbientStore) is a *correct*
//! fallback for the `Scoped` path, not merely a degraded one.)
//!
//! `SmolAmbient` is its OWN thread-local (distinct from core's fallback) so that
//! installing it is a genuine runtime-provided backing through the
//! [`install_ambient_store`](leaf_core::install_ambient_store) seam — proving the
//! seam admits a non-tokio backing (charter §2.6). The contract is exact:
//! [`with_installed`](AmbientStore::with_installed) shadows the slot for the
//! duration of the synchronous closure, then restores the previous binding on
//! return (RAII, cancel-safe — no `.await` between install and restore).

use std::cell::RefCell;
use std::sync::Arc;

use leaf_core::{AmbientStore, Cx};

thread_local! {
    /// The per-thread ambient [`Cx`] slot. A `RefCell` so a nested
    /// `with_installed` (e.g. `Cx::enter` inside a `Scoped` poll) can shadow and
    /// restore the binding within the same synchronous region.
    static SMOL_CX: RefCell<Option<Cx>> = const { RefCell::new(None) };
}

/// The smol-backed [`AmbientStore`]: a `std::thread_local!` holding the current
/// [`Cx`] bundle, re-installed per poll by [`Scoped`](leaf_core::Scoped).
///
/// Install it process-wide at boot via
/// [`crate::install_ambient_store`] so `Cx::current`/`Scoped` read this backing.
#[derive(Default, Clone)]
pub struct SmolAmbient {
    _priv: (),
}

impl SmolAmbient {
    /// Construct the smol ambient store.
    #[must_use]
    pub fn new() -> Self {
        SmolAmbient { _priv: () }
    }

    /// An `Arc<dyn AmbientStore>` ready for
    /// [`leaf_core::install_ambient_store`].
    #[must_use]
    pub fn shared() -> Arc<dyn AmbientStore> {
        Arc::new(SmolAmbient::new())
    }
}

impl AmbientStore for SmolAmbient {
    fn with_installed(&self, cx: &Cx, f: &mut dyn FnMut()) {
        // Shadow the slot with the new bundle, remembering the previous; a guard
        // restores it on scope exit even if `f` panics (cancel-safe sync restore,
        // the slot is pristine between polls).
        let prev = SMOL_CX.with(|slot| slot.borrow_mut().replace(cx.clone()));
        struct Restore(Option<Cx>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SMOL_CX.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(prev);
        f();
    }

    fn current(&self) -> Option<Cx> {
        SMOL_CX.with(|slot| slot.borrow().clone())
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

    #[test]
    fn with_installed_round_trips_a_bundle() {
        let store = SmolAmbient::new();
        let cx = Cx::empty().with::<LocaleKey>("de-DE".to_string());
        let mut seen = None;
        store.with_installed(&cx, &mut || {
            seen = store.current().and_then(|c| c.get::<LocaleKey>().cloned());
        });
        assert_eq!(seen.as_deref(), Some("de-DE"));
        // Restored to empty after the region.
        assert!(store.current().is_none());
    }

    #[test]
    fn nested_install_shadows_then_restores() {
        let store = SmolAmbient::new();
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

    #[test]
    fn current_is_none_without_an_install() {
        let store = SmolAmbient::new();
        assert!(store.current().is_none());
    }

    // The headline property: the bundle survives an `.await` re-installed over a
    // smol task via the core `Scoped` combinator threading THIS store.
    #[test]
    fn scoped_propagates_across_an_await_on_this_store() {
        smol::block_on(async {
            let store: Arc<dyn AmbientStore> = SmolAmbient::shared();
            let cx = Cx::empty().with::<LocaleKey>("ja-JP".to_string());

            let got = async {
                // Yield to force a re-poll.
                smol::future::yield_now().await;
                // After the await, the bundle must still be installed
                // (re-installed on every poll by Scoped).
                store.current().and_then(|c| c.get::<LocaleKey>().cloned())
            }
            .scoped_in(cx, store.clone());

            assert_eq!(got.await.as_deref(), Some("ja-JP"));
            // Outside the scoped future the ambient is clean.
            assert!(store.current().is_none());
        });
    }
}
