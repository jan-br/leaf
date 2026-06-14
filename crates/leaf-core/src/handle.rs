//! The ONE shared-bean currency: [`ErasedBean`], [`Ref<T>`], [`Published`].
//!
//! leaf's entire architecture hangs off one shared-handle type
//! (ADR-01 ownership-model, TOOLKIT.md). Everything shared inside the container
//! is an [`ErasedBean`] = `Arc<dyn Any + Send + Sync>`, sugared at typed
//! boundaries as [`Ref<T>`] (over `dyn Svc` or a concrete type, using trait
//! upcasting — stable since 1.86). There is exactly ONE shared-handle type; the
//! genuine ownership divergence rides a tiny total [`Published`] enum produced
//! at the publish step — [`Published::Shared`] for singleton + every
//! context-scope instance, [`Published::Owned`] for prototype (a real owned
//! move, never refcounted, never stored, no teardown).
//!
//! The `Send + Sync + 'static` safe-publication contract rides [`ErasedBean`]/
//! [`Bean`] in this ONE leaf-core location; the atomic `Arc` clone IS the
//! happens-before edge, so there is no singleton mutex and no global lock.

use std::any::Any;
use std::sync::Arc;

/// THE canonical stored / published SHARED shape — exactly one type.
///
/// Singleton beans, every context-scope (request/session/custom) instance, the
/// `NULL_BEAN` sentinel, FactoryBean products, parent-supplied cross-level
/// beans, and test doubles are ALL this identical `Arc<dyn Any + Send + Sync>`.
/// The container cannot tell origins apart — "WASM-ness stops at the proxy".
pub type ErasedBean = Arc<dyn Any + Send + Sync>;

/// Marker supertrait every managed service trait extends so that
/// `Arc<dyn Bean>` upcasts to any declared service trait AND to `dyn Any`
/// (trait upcasting, stable 1.86).
///
/// The `Send + Sync + 'static` safe-publication bound lives HERE, in one place:
/// declaring a shared service `: Bean` propagates the concurrency contract to
/// every cross-crate contribution with zero per-crate restating.
pub trait Bean: Any + Send + Sync {}

/// Typed sugar over an [`Arc`] / [`ErasedBean`].
///
/// `Ref<Concrete>` (from an exact-`TypeId` downcast) or `Ref<dyn Svc>` (from a
/// trait upcast) is what `Container::get` hands back for shared beans. It is a
/// cheap-clone `Deref` handle: constructor-injected collaborators are resolved
/// once and stored as `Ref<T>` fields, so steady-state method calls touch no
/// refcount and no downcast.
pub struct Ref<T: ?Sized>(Arc<T>);

impl<T: ?Sized> Ref<T> {
    /// Wrap an existing `Arc<T>` as a `Ref<T>`.
    #[must_use]
    pub fn from_arc(arc: Arc<T>) -> Self {
        Ref(arc)
    }

    /// Construct a `Ref<T>` directly from an owned `T` (sized).
    #[must_use]
    pub fn new(value: T) -> Self
    where
        T: Sized,
    {
        Ref(Arc::new(value))
    }

    /// Borrow the inner `Arc<T>` (e.g. to clone it into an [`ErasedBean`] via
    /// an unsizing coercion at the call site).
    #[must_use]
    pub fn as_arc(&self) -> &Arc<T> {
        &self.0
    }

    /// Consume the `Ref<T>`, yielding the inner `Arc<T>`.
    #[must_use]
    pub fn into_arc(self) -> Arc<T> {
        self.0
    }

    /// Number of strong references to the shared value (diagnostics/tests).
    #[must_use]
    pub fn strong_count(this: &Self) -> usize {
        Arc::strong_count(&this.0)
    }
}

impl<T: ?Sized> std::ops::Deref for Ref<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

// Manual `Clone` (not `derive`) so it holds for `T: ?Sized` without requiring
// `T: Clone` — cloning a `Ref` is just an atomic refcount bump.
impl<T: ?Sized> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Ref(Arc::clone(&self.0))
    }
}

