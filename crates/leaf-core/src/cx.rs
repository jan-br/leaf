//! The ONE ambient-context substrate: the cheap-clone [`Cx`] bundle, typed
//! [`CxKey`] declarations with per-key [`Propagation`], the [`AmbientStore`]
//! storage seam, the per-poll-re-installing [`Scoped`] combinator, and the typed
//! [`Holder`] accessor.
//!
//! Realizes phase3/10 `context-propagation` + `locale-context` and ADR-07 5a:
//!
//! - **ONE bundle.** [`Cx`] is `Arc<CxNode>` — `Send + Sync + 'static`,
//!   structurally shared. [`Cx::with`] returns a derived child (one-directional
//!   shadow over the parent), so a hop is ONE `Arc` clone + a poll-wrapper, not
//!   R1's per-hop box-dance. This is "the current X" for locale, request scope,
//!   the tx binding, and any tracing bridge — all read this one bundle.
//! - **Typed keys, by data.** A [`CxKey`] is `{type Value, NAME, POLICY}` —
//!   data, not a registered accessor object, so there is no DCE-prone
//!   empty-registry bug. [`Propagation::Inherit`] keys (locale/trace) are
//!   auto-captured across a spawn hop by the facility's `CxDecorator`;
//!   [`Propagation::Isolate`] keys (tx/connection) are NEVER auto-captured — the
//!   never-inheritable rule realized as a TYPED default.
//! - **Per-poll re-install.** [`Scoped`] re-installs the bundle on EVERY poll
//!   (`store.with_installed(&cx, || inner.poll())`), the only model correct
//!   across work-stealing migration. Sync restore on poll-exit rides RAII
//!   `Drop` (cancel-safe; the slot is pristine between polls).
//! - **One storage seam.** [`AmbientStore`] is the sole task-local seam; the
//!   concrete backing is `leaf-tokio` (`tokio::task_local!`) / `leaf-smol` and
//!   is NEVER named here. leaf-core ships a built-in [`ThreadLocalAmbientStore`]
//!   default — the sanctioned bounded sync-bridge fallback — so `Cx::current`/
//!   `enter`/`Scoped` work without a runtime; a runtime [`install_ambient_store`]
//!   replaces it at boot.

use std::any::{Any, TypeId};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

// ──────────────────────────── Propagation ───────────────────────────────────

/// Per-[`CxKey`] propagation policy across a spawn/scheduled hop (ADR-07 5a).
///
/// The typed dual-visibility that replaces Spring's inheritable/non-inheritable
/// `ThreadLocal` dual-field. The default discipline is ISOLATE (data-bleed
/// defense); [`Inherit`](Propagation::Inherit) is the per-key opt-in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Propagation {
    /// Auto-captured into spawned children by the facility's `CxDecorator`
    /// (locale, request attrs, trace).
    Inherit,
    /// NEVER auto-captured across a hop (tx/connection resource) — the
    /// never-inheritable rule as the typed DEFAULT.
    #[default]
    Isolate,
}

// ───────────────────────────────── CxKey ────────────────────────────────────

/// A typed context-bundle key: data, not a registered accessor object.
///
/// `{type Value, NAME, POLICY}` is the whole declaration. The [`NAME`](CxKey::NAME)
/// const repurposes Spring's `NamedThreadLocal` name into the
/// "no current X bound — were you inside `HOLDER.scope(...)`?" diagnostic and the
/// startup-inspectable bundle schema. Keys live in the concept-owning crate
/// (locale in i18n, request attrs in a web crate, tx in the tx crate) — NEVER
/// hardcoded in core; sugared by the thin `#[holder]` macro.
///
/// `Value: Send + Sync + 'static` rides the one [`Bean`](crate::Bean)/
/// [`ErasedBean`](crate::ErasedBean) safe-publication bound: an ambient value
/// that crosses a hop MUST be `Send + Sync` (surfaced by the doctrine
/// diagnostic upstream).
pub trait CxKey: 'static {
    /// The value type this key carries in the bundle.
    type Value: Send + Sync + 'static;
    /// The stable name (diagnostics + bundle schema); declare-once, collision
    /// is a loud `AssemblyError` at freeze (keyed by `NAME`).
    const NAME: &'static str;
    /// Whether this key is auto-captured across a hop ([`Propagation`]).
    const POLICY: Propagation;
}

// ───────────────────────────────── Cx ───────────────────────────────────────

