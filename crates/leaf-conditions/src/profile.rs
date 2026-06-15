//! `OnProfile` — the `ON_PROFILE` preset condition (Runtime, Parse).
//!
//! conditions-autoconfig (phase3/05) profiles: `#[profile("prod & (eu | us)")]`
//! lowers to `Leaf(ON_PROFILE, attrs)`. The const `ProfileExpr` is the authoring
//! target; this runtime impl reads the `expr` string attr and delegates to the
//! kernel's pure `accepts_profiles` evaluator over the sealed `ActiveProfiles`.
//!
//! The sealed `ActiveProfiles` is produced by `resolve_active` inside
//! `seal_environment` and lives on the `Env`. The frozen `ConditionCtx` does not
//! yet expose it (deferred to the env/profiles seam), so the active set is read
//! from the ambient profile scope leaf-boot installs alongside the probe scope.
//! See [`crate::profiles`] for the install helpers.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, ReasonMsg};

use crate::attrs;
use crate::profiles::current_active_profiles;

const EXPR: &str = "expr";

/// The runtime `OnProfile` impl.
pub struct OnProfileCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_PROFILE_COND: OnProfileCondition = OnProfileCondition;

impl Condition for OnProfileCondition {
    fn matches(&self, _ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let Some(expr) = attrs::str_of(attrs, EXPR) else {
            // No expression: vacuously active (matches Spring's empty-@Profile).
            return ConditionOutcome::new(true, ReasonMsg::of("OnProfile"));
        };
        let active = current_active_profiles();
        match leaf_core::accepts_profiles(expr, &active) {
            Ok(matched) => ConditionOutcome::new(
                matched,
                ReasonMsg {
                    kind: "OnProfile",
                    expected: Some(expr.to_string()),
                    found: Some(format!("active: {:?}", active.set())),
                    gate: None,
                },
            ),
            Err(e) => ConditionOutcome::new(
                false,
                ReasonMsg {
                    kind: "OnProfile",
                    expected: Some(expr.to_string()),
                    found: Some(format!("parse error: {e}")),
                    gate: None,
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{resolve_active, with_active_profiles, ProfileLevers};
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;

    fn active(names: &[&str]) -> leaf_core::ActiveProfiles {
        let levers = ProfileLevers {
            active: names.iter().map(|n| (*n).into()).collect(),
            ..Default::default()
        };
        resolve_active(levers, false).unwrap()
    }

    #[test]
    fn matches_when_named_profile_is_active() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "prod")];
        with_active_profiles(active(&["prod"]), || {
            assert!(ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
        with_active_profiles(active(&["dev"]), || {
            assert!(!ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
    }

    #[test]
    fn evaluates_the_boolean_grammar() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "prod & (eu | us)")];
        with_active_profiles(active(&["prod", "eu"]), || {
            assert!(ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
        with_active_profiles(active(&["prod", "asia"]), || {
            assert!(!ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
    }

    #[test]
    fn negation_matches_absence() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "!prod")];
        with_active_profiles(active(&["dev"]), || {
            assert!(ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
        with_active_profiles(active(&["prod"]), || {
            assert!(!ON_PROFILE_COND.matches(&ctx, &attrs).matched);
        });
    }
}
