//! `OnBean` / `OnMissingBean` / `OnSingleCandidate` — (Runtime, Register).
//!
//! conditions-autoconfig (phase3/05) condition-family: the OnBean family delegates
//! its verdict to the no-instantiation [`DefinitionProbe`](crate::probe) over the
//! growing definition set — the SAME primary/fallback/qualifier policy injection
//! and `App<Wired>` validation run, so there is ONE definition of "unambiguous":
//!
//! - `on_bean(T)`             matches iff `would_resolve_unique(T) == Unique`
//! - `on_missing_bean(T)`     matches iff `would_resolve_unique(T) != Unique`
//! - `on_single_candidate(T)` matches iff `would_resolve_unique(T) == Unique`
//!   (which, by running the resolver policy, counts a `@Primary`-among-several as
//!   unique and a lone `@Fallback` as resolving)
//!
//! The probe is read from the ambient scope leaf-boot installs around the
//! Register sub-pass (see [`crate::probe`]); the frozen `ConditionCtx` does not
//! yet carry it. With NO probe installed the definition set is treated as EMPTY
//! (`Resolvability::None`) — so `on_missing_bean` passes and `on_bean` backs off,
//! the sound default before any candidate has registered.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, ReasonMsg, Resolvability};

use crate::attrs;
use crate::probe::current_probe_query;

const TYPE: &str = "type";

/// Probe the queried `type` attr, defaulting to `None` (empty set) with no probe.
fn probe_type(attrs: &AttrSlice) -> Resolvability {
    match attrs::type_of(attrs, TYPE) {
        Some(ty) => current_probe_query(ty).unwrap_or(Resolvability::None),
        // No type attr: nothing to probe — treat as absent.
        None => Resolvability::None,
    }
}

fn describe(r: Resolvability) -> String {
    match r {
        Resolvability::None => "none".to_string(),
        Resolvability::Unique(id) => format!("unique(slot {id})"),
        Resolvability::Ambiguous(n) => format!("ambiguous({n})"),
    }
}

/// The runtime `OnBean` impl.
pub struct OnBeanCondition;
/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_BEAN: OnBeanCondition = OnBeanCondition;

impl Condition for OnBeanCondition {
    fn matches(&self, _ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let r = probe_type(attrs);
        ConditionOutcome::new(
            r.is_unique(),
            ReasonMsg {
                kind: "OnBean",
                expected: Some("unique candidate".to_string()),
                found: Some(describe(r)),
                gate: None,
            },
        )
    }
}

/// The runtime `OnMissingBean` impl.
pub struct OnMissingBeanCondition;
/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_MISSING_BEAN: OnMissingBeanCondition = OnMissingBeanCondition;

impl Condition for OnMissingBeanCondition {
    fn matches(&self, _ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let r = probe_type(attrs);
        ConditionOutcome::new(
            !r.is_unique(),
            ReasonMsg {
                kind: "OnMissingBean",
                expected: Some("no unique candidate".to_string()),
                found: Some(describe(r)),
                gate: None,
            },
        )
    }
}

/// The runtime `OnSingleCandidate` impl.
pub struct OnSingleCandidateCondition;
/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_SINGLE_CANDIDATE: OnSingleCandidateCondition = OnSingleCandidateCondition;

impl Condition for OnSingleCandidateCondition {
    fn matches(&self, _ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let r = probe_type(attrs);
        ConditionOutcome::new(
            r.is_unique(),
            ReasonMsg {
                kind: "OnSingleCandidate",
                expected: Some("exactly one resolvable candidate".to_string()),
                found: Some(describe(r)),
                gate: None,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{with_probe, DefinitionProbe};
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;
    use std::any::TypeId;
    use std::sync::Arc;

    struct Fixed(Resolvability);
    impl DefinitionProbe for Fixed {
        fn would_resolve_unique(&self, _ty: TypeId) -> Resolvability {
            self.0
        }
    }

    struct Marker;

    fn marker_attrs() -> AttrSlice {
        static ATTRS: &[Attr] = &[Attr::Type(TYPE, TypeId::of::<Marker>())];
        ATTRS
    }

    fn run(cond: &dyn Condition, r: Resolvability) -> bool {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs = marker_attrs();
        let probe: Arc<dyn DefinitionProbe> = Arc::new(Fixed(r));
        with_probe(probe, || cond.matches(&ctx, &attrs).matched)
    }

    #[test]
    fn on_bean_matches_unique_only() {
        assert!(run(&ON_BEAN, Resolvability::Unique(1)));
        assert!(!run(&ON_BEAN, Resolvability::None));
        assert!(!run(&ON_BEAN, Resolvability::Ambiguous(2)));
    }

    #[test]
    fn on_missing_bean_matches_non_unique() {
        assert!(!run(&ON_MISSING_BEAN, Resolvability::Unique(1)));
        assert!(run(&ON_MISSING_BEAN, Resolvability::None));
        assert!(run(&ON_MISSING_BEAN, Resolvability::Ambiguous(2)));
    }

    #[test]
    fn on_single_candidate_matches_unique_only() {
        assert!(run(&ON_SINGLE_CANDIDATE, Resolvability::Unique(9)));
        assert!(!run(&ON_SINGLE_CANDIDATE, Resolvability::None));
        assert!(!run(&ON_SINGLE_CANDIDATE, Resolvability::Ambiguous(3)));
    }

    #[test]
    fn no_probe_means_empty_set_so_missing_bean_passes() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs = marker_attrs();
        // Outside a probe scope: definition set is empty.
        assert!(ON_MISSING_BEAN.matches(&ctx, &attrs).matched);
        assert!(!ON_BEAN.matches(&ctx, &attrs).matched);
    }
}
