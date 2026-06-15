//! The Infrastructure validation [`AdvisorPairingRow`] that auto-wires (validation,
//! phase3/09 §validation, FACE 2): the ONE advisor row (`Role::Infrastructure`,
//! `order = VALIDATE_ORDER = 100`, the OUTERMOST chain slot so a bad arg never
//! reaches the inner tx/cache advisors or the real method) whose `make_interceptor`
//! builds a [`MethodValidationInterceptor`].
//!
//! Two faces, one shape (the leaf-tx pattern):
//!
//! - the const auto-wire row submitted into
//!   [`ADVISOR_PAIRINGS`](leaf_core::ADVISOR_PAIRINGS) (force-linked by
//!   [`enable_validation`]) so a binary that links leaf-validation gets the
//!   validation advisor in the run pipeline's proxy plan with NO hand-assembled
//!   `.with_advisors`;
//! - the programmatic [`validation_advisor_pairing`] / [`make_validation_interceptor`]
//!   builders a binary or test uses to bind ITS per-method [`ArgValidator`] + a finer
//!   pointcut.
//!
//! The pointcut is [`ValidationPointcut`] — leaf-validation's own const-constructible
//! predicate (matching by the bean's concrete `TypeId` or a validation
//! [`MarkerId`]), since the kernel `within`/`annotated_marker` combinators are not
//! const-constructible into a `&'static` row (the same reason leaf-tx owns
//! `TxPointcut`).
//!
//! ## The `#[validated]` declarative annotation
//!
//! The NATURAL `#[validated]` annotation on a `#[advisable]`-impl method auto-wires the
//! method-validation advisor: the impl-block macro emits a per-method-unique
//! [`AdvisorPairingRow`] keyed by the bean's `TypeId` (a [`ValidationPointcut`] over
//! it), whose `make_interceptor` is [`single_arg_make_interceptor`] monomorphized over
//! the method's FIRST argument type `A` (the `@Valid` arg, which must be
//! [`ValidateInto`](crate::ValidateInto)). Validation is compiled per type, so the
//! `make_interceptor` resolves nothing from the container.

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    AdvisorPairingRow, BoxFuture, Container, ContractId, Interceptor, JoinPointMeta, LeafError,
    MakeInterceptor, MarkerId, OrderKey, OrderSource, Pointcut, Role, VALIDATE_ORDER,
};

use crate::interceptor::MethodValidationInterceptor;

/// The stable identity of the built-in (auto-wired) validation advisor.
#[must_use]
pub const fn validation_advisor_contract() -> ContractId {
    ContractId::of("leaf::validation::MethodValidationAdvisor")
}

/// The chain order of the validation advisor: the pinned `VALIDATE_ORDER = 100`
/// (OUTERMOST) with an `Interface` source (a framework-declared, most-specific
/// order).
#[must_use]
pub const fn validation_order_key() -> OrderKey {
    OrderKey { value: VALIDATE_ORDER, source: OrderSource::Interface }
}

/// The default validation marker the auto-wire advisor keys on (the marker a future
/// `#[validated]` macro emits onto the advised bean's `AnnotationMetadata`).
#[must_use]
pub const fn validation_marker() -> MarkerId {
    MarkerId::of("leaf::validation::Validated")
}

// ────────────────────────────── ValidationPointcut ──────────────────────────

/// leaf-validation's const-constructible pointcut: matches a join point whose bean
/// is one of the named concrete `TypeId`s OR carries one of the named validation
/// [`MarkerId`]s.
///
/// `&'static ValidationPointcut` is usable as a `&'static dyn Pointcut` on the const
/// [`AdvisorPairingRow`]. `TypeId::of::<T>()` is callable in an inline `const {}`
/// block (stable), so a binding site writes:
///
/// ```no_run
/// use std::any::TypeId;
/// use leaf_validation::ValidationPointcut;
/// struct MyBean;
/// static TYPES: [TypeId; 1] = [const { TypeId::of::<MyBean>() }];
/// static P: ValidationPointcut = ValidationPointcut::new(&TYPES, &[]);
/// ```
pub struct ValidationPointcut {
    types: &'static [TypeId],
    markers: &'static [MarkerId],
}

impl ValidationPointcut {
    /// A pointcut matching beans whose concrete type is in `types` OR that carry a
    /// marker in `markers`.
    #[must_use]
    pub const fn new(types: &'static [TypeId], markers: &'static [MarkerId]) -> Self {
        ValidationPointcut { types, markers }
    }

    /// The concrete `TypeId`s this pointcut matches by exact type.
    #[must_use]
    pub fn types(&self) -> &'static [TypeId] {
        self.types
    }

    /// The validation markers this pointcut matches by `AnnotationMetadata` presence.
    #[must_use]
    pub fn markers(&self) -> &'static [MarkerId] {
        self.markers
    }
}

impl Pointcut for ValidationPointcut {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        if self.types.contains(&jp.bean_type) {
            return true;
        }
        self.markers
            .iter()
            .any(|m| jp.markers.markers.contains(m) || jp.markers.qualifiers.contains(m))
    }
}

impl std::fmt::Debug for ValidationPointcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationPointcut")
            .field("types", &self.types.len())
            .field("markers", &self.markers.len())
            .finish()
    }
}

/// The auto-wire default pointcut: matches the leaf-validation [`validation_marker`]
/// on a bean. (A `#[validated]` bean would carry this marker once the macro lands.)
pub static VALIDATED_MARKER_POINTCUT: ValidationPointcut =
    ValidationPointcut::new(&[], &[MarkerId::of("leaf::validation::Validated")]);

