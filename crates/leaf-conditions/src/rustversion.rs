//! `OnRustVersion` — (ConstFold) the `@ConditionalOnJava` reframing.
//!
//! conditions-autoconfig (phase3/05) condition-family: JVM `OnJava` → Rust
//! `OnRustVersion`, a ConstFold-tier member: it is decided at BUILD against the
//! compiling toolchain and lowered to `CondExpr::Const(bool)` so it never reaches
//! the runtime registry. This module owns:
//!
//! - [`Version`] — a parsed `major.minor.patch` triple with ordering.
//! - [`meets`] — the pure comparison (`current >= at_least`, optional `at_most`).
//! - [`lower`] — the const-fold entry: given the (build-captured) toolchain
//!   version and the attrs, return `CondExpr::Const(bool)` — the tier refinement
//!   the design mandates ("a `ConstFold` leaf arrives as `Const(bool)`").
//! - The runtime [`Condition`] impl (the rare un-lowered / forced-runtime path),
//!   comparing against the toolchain version captured by `build.rs`.

use leaf_core::{AttrSlice, CondExpr, Condition, ConditionCtx, ConditionOutcome, ReasonMsg};

use crate::attrs;

const AT_LEAST: &str = "at_least";
const AT_MOST: &str = "at_most";

/// The compiling toolchain version, captured by `build.rs` (`rustc -V`). Falls
/// back to a compile-time `option_env!` so the crate still builds if the env is
/// absent (the comparison then degrades to "unknown" handling in [`lower`]).
pub const TOOLCHAIN_VERSION: Option<&str> = option_env!("LEAF_RUSTC_VERSION");

/// A parsed semantic-ish version triple (`major.minor.patch`; missing parts = 0).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    /// Major component.
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
}

impl Version {
    /// Parse `major[.minor[.patch]]`, ignoring any trailing pre-release/build
    /// metadata (`1.80.0-nightly` → `1.80.0`). Returns `None` if no leading
    /// numeric component is present.
    #[must_use]
    pub fn parse(s: &str) -> Option<Version> {
        // Take the leading "x.y.z" run; tolerate a "rustc " prefix.
        let s = s.trim().strip_prefix("rustc ").unwrap_or(s.trim());
        let head: String = s
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let mut it = head.split('.').filter(|p| !p.is_empty());
        let major = it.next()?.parse().ok()?;
        let minor = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(Version { major, minor, patch })
    }
}

/// Whether `current` satisfies the `[at_least, at_most]` window (either bound
/// optional). `current >= at_least && current <= at_most`.
#[must_use]
pub fn meets(current: Version, at_least: Option<Version>, at_most: Option<Version>) -> bool {
    at_least.is_none_or(|lo| current >= lo) && at_most.is_none_or(|hi| current <= hi)
}

/// The ConstFold tier refinement: fold an `OnRustVersion` leaf to
/// `CondExpr::Const(bool)` against the build-captured toolchain version.
///
/// When the toolchain version is unknown (no `LEAF_RUSTC_VERSION`), it folds to
/// `Const(true)` (fail-open at build is sound: a missing build signal must not
/// silently prune a candidate — the runtime impl re-checks if needed).
#[must_use]
pub fn lower(attrs: &AttrSlice) -> CondExpr {
    let current = TOOLCHAIN_VERSION.and_then(Version::parse);
    let Some(current) = current else {
        return CondExpr::Const(true);
    };
    let at_least = attrs::str_of(attrs, AT_LEAST).and_then(Version::parse);
    let at_most = attrs::str_of(attrs, AT_MOST).and_then(Version::parse);
    CondExpr::Const(meets(current, at_least, at_most))
}

/// The runtime `OnRustVersion` impl (used only when not const-folded).
pub struct OnRustVersionCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_RUST_VERSION: OnRustVersionCondition = OnRustVersionCondition;

impl Condition for OnRustVersionCondition {
    fn matches(&self, _ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let current = TOOLCHAIN_VERSION.and_then(Version::parse);
        let at_least = attrs::str_of(attrs, AT_LEAST).and_then(Version::parse);
        let at_most = attrs::str_of(attrs, AT_MOST).and_then(Version::parse);
        let matched = match current {
            Some(v) => meets(v, at_least, at_most),
            None => true, // unknown toolchain ⇒ fail-open (same rule as `lower`)
        };
        ConditionOutcome::new(
            matched,
            ReasonMsg {
                kind: "OnRustVersion",
                expected: attrs::str_of(attrs, AT_LEAST).map(str::to_string),
                found: TOOLCHAIN_VERSION.map(str::to_string),
                gate: None,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::Attr;

    #[test]
    fn version_parse_tolerates_prefix_and_metadata() {
        assert_eq!(
            Version::parse("rustc 1.96.0 (ac68faa20 2026-05-25)"),
            Some(Version { major: 1, minor: 96, patch: 0 })
        );
        assert_eq!(
            Version::parse("1.80"),
            Some(Version { major: 1, minor: 80, patch: 0 })
        );
        assert_eq!(Version::parse("nope"), None);
    }

    #[test]
    fn meets_respects_both_bounds() {
        let v = Version { major: 1, minor: 85, patch: 0 };
        assert!(meets(v, Version::parse("1.80"), None));
        assert!(!meets(v, Version::parse("1.90"), None));
        assert!(meets(v, Version::parse("1.80"), Version::parse("1.90")));
        assert!(!meets(v, None, Version::parse("1.80")));
    }

    #[test]
    fn lower_folds_to_a_const() {
        // With a real captured toolchain, an absurdly-low floor folds to true.
        let attrs: AttrSlice = &[Attr::Str(AT_LEAST, "1.0")];
        assert!(matches!(lower(&attrs), CondExpr::Const(true)));
        // An absurdly-high floor folds to false (when the toolchain is known).
        let attrs_hi: AttrSlice = &[Attr::Str(AT_LEAST, "999.0")];
        if TOOLCHAIN_VERSION.and_then(Version::parse).is_some() {
            assert!(matches!(lower(&attrs_hi), CondExpr::Const(false)));
        }
    }

    #[test]
    fn lower_always_yields_a_const_leaf() {
        let attrs: AttrSlice = &[];
        assert!(lower(&attrs).is_const(), "ConstFold tier ⇒ Const(bool)");
    }
}
