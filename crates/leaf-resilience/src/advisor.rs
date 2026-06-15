//! The TWO Infrastructure resilience [`AdvisorPairingRow`]s that auto-wire
//! (retry/resilience, phase3/09): the RETRY advisor (`order = RETRY_ORDER = 200`,
//! outside tx) and the CONCURRENCY-LIMIT advisor (`order = CONCURRENCY_ORDER = 550`,
//! inside tx), both `Role::Infrastructure`.
//!
//! Two faces, one shape (the leaf-tx pattern):
//!
//! - the const auto-wire rows submitted into
//!   [`ADVISOR_PAIRINGS`](leaf_core::ADVISOR_PAIRINGS) so a binary that links a
//!   resilience row gets the advisor in the run pipeline's proxy plan with NO
//!   hand-assembled `.with_advisors`;
//! - the programmatic [`retry_advisor_pairing`] / [`concurrency_advisor_pairing`]
//!   builders a binding site / integration crate uses.
//!
//! The pointcut is [`ResiliencePointcut`] — leaf-resilience's own const-constructible
//! predicate (by concrete `TypeId` or a resilience [`MarkerId`]), since the kernel
//! `within`/`annotated_marker` combinators are not const-constructible into a
//! `&'static` row. (When the `#[retryable]`/`#[concurrency_limit]` macros land, the
//! auto-wire pointcut keys on the emitted markers; until then it is a resilience-owned
//! marker — a NOTE in the crate docs.)
//!
//! ## Mandatory two-advisor self-check
//!
//! [`enable_resilient_methods`] force-links leaf-resilience and returns BOTH advisor
//! identities ([`retry_advisor_contract`] + [`concurrency_advisor_contract`]) so a
//! binary's expected-vs-found manifest asserts BOTH are present — a DCE'd resilience
//! crate is a loud `AntiDceError`, never a silently-disabled retry.

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    AdvisorPairingRow, BackoffPolicy, BoxFuture, ConcurrencyGate, Container, ContractId,
    Interceptor, JoinPointMeta, MakeInterceptor, MarkerId, OrderKey, OrderSource, Pointcut,
    ResolveError, RetryPolicy, Role, CONCURRENCY_ORDER, RETRY_ORDER,
};

use crate::concurrency::ConcurrencyLimitInterceptor;
use crate::retry::RetryInterceptor;
use crate::template::ResilientRetry;

/// The stable identity of the built-in (auto-wired) retry advisor.
#[must_use]
pub fn retry_advisor_contract() -> ContractId {
    ContractId::of("leaf::resilience::RetryAdvisor")
}

/// The stable identity of the built-in (auto-wired) concurrency-limit advisor.
#[must_use]
pub fn concurrency_advisor_contract() -> ContractId {
    ContractId::of("leaf::resilience::ConcurrencyLimitAdvisor")
}

/// The chain order of the retry advisor: the pinned `RETRY_ORDER = 200` with an
/// `Interface` source (a framework-declared order — OUTSIDE tx, INSIDE validation).
#[must_use]
pub fn retry_order_key() -> OrderKey {
    OrderKey { value: RETRY_ORDER, source: OrderSource::Interface }
}

/// The chain order of the concurrency-limit advisor: the pinned
/// `CONCURRENCY_ORDER = 550` with an `Interface` source (INSIDE tx).
#[must_use]
pub fn concurrency_order_key() -> OrderKey {
    OrderKey { value: CONCURRENCY_ORDER, source: OrderSource::Interface }
}

/// The marker the auto-wire retry advisor keys on (the marker a future
/// `#[retryable]` macro emits onto the advised bean's `AnnotationMetadata`).
#[must_use]
pub fn retryable_marker() -> MarkerId {
    MarkerId::of("leaf::resilience::Retryable")
}

/// The marker the auto-wire concurrency-limit advisor keys on (the marker a future
/// `#[concurrency_limit]` macro emits).
#[must_use]
pub fn concurrency_limit_marker() -> MarkerId {
    MarkerId::of("leaf::resilience::ConcurrencyLimit")
}

// ──────────────────────────────── ResiliencePointcut ────────────────────────

