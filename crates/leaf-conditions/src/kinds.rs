//! The condition-family tier-map rows + the `ConditionId` -> `&dyn Condition`
//! registry.
//!
//! conditions-autoconfig (phase3/05) condition-family: each member is (1) a
//! stable [`ConditionId`] const minted through the one
//! [`contract_hash`](leaf_core::contract_hash) over a canonical FQN (reproducible
//! cross-build); (2) a [`ConditionKind`] tier-map row (`TIER`/`SUB`); (3) a
//! hand-written [`Condition`] impl. The fixed tier-map entries:
//!
//! - `OnProperty`/`OnBooleanProperty`/`OnExpression`/`OnResource` = (Runtime, Parse)
//! - `OnProfile` = (Runtime, Parse)  ← also `leaf_core::ON_PROFILE`
//! - `OnBean`/`OnMissingBean`/`OnSingleCandidate` = (Runtime, Register)
//! - `OnRustVersion` = (ConstFold, decided at build → `Const(bool)`)
//!
//! The leaf-core `CONDITIONS` linkme channel is the production lift path; this
//! module ALSO exposes a plain `resolve` fn over the same rows so the catalog is
//! unit-testable in a bare crate (no force-link required), which `evaluate_all`
//! drives.

use leaf_core::{
    contract_hash, Condition, ConditionId, ConditionKind, EarliestTier, SubPhase, ON_PROFILE,
};

use crate::bean::{ON_BEAN, ON_MISSING_BEAN, ON_SINGLE_CANDIDATE};
use crate::expression::ON_EXPRESSION;
use crate::profile::ON_PROFILE_COND;
use crate::property::{ON_BOOLEAN_PROPERTY, ON_PROPERTY};
use crate::resource::ON_RESOURCE;
use crate::rustversion::ON_RUST_VERSION;

/// Mint a stable `ConditionId` for a member FQN (truncated to the dense `u32`
/// space, exactly as leaf-core mints [`ON_PROFILE`]).
#[must_use]
pub const fn mint(fqn: &str) -> ConditionId {
    ConditionId(contract_hash(fqn) as u32)
}

/// Helper for compile-time assertions over a [`ConditionKind`].
pub const fn kind_id<K: ConditionKind>() -> ConditionId {
    K::ID
}

// ───────────────────────────── tier-map rows ────────────────────────────────

/// `@ConditionalOnProperty` — (Runtime, Parse).
pub struct OnProperty;
impl ConditionKind for OnProperty {
    const ID: ConditionId = mint("leaf::condition::OnProperty");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Parse;
}

/// `@ConditionalOnBooleanProperty` — (Runtime, Parse).
pub struct OnBooleanProperty;
impl ConditionKind for OnBooleanProperty {
    const ID: ConditionId = mint("leaf::condition::OnBooleanProperty");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Parse;
}

/// `@ConditionalOnExpression` — (Runtime, Parse).
pub struct OnExpression;
impl ConditionKind for OnExpression {
    const ID: ConditionId = mint("leaf::condition::OnExpression");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Parse;
}

/// `@ConditionalOnResource` — (Runtime, Parse).
pub struct OnResource;
impl ConditionKind for OnResource {
    const ID: ConditionId = mint("leaf::condition::OnResource");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Parse;
}

/// `@Profile` — (Runtime, Parse). Shares leaf-core's fixed [`ON_PROFILE`] id so
/// the macro-emitted `Leaf(ON_PROFILE, ..)` resolves to this catalog impl.
pub struct OnProfile;
impl ConditionKind for OnProfile {
    const ID: ConditionId = ON_PROFILE;
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Parse;
}

/// `@ConditionalOnBean` — (Runtime, Register).
pub struct OnBean;
impl ConditionKind for OnBean {
    const ID: ConditionId = mint("leaf::condition::OnBean");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Register;
}