/// One node of the structurally-shared context bundle.
///
/// A persistent shadow list: each [`Cx::with`] cons-es a new node pointing at
/// the parent, so deriving a child is `O(1)` (one allocation + an `Arc` clone of
/// the tail) and the parent is untouched. Lookup walks the chain newest-first,
/// so a child's binding for a key shadows the parent's.
struct CxNode {
    /// The `TypeId` of the `CxKey` impl this node binds.
    key: TypeId,
    /// The bound value, type-erased (`K::Value` is `Send + Sync + 'static`).
    value: Box<dyn Any + Send + Sync>,
    /// The parent bundle, if any (the shadowed bindings).
    parent: Option<Cx>,
}

/// THE one cheap-clone ambient bundle (ADR-07 5a) — `Send + Sync + 'static`.
///
/// Cloning is one `Arc` bump. An empty bundle ([`Cx::empty`]/[`Cx::default`])
/// holds no node; [`with`](Cx::with) derives children by structural sharing.
#[derive(Clone, Default)]
pub struct Cx(Option<Arc<CxNode>>);

impl Cx {
    /// The empty bundle (no bindings).
    #[must_use]
    pub fn empty() -> Cx {
        Cx(None)
    }

    /// `true` iff this bundle binds no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// Look up `K`'s value in this bundle (newest binding wins), if bound.
    ///
    /// Walks the structural-sharing chain newest-first, so a child's
    /// [`with`](Cx::with) shadows the parent. Pure task-local read — no global
    /// lock, no `Arc` churn for the lookup itself.
    #[must_use]
    pub fn get<K: CxKey>(&self) -> Option<&K::Value> {
        let want = TypeId::of::<K>();
        let mut cur = self.0.as_deref();
        while let Some(node) = cur {
            if node.key == want {
                // The node for `K` always holds a `K::Value` (only `with::<K>`
                // creates it), so the downcast cannot fail.
                return node.value.downcast_ref::<K::Value>();
            }
            cur = node.parent.as_ref().and_then(|p| p.0.as_deref());
        }
        None
    }

    /// `true` iff `K` is bound somewhere in this bundle.
    #[must_use]
    pub fn contains<K: CxKey>(&self) -> bool {
        self.get::<K>().is_some()
    }

    /// Derive a child bundle binding `K` to `v` (a one-directional shadow).
    ///
    /// `O(1)`: cons a new node over `self`. The parent is untouched, so the
    /// child can outlive a `scoped` hop without mutating shared state.
    #[must_use]
    pub fn with<K: CxKey>(&self, v: K::Value) -> Cx {
        Cx(Some(Arc::new(CxNode {
            key: TypeId::of::<K>(),
            value: Box::new(v),
            parent: if self.is_empty() { None } else { Some(self.clone()) },
        })))
    }

    /// The currently-installed ambient bundle, if any.
    ///
    /// Reads the installed [`AmbientStore`] (the runtime task-local, or the
    /// built-in thread-local fallback). `None` when nothing is in scope.
    #[must_use]
    pub fn current() -> Option<Cx> {
        ambient_store().current()
    }

    /// The current bundle, or the empty bundle if nothing is installed.
    #[must_use]
    pub fn current_or_empty() -> Cx {
        Cx::current().unwrap_or_default()
    }

    /// Install this bundle as ambient for ONE synchronous region and run `f`.
    ///
    /// The sync escape hatch (Spring's `LocaleContextHolder.setLocale` + restore
    /// in a `finally`). The previous binding is restored on return via the
    /// store's RAII; cancel-safe because there is no `.await` inside the region.
    pub fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        // `with_installed` takes `&mut dyn FnMut` (which it calls exactly once);
        // funnel the `FnOnce` and its return through single-shot `Option`s.
        let mut once = Some(f);
        let mut ret = None;
        ambient_store().with_installed(self, &mut || {
            if let Some(g) = once.take() {
                ret = Some(g());
            }
        });
        ret.expect("enter closure ran exactly once")
    }
}

impl std::fmt::Debug for Cx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Names of bound keys are erased here (the values are `dyn Any`); we
        // report the binding depth, which is the inspectable structural fact.
        let mut depth = 0usize;
        let mut cur = self.0.as_deref();
        while let Some(node) = cur {
            depth += 1;
            cur = node.parent.as_ref().and_then(|p| p.0.as_deref());
        }
        f.debug_struct("Cx").field("bindings", &depth).finish()
    }
}

