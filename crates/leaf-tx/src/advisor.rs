//! The Infrastructure tx [`AdvisorDescriptor`](leaf_core::AdvisorDescriptor) that
//! auto-wires (transaction-management, phase3/09): the ONE advisor row
//! (`Role::Infrastructure`, `order = TX_ORDER = 500`) whose `make_interceptor`
//! resolves a [`TransactionManager`] through the ordinary container + builds a
//! [`TransactionInterceptor`].
//!
//! Two faces, one shape:
//!
//! - the const auto-wire row submitted into
//!   [`ADVISOR_PAIRINGS`](leaf_core::ADVISOR_PAIRINGS) (force-linked by
//!   [`enable_transaction_management`]) so a binary that links leaf-tx gets the tx
//!   advisor in the run pipeline's proxy plan with NO hand-assembled
//!   `.with_advisors` — the headline "the Infrastructure advisor auto-wires";
//! - the programmatic [`tx_advisor_pairing`] / [`make_transaction_interceptor`]
//!   builders an integration crate (leaf-sqlx-tx, …) or a test uses to bind ITS
//!   concrete manager + a finer pointcut.
//!
//! The pointcut is [`TxPointcut`] — leaf-tx's own const-constructible predicate
//! (matching by the bean's concrete `TypeId` or a tx [`MarkerId`]), since the
//! kernel `within`/`annotated_marker` combinators are not const-constructible into
//! a `&'static` row. (When the `#[transactional]` macro lands a per-bean TX marker,
//! the auto-wire pointcut keys on it; until then it is a leaf-tx-owned marker — a
//! NOTE in the crate docs.)
//!
//! ## Attribute NOTE
//!
//! `MakeInterceptor` is a bare fn-pointer (no captured env), so the auto-wire row
//! always applies [`TxAttribute::DEFAULT`] (propagation `Required`, any-`Err`
//! rolls back). A per-method [`TxAttribute`] table — the macro-emitted const the
//! design pins — is deferred (a NOTE in the crate docs); the interceptor already
//! honors a non-default attribute when one is supplied programmatically via
//! [`TransactionInterceptor::new`].

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    AdvisorPairingRow, BeanKey, BoxFuture, Cardinality, Container, ContractId, Interceptor,
    JoinPointMeta, LeafError, MakeInterceptor, MarkerId, OrderKey, OrderSource, Pointcut, Role,
    Strictness, TransactionManager, TxAttribute, TX_ORDER,
};

use crate::interceptor::{result_classifier, TransactionInterceptor};

/// The stable identity of the built-in (auto-wired) tx advisor.
#[must_use]
pub fn tx_advisor_contract() -> ContractId {
    ContractId::of("leaf::tx::TransactionAdvisor")
}

/// The chain order of the tx advisor: the pinned `TX_ORDER = 500` with an
/// `Interface` source (a framework-declared, most-specific order).
#[must_use]
pub fn tx_order_key() -> OrderKey {
    OrderKey { value: TX_ORDER, source: OrderSource::Interface }
}

/// The default tx marker the auto-wire advisor keys on (the marker a future
/// `#[transactional]` macro emits onto the advised bean's `AnnotationMetadata`).
#[must_use]
pub fn tx_marker() -> MarkerId {
    MarkerId::of("leaf::tx::Transactional")
}

// ───────────────────────────────── TxPointcut ───────────────────────────────

/// leaf-tx's const-constructible pointcut: matches a join point whose bean is one
/// of the named concrete `TypeId`s OR carries one of the named tx [`MarkerId`]s.
///
/// `&'static TxPointcut` is usable as a `&'static dyn Pointcut` on the const
/// [`AdvisorPairingRow`] — the kernel `within`/`annotated_marker` combinators are
/// not const-constructible into a `&'static` row (their fields are private), so
/// leaf-tx owns this one. `TypeId::of::<T>()` is callable in an inline `const {}`
/// block (stable), so a binding site writes:
///
/// ```ignore
/// static P: TxPointcut = TxPointcut::new(&[const { TypeId::of::<MyBean>() }], &[]);
/// ```
pub struct TxPointcut {
    types: &'static [TypeId],
    markers: &'static [MarkerId],
}

impl TxPointcut {
    /// A pointcut matching beans whose concrete type is in `types` OR that carry a
    /// marker in `markers`.
    #[must_use]
    pub const fn new(types: &'static [TypeId], markers: &'static [MarkerId]) -> Self {
        TxPointcut { types, markers }
    }