/// `@ConditionalOnMissingBean` — (Runtime, Register).
pub struct OnMissingBean;
impl ConditionKind for OnMissingBean {
    const ID: ConditionId = mint("leaf::condition::OnMissingBean");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Register;
}

/// `@ConditionalOnSingleCandidate` — (Runtime, Register).
pub struct OnSingleCandidate;
impl ConditionKind for OnSingleCandidate {
    const ID: ConditionId = mint("leaf::condition::OnSingleCandidate");
    const TIER: EarliestTier = EarliestTier::Runtime;
    const SUB: SubPhase = SubPhase::Register;
}

/// `@ConditionalOnRustVersion` — (ConstFold). Decided at build and lowered to
/// `CondExpr::Const(bool)` (so it never reaches the runtime registry), but a
/// runtime impl is registered for the rare un-lowered/forced-runtime case.
pub struct OnRustVersion;
impl ConditionKind for OnRustVersion {
    const ID: ConditionId = mint("leaf::condition::OnRustVersion");
    const TIER: EarliestTier = EarliestTier::ConstFold;
    // No `Register` ordering need; it evaluates in the Parse sub-pass when not
    // already const-folded.
    const SUB: SubPhase = SubPhase::Parse;
}

// ───────────────────────── the catalog resolver ─────────────────────────────

/// The catalog: every `(ConditionId, &'static dyn Condition)` row. The same
/// pairs leaf-boot lifts into the `CONDITIONS` linkme channel; exposed directly
/// so the catalog is unit-testable without force-link.
#[must_use]
pub fn catalog() -> [(ConditionId, &'static dyn Condition); 9] {
    [
        (OnProperty::ID, &ON_PROPERTY),
        (OnBooleanProperty::ID, &ON_BOOLEAN_PROPERTY),
        (OnExpression::ID, &ON_EXPRESSION),
        (OnResource::ID, &ON_RESOURCE),
        (OnProfile::ID, &ON_PROFILE_COND),
        (OnBean::ID, &ON_BEAN),
        (OnMissingBean::ID, &ON_MISSING_BEAN),
        (OnSingleCandidate::ID, &ON_SINGLE_CANDIDATE),
        (OnRustVersion::ID, &ON_RUST_VERSION),
    ]
}

/// Resolve a [`ConditionId`] to its catalog [`Condition`] impl (the in-crate
/// resolver `evaluate_all` passes to `leaf_core::evaluate`).
#[must_use]
pub fn resolve(id: ConditionId) -> Option<&'static dyn Condition> {
    catalog().into_iter().find(|(i, _)| *i == id).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_across_the_catalog() {
        let ids: Vec<ConditionId> = catalog().iter().map(|(i, _)| *i).collect();
        let mut deduped = ids.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(ids.len(), deduped.len(), "no two members share a ConditionId");
    }

    #[test]
    fn resolve_finds_every_catalog_member() {
        for (id, _) in catalog() {
            assert!(resolve(id).is_some(), "{id:?} must resolve");
        }
        assert!(resolve(ConditionId(0xDEAD_BEEF)).is_none());
    }

    #[test]
    fn on_profile_reuses_the_kernel_id() {
        assert_eq!(OnProfile::ID, ON_PROFILE);
    }

    #[test]
    fn bean_family_is_register_phase() {
        assert_eq!(OnBean::SUB, SubPhase::Register);
        assert_eq!(OnMissingBean::SUB, SubPhase::Register);
        assert_eq!(OnSingleCandidate::SUB, SubPhase::Register);
    }

    #[test]
    fn parse_family_is_parse_phase() {
        assert_eq!(OnProperty::SUB, SubPhase::Parse);
        assert_eq!(OnProfile::SUB, SubPhase::Parse);
        assert_eq!(OnResource::SUB, SubPhase::Parse);
    }

    #[test]
    fn rust_version_is_constfold_tier() {
        assert_eq!(OnRustVersion::TIER, EarliestTier::ConstFold);
    }
}