// ─────────────────────────── AmbientStore seam ──────────────────────────────

/// THE one ambient-storage seam (ADR-07 5a) — the runtime task-local.
///
/// Object-safe. [`with_installed`](AmbientStore::with_installed) installs `cx`
/// as the current bundle, runs `f` (which observes [`current`](AmbientStore::current)
/// returning `cx`), then restores the previous binding via RAII — the sync
/// restore [`Scoped`] relies on for cancel-safety. The concrete backing is
/// `leaf-tokio` (`tokio::task_local!`) / `leaf-smol`, NEVER named here; leaf-core
/// ships [`ThreadLocalAmbientStore`] as the default sync-bridge fallback.
pub trait AmbientStore: Send + Sync {
    /// Install `cx`, run `f` while it is current, restore on return (RAII).
    fn with_installed(&self, cx: &Cx, f: &mut dyn FnMut());

    /// The currently-installed bundle on THIS task/thread, if any.
    fn current(&self) -> Option<Cx>;
}

/// The built-in default [`AmbientStore`]: a `std::thread_local!` slot.
///
/// This is the sanctioned bounded sync-bridge fallback ADR-07 5g names — it lets
/// `Cx::current`/`enter`/[`Scoped`] work without a runtime (and in core's own
/// tests). A runtime installs its task-local backing via
/// [`install_ambient_store`]; until then this is degraded-not-fatal (context
/// moves correctly within a thread, but a work-stealing hop on a multi-threaded
/// runtime would lose it — the loud-WARN case the self-check surfaces).
#[derive(Default)]
pub struct ThreadLocalAmbientStore {
    _priv: (),
}

thread_local! {
    static TLS_CX: std::cell::RefCell<Option<Cx>> = const { std::cell::RefCell::new(None) };
}

impl ThreadLocalAmbientStore {
    /// Construct the thread-local store.
    #[must_use]
    pub fn new() -> Self {
        ThreadLocalAmbientStore { _priv: () }
    }
}