impl<T: ?Sized> From<Arc<T>> for Ref<T> {
    fn from(arc: Arc<T>) -> Self {
        Ref(arc)
    }
}

impl<T: ?Sized> From<Ref<T>> for Arc<T> {
    fn from(r: Ref<T>) -> Self {
        r.0
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Ref").field(&&*self.0).finish()
    }
}

/// PUBLICATION: the only place the two genuine ownership shapes diverge.
///
/// A total, tiny two-arm map from `Multiplicity`:
/// `{Once, PerContextKey} -> Shared`, `PerResolution -> Owned`. Singleton and
/// every context-scope instance are the SAME [`Published::Shared`]; they differ
/// only in WHICH store holds the `Arc` and WHEN it drops (data on three
/// orthogonal axes, never the handle type). Prototype is an owned MOVE
/// ([`Published::Owned`]) — never stored, never refcounted, no teardown.
pub enum Published {
    /// Singleton + every context-scope (request/session/custom) instance.
    Shared(ErasedBean),
    /// Prototype: an owned move — never stored, never refcounted, no teardown.
    Owned(Box<dyn Any + Send>),
}

impl Published {
    /// Publish an already-erased shared handle.
    #[must_use]
    pub fn shared(bean: ErasedBean) -> Self {
        Published::Shared(bean)
    }

    /// Publish a concrete value as a SHARED handle (the common native-bean
    /// path: `Published::shared_value(MyService::new())`).
    #[must_use]
    pub fn shared_value<T: Any + Send + Sync>(value: T) -> Self {
        Published::Shared(Arc::new(value))
    }

    /// Publish a concrete value as an OWNED move (the prototype path).
    #[must_use]
    pub fn owned<T: Any + Send>(value: T) -> Self {
        Published::Owned(Box::new(value))
    }

    /// `true` iff this is the shared (refcounted) publication shape.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        matches!(self, Published::Shared(_))
    }

    /// `true` iff this is the owned-move (prototype) publication shape.
    #[must_use]
    pub fn is_owned(&self) -> bool {
        matches!(self, Published::Owned(_))
    }

    /// Take the shared handle if this is a [`Published::Shared`].
    #[must_use]
    pub fn into_shared(self) -> Option<ErasedBean> {
        match self {
            Published::Shared(b) => Some(b),
            Published::Owned(_) => None,
        }
    }

    /// Take the owned box if this is a [`Published::Owned`].
    #[must_use]
    pub fn into_owned(self) -> Option<Box<dyn Any + Send>> {
        match self {
            Published::Owned(b) => Some(b),
            Published::Shared(_) => None,
        }
    }
}

impl std::fmt::Debug for Published {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Published::Shared(_) => f.write_str("Published::Shared(..)"),
            Published::Owned(_) => f.write_str("Published::Owned(..)"),
        }
    }
}

/// Downcast a shared [`ErasedBean`] to a concrete `Ref<T>` by exact `TypeId`.
///
/// This is RECOGNITION, not validation: `Arc<dyn Any>` proves nothing at wiring
/// time about whether `T` was registered, so a mismatch is `Err(original)`
/// (the caller turns it into a rich `NoSuchBean`/type-mismatch diagnostic).
///
/// # Errors
/// Returns the original [`ErasedBean`] unchanged if its concrete type is not
/// exactly `T`.
pub fn downcast_ref<T: Any + Send + Sync>(bean: ErasedBean) -> Result<Ref<T>, ErasedBean> {
    match bean.downcast::<T>() {
        Ok(arc) => Ok(Ref(arc)),
        Err(original) => Err(original),
    }
}

