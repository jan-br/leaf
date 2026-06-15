//! `leaf-conditions` — the concrete `@ConditionalOnX` family over leaf-core's
//! `CondExpr`/`ConditionId`/`ConditionCtx` SPIs.
//!
//! Realizes conditions-autoconfig (phase3/05) `condition-family` + `profiles`:
//! the hand-written runtime [`Condition`](leaf_core::Condition) impls behind
//! stable [`ConditionId`](leaf_core::ConditionId)s, each a
//! [`ConditionKind`](leaf_core::ConditionKind) tier-map row, plus the profile
//! activation algebra + evaluator. The ONE gating algebra (`CondExpr`), the
//! evaluator ([`evaluate`](leaf_core::evaluate)), the report shapes, and the
//! profile grammar are leaf-core's frozen ABI — this crate POPULATES the catalog
//! and refines each leaf to its soundest tier; it mints no second gating spine.
//!
//! ## Members + fixed tier-map entries
//!
//! | member | tier | sub-phase | verdict source |
//! |---|---|---|---|
//! | [`OnProperty`](kinds::OnProperty) / [`OnBooleanProperty`](kinds::OnBooleanProperty) | Runtime | Parse | sealed `Env` |
//! | [`OnExpression`](kinds::OnExpression) | Runtime | Parse | `Env` `${}` / `ctx.expr` `#{}` |
//! | [`OnResource`](kinds::OnResource) | Runtime | Parse | filesystem / `ctx.loader` |
//! | [`OnProfile`](kinds::OnProfile) (`ON_PROFILE`) | Runtime | Parse | `ctx.profiles` |
//! | [`OnBean`](kinds::OnBean) / [`OnMissingBean`](kinds::OnMissingBean) / [`OnSingleCandidate`](kinds::OnSingleCandidate) | Runtime | Register | [`DefinitionProbe`] |
//! | [`OnRustVersion`](kinds::OnRustVersion) | ConstFold | (folded) | toolchain version |
//!
//! ## Tier refinement (lowering each leaf to where it can decide)
//!
//! The const-fold tier is realized by [`rustversion::lower`], which collapses an
//! `OnRustVersion` leaf to [`CondExpr::Const`](leaf_core::CondExpr) at build —
//! "a `ConstFold` leaf arrives as `Const(bool)`". The Parse/Register split is the
//! per-kind [`SubPhase`](leaf_core::SubPhase) const each member declares; leaf-boot's
//! `route_conditions`/`run_autoconfig` read `CondExpr::phase()` to sequence the
//! sub-passes. `OnClass`/`OnCapability` (Cfg tier) are compile-pruned `#[cfg]`
//! leaves emitted by the macro, not runtime impls — out of this crate's scope.
//!
//! ## The ctx fields + the residual leaf-boot probe bridge
//!
//! The [`ConditionCtx`](leaf_core::ConditionCtx) now carries the sealed
//! `&ActiveProfiles` (`ctx.profiles`) plus the optional
//! [`ExpressionEvaluator`](leaf_core::ExpressionEvaluator) (`ctx.expr`) and
//! [`ResourceLoader`](leaf_core::ResourceLoader) (`ctx.loader`) borrows, in
//! addition to the always-present `&Env` + `&dyn ReportSink`. `OnProfile` is
//! fully live off `ctx.profiles` (no ambient thread-local). `OnExpression`'s
//! `#{...}` form and `OnResource`'s `classpath:`/`url:` schemes read `ctx.expr` /
//! `ctx.loader`: the scaffolding + the `None`-degradation are in place, but they
//! only become USEFUL once leaf-boot installs a concrete evaluator / scheme-aware
//! loader (a later ecosystem step).
//!
//! The `OnBean` family still reads the per-assembly probe
//! ([`probe::with_probe`]) leaf-boot installs in a thin ambient scope (its
//! `DefinitionProbe` borrow is the one ctx field not yet grown). No global lock,
//! single-threaded, cold. `OnProfile` no longer needs an ambient scope — it reads
//! the sealed active set straight off [`ConditionCtx::profiles`](leaf_core::ConditionCtx).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod attrs;
pub mod bean;
pub mod expression;
pub mod kinds;
pub mod probe;
pub mod profile;
pub mod profiles;
pub mod property;
pub mod resource;
pub mod rustversion;

#[cfg(test)]
mod test_support;

// ── curated surface ──