impl AmbientStore for ThreadLocalAmbientStore {
    fn with_installed(&self, cx: &Cx, f: &mut dyn FnMut()) {
        // Swap in the new bundle, remembering the previous; a guard restores it
        // on scope exit even if `f` panics (cancel-safe sync restore).
        let prev = TLS_CX.with(|slot| slot.borrow_mut().replace(cx.clone()));
        struct Restore(Option<Cx>);
        impl Drop for Restore {
            fn drop(&mut self) {
                TLS_CX.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(prev);
        f();
    }

    fn current(&self) -> Option<Cx> {
        TLS_CX.with(|slot| slot.borrow().clone())
    }
}

/// The process-wide ambient-store backend, swappable once at boot.
///
/// `arc_swap`-free: an `Arc<dyn AmbientStore>` behind a `OnceCell` for the
/// runtime install, defaulting to the thread-local fallback. A runtime calls
/// [`install_ambient_store`] BEFORE refresh; subsequent installs are rejected
/// (one backing per process — the degraded-WARN-if-absent invariant).
static AMBIENT: once_cell::sync::OnceCell<Arc<dyn AmbientStore>> = once_cell::sync::OnceCell::new();
static DEFAULT_AMBIENT: once_cell::sync::Lazy<Arc<dyn AmbientStore>> =
    once_cell::sync::Lazy::new(|| Arc::new(ThreadLocalAmbientStore::new()));

/// Install the runtime's [`AmbientStore`] backing (leaf-tokio/leaf-smol, at boot).
///
/// # Errors
/// Returns the supplied store back as `Err` if a backing was already installed
/// (one backing per process; a second install is a programming error, surfaced
/// by the caller as the appropriate `AssemblyError`).
pub fn install_ambient_store(
    store: Arc<dyn AmbientStore>,
) -> Result<(), Arc<dyn AmbientStore>> {
    AMBIENT.set(store)
}

/// The active ambient store: the runtime-installed one, else the thread-local
/// fallback.
#[must_use]
pub fn ambient_store() -> Arc<dyn AmbientStore> {
    AMBIENT.get().cloned().unwrap_or_else(|| Arc::clone(&DEFAULT_AMBIENT))
}

// ─────────────────────────── Scoped combinator ──────────────────────────────

pin_project_lite::pin_project! {
    /// The per-poll-re-installing future combinator (ADR-07 5a keystone).
    ///
    /// Wraps an inner future and, on EVERY poll, installs `cx` as ambient via the
    /// [`AmbientStore`] for exactly the duration of the inner poll — the only
    /// model correct across work-stealing migration (a thread-stealing executor
    /// may run successive polls on different threads). The sync restore on
    /// poll-exit rides the store's RAII (`with_installed`), so cancellation
    /// cannot leave the slot dirty: the bundle is pristine between polls.
    ///
    /// Propagation across a hop is `child.scoped(Cx::current())` = ONE `Arc`
    /// clone + this wrapper.
    #[must_use = "futures do nothing unless awaited"]
    pub struct Scoped<F> {
        cx: Cx,
        store: Arc<dyn AmbientStore>,
        #[pin]
        inner: F,
    }
}

impl<F> Scoped<F> {
    /// Wrap `inner`, re-installing `cx` (over the active ambient store) per poll.
    pub fn new(inner: F, cx: Cx) -> Self {
        Scoped {
            cx,
            store: ambient_store(),
            inner,
        }
    }

    /// Wrap `inner`, re-installing `cx` over an EXPLICIT store (tests / a
    /// runtime threading its own backing without the global install).
    pub fn new_in(inner: F, cx: Cx, store: Arc<dyn AmbientStore>) -> Self {
        Scoped { cx, store, inner }
    }
}

impl<F: std::future::Future> std::future::Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // Safe pin projection via pin-project-lite: `inner` is the one pinned
        // field; `cx`/`store` are plain refs read each poll. No local `unsafe`.
        let this = self.project();
        let bundle: &Cx = this.cx;
        let store: &Arc<dyn AmbientStore> = this.store;
        let inner: Pin<&mut F> = this.inner;

        // `with_installed` takes `&mut dyn FnMut()` and calls it EXACTLY once.
        // A `Pin<&mut F>` is not `Copy` and an `FnMut` may (in general) run many
        // times, so we funnel the pinned ref through a single-shot `Option` taken
        // on that one call, and carry the result out the same way.
        let mut inner_slot = Some(inner);
        let mut result = Poll::Pending;
        // Install the bundle for exactly this poll; restore on poll-exit (the
        // RAII inside `with_installed`), cancel-safe.
        store.with_installed(bundle, &mut || {
            let inner = inner_slot
                .take()
                .expect("Scoped inner polled once per with_installed");
            result = inner.poll(cx);
        });
        result
    }
}

/// Extension trait adding [`scoped`](CxFutureExt::scoped) to any future.
pub trait CxFutureExt: std::future::Future + Sized {
    /// Re-install `cx` as ambient on every poll of `self` (the propagation hop).
    fn scoped(self, cx: Cx) -> Scoped<Self> {
        Scoped::new(self, cx)
    }

    /// Re-install `cx` over an EXPLICIT store on every poll of `self`.
    fn scoped_in(self, cx: Cx, store: Arc<dyn AmbientStore>) -> Scoped<Self> {
        Scoped::new_in(self, cx, store)
    }
}

impl<F: std::future::Future + Sized> CxFutureExt for F {}

// ─────────────────────────── CxBridge / CxDecorator ─────────────────────────

