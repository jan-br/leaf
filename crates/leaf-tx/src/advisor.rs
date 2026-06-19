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
//! (matching by the bean's concrete `TypeId`), since the kernel `within` combinator
//! is not const-constructible into a `&'static` row.
//!
//! ## The `#[transactional]` declarative annotation
//!
//! The NATURAL `#[transactional(manager = Mgr)]` annotation on a `#[advisable]`-impl
//! method auto-wires the tx advisor: the impl-block macro emits a per-method-unique
//! [`AdvisorPairingRow`] keyed by the bean's `TypeId` (a [`TxPointcut`] over it),
//! binding the named manager `M` + the method's `Result<T,_>` return classifier via
//! [`make_transaction_interceptor_for`] (all the const builders here are `const fn` so
//! the emitted row is a `static` initializer). The auto-wire row applies
//! [`TxAttribute::DEFAULT`] (propagation `Required`, any-`Err` rolls back); a finer
//! per-method [`TxAttribute`] is supplied programmatically via
//! [`TransactionInterceptor::new`].

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    view_from_holder, AdvisorPairingRow, BeanKey, BoxFuture, Cardinality, Container, ContractId,
    Interceptor, JoinPointMeta, LeafError, MakeInterceptor, OrderKey, OrderSource, Pointcut, Role,
    Strictness, TransactionManager, TxAttribute, TX_ORDER,
};

use crate::interceptor::{result_classifier, TransactionInterceptor};

/// The stable identity of the built-in (auto-wired) tx advisor.
#[must_use]
pub const fn tx_advisor_contract() -> ContractId {
    ContractId::of("leaf::tx::TransactionAdvisor")
}

/// The chain order of the tx advisor: the pinned `TX_ORDER = 500` with an
/// `Interface` source (a framework-declared, most-specific order).
#[must_use]
pub const fn tx_order_key() -> OrderKey {
    OrderKey { value: TX_ORDER, source: OrderSource::Interface }
}

// ───────────────────────────────── TxPointcut ───────────────────────────────

/// leaf-tx's const-constructible pointcut: matches a join point whose bean is one
/// of the named concrete `TypeId`s.
///
/// `&'static TxPointcut` is usable as a `&'static dyn Pointcut` on the const
/// [`AdvisorPairingRow`] — the kernel `within` combinator is not const-constructible
/// into a `&'static` row (its fields are private), so leaf-tx owns this one.
/// `TypeId::of::<T>()` is callable in an inline `const {}` block (stable), so a
/// binding site writes:
///
/// ```no_run
/// use std::any::TypeId;
/// use leaf_tx::TxPointcut;
/// struct MyBean;
/// static TYPES: [TypeId; 1] = [const { TypeId::of::<MyBean>() }];
/// static P: TxPointcut = TxPointcut::new(&TYPES);
/// ```
pub struct TxPointcut {
    types: &'static [TypeId],
}

impl TxPointcut {
    /// A pointcut matching beans whose concrete type is in `types`.
    #[must_use]
    pub const fn new(types: &'static [TypeId]) -> Self {
        TxPointcut { types }
    }

    /// The concrete `TypeId`s this pointcut matches by exact type.
    #[must_use]
    pub fn types(&self) -> &'static [TypeId] {
        self.types
    }
}

impl Pointcut for TxPointcut {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        self.types.contains(&jp.bean_type)
    }
}

impl std::fmt::Debug for TxPointcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxPointcut").field("types", &self.types.len()).finish()
    }
}

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
pub const fn make_transaction_interceptor<M>() -> MakeInterceptor
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
/// [`result_classifier`] for a method
/// returning `Result<T, LeafError>`, so a business `Result::Err` (returned through
/// the chain's `Ok(ErasedRet)`) also drives the commit-vs-rollback decision.
///
/// `M` is the concrete manager bean type; `R` is the advised method's WHOLE return type
/// (a `Result<T, LeafError>`, bounded by the sealed [`ReturnShape`](leaf_core::ReturnShape)
/// — so the codegen never name-peels `Result` and a `Result` alias classifies
/// identically). The monomorphized fn-item coerces to the bare [`MakeInterceptor`]
/// fn-pointer, baking the per-`R` classifier in.
#[must_use]
pub const fn make_transaction_interceptor_for<M, R>() -> MakeInterceptor
where
    M: TransactionManager + 'static,
    R: leaf_core::ReturnShape,
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
                .with_return_classifier(result_classifier::<R>());
            Ok(Arc::new(interceptor) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, LeafError>>
    }
}

/// The by-VIEW counterpart of [`make_transaction_interceptor_for`]: resolve the
/// manager through the GENERAL by-trait injection path — the same
/// [`Container`]`::resolve_view` primitive a `Ref<dyn TransactionManager>` injection
/// point drives — rather than by a concrete `TypeId` + downcast.
///
/// `#[transactional(manager = dyn TransactionManager)]` emits this (the macro
/// dispatches on the parameter's SYNTACTIC SHAPE, never a spelled name), so the app
/// names the VIEW and whatever bean provides `dyn TransactionManager` (the
/// auto-configured in-memory default, an integration crate's manager, …) backs it —
/// no concrete pin, no wrapper. `R` is the advised method's WHOLE return type (the
/// per-`R` `Result` classifier rides exactly as in the concrete builder).
#[must_use]
pub const fn make_transaction_interceptor_for_view<R>() -> MakeInterceptor
where
    R: leaf_core::ReturnShape,
{
    |container: &dyn Container| {
        Box::pin(async move {
            let holder =
                container.resolve_view(TypeId::of::<dyn TransactionManager>()).await?;
            let manager: Arc<dyn TransactionManager> =
                view_from_holder::<dyn TransactionManager>(holder)?.into_arc();
            let interceptor = TransactionInterceptor::new(manager, TxAttribute::DEFAULT)
                .with_return_classifier(result_classifier::<R>());
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
pub const fn tx_advisor_pairing<M>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
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
/// [`result_classifier`] (via
/// [`make_transaction_interceptor_for`]) so a method returning `Result<T, LeafError>`
/// rolls back on a business `Result::Err` too. `R` is the advised method's WHOLE return
/// type (bounded [`ReturnShape`](leaf_core::ReturnShape)).
#[must_use]
pub const fn tx_advisor_pairing_for<M, R>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
where
    M: TransactionManager + 'static,
    R: leaf_core::ReturnShape,
{
    AdvisorPairingRow {
        contract: tx_advisor_contract(),
        order: tx_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: make_transaction_interceptor_for::<M, R>(),
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

    static BEAN_TYPES: [TypeId; 1] = [const { TypeId::of::<Bean>() }];
    static BEAN_POINTCUT: TxPointcut = TxPointcut::new(&BEAN_TYPES);

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
        let p: &'static dyn Pointcut = &BEAN_POINTCUT;
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
        let pc = TxPointcut::new(&BEAN_TYPES);
        let empty = AnnotationMetadata::EMPTY;
        assert!(pc.matches(&jp(TypeId::of::<Bean>(), &empty)), "matches the named concrete type");
        assert!(
            !pc.matches(&jp(TypeId::of::<InMemoryTransactionManager>(), &empty)),
            "does NOT match an unrelated bean"
        );
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
