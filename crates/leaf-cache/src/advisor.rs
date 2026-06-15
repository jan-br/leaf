//! The Infrastructure cache [`AdvisorDescriptor`](leaf_core::AdvisorDescriptor)
//! that auto-wires (caching, phase3/09): the ONE advisor row
//! (`Role::Infrastructure`, `order = CACHE_ORDER = 400`, sorted OUTSIDE/before tx
//! since `CACHE_ORDER < TX_ORDER`) whose `make_interceptor` resolves a
//! [`CacheManager`](leaf_core::CacheManager) through the ordinary container + builds
//! a [`CacheInterceptor`](crate::CacheInterceptor).
//!
//! Two faces, one shape (mirroring leaf-tx):
//!
//! - the const auto-wire row submitted into
//!   [`ADVISOR_PAIRINGS`](leaf_core::ADVISOR_PAIRINGS) (force-linked by
//!   [`enable_caching`]) so a binary that links leaf-cache gets the cache advisor in
//!   the run pipeline's proxy plan with NO hand-assembled `.with_advisors`;
//! - the programmatic [`cache_advisor_pairing`] / [`make_cache_interceptor`]
//!   builders an integration crate (leaf-redis, …) or a test uses to bind ITS
//!   concrete manager + a finer pointcut + the per-method [`CacheOpMeta`] + key fn.
//!
//! The pointcut is [`CachePointcut`] — leaf-cache's own const-constructible
//! predicate (matching by the bean's concrete `TypeId` or a cache
//! [`MarkerId`](leaf_core::MarkerId)), since the kernel
//! `within`/`annotated_marker` combinators are not const-constructible into a
//! `&'static` row.
//!
//! ## Attribute NOTE
//!
//! `MakeInterceptor` is a bare fn-pointer (no captured env). The `#[cacheable]`
//! macro emits the per-method [`CacheOpMeta`] PUBLIC const + the `ADVISORS`
//! identity row, but not the `ADVISOR_PAIRINGS` auto-wire row nor a typed key fn
//! (the same staging as leaf-tx's `#[transactional]`); so a binding site supplies
//! the [`CacheOpMeta`] reference + the typed [`CacheKeyFn`](crate::CacheKeyFn) + the
//! return type `T` to [`make_cache_interceptor`]. A per-method
//! `(CacheOpMeta, key_fn, T)` table threaded by the macro is deferred (a NOTE).

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    AdvisorPairingRow, BeanKey, Cardinality, Container, ContractId, Interceptor, JoinPointMeta,
    LeafError, MakeInterceptor, MarkerId, OrderKey, OrderSource, Pointcut, Role, Strictness,
    CACHE_ORDER,
};

use crate::interceptor::{CacheInterceptor, CacheKeyFn, CacheOp};

/// The stable identity of the built-in (auto-wired) cache advisor.
#[must_use]
pub fn cache_advisor_contract() -> ContractId {
    ContractId::of("leaf::cache::CacheAdvisor")
}

/// The chain order of the cache advisor: the pinned `CACHE_ORDER = 400` with an
/// `Interface` source (a framework-declared, most-specific order) — OUTSIDE tx
/// (`CACHE_ORDER < TX_ORDER`) so a hit short-circuits before a tx opens.
#[must_use]
pub fn cache_order_key() -> OrderKey {
    OrderKey { value: CACHE_ORDER, source: OrderSource::Interface }
}

/// The default cache marker the auto-wire advisor keys on (the marker a future
/// `#[cacheable]` bean-attribute would emit onto the advised bean's
/// `AnnotationMetadata`).
#[must_use]
pub fn cache_marker() -> MarkerId {
    MarkerId::of("leaf::cache::Cacheable")
}

// ───────────────────────────────── CachePointcut ────────────────────────────

/// leaf-cache's const-constructible pointcut: matches a join point whose bean is
/// one of the named concrete `TypeId`s OR carries one of the named cache
/// [`MarkerId`]s.
///
/// `&'static CachePointcut` is usable as a `&'static dyn Pointcut` on the const
/// [`AdvisorPairingRow`] — the kernel `within`/`annotated_marker` combinators are
/// not const-constructible into a `&'static` row, so leaf-cache owns this one. A
/// binding site writes:
///
/// ```ignore
/// static P: CachePointcut = CachePointcut::new(&[const { TypeId::of::<MyBean>() }], &[]);
/// ```
pub struct CachePointcut {
    types: &'static [TypeId],
    markers: &'static [MarkerId],
}