/// A guard returned by [`CxBridge::project`]; its `Drop` pops the projected
/// scope (sync, cancel-safe). The demoted R1 accessor-registry seam.
#[must_use = "the bridge scope is popped when this guard drops"]
pub struct BridgeGuard {
    /// A boxed teardown run on drop (pop the tracing scope / blocking island).
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl BridgeGuard {
    /// A guard that runs `on_drop` (the scope pop) when dropped.
    pub fn new(on_drop: impl FnOnce() + Send + 'static) -> Self {
        BridgeGuard {
            on_drop: Some(Box::new(on_drop)),
        }
    }

    /// A no-op guard (the bridge projected nothing).
    pub fn noop() -> Self {
        BridgeGuard { on_drop: None }
    }
}

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

/// The demoted R1 seam-adapter: projects the bundle into a foreign scope-stack
/// (tracing scopes, a `spawn_blocking` island) and owns its push/pop inside the
/// bridge (sync `Drop` = pop). An explicit bounded non-uniformity outside the
/// value bundle.
pub trait CxBridge: Send + Sync {
    /// Project `cx` into the foreign scope; the returned [`BridgeGuard`] pops it.
    fn project(&self, cx: &Cx) -> BridgeGuard;
}

/// The opt-in submit-time capture seam: the `ExecutionFacility`'s decorator
/// snapshots the bundle to inherit (the `task.execution.propagate-context`
/// analogue). ISOLATE-by-default means an undecorated spawn captures nothing.
pub trait CxDecorator: Send + Sync {
    /// Capture the bundle to propagate at submit time (Inherit keys only, in a
    /// real decorator; the default trait does not filter).
    fn capture(&self) -> Cx;
}

// ───────────────────────────────── Holder ───────────────────────────────────

/// The typed accessor handle over a [`CxKey`] — what `#[holder]` emits as a
/// `static`.
///
/// Pure sugar: a zero-sized typed front-end for `Cx::with`/`Cx::get`/`scoped`.
/// `HOLDER.scope(v, fut)` derives a child bundle from `Cx::current()` and scopes
/// the future to it; `HOLDER.get()` reads the ambient value; `HOLDER.with(f)`
/// borrows it.
pub struct Holder<K: CxKey> {
    _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: CxKey> Holder<K> {
    /// Construct the holder (a `const` so it can seed a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Holder {
            _marker: std::marker::PhantomData,
        }
    }

    /// This key's stable name (the diagnostic / bundle-schema name).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        K::NAME
    }

    /// This key's propagation policy.
    #[must_use]
    pub const fn policy(&self) -> Propagation {
        K::POLICY
    }

    /// Scope `fut` with `v` bound for `K`, derived from the current bundle.
    ///
    /// `HOLDER.scope(v, fut)` ≡ `fut.scoped(Cx::current_or_empty().with::<K>(v))`
    /// — set-on-open / restore-on-poll-exit (RAII via [`Scoped`]).
    pub fn scope<F: std::future::Future>(&self, v: K::Value, fut: F) -> Scoped<F> {
        fut.scoped(Cx::current_or_empty().with::<K>(v))
    }

    /// Read the ambient value for `K`, cloned out, if bound.
    ///
    /// `None` is the "no current X bound — were you inside `HOLDER.scope(...)`?"
    /// diagnostic case (the caller renders [`name`](Holder::name)).
    #[must_use]
    pub fn get(&self) -> Option<K::Value>
    where
        K::Value: Clone,
    {
        Cx::current().and_then(|c| c.get::<K>().cloned())
    }

    /// Borrow the ambient value for `K` and map it with `f`, if bound.
    ///
    /// Avoids cloning for non-`Copy`/non-`Clone` values; the borrow is valid for
    /// the duration of `f` (it holds the current `Cx` `Arc` alive).
    pub fn with<R>(&self, f: impl FnOnce(&K::Value) -> R) -> Option<R> {
        let cx = Cx::current()?;
        cx.get::<K>().map(f)
    }
}

