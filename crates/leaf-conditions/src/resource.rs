//! `OnResource` — (Runtime, Parse) resource-existence leaf.
//!
//! conditions-autoconfig (phase3/05) condition-family: `OnResource` matches iff
//! ALL listed resources exist. Each `resource` attr is a scheme-prefixed location
//! (`file:./config/app.yml`, `classpath:banner.txt`); placeholders are resolved
//! against the sealed `Env` first.
//!
//! Scheme handling here:
//! - `file:` (and bare paths) are checked directly with `std::fs` — cold,
//!   synchronous existence checks are sound at the Parse sub-pass.
//! - `classpath:`/`url:` resolution rides leaf-core's `ResourceLoader`, which the
//!   frozen `ConditionCtx` does not yet expose (deferred to leaf-boot's loader
//!   seam). NOTE: until then a `classpath:`/`url:` resource degrades to a
//!   recorded miss with a clear reason rather than a silent pass — honest, never
//!   a false positive.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, PropertyResolver, ReasonMsg};

use crate::attrs;

const RESOURCE: &str = "resource";

/// The runtime `OnResource` impl.
pub struct OnResourceCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_RESOURCE: OnResourceCondition = OnResourceCondition;

impl Condition for OnResourceCondition {
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let locations = attrs::all_str(attrs, RESOURCE);
        if locations.is_empty() {
            return ConditionOutcome::new(true, ReasonMsg::of("OnResource"));
        }
        for loc in &locations {
            let resolved = ctx.env.resolve_placeholders(loc);
            match check_exists(resolved.trim()) {
                Existence::Present => {}
                Existence::Missing => {
                    return miss(loc, "not found");
                }
                Existence::Undeterminable(why) => {
                    return miss(loc, why);
                }
            }
        }
        ConditionOutcome::new(
            true,
            ReasonMsg {
                kind: "OnResource",
                expected: Some("present".to_string()),
                found: None,
                gate: None,
            },
        )
    }
}

fn miss(loc: &str, why: &str) -> ConditionOutcome {
    ConditionOutcome::new(
        false,
        ReasonMsg {
            kind: "OnResource",
            expected: Some(loc.to_string()),
            found: Some(why.to_string()),
            gate: None,
        },
    )
}

enum Existence {
    Present,
    Missing,
    Undeterminable(&'static str),
}

fn check_exists(loc: &str) -> Existence {
    if let Some(path) = loc.strip_prefix("file:") {
        return fs_exists(path);
    }
    if loc.starts_with("classpath:") {
        // NOTE: classpath resolution needs the compiled-in RESOURCES table via a
        // ResourceLoader the ConditionCtx does not yet carry (leaf-boot seam).
        return Existence::Undeterminable("classpath loader not available (leaf-boot seam)");
    }
    if loc.starts_with("http:") || loc.starts_with("https:") || loc.starts_with("url:") {
        return Existence::Undeterminable("url resolution not available at planning time");
    }
    // A bare path is treated as a filesystem path.
    fs_exists(loc)
}

fn fs_exists(path: &str) -> Existence {
    if std::path::Path::new(path).exists() {
        Existence::Present
    } else {
        Existence::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;

    #[test]
    fn an_existing_file_matches() {
        // Cargo.toml is always present at the crate root during tests.
        static ATTRS: &[Attr] =
            &[Attr::Str(RESOURCE, concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))];
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        assert!(ON_RESOURCE.matches(&ctx, &ATTRS).matched);
    }

    #[test]
    fn a_missing_file_does_not_match() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(RESOURCE, "file:/no/such/path/xyzzy.cfg")];
        let out = ON_RESOURCE.matches(&ctx, &attrs);
        assert!(!out.matched);
        assert_eq!(out.reason.found.as_deref(), Some("not found"));
    }

    #[test]
    fn placeholders_in_the_location_resolve() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let env = env_with(&[("base", dir)]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(RESOURCE, "file:${base}/Cargo.toml")];
        assert!(ON_RESOURCE.matches(&ctx, &attrs).matched);
    }

    #[test]
    fn classpath_degrades_to_an_honest_miss() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(RESOURCE, "classpath:banner.txt")];
        assert!(!ON_RESOURCE.matches(&ctx, &attrs).matched);
    }
}
