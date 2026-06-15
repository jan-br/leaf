//! The build.rs anti-DCE support emitters: the force-link shim + the const
//! `ExpectedManifest` self-check anchor (cross-crate-discovery, ADR-09 / the
//! triple anti-DCE defense in `rust-cross-crate-composition.md` §Layer 0).
//!
//! These are the two codegen halves of the binary crate's anti-DCE strategy that
//! the generated `#[leaf::main]` / `build.rs` splices in. Both are pure functions
//! over the *participating set* (the binary crate + enabled capability features ∪
//! the `scan!` list), so they are unit-testable WITHOUT a compiler/link/runtime:
//! feed a set of crate names, assert on the emitted [`proc_macro2::TokenStream`].
//!
//! ## Why two emitters, and the layer each defeats
//!
//! `linkme` rows can vanish at three independent layers (the reference §345);
//! this unit owns the codegen for two of the three defenses:
//!
//! - **[`emit_force_link`] — Layer 0 (linkage).** A crate the binary never
//!   path-references is not linked at all, so its `#[distributed_slice]` sections
//!   never exist and the row silently vanishes. The fix (`inventory#7`, dtolnay)
//!   is `use <crate> as _;` — an extern-crate reference with the link side-effect
//!   but no name binding. This emits exactly one such line per participating
//!   crate, ABSOLUTE-pathed, hidden behind a `#[doc(hidden)]` private module so a
//!   user crate's imports cannot collide with it.
//! - **[`emit_expected_manifest`] — the self-check anchor.** A const
//!   `&[::leaf_core::SourceTag]` the cold assembly pass joins against the
//!   link-collected [`leaf_core::SOURCES`] slice: a tag present in the manifest
//!   but absent from `SOURCES` (or present-but-contributing-zero-rows) is the
//!   LOUD [`ErrorKind::AntiDce`](leaf_core::ErrorKind) instead of a confusing
//!   silent empty registry (the headline defense, ADR-09 Defense MANIFEST).
//!
//! Layer B (`--gc-sections`) is defeated by linker args (`.cargo/config.toml` /
//! `cargo::rustc-link-arg`), not codegen, so it is out of this module's scope —
//! see [`crate::cargo_leaf`] for the directive the prepare step recommends.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::quote;

/// Normalize a Cargo package name (`leaf-redis`) to its extern-crate identifier
/// (`leaf_redis`): Rust crate idents replace `-` with `_`.
///
/// The participating set is expressed in Cargo package names (the same string a
/// [`leaf_core::SourceTag`] carries), but `use <ident> as _;` needs the crate
/// IDENT, so the force-link shim maps across the one well-known rule.
#[must_use]
pub fn crate_ident(package_name: &str) -> String {
    package_name.replace('-', "_")
}

/// The de-duplicated, deterministically-ordered participating set the emitters
/// fold over (the binary crate + enabled capability features ∪ the `scan!` list).
///
/// Determinism is load-bearing: the same opted-in set must produce byte-identical
/// emitted artifacts so `cargo leaf prepare --check` is stable and a checked-in
/// manifest does not churn. The set is a `BTreeSet` so iteration is sorted by the
/// stable Cargo package name (NEVER link/declaration order, which is unspecified).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParticipatingSet {
    crates: BTreeSet<String>,
}

impl ParticipatingSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        ParticipatingSet::default()
    }

    /// Add one participating crate (by its Cargo package name). Idempotent.
    pub fn add(&mut self, package_name: impl Into<String>) {
        self.crates.insert(package_name.into());
    }

    /// Build a set from an iterator of package names.
    #[must_use]
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut set = ParticipatingSet::new();
        for n in names {
            set.add(n);
        }
        set
    }

    /// The package names in deterministic (sorted) order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.crates.iter().map(String::as_str).collect()
    }

    /// `true` iff the set is empty (no participating crates).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.crates.is_empty()
    }

    /// The number of participating crates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.crates.len()
    }
}