impl<K: CxKey> Default for Holder<K> {
    fn default() -> Self {
        Holder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── test keys ──
    struct LocaleKey;
    impl CxKey for LocaleKey {
        type Value = String;
        const NAME: &'static str = "locale";
        const POLICY: Propagation = Propagation::Inherit;
    }

    struct TxKey;
    impl CxKey for TxKey {
        type Value = u64; // a fake tx-binding id
        const NAME: &'static str = "tx.resource";
        const POLICY: Propagation = Propagation::Isolate;
    }

    struct CounterKey;
    impl CxKey for CounterKey {
        type Value = i32;
        const NAME: &'static str = "counter";
        const POLICY: Propagation = Propagation::Inherit;
    }

    // ── Cx get/set/with ──

    #[test]
    fn empty_bundle_binds_nothing() {
        let cx = Cx::empty();
        assert!(cx.is_empty());
        assert!(cx.get::<LocaleKey>().is_none());
        assert!(!cx.contains::<LocaleKey>());
    }

    #[test]
    fn with_binds_and_get_reads() {
        let cx = Cx::empty().with::<LocaleKey>("de-DE".to_string());
        assert!(!cx.is_empty());
        assert_eq!(cx.get::<LocaleKey>().map(String::as_str), Some("de-DE"));
        assert!(cx.contains::<LocaleKey>());
    }

    #[test]
    fn with_is_structural_sharing_parent_untouched() {
        let parent = Cx::empty().with::<LocaleKey>("en".to_string());
        let child = parent.with::<TxKey>(42);
        // Child sees both bindings; parent sees only its own.
        assert_eq!(child.get::<LocaleKey>().map(String::as_str), Some("en"));
        assert_eq!(child.get::<TxKey>().copied(), Some(42));
        assert!(parent.get::<TxKey>().is_none());
        assert_eq!(parent.get::<LocaleKey>().map(String::as_str), Some("en"));
    }

    #[test]
    fn child_binding_shadows_parent() {
        let parent = Cx::empty().with::<CounterKey>(1);
        let child = parent.with::<CounterKey>(2);
        assert_eq!(child.get::<CounterKey>().copied(), Some(2));
        assert_eq!(parent.get::<CounterKey>().copied(), Some(1));
    }

    #[test]
    fn distinct_keys_with_same_value_type_do_not_collide() {
        // CounterKey and (a second i32 key) share Value=i32 but are distinct
        // CxKey types, so they must not alias in the bundle.
        struct OtherIntKey;
        impl CxKey for OtherIntKey {
            type Value = i32;
            const NAME: &'static str = "other-int";
            const POLICY: Propagation = Propagation::Isolate;
        }
        let cx = Cx::empty().with::<CounterKey>(10).with::<OtherIntKey>(20);
        assert_eq!(cx.get::<CounterKey>().copied(), Some(10));
        assert_eq!(cx.get::<OtherIntKey>().copied(), Some(20));
    }

    #[test]
    fn cx_clone_is_cheap_and_shares() {
        let cx = Cx::empty().with::<CounterKey>(7);
        let cloned = cx.clone();
        assert_eq!(cloned.get::<CounterKey>().copied(), Some(7));
    }

    #[test]
    fn propagation_policy_is_per_key_typed() {
        assert_eq!(LocaleKey::POLICY, Propagation::Inherit);
        assert_eq!(TxKey::POLICY, Propagation::Isolate);
        // Default Propagation is Isolate (data-bleed defense).
        assert_eq!(Propagation::default(), Propagation::Isolate);
    }

    // ── enter (sync install) ──

    #[test]
    fn enter_installs_for_a_sync_region_and_restores() {
        assert!(Cx::current().is_none());
        let cx = Cx::empty().with::<LocaleKey>("fr".to_string());
        let seen = cx.enter(|| Cx::current().and_then(|c| c.get::<LocaleKey>().cloned()));
        assert_eq!(seen.as_deref(), Some("fr"));
        // Restored after the region.
        assert!(Cx::current().is_none());
    }

    #[test]
    fn enter_nests_and_restores_outer() {
        let outer = Cx::empty().with::<CounterKey>(1);
        let inner = Cx::empty().with::<CounterKey>(2);
        outer.enter(|| {
            assert_eq!(Cx::current().unwrap().get::<CounterKey>().copied(), Some(1));
            inner.enter(|| {
                assert_eq!(Cx::current().unwrap().get::<CounterKey>().copied(), Some(2));
            });
            // Outer restored after the inner region.
            assert_eq!(Cx::current().unwrap().get::<CounterKey>().copied(), Some(1));
        });
        assert!(Cx::current().is_none());
    }

    // ── Scoped: per-poll re-install, hand-driven ──

    #[test]
    fn scoped_installs_bundle_for_each_poll_handdriven() {
        // A future that, on each poll, records what `Cx::current()` sees, then
        // returns Pending the first time and Ready the second — proving the
        // bundle is installed on EVERY poll, not just the first.
        let observations = Arc::new(std::sync::Mutex::new(Vec::<Option<i32>>::new()));
        let obs = observations.clone();
        let polls = AtomicUsize::new(0);

        let inner = futures::future::poll_fn(move |_cx| {
            let seen = Cx::current().and_then(|c| c.get::<CounterKey>().copied());
            obs.lock().unwrap().push(seen);
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });

        let cx = Cx::empty().with::<CounterKey>(99);
        let mut scoped = Box::pin(inner.scoped(cx));

        let waker = futures::task::noop_waker();
        let mut ctx = Context::from_waker(&waker);
        assert!(matches!(scoped.as_mut().poll(&mut ctx), Poll::Pending));
        assert!(matches!(scoped.as_mut().poll(&mut ctx), Poll::Ready(())));

        // Both polls saw the installed bundle.
        let recorded = observations.lock().unwrap().clone();
        assert_eq!(recorded, vec![Some(99), Some(99)]);
    }

    #[test]
    fn scoped_restores_ambient_between_polls() {
        // Outside the scoped poll, the ambient must be clean (restored on
        // poll-exit). We assert between the two polls of a Pending-then-Ready.
        let polls = AtomicUsize::new(0);
        let inner = futures::future::poll_fn(move |_cx| {
            // Inside the poll the bundle is present.
            assert!(Cx::current().is_some());
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        let cx = Cx::empty().with::<CounterKey>(5);
        let mut scoped = Box::pin(inner.scoped(cx));
        let waker = futures::task::noop_waker();
        let mut ctx = Context::from_waker(&waker);

        assert!(Cx::current().is_none());
        assert!(matches!(scoped.as_mut().poll(&mut ctx), Poll::Pending));
        // Between polls the ambient is restored to the (empty) outer scope.
        assert!(Cx::current().is_none(), "ambient must be restored on poll-exit");
        assert!(matches!(scoped.as_mut().poll(&mut ctx), Poll::Ready(())));
        assert!(Cx::current().is_none());
    }

    #[test]
    fn scoped_runs_to_completion_via_executor() {
        let cx = Cx::empty().with::<LocaleKey>("ja".to_string());
        let got = block_on(
            async { Cx::current().and_then(|c| c.get::<LocaleKey>().cloned()) }.scoped(cx),
        );
        assert_eq!(got.as_deref(), Some("ja"));
    }

    // ── Holder typed accessor ──

    #[test]
    fn holder_name_and_policy() {
        const LOCALE: Holder<LocaleKey> = Holder::new();
        assert_eq!(LOCALE.name(), "locale");
        assert_eq!(LOCALE.policy(), Propagation::Inherit);
    }

    #[test]
    fn holder_scope_then_get() {
        const LOCALE: Holder<LocaleKey> = Holder::new();
        // No ambient => get is None.
        assert!(LOCALE.get().is_none());
        let got = block_on(LOCALE.scope("it".to_string(), async { LOCALE.get() }));
        assert_eq!(got.as_deref(), Some("it"));
    }

    #[test]
    fn holder_with_borrows_without_clone() {
        const COUNTER: Holder<CounterKey> = Holder::new();
        let len = block_on(COUNTER.scope(123, async {
            COUNTER.with(|v| *v + 1)
        }));
        assert_eq!(len, Some(124));
    }

    #[test]
    fn holder_scope_derives_from_current() {
        // An outer Inherit binding survives a nested holder scope (the child is
        // derived from Cx::current()).
        const LOCALE: Holder<LocaleKey> = Holder::new();
        const COUNTER: Holder<CounterKey> = Holder::new();
        let (loc, cnt) = block_on(LOCALE.scope("es".to_string(), async {
            COUNTER
                .scope(8, async { (LOCALE.get(), COUNTER.get()) })
                .await
        }));
        assert_eq!(loc.as_deref(), Some("es"));
        assert_eq!(cnt, Some(8));
    }

    // ── CxBridge guard ──

    #[test]
    fn bridge_guard_pops_on_drop() {
        let popped = Arc::new(AtomicUsize::new(0));
        let p = popped.clone();
        {
            let _g = BridgeGuard::new(move || {
                p.fetch_add(1, Ordering::SeqCst);
            });
            assert_eq!(popped.load(Ordering::SeqCst), 0);
        }
        assert_eq!(popped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ambient_store_default_is_thread_local() {
        // The default backend is present and round-trips a bundle.
        let store = ambient_store();
        let cx = Cx::empty().with::<CounterKey>(3);
        let mut seen = None;
        store.with_installed(&cx, &mut || {
            seen = store.current().and_then(|c| c.get::<CounterKey>().copied());
        });
        assert_eq!(seen, Some(3));
        assert!(store.current().is_none());
    }

    // dyn-compatibility smoke for the bridge/decorator seams.
    #[test]
    fn bridge_and_decorator_are_object_safe() {
        struct B;
        impl CxBridge for B {
            fn project(&self, _cx: &Cx) -> BridgeGuard {
                BridgeGuard::noop()
            }
        }
        struct D;
        impl CxDecorator for D {
            fn capture(&self) -> Cx {
                Cx::current_or_empty()
            }
        }
        let _b: &dyn CxBridge = &B;
        let _d: &dyn CxDecorator = &D;
        let _ = _b.project(&Cx::empty());
        let _ = _d.capture();
    }
}