impl CachePointcut {
    /// A pointcut matching beans whose concrete type is in `types` OR that carry a
    /// marker in `markers`.
    #[must_use]
    pub const fn new(types: &'static [TypeId], markers: &'static [MarkerId]) -> Self {
        CachePointcut { types, markers }
    }

    /// The concrete `TypeId`s this pointcut matches by exact type.
    #[must_use]
    pub fn types(&self) -> &'static [TypeId] {
        self.types
    }

    /// The cache markers this pointcut matches by `AnnotationMetadata` presence.
    #[must_use]
    pub fn markers(&self) -> &'static [MarkerId] {
        self.markers
    }
}

impl Pointcut for CachePointcut {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        if self.types.contains(&jp.bean_type) {
            return true;
        }
        self.markers
            .iter()
            .any(|m| jp.markers.markers.contains(m) || jp.markers.qualifiers.contains(m))
    }
}

impl std::fmt::Debug for CachePointcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachePointcut")
            .field("types", &self.types.len())
            .field("markers", &self.markers.len())
            .finish()
    }
}

/// The auto-wire default pointcut: matches the leaf-cache [`cache_marker`] on a
/// bean. (A `#[cacheable]` bean would carry this marker once the macro emits it.)
pub static CACHE_MARKER_POINTCUT: CachePointcut =
    CachePointcut::new(&[], &[MarkerId::of("leaf::cache::Cacheable")]);

// ──────────────────────────── make_interceptor builders ─────────────────────

/// Resolve the concrete manager `M` from the container and upcast it to
/// `Arc<dyn CacheManager>` (the bean bridge a `make_interceptor` closure calls).
///
/// # Errors
/// A [`LeafError`] if the manager bean is absent or not the expected concrete type.
pub async fn resolve_manager<M>(
    container: &dyn Container,
) -> Result<Arc<dyn leaf_core::CacheManager>, LeafError>
where
    M: leaf_core::CacheManager + 'static,
{
    let published = container
        .resolve(BeanKey::ByType(TypeId::of::<M>()), Strictness::Strict, Cardinality::Single)
        .await?;
    let erased = published.into_shared().ok_or_else(manager_mismatch)?;
    let manager: Arc<M> = erased.downcast::<M>().map_err(|_| manager_mismatch())?;
    Ok(manager as Arc<dyn leaf_core::CacheManager>)
}

/// Build a `make_interceptor` bean-bridge future resolving manager `M` and
/// building a single-rule [`CacheInterceptor`] caching `method` with `op`/`meta`/
/// `key_fn` over return type `T`.
///
/// The returned future is usable inside a [`MakeInterceptor`] fn-pointer body at
/// the binding site (a non-capturing closure literal calls this with `method`/`op`/
/// `meta`/`key_fn`/`T` baked in). It is the same resolve-and-build shape
/// `#[aspect]`'s `make_interceptor` uses.
///
/// For a bean with MULTIPLE cached methods (mixed `@Cacheable`/`@CacheEvict`),
/// build the [`CacheInterceptor`] directly from a `Vec<CacheRule>` via
/// [`CacheInterceptor::new`](crate::CacheInterceptor::new) + [`resolve_manager`].
///
/// # Errors
/// A [`LeafError`] if the manager bean resolution fails.
pub async fn build_cache_interceptor<M, T>(
    container: &dyn Container,
    method: leaf_core::MethodKey,
    op: CacheOp,
    meta: &'static leaf_core::CacheOpMeta,
    key_fn: CacheKeyFn,
) -> Result<Arc<dyn Interceptor>, LeafError>
where
    M: leaf_core::CacheManager + 'static,
    T: Clone + Send + Sync + 'static,
{
    let manager = resolve_manager::<M>(container).await?;
    Ok(Arc::new(CacheInterceptor::single::<T>(manager, method, op, meta, key_fn))
        as Arc<dyn Interceptor>)
}

fn manager_mismatch() -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "cache advisor make_interceptor",
        "the resolved cache-manager bean was not the expected concrete type",
    ))
}

// ────────────────────────────── pairing builders ────────────────────────────

/// Build an [`AdvisorPairingRow`] for the cache advisor binding the concrete
/// manager `M`, the per-method `make_interceptor` bean bridge, and matching
/// `pointcut` (the programmatic / integration-crate face).
///
/// `Role::Infrastructure` + `CACHE_ORDER` (the canonical chain slot, OUTSIDE tx —
/// `CACHE_ORDER < TX_ORDER` — so a cache hit short-circuits before a tx opens).
///
/// The `make_interceptor` MUST be a non-capturing fn-pointer that calls
/// [`build_cache_interceptor::<M, T>`](build_cache_interceptor) with the per-method
/// `op`/`meta`/`key_fn` baked in (a const closure literal, exactly like the
/// `#[aspect]` codegen emits).
#[must_use]
pub fn cache_advisor_pairing(
    pointcut: &'static dyn Pointcut,
    make_interceptor: MakeInterceptor,
) -> AdvisorPairingRow {
    AdvisorPairingRow {
        contract: cache_advisor_contract(),
        order: cache_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor,
    }
}