    /// The concrete `TypeId`s this pointcut matches by exact type.
    #[must_use]
    pub fn types(&self) -> &'static [TypeId] {
        self.types
    }

    /// The tx markers this pointcut matches by `AnnotationMetadata` presence.
    #[must_use]
    pub fn markers(&self) -> &'static [MarkerId] {
        self.markers
    }
}

impl Pointcut for TxPointcut {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        if self.types.contains(&jp.bean_type) {
            return true;
        }
        self.markers
            .iter()
            .any(|m| jp.markers.markers.contains(m) || jp.markers.qualifiers.contains(m))
    }
}

impl std::fmt::Debug for TxPointcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxPointcut")
            .field("types", &self.types.len())
            .field("markers", &self.markers.len())
            .finish()
    }
}

/// The auto-wire default pointcut: matches the leaf-tx [`tx_marker`] on a bean.
/// (A `#[transactional]` bean would carry this marker once the macro lands.)
pub static TX_MARKER_POINTCUT: TxPointcut =
    TxPointcut::new(&[], &[MarkerId::of("leaf::tx::Transactional")]);

// ──────────────────────────── make_interceptor builders ─────────────────────

/// Build a [`MakeInterceptor`] that resolves the CONCRETE manager `M` by its
/// `TypeId` through the container and wraps it in a [`TransactionInterceptor`]
/// applying [`TxAttribute::DEFAULT`].
///
/// `M` is the concrete manager bean type (e.g. an integration crate's
/// `SqlxTransactionManager`, or leaf-tx's
/// [`InMemoryTransactionManager`](crate::InMemoryTransactionManager)); it is
/// resolved by `BeanKey::ByType(TypeId::of::<M>())` and downcast to `Arc<M>`, then
/// re-wrapped as `Arc<dyn TransactionManager>` — the same resolve-and-upcast bean
/// bridge the `#[aspect]` `make_interceptor` uses.
#[must_use]
pub fn make_transaction_interceptor<M>() -> MakeInterceptor
where
    M: TransactionManager + 'static,
{
    |container: &dyn Container| {
        Box::pin(async move {
            let published = container
                .resolve(
                    BeanKey::ByType(TypeId::of::<M>()),
                    Strictness::Strict,
                    Cardinality::Single,
                )
                .await?;
            let erased = published.into_shared().ok_or_else(manager_mismatch)?;
            let manager: Arc<M> = erased.downcast::<M>().map_err(|_| manager_mismatch())?;
            let manager: Arc<dyn TransactionManager> = manager;
            Ok(Arc::new(TransactionInterceptor::new(manager, TxAttribute::DEFAULT))
                as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, LeafError>>
    }
}

/// Like [`make_transaction_interceptor`] but ALSO installs a
/// [`result_classifier`](crate::interceptor::result_classifier) for a method
/// returning `Result<T, LeafError>`, so a business `Result::Err` (returned through
/// the chain's `Ok(ErasedRet)`) also drives the commit-vs-rollback decision.
///
/// `M` is the concrete manager bean type; `T` is the advised method's `Ok` type
/// (its return is `Result<T, LeafError>`). The monomorphized fn-item coerces to the
/// bare [`MakeInterceptor`] fn-pointer, baking the per-`T` classifier in.
#[must_use]
pub fn make_transaction_interceptor_for<M, T>() -> MakeInterceptor
where
    M: TransactionManager + 'static,
    T: std::any::Any + Send + 'static,
{
    |container: &dyn Container| {
        Box::pin(async move {
            let published = container
                .resolve(
                    BeanKey::ByType(TypeId::of::<M>()),
                    Strictness::Strict,
                    Cardinality::Single,
                )
                .await?;
            let erased = published.into_shared().ok_or_else(manager_mismatch)?;
            let manager: Arc<M> = erased.downcast::<M>().map_err(|_| manager_mismatch())?;
            let manager: Arc<dyn TransactionManager> = manager;
            let interceptor = TransactionInterceptor::new(manager, TxAttribute::DEFAULT)
                .with_return_classifier(result_classifier::<T>());
            Ok(Arc::new(interceptor) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, LeafError>>
    }
}

fn manager_mismatch() -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "tx advisor make_interceptor",
        "the resolved transaction-manager bean was not the expected concrete type",
    ))
}

// ────────────────────────────── pairing builders ────────────────────────────

/// Build an [`AdvisorPairingRow`] for the tx advisor binding the concrete manager
/// `M` and matching `pointcut` (the programmatic / integration-crate face).
///
/// `Role::Infrastructure` + `TX_ORDER` (the canonical chain slot, INSIDE cache —
/// `CACHE_ORDER < TX_ORDER` — so a cache hit short-circuits before a tx opens).
#[must_use]
pub fn tx_advisor_pairing<M>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
where
    M: TransactionManager + 'static,
{
    AdvisorPairingRow {
        contract: tx_advisor_contract(),
        order: tx_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: make_transaction_interceptor::<M>(),
    }
}

/// Like [`tx_advisor_pairing`] but binds a per-return-type
/// [`result_classifier`](crate::interceptor::result_classifier) (via
/// [`make_transaction_interceptor_for`]) so a method returning `Result<T, LeafError>`
/// rolls back on a business `Result::Err` too. `T` is the advised method's `Ok` type.
#[must_use]
pub fn tx_advisor_pairing_for<M, T>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
where
    M: TransactionManager + 'static,
    T: std::any::Any + Send + 'static,
{
    AdvisorPairingRow {
        contract: tx_advisor_contract(),
        order: tx_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: make_transaction_interceptor_for::<M, T>(),
    }
}

/// Force-link leaf-tx so its tx advisor participates (the
/// `enable_transaction_management!()` analogue, ADR-09 anti-DCE force-link).
/// Returns the tx advisor's stable identity so a binary can add it to its
/// expected-vs-found manifest.
///
/// A concrete manager + the auto-wire row are an integration crate's / the
/// binary's concern (leaf-tx ships the manager-agnostic interceptor + builders);
/// this names the advisor identity for the manifest.
#[must_use]
pub fn enable_transaction_management() -> ContractId {
    tx_advisor_contract()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{AnnotationMetadata, MethodKey, TX_ORDER};

    use crate::manager::InMemoryTransactionManager;

    struct Bean;

    fn jp<'a>(bean_type: TypeId, markers: &'a AnnotationMetadata) -> JoinPointMeta<'a> {
        JoinPointMeta {
            bean_type,
            method: MethodKey::of("Bean::m"),
            markers,
            arg_types: &[],
            ret_type: TypeId::of::<()>(),
        }
    }

    #[test]
    fn tx_advisor_is_infrastructure_at_tx_order() {
        let p: &'static dyn Pointcut = &TX_MARKER_POINTCUT;
        let row = tx_advisor_pairing::<InMemoryTransactionManager>(p);
        assert_eq!(row.role, Role::Infrastructure, "tx advice is framework infrastructure");
        assert_eq!(row.order.value, TX_ORDER, "the pinned TX_ORDER chain slot (500)");
        assert_eq!(row.order.source, OrderSource::Interface, "framework-declared order");
        assert_eq!(row.contract, tx_advisor_contract());
    }

    #[test]
    fn tx_pointcut_matches_by_concrete_type() {
        // A pointcut over the bean's concrete TypeId matches it (the recursion-safe
        // form — never advises the manager bean itself). The const TypeId-of seam
        // mints a 'static slice exactly as a binding site does.
        static BEAN_TYPES: [TypeId; 1] = [const { TypeId::of::<Bean>() }];
        let pc = TxPointcut::new(&BEAN_TYPES, &[]);
        let empty = AnnotationMetadata::EMPTY;
        assert!(pc.matches(&jp(TypeId::of::<Bean>(), &empty)), "matches the named concrete type");
        assert!(
            !pc.matches(&jp(TypeId::of::<InMemoryTransactionManager>(), &empty)),
            "does NOT match an unrelated bean"
        );
    }

    #[test]
    fn tx_marker_pointcut_matches_a_transactional_marker() {
        static MARKED: AnnotationMetadata = AnnotationMetadata {
            markers: &[MarkerId::of("leaf::tx::Transactional")],
            ..AnnotationMetadata::EMPTY
        };
        let other = AnnotationMetadata::EMPTY;
        let bean_ty = TypeId::of::<Bean>();
        assert!(
            TX_MARKER_POINTCUT.matches(&jp(bean_ty, &MARKED)),
            "matches a bean carrying the tx marker"
        );
        assert!(
            !TX_MARKER_POINTCUT.matches(&jp(bean_ty, &other)),
            "does NOT match an unmarked bean"
        );
    }

    #[test]
    fn tx_marker_pointcut_equals_the_public_marker() {
        assert_eq!(tx_marker(), MarkerId::of("leaf::tx::Transactional"));
    }

    #[test]
    fn order_key_is_the_canonical_tx_slot() {
        assert_eq!(tx_order_key().value, TX_ORDER);
        // Cache MUST sit outside tx (the cache-outside-tx invariant) — pin it here.
        assert!(leaf_core::CACHE_ORDER < tx_order_key().value);
    }

    #[test]
    fn enable_transaction_management_names_the_advisor_identity() {
        assert_eq!(enable_transaction_management(), tx_advisor_contract());
    }
}