/// leaf-resilience's const-constructible pointcut: matches a join point whose bean
/// is one of the named concrete `TypeId`s OR carries one of the named resilience
/// [`MarkerId`]s.
///
/// `&'static ResiliencePointcut` is usable as a `&'static dyn Pointcut` on a const
/// [`AdvisorPairingRow`] (the kernel combinators are not const-constructible into a
/// `&'static` row). `TypeId::of::<T>()` is callable in an inline `const {}` block,
/// so a binding site writes:
///
/// ```ignore
/// static P: ResiliencePointcut =
///     ResiliencePointcut::new(&[const { TypeId::of::<MyBean>() }], &[]);
/// ```
pub struct ResiliencePointcut {
    types: &'static [TypeId],
    markers: &'static [MarkerId],
}

impl ResiliencePointcut {
    /// A pointcut matching beans whose concrete type is in `types` OR that carry a
    /// marker in `markers`.
    #[must_use]
    pub const fn new(types: &'static [TypeId], markers: &'static [MarkerId]) -> Self {
        ResiliencePointcut { types, markers }
    }

    /// The concrete `TypeId`s this pointcut matches by exact type.
    #[must_use]
    pub fn types(&self) -> &'static [TypeId] {
        self.types
    }

    /// The resilience markers this pointcut matches by `AnnotationMetadata` presence.
    #[must_use]
    pub fn markers(&self) -> &'static [MarkerId] {
        self.markers
    }
}

impl Pointcut for ResiliencePointcut {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        if self.types.contains(&jp.bean_type) {
            return true;
        }
        self.markers
            .iter()
            .any(|m| jp.markers.markers.contains(m) || jp.markers.qualifiers.contains(m))
    }
}

impl std::fmt::Debug for ResiliencePointcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResiliencePointcut")
            .field("types", &self.types.len())
            .field("markers", &self.markers.len())
            .finish()
    }
}

/// The auto-wire default retry pointcut: matches the [`retryable_marker`] on a bean.
pub static RETRYABLE_MARKER_POINTCUT: ResiliencePointcut =
    ResiliencePointcut::new(&[], &[MarkerId::of("leaf::resilience::Retryable")]);

/// The auto-wire default concurrency-limit pointcut: matches the
/// [`concurrency_limit_marker`] on a bean.
pub static CONCURRENCY_LIMIT_MARKER_POINTCUT: ResiliencePointcut =
    ResiliencePointcut::new(&[], &[MarkerId::of("leaf::resilience::ConcurrencyLimit")]);

// ──────────────────────────── make_interceptor builders ─────────────────────

/// A const-supplied retry specification: the per-advisor [`RetryPolicy`] +
/// [`BackoffPolicy`] (the data a `#[retryable(max=…, backoff=…)]` macro would emit).
///
/// Implemented by a zero-sized marker type so [`make_retry_interceptor`] can bake
/// the spec into the bare-fn-pointer [`MakeInterceptor`] WITHOUT a captured env —
/// the same monomorphize-a-ZST trick the tx advisor uses to carry its concrete
/// manager type. A binding site (or the macro) declares a unit struct `impl RetrySpec`.
pub trait RetrySpec: Send + Sync + 'static {
    /// The retry policy (max attempts + retryability predicate).
    fn policy() -> RetryPolicy;
    /// The backoff policy (fixed / exponential / none).
    fn backoff() -> Arc<dyn BackoffPolicy>;
}

