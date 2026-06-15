//! The cloneable, typed-rebuildable cache value carrier ([`CachedValue`]) that
//! rides leaf-core's [`StoredValue`] transport (caching,
//! phase3/09).
//!
//! leaf-core's [`StoredValue`] is a `TypeId`-checked `Box<dyn Any + Send + Sync>`
//! — sound, but NOT cloneable, so a heterogeneous physical store cannot hand back
//! the SAME value twice through it. leaf-cache bridges this with ONE concrete
//! wrapper type the store always sees: [`CachedValue`] is an `Arc<dyn CachedInner>`
//! (so it is `Clone` — a refcount bump, never a deep copy, realizing the design's
//! "Arc-shared StoredValue"), and the in-memory store keys on `CachedValue` —
//! recovering it from the erased [`StoredValue`] by downcasting to this ONE known
//! wrapper type regardless of the inner `T`.
//!
//! The win: on a HIT the interceptor rebuilds the TYPED
//! [`ErasedRet`] directly from the carrier
//! ([`CachedValue::pack_ret_as`]) — the value's concrete `T` is preserved through the
//! erased boundary via a monomorphized `Holder<T>` that knows how to re-pack it,
//! with a `TypeId` guard so a cross-method wrong-type read is a loud
//! [`LeafError`], never a silent mis-cast.

use std::any::{Any, TypeId};
use std::sync::Arc;

use leaf_core::{Cause, ErasedRet, ErrorKind, LeafError, StoredValue};

/// The object-safe inner of a [`CachedValue`]: a typed value that can re-pack
/// itself into an [`ErasedRet`] (the hit return) AND report its `TypeId` (the
/// read-time guard).
trait CachedInner: Send + Sync {
    /// The concrete cached `T`'s `TypeId` (the wrong-type read guard).
    fn value_type(&self) -> TypeId;
    /// Re-pack a CLONE of the cached value into the typed [`ErasedRet`] the chain
    /// unwinds (the SKIP-proceed hit path).
    fn pack_ret(&self) -> ErasedRet;
    /// An `Any` view for diagnostics / typed peeks (tests).
    fn as_any(&self) -> &dyn Any;
}

/// The monomorphized carrier for a concrete cached `T: Clone + Send + Sync`.
struct Holder<T: Clone + Send + Sync + 'static>(T);

impl<T: Clone + Send + Sync + 'static> CachedInner for Holder<T> {
    fn value_type(&self) -> TypeId {
        TypeId::of::<T>()
    }
    fn pack_ret(&self) -> ErasedRet {
        ErasedRet::pack(self.0.clone())
    }
    fn as_any(&self) -> &dyn Any {
        &self.0
    }
}

/// The cloneable cache-value carrier the in-memory store keys on (a refcount-cheap
/// `Arc<dyn CachedInner>`).
///
/// Built by the interceptor from a method's typed return
/// ([`CachedValue::of`]); transported through the erased
/// [`Cache`](leaf_core::Cache) trait inside a [`StoredValue`]
/// ([`CachedValue::into_stored`] / [`CachedValue::from_stored`]); and on a HIT
/// re-packed into the typed [`ErasedRet`] ([`CachedValue::pack_ret_as`]).
#[derive(Clone)]
pub struct CachedValue(Arc<dyn CachedInner>);

impl CachedValue {
    /// Wrap a concrete cached value (the interceptor's put path; `T` is the
    /// advised method's return type, `Clone` so a hit re-yields a fresh value).
    #[must_use]
    pub fn of<T: Clone + Send + Sync + 'static>(value: T) -> Self {
        CachedValue(Arc::new(Holder(value)))
    }

    /// The cached value's concrete `TypeId`.
    #[must_use]
    pub fn value_type(&self) -> TypeId {
        self.0.value_type()
    }

    /// Re-pack a CLONE of the cached value as the typed [`ErasedRet`] iff its
    /// concrete type is exactly `T` (the read-time guard prevents a wrong-type
    /// downcast across two methods sharing a cache name+payload).
    ///
    /// # Errors
    /// A [`LeafError`] if the stored value's type is not exactly `T`.
    pub fn pack_ret_as<T: 'static>(&self, method: &leaf_core::MethodKey) -> Result<ErasedRet, LeafError> {
        if self.0.value_type() != TypeId::of::<T>() {
            return Err(type_mismatch(method));
        }
        Ok(self.0.pack_ret())
    }

    /// A typed peek at the cached value (diagnostics / tests).
    #[must_use]
    pub fn peek<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }

    /// Wrap into the leaf-core [`StoredValue`] transport (so it rides the erased
    /// [`Cache`](leaf_core::Cache) trait). The stored concrete type is ALWAYS this
    /// ONE [`CachedValue`] wrapper, so a backend can recover it by downcast.
    #[must_use]
    pub fn into_stored(self) -> StoredValue {
        StoredValue::new(self)
    }

    /// Recover a [`CachedValue`] from the [`StoredValue`] transport (the in-memory
    /// store + the interceptor's hit path). `None` if the stored value was not put
    /// through the [`CachedValue`] carrier (a foreign backend's raw value).
    #[must_use]
    pub fn from_stored(v: &StoredValue) -> Option<CachedValue> {
        v.downcast_ref::<CachedValue>().cloned()
    }
}

impl std::fmt::Debug for CachedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedValue").field("value_type", &self.0.value_type()).finish_non_exhaustive()
    }
}

/// The wrong-type read error (a [`CachedValue`] read as the wrong `T`).
fn type_mismatch(method: &leaf_core::MethodKey) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "cache value read",
        format!("the cached value's type did not match the advised return type for {method:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::MethodKey;

    fn m() -> MethodKey {
        MethodKey::of("svc::find")
    }

    #[test]
    fn round_trips_a_typed_value_through_the_stored_transport() {
        let v = CachedValue::of(42_u64);
        let stored = v.into_stored();
        let back = CachedValue::from_stored(&stored).expect("a CachedValue carrier");
        let ret = back.pack_ret_as::<u64>(&m()).expect("typed re-pack");
        assert_eq!(ret.unpack::<u64>().unwrap(), 42);
    }

    #[test]
    fn cloning_yields_an_independent_value_each_read() {
        // A Clone-able value re-yields a FRESH value per hit (Arc-shared carrier).
        let v = CachedValue::of(vec![1_u8, 2, 3]);
        let a = v.pack_ret_as::<Vec<u8>>(&m()).unwrap().unpack::<Vec<u8>>().unwrap();
        let b = v.pack_ret_as::<Vec<u8>>(&m()).unwrap().unpack::<Vec<u8>>().unwrap();
        assert_eq!(a, vec![1, 2, 3]);
        assert_eq!(b, vec![1, 2, 3]);
    }

    #[test]
    fn a_wrong_type_read_is_a_loud_error_not_a_miscast() {
        let v = CachedValue::of(7_u64);
        assert!(v.pack_ret_as::<String>(&m()).is_err(), "a wrong-type read is a LeafError");
        assert!(v.pack_ret_as::<u64>(&m()).is_ok());
    }

    #[test]
    fn peek_reads_the_typed_value() {
        let v = CachedValue::of("hello".to_string());
        assert_eq!(v.peek::<String>().map(String::as_str), Some("hello"));
        assert!(v.peek::<u64>().is_none());
    }
}
