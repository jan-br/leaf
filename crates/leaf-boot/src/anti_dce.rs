//! The expected-vs-found anti-DCE self-check (cross-crate-discovery ADR-09 /
//! `rust-cross-crate-composition.md` §Layer 0; bootstrap-diagnostics phase3/14).
//!
//! `linkme` rows can vanish at three independent layers (linkage, rustc
//! reachability, `--gc-sections`), and a dropped row is a SILENT empty iterable,
//! never a link error. The headline defense is the expected-vs-found
//! [`SourceTag`](leaf_core::SourceTag) self-check that runs at the
//! `App<Define>→App<Resolve>` edge: a crate present in the binary's
//! `ExpectedManifest` (emitted by `#[leaf::main]` / `build.rs`, see
//! [`leaf_codegen::forcelink`]) but absent from the link-collected
//! [`leaf_core::SOURCES`] slice becomes a LOUD
//! [`AntiDceError::SourceVanished`] naming the vanished crate — never a confusing
//! `NoSuchBean` later.
//!
//! This is the ONE place the asymmetric quiet-DCE hazards bootstrap-diagnostics
//! flags (a DCE'd early listener / migration runner / analyzer / deducer) all
//! converge: they are ALL one `SourceVanished` surfaced here, per phase3/14.

use leaf_core::{collect_slice, Cause, ErrorKind, LeafError, SourceTag, SOURCES};

/// The headline anti-DCE failure: a source the binary EXPECTED to contribute
/// link-collected rows produced none (it was never linked, or its
/// `#[distributed_slice]` sections were garbage-collected).
///
/// This is the single `AntiDceError::SourceVanished` the design names: every
/// asymmetric quiet-DCE hazard (silent listener / migration / analyzer /
/// deducer) collapses into it (bootstrap-diagnostics phase3/14). It lifts into
/// the one [`LeafError`] spine via `From` with [`ErrorKind::AntiDce`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AntiDceError {
    /// A crate in the `ExpectedManifest` contributed zero rows to the
    /// link-collected [`SOURCES`] slice — it was never force-linked, or a section
    /// GC dropped its anchor. The remediation is to add AND force-link the crate.
    SourceVanished {
        /// The author-stable Cargo package name of the vanished crate (the
        /// [`SourceTag`] string), so the diagnostic can name it precisely.
        crate_name: &'static str,
    },
}

impl std::fmt::Display for AntiDceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AntiDceError::SourceVanished { crate_name } => write!(
                f,
                "anti-DCE: crate `{crate_name}` was expected but contributed zero \
                 link-collected rows (it was never force-linked, or `--gc-sections` \
                 dropped its `#[distributed_slice]` anchors). Add AND force-link \
                 `{crate_name}` (a `use {crate_name} as _;` / a `scan(\"{crate_name}\")` \
                 entry on `#[leaf::main]`)."
            ),
        }
    }
}

impl std::error::Error for AntiDceError {}

impl From<AntiDceError> for LeafError {
    fn from(err: AntiDceError) -> Self {
        let cause = match &err {
            AntiDceError::SourceVanished { crate_name } => Cause::plain(
                "anti-DCE self-check",
                format!("expected source `{crate_name}` produced zero rows"),
            ),
        };
        LeafError::new(ErrorKind::AntiDce).caused_by(cause)
    }
}

/// Run the expected-vs-found self-check: every [`SourceTag`] in `expected` must
/// appear in the link-collected [`leaf_core::SOURCES`] slice.
///
/// Returns the FIRST vanished source (deterministic — `expected` is the binary's
/// stable-sorted `ExpectedManifest`), so the loudest, earliest diagnostic names a
/// concrete crate rather than emitting a cascade.
///
/// # Errors
/// [`AntiDceError::SourceVanished`] naming the first expected-but-absent crate.
pub fn self_check(expected: &[SourceTag]) -> Result<(), AntiDceError> {
    // The link-collected anchors actually present in this binary.
    let found = collect_slice(&SOURCES);
    for tag in expected {
        if !found.contains(tag) {
            return Err(AntiDceError::SourceVanished { crate_name: tag.0 });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // leaf-core's own discovery tests submit a SourceTag, but those are gated on
    // `#[cfg(test)]` IN leaf-core and not present in a downstream build. So we
    // submit our OWN anchor here to exercise the present-source path.
    const PRESENT: SourceTag = SourceTag("leaf-boot::anti_dce::test");

    #[leaf_core::linkme::distributed_slice(SOURCES)]
    #[linkme(crate = leaf_core::linkme)]
    static SUBMITTED: SourceTag = PRESENT;

    #[test]
    fn an_empty_manifest_always_passes() {
        self_check(&[]).expect("nothing expected, nothing can vanish");
    }

    #[test]
    fn a_present_source_passes_the_check() {
        // Our submitted anchor is link-collected, so expecting it passes.
        self_check(&[PRESENT]).expect("a present source passes");
    }

    #[test]
    fn a_vanished_source_is_a_loud_error_naming_the_crate() {
        let err = self_check(&[SourceTag("leaf-never-linked")])
            .expect_err("an absent source vanished");
        assert_eq!(
            err,
            AntiDceError::SourceVanished { crate_name: "leaf-never-linked" }
        );
        // The message names the crate + the remediation.
        let msg = err.to_string();
        assert!(msg.contains("leaf-never-linked"), "got: {msg}");
        assert!(msg.contains("force-link"), "got: {msg}");
    }

    #[test]
    fn the_first_vanished_source_is_reported() {
        // Two missing crates: the FIRST (deterministic) is reported.
        let err = self_check(&[SourceTag("aaa-missing"), SourceTag("zzz-missing")])
            .expect_err("vanished");
        assert_eq!(err, AntiDceError::SourceVanished { crate_name: "aaa-missing" });
    }

    #[test]
    fn anti_dce_lifts_into_the_one_leaf_error_spine() {
        let err = AntiDceError::SourceVanished { crate_name: "leaf-x" };
        let leaf: LeafError = err.into();
        assert_eq!(leaf.kind, ErrorKind::AntiDce);
        // The cause chain narrates the vanished source.
        assert!(leaf.to_string().contains("leaf-x"), "got: {leaf}");
    }

    #[test]
    fn a_present_source_among_an_absent_one_still_fails_on_the_absent() {
        let err = self_check(&[PRESENT, SourceTag("leaf-absent")]).expect_err("absent fails");
        assert_eq!(err, AntiDceError::SourceVanished { crate_name: "leaf-absent" });
    }
}