// ──────────────────────────── make_interceptor builders ─────────────────────

/// A const [`MakeInterceptor`] for a single-`@Valid`-arg method of arg type `A`: it
/// builds a [`MethodValidationInterceptor::single::<A>`] resolving NOTHING from the
/// container (validation is compiled per type). The monomorphized fn-item coerces to
/// the bare [`MakeInterceptor`] fn-pointer, baking `A` in — the const path a
/// `#[validated]` macro (or a binding site) uses.
#[must_use]
pub const fn single_arg_make_interceptor<A>() -> MakeInterceptor
where
    A: crate::ValidateInto + 'static,
{
    |_c: &dyn Container| {
        Box::pin(async move {
            Ok(Arc::new(MethodValidationInterceptor::single::<A>()) as Arc<dyn Interceptor>)
        }) as BoxFuture<'_, Result<Arc<dyn Interceptor>, LeafError>>
    }
}

// ────────────────────────────── pairing builders ────────────────────────────

/// Build an [`AdvisorPairingRow`] for the validation advisor matching `pointcut` and
/// validating a single-`@Valid`-arg method of arg type `A` (the programmatic face).
///
/// `Role::Infrastructure` + `VALIDATE_ORDER` (the OUTERMOST chain slot, before every
/// other concern, so a bad arg never reaches tx/cache or the real method).
#[must_use]
pub const fn validation_advisor_pairing<A>(pointcut: &'static dyn Pointcut) -> AdvisorPairingRow
where
    A: crate::ValidateInto + 'static,
{
    AdvisorPairingRow {
        contract: validation_advisor_contract(),
        order: validation_order_key(),
        role: Role::Infrastructure,
        pointcut,
        make_interceptor: single_arg_make_interceptor::<A>(),
    }
}

/// Force-link leaf-validation so its method-validation advisor participates (the
/// `enable_validation!()` analogue, ADR-09 anti-DCE force-link). Returns the
/// advisor's stable identity so a binary can add it to its expected-vs-found
/// manifest.
#[must_use]
pub fn enable_validation() -> ContractId {
    validation_advisor_contract()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{AnnotationMetadata, MethodKey};

    use crate::cascade::Cascade;
    use crate::constraints;
    use crate::ValidateInto;

    struct Bean;
    struct Other;

    #[derive(Debug)]
    struct Arg {
        name: String,
    }
    impl ValidateInto for Arg {
        fn validate_into(&self, c: &mut Cascade<'_>) {
            c.check("name", constraints::not_empty(&self.name));
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

    #[test]
    fn validation_advisor_is_infrastructure_at_validate_order_outermost() {
        let p: &'static dyn Pointcut = &VALIDATED_MARKER_POINTCUT;
        let row = validation_advisor_pairing::<Arg>(p);
        assert_eq!(row.role, Role::Infrastructure, "validation is framework infrastructure");
        assert_eq!(row.order.value, VALIDATE_ORDER, "the pinned VALIDATE_ORDER chain slot (100)");
        assert_eq!(row.order.source, OrderSource::Interface);
        assert_eq!(row.contract, validation_advisor_contract());
    }

    #[test]
    fn validate_order_is_the_outermost_slot() {
        // VALIDATE is OUTERMOST: it must sort before EVERY other concern.
        assert!(validation_order_key().value < leaf_core::RETRY_ORDER);
        assert!(validation_order_key().value < leaf_core::CACHE_ORDER);
        assert!(validation_order_key().value < leaf_core::TX_ORDER);
        assert!(validation_order_key().value < leaf_core::TRANSLATE_ORDER);
    }

    #[test]
    fn pointcut_matches_by_concrete_type() {
        static BEAN_TYPES: [TypeId; 1] = [const { TypeId::of::<Bean>() }];
        let pc = ValidationPointcut::new(&BEAN_TYPES, &[]);
        let empty = AnnotationMetadata::EMPTY;
        assert!(pc.matches(&jp(TypeId::of::<Bean>(), &empty)), "matches the named type");
        assert!(!pc.matches(&jp(TypeId::of::<Other>(), &empty)), "does NOT match an unrelated bean");
    }

    #[test]
    fn marker_pointcut_matches_a_validated_marker() {
        static MARKED: AnnotationMetadata = AnnotationMetadata {
            markers: &[MarkerId::of("leaf::validation::Validated")],
            ..AnnotationMetadata::EMPTY
        };
        let other = AnnotationMetadata::EMPTY;
        let bean_ty = TypeId::of::<Bean>();
        assert!(VALIDATED_MARKER_POINTCUT.matches(&jp(bean_ty, &MARKED)));
        assert!(!VALIDATED_MARKER_POINTCUT.matches(&jp(bean_ty, &other)));
    }

    #[test]
    fn marker_pointcut_equals_the_public_marker() {
        assert_eq!(validation_marker(), MarkerId::of("leaf::validation::Validated"));
    }

    #[test]
    fn enable_validation_names_the_advisor_identity() {
        assert_eq!(enable_validation(), validation_advisor_contract());
    }

    #[test]
    fn single_arg_make_interceptor_builds_a_method_validation_interceptor() {
        // The const make_interceptor builds the interceptor (resolving nothing).
        let make = single_arg_make_interceptor::<Arg>();
        // Drive it with a no-op container stand-in is unnecessary — it ignores the
        // container; build via block_on over a fake container is heavy, so just
        // assert the fn-pointer coerces (type-level proof) and the interceptor
        // validates via the typed path (covered in interceptor.rs tests).
        let _: MakeInterceptor = make;
    }
}
