//! The FORCE-LINK + `ExpectedManifest` anti-DCE seam the umbrella owns
//! (cross-crate-discovery ADR-09 / `rust-cross-crate-composition.md` §Layer 0;
//! phase3 TOPOLOGY "Starters & BOM").
//!
//! ## The two-gate activation, and the gate this seam closes
//!
//! A cargo feature gates COMPILATION, not LINKAGE. So enabling the umbrella's
//! `redis` capability feature `dep:`-pulls `leaf-starter-redis` (→ `leaf-redis` +
//! `leaf-tokio`) — that is the PARTICIPATION gate's first half (the crate is
//! compiled + present). But a crate the binary never PATH-REFERENCES is dropped at
//! link time, so its `#[distributed_slice]` rows (the `AUTO_CONFIGS` auto-config
//! rows, the `COMPONENTS` infrastructure providers) silently vanish — a dropped row
//! is a quiet empty iterable, never a link error. This seam closes that Layer-0
//! gap two ways, gated by the SAME capability features:
//!
//! - **The umbrella's own force-link.** This module path-references each enabled
//!   integration crate (`use leaf_redis as _;`) behind its `#[cfg(feature = …)]`,
//!   so MERELY depending on `leaf` with the feature pins the rlib onto the link
//!   graph. This is the always-on half (no app boilerplate required).
//! - **[`force_link!`] — the app-invoked belt-and-suspenders.** The binary crate
//!   can ALSO invoke `leaf::force_link!();` in its `main` module so the references
//!   originate in the BINARY crate itself (the strongest anti-`--gc-sections`
//!   anchor — a reference from the final link unit, not a transitive rlib). It
//!   expands to the same `use <crate> as _;` set over the enabled features.
//!
//! ## The `ExpectedManifest` self-check anchor
//!
//! [`expected_manifest`] is the const `&[SourceTag]` over the enabled participating
//! set the cold assembly pass joins against the link-collected
//! [`leaf_core::SOURCES`] slice: a crate EXPECTED to contribute rows but absent from
//! `SOURCES` is the LOUD [`AntiDceError::SourceVanished`](leaf_boot::AntiDceError)
//! naming the crate — never a confusing silent empty registry later. The binary
//! crate adds its OWN package name to the manifest it feeds the run pipeline (it
//! contributes its own rows); this seam supplies the ENABLED-INTEGRATION rows.
//!
//! NOTE (honest deferral — the self-check's found side): the expected-vs-found
//! check is only as strong as the `SOURCES` rows the integration crates contribute.
//! Today `leaf-tokio`/`leaf-redis` register their beans into `COMPONENTS`/
//! `AUTO_CONFIGS` but do NOT yet submit a per-crate [`SourceTag`](leaf_core::SourceTag)
//! into `SOURCES` (that per-crate anchor is the integration crates' codegen seam,
//! not the umbrella's). So feeding this manifest to
//! [`leaf_boot::self_check`] would currently flag those crates as vanished even when
//! present. The umbrella therefore EXPOSES the manifest (the seam + the capability
//! vocabulary are complete here) and force-links the crates (the load-bearing
//! Layer-0 defense), but [`bootstrap`](crate::bootstrap) runs the self-check over
//! the EMPTY manifest until the integration crates emit their `SOURCES` anchors —
//! the same posture `leaf_boot::Application::run` ships with. [`expected_manifest`]
//! is ready to wire through the moment those anchors land.

use leaf_core::SourceTag;

// ── the always-on umbrella force-link (one `use … as _;` per enabled crate) ──
//
// Hidden + private so the references never pollute a downstream namespace. Merely
// depending on `leaf` with a capability feature pins the integration crate's rlib
// onto the link graph (the Layer-0 anti-DCE fix, dtolnay `inventory#7`).
#[doc(hidden)]
mod __leaf_force_link {
    #[cfg(feature = "redis")]
    pub(crate) use leaf_starter_redis as _;
    #[cfg(feature = "web")]
    pub(crate) use leaf_starter_web as _;
}

/// The Cargo PACKAGE names of the integration crates pulled into the participating
/// set by the currently-enabled capability features (the author-stable strings a
/// [`SourceTag`] carries — NOT crate idents, and NOT the binary crate, which the
/// app adds itself).
///
/// A CAPABILITY feature contributes its integration crate + its runtime peer; a
/// STACK feature contributes its whole curated bundle. The base set (`leaf-core`/
/// `leaf-macros`/`leaf-boot`/`leaf-tokio`) is always linked through the umbrella's
/// own dependency edges, so it is NOT repeated here — this is the FEATURE-GATED
/// delta the self-check must additionally expect.
///
/// The list is deterministic (sorted, de-duplicated) so a checked-in
/// `ExpectedManifest` does not churn across feature-set permutations.
#[must_use]
pub fn participating_crates() -> Vec<&'static str> {
    // The feature-gated delta (each capability's crate names). Collected into a
    // de-duplicating, sorted set — leaf-tokio appears under both the redis
    // capability peer and the web bundle, so a raw concat would double it. The
    // slices are gated so an app with no capability feature contributes nothing.
    #[cfg(feature = "redis")]
    // The CAPABILITY starter: the integration crate + its runtime peer.
    const REDIS: &[&str] = &["leaf-redis", "leaf-tokio"];
    #[cfg(not(feature = "redis"))]
    const REDIS: &[&str] = &[];

    #[cfg(feature = "web")]
    // The STACK starter: the curated additive bundle (the non-ecosystem set that
    // exists today — leaf-tokio + leaf-validation + leaf-cache).
    const WEB: &[&str] = &["leaf-tokio", "leaf-validation", "leaf-cache"];
    #[cfg(not(feature = "web"))]
    const WEB: &[&str] = &[];

    let set: std::collections::BTreeSet<&'static str> =
        REDIS.iter().chain(WEB).copied().collect();
    set.into_iter().collect()
}

