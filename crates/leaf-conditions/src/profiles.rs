//! Profiles: `resolve_active` activation algebra + the `ProfileExpr` evaluator.
//!
//! conditions-autoconfig (phase3/05) profiles: profiles own only the ALGEBRA;
//! environment-config harvests the [`ProfileLevers`] transport. The pure
//! activation algebra ([`resolve_active`]), the pure evaluator ([`matches`] over
//! `& | !`), and the runtime-string escape hatch ([`accepts_profiles`]) all live
//! in leaf-core (frozen ABI); this module RE-EXPORTS them as the leaf-conditions
//! surface and adds the ambient `ActiveProfiles` scope the `OnProfile` runtime
//! impl reads (until the kernel `ConditionCtx` exposes the sealed set directly).

use std::cell::RefCell;

pub use leaf_core::{
    accepts_profiles, resolve_active, ActivationReason, ActiveProfiles, ProfileError, ProfileExpr,
    ProfileLevers, ProfileParseError,
};
// leaf-core re-exports the free profile evaluator as `profile_matches` at the
// root (the bare name `matches` collides with `Condition::matches`); we surface
// it under the conventional `matches` name for the profiles module.
pub use leaf_core::profile_matches as matches;

thread_local! {
    static ACTIVE: RefCell<Option<ActiveProfiles>> = const { RefCell::new(None) };
}

/// A scope guard restoring the previous ambient `ActiveProfiles` on drop.
#[must_use = "dropping the guard immediately uninstalls the active set"]
pub struct ActiveProfilesScope {
    prev: Option<ActiveProfiles>,
}

impl Drop for ActiveProfilesScope {
    fn drop(&mut self) {
        ACTIVE.with(|c| *c.borrow_mut() = self.prev.take());
    }
}

/// Install the sealed `ActiveProfiles` for the `OnProfile` evaluations inside the
/// scope (leaf-boot installs it alongside the probe scope in App<Resolve>).
pub fn install_active_profiles(active: ActiveProfiles) -> ActiveProfilesScope {
    let prev = ACTIVE.with(|c| c.borrow_mut().replace(active));
    ActiveProfilesScope { prev }
}

/// Run `f` with `active` installed as the ambient active-profile set.
pub fn with_active_profiles<R>(active: ActiveProfiles, f: impl FnOnce() -> R) -> R {
    let _scope = install_active_profiles(active);
    f()
}

/// The ambient active-profile set, or the empty set (no scope installed →
/// `{default}` semantics resolve to "nothing active").
#[must_use]
pub fn current_active_profiles() -> ActiveProfiles {
    ACTIVE.with(|c| c.borrow().clone().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levers(active: &[&str]) -> ProfileLevers {
        ProfileLevers {
            active: active.iter().map(|s| (*s).into()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_levers_activate_the_default_profile() {
        let l = ProfileLevers {
            default: "default".into(),
            ..Default::default()
        };
        let resolved = resolve_active(l, false).unwrap();
        assert!(resolved.contains("default"));
    }

    #[test]
    fn explicit_active_suppresses_the_default() {
        let l = ProfileLevers {
            default: "default".into(),
            ..levers(&["prod"])
        };
        let resolved = resolve_active(l, false).unwrap();
        assert!(resolved.contains("prod"));
        assert!(!resolved.contains("default"), "default is dropped");
    }

    #[test]
    fn profile_expr_evaluator_and_or_not() {
        let resolved = resolve_active(levers(&["a", "b"]), false).unwrap();
        let and = ProfileExpr::And(&[ProfileExpr::Name("a"), ProfileExpr::Name("b")]);
        let or = ProfileExpr::Or(&[ProfileExpr::Name("a"), ProfileExpr::Name("z")]);
        let not = ProfileExpr::Not(&ProfileExpr::Name("z"));
        assert!(matches(&and, &resolved));
        assert!(matches(&or, &resolved));
        assert!(matches(&not, &resolved));
        assert!(!matches(&ProfileExpr::Name("z"), &resolved));
    }

    #[test]
    fn ambient_scope_installs_and_restores() {
        assert!(current_active_profiles().is_empty());
        let resolved = resolve_active(levers(&["prod"]), false).unwrap();
        with_active_profiles(resolved, || {
            assert!(current_active_profiles().contains("prod"));
        });
        assert!(current_active_profiles().is_empty());
    }

    #[test]
    fn accepts_profiles_escape_hatch_parses_and_evaluates() {
        let resolved = resolve_active(levers(&["prod", "eu"]), false).unwrap();
        assert!(accepts_profiles("prod & (eu | us)", &resolved).unwrap());
        assert!(accepts_profiles("!dev", &resolved).unwrap());
        assert!(!accepts_profiles("prod & us", &resolved).unwrap());
        // mixed operators without parens is a parse error
        assert!(accepts_profiles("a & b | c", &resolved).is_err());
    }
}