pub use kinds::{catalog, resolve, OnBean, OnBooleanProperty, OnExpression, OnMissingBean, OnProfile, OnProperty, OnResource, OnRustVersion, OnSingleCandidate};
pub use probe::{current_probe_query, has_probe, install_probe, with_probe, DefinitionProbe, ProbeScope};
pub use profiles::{accepts_profiles, matches, resolve_active};

// Re-export the leaf-core verdict/profile types so downstream crates have one
// import path for the family (they remain leaf-core's frozen definitions).
pub use leaf_core::{
    ActivationReason, ActiveProfiles, CondExpr, Condition, ConditionCtx, ConditionId, ConditionKind,
    ConditionOutcome, ProfileError, ProfileExpr, ProfileLevers, ProfileParseError, ReasonMsg,
    Resolvability,
};

/// Evaluate a [`CondExpr`] tree against `ctx` using THIS crate's catalog as the
/// `ConditionId` resolver — the bare-crate driver `leaf_core::evaluate` needs.
///
/// leaf-boot's production path resolves through the `CONDITIONS` linkme channel
/// (anti-DCE self-checked); this convenience resolves through [`resolve`] so the
/// catalog is drivable without force-link in tests and embedded uses.
///
/// # Errors
/// Propagates [`leaf_core::evaluate`]'s error: an unresolved [`ConditionId`] is a
/// loud `ConditionError` (never a silent pass-all).
pub fn evaluate_all(
    expr: &CondExpr,
    ctx: &ConditionCtx<'_>,
) -> Result<ConditionOutcome, leaf_core::LeafError> {
    leaf_core::evaluate(expr, ctx, &|id| resolve(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;
    use std::any::TypeId;
    use std::sync::Arc;

    // A composite tree exercised end-to-end through `evaluate_all`:
    //   all( on_property("feature.x"), any( on_missing_bean(T), on_profile("prod") ) )
    fn tree() -> CondExpr {
        // Leaks are fine: const-shaped data living for the test's duration.
        let on_prop: &'static [Attr] = Box::leak(Box::new([Attr::Str("name", "feature.x")]));
        let on_missing: &'static [Attr] =
            Box::leak(Box::new([Attr::Type("type", TypeId::of::<Marker>())]));
        let on_profile: &'static [Attr] = Box::leak(Box::new([Attr::Str("expr", "prod")]));

        let children: &'static [CondExpr] = Box::leak(Box::new([
            CondExpr::Leaf(OnProperty::ID, on_prop),
            CondExpr::Any(Box::leak(Box::new([
                CondExpr::Leaf(OnMissingBean::ID, on_missing),
                CondExpr::Leaf(OnProfile::ID, on_profile),
            ]))),
        ]));
        CondExpr::All(children)
    }

    struct Marker;

    struct EmptyProbe;
    impl DefinitionProbe for EmptyProbe {
        fn would_resolve_unique(&self, _ty: TypeId) -> Resolvability {
            Resolvability::None
        }
    }

    #[test]
    fn composite_tree_evaluates_through_the_catalog() {
        let env = env_with(&[("feature.x", "true")]);
        let (ctx, _s) = ctx_over(&env);
        let t = tree();
        // feature.x=true AND (missing_bean(empty)=true OR ...) => true
        let probe: Arc<dyn DefinitionProbe> = Arc::new(EmptyProbe);
        let out = with_probe(probe, || evaluate_all(&t, &ctx).unwrap());
        assert!(out.matched);
    }

    #[test]
    fn composite_backs_off_when_property_absent() {
        let env = env_with(&[]); // feature.x missing
        let (ctx, _s) = ctx_over(&env);
        let t = tree();
        let probe: Arc<dyn DefinitionProbe> = Arc::new(EmptyProbe);
        let out = with_probe(probe, || evaluate_all(&t, &ctx).unwrap());
        assert!(!out.matched, "the All fails on the missing property leaf");
        assert_eq!(out.reason.kind, "OnProperty");
    }

    #[test]
    fn tree_phase_is_register_when_an_onbean_leaf_is_present() {
        // The macro lowers OnBean-family leaves so the tree's phase is Register.
        // Here we assert the structural rule the design relies on: a Register-SUB
        // member nested anywhere defers the whole guard. We model that by an
        // explicit Register marker via the kind consts.
        assert_eq!(OnMissingBean::SUB, leaf_core::SubPhase::Register);
        assert_eq!(OnProperty::SUB, leaf_core::SubPhase::Parse);
    }
}