/// Downcast an owned prototype box ([`Published::Owned`]) to a concrete `T`.
///
/// # Errors
/// Returns the original box unchanged if its concrete type is not exactly `T`.
pub fn downcast_owned<T: Any>(boxed: Box<dyn Any + Send>) -> Result<T, Box<dyn Any + Send>> {
    boxed.downcast::<T>().map(|b| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Svc {
        id: u32,
    }

    trait Greeter: Bean {
        fn greet(&self) -> String;
    }

    struct EnglishGreeter;
    impl Bean for EnglishGreeter {}
    impl Greeter for EnglishGreeter {
        fn greet(&self) -> String {
            "hello".to_string()
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn erased_bean_is_send_sync() {
        assert_send_sync::<ErasedBean>();
        assert_send_sync::<Ref<Svc>>();
    }

    #[test]
    fn ref_derefs_to_target() {
        let r = Ref::new(Svc { id: 7 });
        assert_eq!(r.id, 7);
        assert_eq!(&*r, &Svc { id: 7 });
    }

    #[test]
    fn ref_clone_is_a_refcount_bump_not_a_deep_copy() {
        let r = Ref::new(Svc { id: 1 });
        assert_eq!(Ref::strong_count(&r), 1);
        let r2 = r.clone();
        assert_eq!(Ref::strong_count(&r), 2);
        // Same underlying allocation.
        assert!(std::ptr::eq(r.as_arc().as_ref(), r2.as_arc().as_ref()));
    }

    #[test]
    fn ref_from_arc_and_back() {
        let arc = Arc::new(Svc { id: 9 });
        let r: Ref<Svc> = Arc::clone(&arc).into();
        assert_eq!(r.id, 9);
        let back: Arc<Svc> = r.into();
        assert!(std::ptr::eq(arc.as_ref(), back.as_ref()));
    }

    #[test]
    fn downcast_round_trip_concrete() {
        let bean: ErasedBean = Arc::new(Svc { id: 42 });
        let r = downcast_ref::<Svc>(bean).expect("downcast to Svc");
        assert_eq!(r.id, 42);
    }

    #[test]
    fn downcast_wrong_type_returns_original_handle() {
        let bean: ErasedBean = Arc::new(Svc { id: 42 });
        // Asking for the wrong concrete type fails and hands the Arc back.
        let err = downcast_ref::<String>(bean).expect_err("must not downcast to String");
        // The original handle is intact and still downcasts to the right type.
        let r = downcast_ref::<Svc>(err).expect("original still valid");
        assert_eq!(r.id, 42);
    }

    #[test]
    fn ref_dyn_trait_upcast_to_any_and_downcast_back() {
        // Trait upcasting (stable 1.86): a Ref<dyn Greeter> built from a
        // concrete, erased to dyn Any+Send+Sync, downcasts back to concrete.
        let concrete = Arc::new(EnglishGreeter);
        let trait_ref: Ref<dyn Greeter> = Ref::from_arc(concrete.clone());
        assert_eq!(trait_ref.greet(), "hello");

        // Upcast the concrete Arc to the erased handle, downcast it back.
        let erased: ErasedBean = concrete;
        let back = downcast_ref::<EnglishGreeter>(erased).expect("downcast to concrete");
        assert_eq!(back.greet(), "hello");
    }

    #[test]
    fn published_shared_variant_handling() {
        let p = Published::shared_value(Svc { id: 5 });
        assert!(p.is_shared());
        assert!(!p.is_owned());
        let bean = p.into_shared().expect("shared");
        let r = downcast_ref::<Svc>(bean).expect("downcast");
        assert_eq!(r.id, 5);
    }

    #[test]
    fn published_owned_variant_handling() {
        let p = Published::owned(Svc { id: 11 });
        assert!(p.is_owned());
        assert!(!p.is_shared());
        // The wrong-arm accessor yields None without consuming the contents.
        let boxed = p.into_owned().expect("owned");
        let val = downcast_owned::<Svc>(boxed).expect("downcast owned");
        assert_eq!(val, Svc { id: 11 });
    }

    #[test]
    fn published_shared_does_not_yield_owned_and_vice_versa() {
        assert!(Published::shared_value(Svc { id: 1 }).into_owned().is_none());
        assert!(Published::owned(Svc { id: 1 }).into_shared().is_none());
    }

    #[test]
    fn downcast_owned_wrong_type_returns_box() {
        let boxed: Box<dyn Any + Send> = Box::new(Svc { id: 3 });
        let err = downcast_owned::<u32>(boxed).expect_err("not a u32");
        let val = downcast_owned::<Svc>(err).expect("still a Svc");
        assert_eq!(val, Svc { id: 3 });
    }
}