/// Force-link leaf-cache so its cache advisor participates (the
/// `enable_caching!()` analogue, ADR-09 anti-DCE force-link). Returns the cache
/// advisor's stable identity so a binary can add it to its expected-vs-found
/// manifest.
///
/// A concrete manager bean + the auto-wire row are an integration crate's / the
/// binary's concern (leaf-cache ships the in-memory default + the manager-agnostic
/// interceptor + builders); this names the advisor identity for the manifest.
#[must_use]
pub fn enable_caching() -> ContractId {
    cache_advisor_contract()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{AnnotationMetadata, MethodKey, CACHE_ORDER, TX_ORDER};

    use crate::interceptor::unit_key_fn;
    use crate::manager::InMemoryCacheManager;

    struct Bean;

    static USERS: leaf_core::CacheOpMeta = leaf_core::CacheOpMeta {
        cache_names: &["users"],
        all_entries: false,
        before_invocation: false,
        sync: false,
    };

    fn jp<'a>(bean_type: TypeId, markers: &'a AnnotationMetadata) -> JoinPointMeta<'a> {
        JoinPointMeta {
            bean_type,
            method: MethodKey::of("Bean::m"),
            markers,
            arg_types: &[],
            ret_type: TypeId::of::<()>(),
        }
    }

    // A non-capturing make_interceptor fn-pointer (the row literal shape).
    fn make_users_cache() -> MakeInterceptor {
        |c| {
            Box::pin(build_cache_interceptor::<InMemoryCacheManager, u64>(
                c,
                MethodKey::of("svc::find"),
                CacheOp::Cacheable,
                &USERS,
                unit_key_fn(),
            ))
        }
    }

    #[test]
    fn cache_advisor_is_infrastructure_at_cache_order() {
        let p: &'static dyn Pointcut = &CACHE_MARKER_POINTCUT;
        let row = cache_advisor_pairing(p, make_users_cache());
        assert_eq!(row.role, Role::Infrastructure, "cache advice is framework infrastructure");
        assert_eq!(row.order.value, CACHE_ORDER, "the pinned CACHE_ORDER chain slot (400)");
        assert_eq!(row.order.source, OrderSource::Interface, "framework-declared order");
        assert_eq!(row.contract, cache_advisor_contract());
    }

    #[test]
    fn cache_sorts_outside_tx() {
        // The cache-outside-tx correctness invariant, as DATA: CACHE_ORDER < TX_ORDER.
        assert!(cache_order_key().value < TX_ORDER, "cache must wrap (sit outside) tx");
    }

    #[test]
    fn cache_pointcut_matches_by_concrete_type() {
        static BEAN_TYPES: [TypeId; 1] = [const { TypeId::of::<Bean>() }];
        let pc = CachePointcut::new(&BEAN_TYPES, &[]);
        let empty = AnnotationMetadata::EMPTY;
        assert!(pc.matches(&jp(TypeId::of::<Bean>(), &empty)), "matches the named concrete type");
        assert!(
            !pc.matches(&jp(TypeId::of::<InMemoryCacheManager>(), &empty)),
            "does NOT match an unrelated bean (never advises the manager itself)"
        );
    }

    #[test]
    fn cache_marker_pointcut_matches_a_cacheable_marker() {
        static MARKED: AnnotationMetadata = AnnotationMetadata {
            markers: &[MarkerId::of("leaf::cache::Cacheable")],
            ..AnnotationMetadata::EMPTY
        };
        let other = AnnotationMetadata::EMPTY;
        let bean_ty = TypeId::of::<Bean>();
        assert!(
            CACHE_MARKER_POINTCUT.matches(&jp(bean_ty, &MARKED)),
            "matches a bean carrying the cache marker"
        );
        assert!(
            !CACHE_MARKER_POINTCUT.matches(&jp(bean_ty, &other)),
            "does NOT match an unmarked bean"
        );
    }

    #[test]
    fn cache_marker_equals_the_public_marker() {
        assert_eq!(cache_marker(), MarkerId::of("leaf::cache::Cacheable"));
    }

    #[test]
    fn enable_caching_names_the_advisor_identity() {
        assert_eq!(enable_caching(), cache_advisor_contract());
    }
}
