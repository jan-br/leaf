//! `OnProfile` — the `ON_PROFILE` preset condition (Runtime, Parse).
//!
//! conditions-autoconfig (phase3/05) profiles: `#[profile("prod & (eu | us)")]`
//! lowers to `Leaf(ON_PROFILE, attrs)`. The const `ProfileExpr` is the authoring
//! target; this runtime impl reads the `expr` string attr and delegates to the
//! kernel's pure `accepts_profiles` evaluator over the sealed `ActiveProfiles`.
//!
//! The sealed `ActiveProfiles` is produced by `resolve_active` inside
//! `seal_environment` and rides the `ConditionCtx` directly (`ctx.profiles`):
//! leaf-boot threads the real active set in via
//! [`ConditionCtx::with_profiles`](leaf_core::ConditionCtx::with_profiles). A ctx
//! built with the 2-arg `new` carries the shared empty set
//! ([`ActiveProfiles::empty`](leaf_core::ActiveProfiles::empty)) — the no-scope
//! `{}` fallback. This impl no longer depends on any ambient thread-local.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, ReasonMsg};

use crate::attrs;

const EXPR: &str = "expr";

/// The runtime `OnProfile` impl.
pub struct OnProfileCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_PROFILE_COND: OnProfileCondition = OnProfileCondition;

impl Condition for OnProfileCondition {
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let Some(expr) = attrs::str_of(attrs, EXPR) else {
            // No expression: vacuously active (matches Spring's empty-@Profile).
            return ConditionOutcome::new(true, ReasonMsg::of("OnProfile"));
        };
        let active = ctx.profiles;
        match leaf_core::accepts_profiles(expr, active) {
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
    use crate::profiles::{resolve_active, ProfileLevers};
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
        let attrs: AttrSlice = &[Attr::Str(EXPR, "prod")];

        let (ctx, _s) = ctx_over(&env);
        let on = active(&["prod"]);
        assert!(ON_PROFILE_COND.matches(&ctx.with_profiles(&on), &attrs).matched);

        let (ctx, _s) = ctx_over(&env);
        let off = active(&["dev"]);
        assert!(!ON_PROFILE_COND.matches(&ctx.with_profiles(&off), &attrs).matched);
    }

    #[test]
    fn evaluates_the_boolean_grammar() {
        let env = env_with(&[]);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "prod & (eu | us)")];

        let (ctx, _s) = ctx_over(&env);
        let on = active(&["prod", "eu"]);
        assert!(ON_PROFILE_COND.matches(&ctx.with_profiles(&on), &attrs).matched);

        let (ctx, _s) = ctx_over(&env);
        let off = active(&["prod", "asia"]);
        assert!(!ON_PROFILE_COND.matches(&ctx.with_profiles(&off), &attrs).matched);
    }

    #[test]
    fn negation_matches_absence() {
        let env = env_with(&[]);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "!prod")];

        let (ctx, _s) = ctx_over(&env);
        let on = active(&["dev"]);
        assert!(ON_PROFILE_COND.matches(&ctx.with_profiles(&on), &attrs).matched);

        let (ctx, _s) = ctx_over(&env);
        let off = active(&["prod"]);
        assert!(!ON_PROFILE_COND.matches(&ctx.with_profiles(&off), &attrs).matched);
    }

    #[test]
    fn empty_default_profiles_make_a_named_profile_back_off() {
        // The 2-arg ctx (no `.with_profiles`) carries the empty active set, so a
        // named-profile guard backs off and a negation matches (the no-scope
        // `{}` fallback, now read off the ctx instead of an ambient default).
        let env = env_with(&[]);

        let (ctx, _s) = ctx_over(&env);
        let named: AttrSlice = &[Attr::Str(EXPR, "prod")];
        assert!(!ON_PROFILE_COND.matches(&ctx, &named).matched);

        let (ctx, _s) = ctx_over(&env);
        let negated: AttrSlice = &[Attr::Str(EXPR, "!prod")];
        assert!(ON_PROFILE_COND.matches(&ctx, &negated).matched);
    }
}