/// The const-shaped `ExpectedManifest`: a `&[SourceTag]` over the enabled
/// participating set, the expected-vs-found anti-DCE self-check anchor the cold
/// assembly pass joins against [`leaf_core::SOURCES`].
///
/// This is the umbrella-owned half of the binary crate's `ExpectedManifest`. The
/// binary adds its OWN [`SourceTag`] (it contributes its own rows); this supplies
/// the rows the enabled capability features pull in. See the module NOTE for why
/// [`bootstrap`](crate::bootstrap) does not yet feed this to the live self-check.
#[must_use]
pub fn expected_manifest() -> Vec<SourceTag> {
    participating_crates().into_iter().map(SourceTag).collect()
}

/// `leaf::force_link!()` — the app-invoked Layer-0 anti-DCE shim: emit one
/// `use <crate> as _;` per enabled integration crate, so the references originate in
/// the BINARY crate (the strongest anchor against `--gc-sections`). The umbrella
/// ALSO force-links these crates itself (so this macro is belt-and-suspenders, not
/// strictly required), but invoking it in the app's `main` module makes the binary
/// crate the originating link unit.
///
/// Expand it at module scope in the binary crate (it emits `use` items):
///
/// ```ignore
/// leaf::force_link!();
///
/// #[leaf::main]
/// async fn main() { /* … */ }
/// ```
///
/// The expansion is gated by the SAME capability features as the umbrella's own
/// force-link, so an app that enables no feature gets an empty (valid) expansion.
#[macro_export]
macro_rules! force_link {
    () => {
        // Each `use <crate> as _;` pins the rlib onto the link graph from the
        // binary crate. Gated per capability feature on the umbrella; an unenabled
        // capability emits nothing. The paths go through the umbrella's own
        // re-exports so the binary need not name the starter crates directly.
        #[cfg(feature = "redis")]
        use $crate::redis as _;
        #[cfg(feature = "web")]
        use $crate::web as _;
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participating_crates_is_sorted_and_deduplicated() {
        let crates = participating_crates();
        let mut sorted = crates.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(crates, sorted, "the participating set is sorted + de-duplicated");
    }

    #[test]
    fn expected_manifest_mirrors_the_participating_crates_as_source_tags() {
        let manifest = expected_manifest();
        let crates = participating_crates();
        assert_eq!(manifest.len(), crates.len());
        for (tag, name) in manifest.iter().zip(crates.iter()) {
            assert_eq!(tag.0, *name, "each manifest row is a SourceTag of the package name");
        }
    }

    #[test]
    fn the_manifest_uses_package_names_not_crate_idents() {
        // A SourceTag carries the author-stable Cargo PACKAGE name (with dashes),
        // matching what a participating crate emits into SOURCES — never the
        // underscore crate ident.
        for tag in expected_manifest() {
            assert!(
                !tag.0.contains('_'),
                "ExpectedManifest must use package names (dashes), got: {}",
                tag.0
            );
            assert!(tag.0.starts_with("leaf-"), "got: {}", tag.0);
        }
    }

    #[cfg(not(any(feature = "redis", feature = "web")))]
    #[test]
    fn the_base_app_has_an_empty_feature_gated_manifest() {
        // With no capability feature, the FEATURE-GATED delta is empty (the base
        // crates link through the umbrella's own edges; the binary adds its own tag).
        assert!(participating_crates().is_empty());
        assert!(expected_manifest().is_empty());
    }

    #[cfg(feature = "redis")]
    #[test]
    fn the_redis_capability_contributes_leaf_redis_and_its_runtime_peer() {
        let crates = participating_crates();
        assert!(crates.contains(&"leaf-redis"), "got: {crates:?}");
        assert!(crates.contains(&"leaf-tokio"), "the redis peer runtime: {crates:?}");
    }

    #[cfg(feature = "web")]
    #[test]
    fn the_web_capability_contributes_the_curated_stack_bundle() {
        let crates = participating_crates();
        assert!(crates.contains(&"leaf-tokio"), "got: {crates:?}");
        assert!(crates.contains(&"leaf-validation"), "got: {crates:?}");
        assert!(crates.contains(&"leaf-cache"), "got: {crates:?}");
    }
}
