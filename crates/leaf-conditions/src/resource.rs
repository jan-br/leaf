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
//! - `classpath:`/`url:` resolution rides the optional `ResourceLoader` borrow
//!   now carried on the `ConditionCtx` (`ctx.loader`): when a loader is installed
//!   the location resolves through it and its synchronous `Resource::exists()`
//!   tri-state decides the verdict. The scaffolding + the `None`-degradation are
//!   in place; this only becomes USEFUL once leaf-boot installs a scheme-aware
//!   `ResourceLoader` (a later ecosystem step). Until then a `classpath:`/`url:`
//!   resource degrades to a recorded miss with a clear reason rather than a
//!   silent pass — honest, never a false positive.

use leaf_core::{
    AttrSlice, Condition, ConditionCtx, ConditionOutcome, Location, PropertyResolver, ReasonMsg,
    ResourceLoader, Scheme,
};

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
            match check_exists(resolved.trim(), ctx.loader) {
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

fn check_exists(loc: &str, loader: Option<&dyn ResourceLoader>) -> Existence {
    if let Some(path) = loc.strip_prefix("file:") {
        return fs_exists(path);
    }
    if let Some(path) = loc.strip_prefix("classpath:") {
        return match loader {
            // A scheme-aware loader resolves the compiled-in RESOURCES table.
            Some(l) => loader_exists(l, Scheme::Classpath, path),
            // NOTE: without an installed loader, a classpath resource cannot be
            // decided here (it only becomes useful once leaf-boot installs a
            // scheme-aware ResourceLoader — a later ecosystem step).
            None => Existence::Undeterminable("classpath loader not available (leaf-boot seam)"),
        };
    }
    if loc.starts_with("http:") || loc.starts_with("https:") || loc.starts_with("url:") {
        let path = loc.strip_prefix("url:").unwrap_or(loc);
        return match loader {
            Some(l) => loader_exists(l, Scheme::Url, path),
            None => Existence::Undeterminable("url resolution not available at planning time"),
        };
    }
    // A bare path is treated as a filesystem path.
    fs_exists(loc)
}

/// Resolve `path` under `scheme` through the installed loader and map its
/// synchronous `Resource::exists()` tri-state onto this module's [`Existence`].
fn loader_exists(loader: &dyn ResourceLoader, scheme: Scheme, path: &str) -> Existence {
    let location = Location::new(scheme, path.to_string());
    match loader.resolve(&location) {
        Ok(resource) => match resource.exists() {
            leaf_core::Existence::Known(true) => Existence::Present,
            leaf_core::Existence::Known(false) => Existence::Missing,
            leaf_core::Existence::Unknown => {
                Existence::Undeterminable("loader reports existence undeterminable")
            }
        },
        Err(_) => Existence::Undeterminable("loader could not resolve the location"),
    }
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
    fn classpath_without_a_loader_degrades_to_an_honest_miss() {
        // No `ctx.loader`: a `classpath:` resource cannot be decided, so it backs
        // off honestly (never a silent pass).
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(RESOURCE, "classpath:banner.txt")];
        assert!(!ON_RESOURCE.matches(&ctx, &attrs).matched);
    }

    // ── stub ResourceLoader (mirrors leaf-core's ClasspathLoader seam) ──
    use leaf_core::{
        BoxFuture, Existence as CoreExistence, LeafError, Location, Resource, ResourceId,
        ResourceLoader, ResourceReader,
    };

    struct StubResource {
        id: ResourceId,
        present: bool,
    }
    impl Resource for StubResource {
        fn id(&self) -> &ResourceId {
            &self.id
        }
        fn exists(&self) -> CoreExistence {
            CoreExistence::Known(self.present)
        }
        fn last_modified(&self) -> Option<std::time::SystemTime> {
            None
        }
        fn open<'a>(&'a self) -> BoxFuture<'a, Result<Box<dyn ResourceReader>, LeafError>> {
            Box::pin(async { Err(LeafError::new(leaf_core::ErrorKind::ConfigIo)) })
        }
        fn read_to_bytes<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, LeafError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    /// A loader that reports a fixed existence verdict for every location.
    struct StubLoader {
        present: bool,
    }
    impl ResourceLoader for StubLoader {
        fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError> {
            Ok(Box::new(StubResource {
                id: ResourceId::new(loc.clone()),
                present: self.present,
            }))
        }
    }

    #[test]
    fn classpath_resolves_through_the_loader_when_present() {
        let env = env_with(&[]);
        let attrs: AttrSlice = &[Attr::Str(RESOURCE, "classpath:banner.txt")];

        let loader = StubLoader { present: true };
        let (ctx, _s) = ctx_over(&env);
        let ctx = ctx.with_loader(&loader);
        assert!(ON_RESOURCE.matches(&ctx, &attrs).matched);

        let loader = StubLoader { present: false };
        let (ctx, _s) = ctx_over(&env);
        let ctx = ctx.with_loader(&loader);
        let out = ON_RESOURCE.matches(&ctx, &attrs);
        assert!(!out.matched);
        assert_eq!(out.reason.found.as_deref(), Some("not found"));
    }
}