/// Build a [`MakeInterceptor`] for the RETRY advisor baking in the const
/// [`RetrySpec`] `S`. The interceptor uses the runtime-free immediate sleeper (zero
/// real sleeps); a binding site that needs real timed backoff hands a runtime
/// sleeper by writing its own closure-literal row instead.
///
/// The monomorphized fn-item coerces to the bare [`MakeInterceptor`] fn-pointer,
/// baking the per-advisor policy/backoff in via `S` (no captured env).
#[must_use]
pub fn make_retry_interceptor<S: RetrySpec>() -> MakeInterceptor {
    |_container: &dyn Container| {
        Box::pin(async move {
            let interceptor = RetryInterceptor::new(ResilientRetry::new(S::policy(), S::backoff()));
            Ok(Arc::new(interceptor) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, ResolveError>>
    }
}

/// Build a [`MakeInterceptor`] for the CONCURRENCY-LIMIT advisor that resolves the
/// CONCRETE gate bean `G` by its `TypeId` through the container, upcasts it to
/// `Arc<dyn ConcurrencyGate>`, and wraps it in a [`ConcurrencyLimitInterceptor`].
///
/// `G` is the concrete gate bean type (e.g. a small-limit `ExecutionFacility` bean
/// sized from `#[concurrency_limit(n)]`); it is resolved by
/// `BeanKey::ByType(TypeId::of::<G>())` and downcast to `Arc<G>` — the same
/// resolve-and-upcast bean bridge the tx advisor uses for its manager.
#[must_use]
pub fn make_concurrency_interceptor<G>() -> MakeInterceptor
where
    G: ConcurrencyGate + 'static,
{
    |container: &dyn Container| {
        Box::pin(async move {
            let published = container
                .resolve(
                    leaf_core::BeanKey::ByType(TypeId::of::<G>()),
                    leaf_core::Strictness::Strict,
                    leaf_core::Cardinality::Single,
                )
                .await?;
            let erased = published.into_shared().ok_or_else(gate_mismatch)?;
            let gate: Arc<G> = erased.downcast::<G>().map_err(|_| gate_mismatch())?;
            let gate: Arc<dyn ConcurrencyGate> = gate;
            Ok(Arc::new(ConcurrencyLimitInterceptor::new(gate)) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, ResolveError>>
    }
}

fn gate_mismatch() -> ResolveError {
    leaf_core::LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(
        leaf_core::Cause::plain(
            "concurrency advisor make_interceptor",
            "the resolved concurrency-gate bean was not the expected concrete type",
        ),
    )
}

// ────────────────────────────── pairing builders ────────────────────────────

/// Build an [`AdvisorPairingRow`] for the RETRY advisor matching `pointcut`, baking
/// in the const [`RetrySpec`] `S`. `Role::Infrastructure` + `RETRY_ORDER` (outside
/// tx, inside validation).
#[must_use]
pub fn retry_advisor_pairing<S: RetrySpec>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow {
    AdvisorPairingRow {
        contract: retry_advisor_contract(),
        order: retry_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: make_retry_interceptor::<S>(),
    }
}

/// Build an [`AdvisorPairingRow`] for the CONCURRENCY-LIMIT advisor binding the
/// concrete gate `G` and matching `pointcut`. `Role::Infrastructure` +
/// `CONCURRENCY_ORDER` (inside tx).
#[must_use]
pub fn concurrency_advisor_pairing<G>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
where
    G: ConcurrencyGate + 'static,
{
    AdvisorPairingRow {
        contract: concurrency_advisor_contract(),
        order: concurrency_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: make_concurrency_interceptor::<G>(),
    }
}

/// Force-link leaf-resilience so BOTH resilience advisors participate (the
/// `enable_resilient_methods!()` analogue, ADR-09 anti-DCE force-link).
///
/// Returns BOTH advisor identities ([`retry_advisor_contract`],
/// [`concurrency_advisor_contract`]) — the mandatory two-advisor self-check: a
/// binary adds BOTH to its expected-vs-found manifest, so a DCE'd resilience crate
/// is a loud `AntiDceError`, never a silently-disabled retry/limit.
#[must_use]
pub fn enable_resilient_methods() -> [ContractId; 2] {
    [retry_advisor_contract(), concurrency_advisor_contract()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{
        AnnotationMetadata, MethodKey, CONCURRENCY_ORDER, RETRY_ORDER, TX_ORDER, VALIDATE_ORDER,
    };

    struct Bean;

    // A local gate type so the pairing builder has a concrete `G` to bind (the
    // gate is resolved from the container at run time; the row only needs the type).
    struct DummyGate;
    impl ConcurrencyGate for DummyGate {
        fn acquire(&self) -> BoxFuture<'static, leaf_core::Permit> {
            Box::pin(async { leaf_core::Permit::unbounded() })
        }
    }

    fn jp<'a>(bean_type: TypeId, markers: &'a AnnotationMetadata) -> JoinPointMeta<'a> {
        JoinPointMeta {
            bean_type,
            method: MethodKey::of("Bean::m"),
            markers,
            arg_types: &[],
            ret_type: TypeId::of::<()>(),
        }
    }

    struct ThreeTries;
    impl RetrySpec for ThreeTries {
        fn policy() -> RetryPolicy {
            RetryPolicy::new(3)
        }
        fn backoff() -> Arc<dyn BackoffPolicy> {
            Arc::new(crate::backoff::NoBackoff)
        }
    }

    #[test]
    fn retry_advisor_is_infrastructure_at_retry_order() {
        let p: &'static dyn Pointcut = &RETRYABLE_MARKER_POINTCUT;
        let row = retry_advisor_pairing::<ThreeTries>(p);
        assert_eq!(row.role, Role::Infrastructure, "retry is framework infrastructure");
        assert_eq!(row.order.value, RETRY_ORDER, "the pinned RETRY_ORDER (200)");
        assert_eq!(row.order.source, OrderSource::Interface);
        assert_eq!(row.contract, retry_advisor_contract());
    }

    #[test]
    fn concurrency_advisor_is_infrastructure_at_concurrency_order() {
        let p: &'static dyn Pointcut = &CONCURRENCY_LIMIT_MARKER_POINTCUT;
        let row = concurrency_advisor_pairing::<DummyGate>(p);
        assert_eq!(row.role, Role::Infrastructure);
        assert_eq!(row.order.value, CONCURRENCY_ORDER, "the pinned CONCURRENCY_ORDER (550)");
        assert_eq!(row.contract, concurrency_advisor_contract());
    }

    #[test]
    fn the_canonical_chain_order_holds() {
        // VALIDATE(100) < RETRY(200) < TX(500) < CONCURRENCY(550): retry is OUTSIDE
        // tx (each attempt a fresh tx) and concurrency-limit is INSIDE tx. Read the
        // orders through the advisor's order-key fns (the runtime chain-sort input).
        let retry = retry_order_key().value;
        let concurrency = concurrency_order_key().value;
        assert!(VALIDATE_ORDER < retry, "retry inside validation");
        assert!(retry < TX_ORDER, "retry OUTSIDE tx (each attempt its own tx)");
        assert!(TX_ORDER < concurrency, "concurrency-limit INSIDE tx");
        assert_eq!(retry, RETRY_ORDER);
        assert_eq!(concurrency, CONCURRENCY_ORDER);
    }

    #[test]
    fn retry_pointcut_matches_by_marker() {
        static MARKED: AnnotationMetadata = AnnotationMetadata {
            markers: &[MarkerId::of("leaf::resilience::Retryable")],
            ..AnnotationMetadata::EMPTY
        };
        let empty = AnnotationMetadata::EMPTY;
        let ty = TypeId::of::<Bean>();
        assert!(RETRYABLE_MARKER_POINTCUT.matches(&jp(ty, &MARKED)), "matches a #[retryable] bean");
        assert!(!RETRYABLE_MARKER_POINTCUT.matches(&jp(ty, &empty)), "ignores an unmarked bean");
    }

    #[test]
    fn pointcut_matches_by_concrete_type() {
        static TYPES: [TypeId; 1] = [const { TypeId::of::<Bean>() }];
        let pc = ResiliencePointcut::new(&TYPES, &[]);
        let empty = AnnotationMetadata::EMPTY;
        assert!(pc.matches(&jp(TypeId::of::<Bean>(), &empty)));
        assert!(!pc.matches(&jp(TypeId::of::<u8>(), &empty)));
    }

    #[test]
    fn enable_resilient_methods_names_both_advisors() {
        let ids = enable_resilient_methods();
        assert_eq!(ids[0], retry_advisor_contract(), "the retry advisor identity");
        assert_eq!(ids[1], concurrency_advisor_contract(), "the concurrency-limit advisor identity");
        assert_ne!(ids[0], ids[1], "two DISTINCT advisor identities");
    }

    #[test]
    fn markers_are_the_public_markers() {
        assert_eq!(retryable_marker(), MarkerId::of("leaf::resilience::Retryable"));
        assert_eq!(concurrency_limit_marker(), MarkerId::of("leaf::resilience::ConcurrencyLimit"));
    }
}