/// Emit the force-link shim for the participating set: one `use <crate> as _;`
/// per crate (Layer-0 anti-DCE), wrapped in a `#[doc(hidden)]` private module so
/// the references cannot collide with a user crate's imports.
///
/// The emitted item is a complete, hand-writable Rust module the binary crate's
/// `#[leaf::main]` / generated `build.rs` splices in. `use <crate> as _;` pins
/// the rlib onto the link graph (the extern-crate reference has the link
/// side-effect with no name binding) without forcing the crate's items into
/// scope. The order is the set's deterministic sort, so the artifact is stable.
#[must_use]
pub fn emit_force_link(set: &ParticipatingSet) -> TokenStream {
    let uses = set.names().into_iter().map(|name| {
        let ident = quote::format_ident!("{}", crate_ident(name));
        quote! { use #ident as _; }
    });
    quote! {
        // The anti-DCE force-link shim (Layer 0): each `use <crate> as _;` pins
        // the rlib onto the link graph so its `#[distributed_slice]` sections
        // exist. Hidden + private so it never pollutes the user namespace.
        #[doc(hidden)]
        mod __leaf_force_link {
            #(#uses)*
        }
    }
}

/// Emit the const `ExpectedManifest` — a `&[::leaf_core::SourceTag]` over the
/// participating set the cold assembly pass joins against the link-collected
/// [`leaf_core::SOURCES`] slice for the loud silent-empty self-check.
///
/// Each row is `::leaf_core::SourceTag("<package-name>")` (the Cargo package
/// name, an author-stable string — NOT the crate ident, and NOT a `TypeId`),
/// matching the tag a participating crate emits into `SOURCES`. The const is
/// named so the assembly pass can read it by a stable path; every path is
/// ABSOLUTE so a user crate's imports cannot shadow it.
#[must_use]
pub fn emit_expected_manifest(set: &ParticipatingSet) -> TokenStream {
    let rows = set.names().into_iter().map(|name| {
        quote! { ::leaf_core::SourceTag(#name) }
    });
    quote! {
        // The anti-DCE ExpectedManifest self-check anchor: a tag here but absent
        // from the link-collected ::leaf_core::SOURCES slice is the LOUD
        // ErrorKind::AntiDce, never a silent empty registry.
        #[doc(hidden)]
        pub const __LEAF_EXPECTED_MANIFEST: &[::leaf_core::SourceTag] = &[ #(#rows),* ];
    }
}

/// Emit both anti-DCE halves (force-link shim + `ExpectedManifest`) as one item
/// sequence — the whole build.rs/`#[leaf::main]` anti-DCE artifact.
#[must_use]
pub fn emit(set: &ParticipatingSet) -> TokenStream {
    let force_link = emit_force_link(set);
    let manifest = emit_expected_manifest(set);
    quote! {
        #force_link
        #manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a `TokenStream` to a whitespace-collapsed string so assertions are
    /// robust against `quote!`'s token spacing (`:: leaf_core` vs `::leaf_core`).
    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn participating() -> ParticipatingSet {
        ParticipatingSet::from_names(["leaf-redis", "leaf-tokio"])
    }

    #[test]
    fn crate_ident_maps_dashes_to_underscores() {
        assert_eq!(crate_ident("leaf-redis"), "leaf_redis");
        assert_eq!(crate_ident("leaf"), "leaf");
        assert_eq!(crate_ident("my-cool-crate"), "my_cool_crate");
    }

    #[test]
    fn participating_set_is_deduplicated_and_sorted() {
        let mut set = ParticipatingSet::new();
        set.add("leaf-tokio");
        set.add("leaf-redis");
        set.add("leaf-tokio"); // duplicate
        assert_eq!(set.len(), 2);
        // Sorted by the stable package name (never link order).
        assert_eq!(set.names(), vec!["leaf-redis", "leaf-tokio"]);
    }

    #[test]
    fn force_link_emits_one_use_as_underscore_per_crate() {
        // The Layer-0 fix: one `use <crate> as _;` per participating crate, with
        // the dash→underscore crate-ident mapping applied.
        let ts = emit_force_link(&participating());
        syn::parse2::<syn::File>(ts.clone()).expect("force-link shim is valid Rust items");
        let s = flat(&ts);
        assert!(s.contains("useleaf_redisas_;"), "got: {s}");
        assert!(s.contains("useleaf_tokioas_;"), "got: {s}");
        // Exactly two force-link lines (one per crate).
        assert_eq!(s.matches("as_;").count(), 2, "got: {s}");
    }

    #[test]
    fn force_link_is_wrapped_in_a_hidden_private_module() {
        // The references live in a `#[doc(hidden)]` private module so they never
        // collide with or pollute the user crate's namespace.
        let s = flat(&emit_force_link(&participating()));
        assert!(s.contains("#[doc(hidden)]"), "got: {s}");
        assert!(s.contains("mod__leaf_force_link"), "got: {s}");
    }

    #[test]
    fn force_link_for_an_empty_set_emits_an_empty_module() {
        // No participating crates => a valid, empty force-link module (no panic,
        // no stray `use`).
        let ts = emit_force_link(&ParticipatingSet::new());
        syn::parse2::<syn::File>(ts.clone()).expect("empty shim is valid Rust");
        let s = flat(&ts);
        assert_eq!(s.matches("as_;").count(), 0, "got: {s}");
    }

    #[test]
    fn expected_manifest_emits_a_source_tag_per_crate_by_package_name() {
        // The ExpectedManifest is a const &[::leaf_core::SourceTag] over the
        // PACKAGE names (the author-stable string SOURCES rows carry) — not the
        // crate idents.
        let ts = emit_expected_manifest(&participating());
        syn::parse2::<syn::File>(ts.clone()).expect("manifest is valid Rust items");
        let s = flat(&ts);
        assert!(s.contains("&[::leaf_core::SourceTag"), "got: {s}");
        assert!(s.contains(r#"::leaf_core::SourceTag("leaf-redis")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::SourceTag("leaf-tokio")"#), "got: {s}");
        // Package names keep their dashes (NOT the crate ident).
        assert!(!s.contains("leaf_redis"), "manifest must use package names: {s}");
    }

    #[test]
    fn expected_manifest_is_a_named_const_for_the_assembly_pass() {
        let s = flat(&emit_expected_manifest(&participating()));
        assert!(s.contains("const__LEAF_EXPECTED_MANIFEST:"), "got: {s}");
    }

    #[test]
    fn expected_manifest_is_deterministic_across_insertion_order() {
        // Determinism is load-bearing for `--check`: two sets with the same
        // members but different insertion order emit byte-identical manifests.
        let a = ParticipatingSet::from_names(["leaf-redis", "leaf-tokio", "leaf"]);
        let b = ParticipatingSet::from_names(["leaf", "leaf-tokio", "leaf-redis"]);
        assert_eq!(
            flat(&emit_expected_manifest(&a)),
            flat(&emit_expected_manifest(&b)),
        );
    }

    #[test]
    fn emit_combines_both_anti_dce_halves_into_one_artifact() {
        // The whole build.rs anti-DCE artifact: the force-link shim + the
        // ExpectedManifest, together a valid Rust file.
        let ts = emit(&participating());
        syn::parse2::<syn::File>(ts.clone()).expect("the whole artifact is valid Rust");
        let s = flat(&ts);
        assert!(s.contains("mod__leaf_force_link"), "got: {s}");
        assert!(s.contains("__LEAF_EXPECTED_MANIFEST"), "got: {s}");
    }
}
