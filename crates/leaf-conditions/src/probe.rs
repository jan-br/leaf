//! The no-instantiation candidate-query seam the `OnBean` family delegates to.
//!
//! conditions-autoconfig (phase3/05): `OnBean`/`OnMissingBean`/`OnSingleCandidate`
//! delegate their verdict to a [`DefinitionProbe`] that runs the candidate
//! resolver's primary/fallback/qualifier policy over the DEFINITION view with
//! ZERO instantiation, returning a [`Resolvability`]
//! (`None`/`Unique`/`Ambiguous`) — the SAME primitive `App<Wired>` whole-graph
//! validation uses, so there is exactly one definition of "unambiguous".
//!
//! ## Why an ambient probe scope
//!
//! The frozen leaf-core [`ConditionCtx`](leaf_core::ConditionCtx) is
//! `#[non_exhaustive]` and currently carries ONLY the always-available sealed
//! `&Env` and the `&dyn ReportSink` — the `probe`/`defs` borrows are documented
//! there as deferred to later registry/injection units. A `CONDITIONS` row is a
//! `&'static dyn Condition` singleton, but the probe is a per-assembly value
//! over the growing `RegistryBuilder`; it cannot be baked into the singleton.
//!
//! Until the kernel ABI grows a `probe` field on `ConditionCtx`, leaf-boot
//! installs the per-assembly probe into the [`with_probe`] ambient scope around
//! the Register sub-pass; the `OnBean`-family impls read it through
//! [`current_probe`](crate::current_probe_query). This is a thin, honest, `unsafe`-free bridge — the probe
//! is shared as an [`Arc`] for the cold, synchronous `App<Resolve>` scope, with NO
//! global lock — and collapses to a direct `ctx.probe` field read once the
//! kernel exposes it.

use std::any::TypeId;
use std::cell::RefCell;
use std::sync::Arc;

pub use leaf_core::Resolvability;

/// The no-instantiation candidate-resolver SPI (conditions-autoconfig): given a
/// queried type, report whether the DEFINITION set so far resolves it uniquely.
///
/// The verdict runs the SAME primary/fallback/qualifier policy as injection (a
/// lone `@Fallback` resolves; a `@Primary`-among-several is `Unique`), never a
/// naive count and never a construction. leaf-boot supplies the concrete impl
/// over the growing `RegistryBuilder`; tests supply a stub.
pub trait DefinitionProbe: Send + Sync {
    /// Whether the queried type would resolve to a unique candidate.
    fn would_resolve_unique(&self, ty: TypeId) -> Resolvability;
}

thread_local! {
    static CURRENT_PROBE: RefCell<Option<Arc<dyn DefinitionProbe>>> =
        const { RefCell::new(None) };
}

/// A scope guard that restores the previous ambient probe on drop (panic-safe).
#[must_use = "dropping the guard immediately uninstalls the probe"]
pub struct ProbeScope {
    prev: Option<Arc<dyn DefinitionProbe>>,
}

impl Drop for ProbeScope {
    fn drop(&mut self) {
        CURRENT_PROBE.with(|c| *c.borrow_mut() = self.prev.take());
    }
}

/// Install `probe` as the ambient definition probe, returning a [`ProbeScope`]
/// guard; the previous probe is restored when the guard drops (scopes nest).
pub fn install_probe(probe: Arc<dyn DefinitionProbe>) -> ProbeScope {
    let prev = CURRENT_PROBE.with(|c| c.borrow_mut().replace(probe));
    ProbeScope { prev }
}

/// Run `f` with `probe` installed as the ambient definition probe for any
/// `OnBean`-family conditions evaluated inside it (leaf-boot's Register sub-pass).
pub fn with_probe<R>(probe: Arc<dyn DefinitionProbe>, f: impl FnOnce() -> R) -> R {
    let _scope = install_probe(probe);
    f()
}

/// Query the ambient probe with `ty`, returning `None` when no probe is
/// installed (outside a [`with_probe`]/[`install_probe`] scope).
///
/// A missing probe is NOT an error here — the calling condition decides how to
/// degrade (the `OnBean` family treats "no probe" as "no candidate set yet").
#[must_use]
pub fn current_probe_query(ty: TypeId) -> Option<Resolvability> {
    CURRENT_PROBE.with(|c| c.borrow().as_ref().map(|p| p.would_resolve_unique(ty)))
}

/// Whether an ambient probe is currently installed.
#[must_use]
pub fn has_probe() -> bool {
    CURRENT_PROBE.with(|c| c.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubProbe(Resolvability);
    impl DefinitionProbe for StubProbe {
        fn would_resolve_unique(&self, _ty: TypeId) -> Resolvability {
            self.0
        }
    }

    #[test]
    fn no_probe_installed_yields_none() {
        assert!(!has_probe());
        assert_eq!(current_probe_query(TypeId::of::<u32>()), None);
    }

    #[test]
    fn with_probe_makes_it_queryable() {
        let probe: Arc<dyn DefinitionProbe> = Arc::new(StubProbe(Resolvability::Unique(7)));
        with_probe(probe, || {
            assert!(has_probe());
            assert_eq!(
                current_probe_query(TypeId::of::<u32>()),
                Some(Resolvability::Unique(7))
            );
        });
        assert!(!has_probe(), "probe is uninstalled after the scope");
    }

    #[test]
    fn scopes_nest_and_restore() {
        let outer: Arc<dyn DefinitionProbe> = Arc::new(StubProbe(Resolvability::None));
        let inner: Arc<dyn DefinitionProbe> = Arc::new(StubProbe(Resolvability::Ambiguous(3)));
        with_probe(outer, || {
            assert_eq!(
                current_probe_query(TypeId::of::<u8>()),
                Some(Resolvability::None)
            );
            with_probe(inner, || {
                assert_eq!(
                    current_probe_query(TypeId::of::<u8>()),
                    Some(Resolvability::Ambiguous(3))
                );
            });
            assert_eq!(
                current_probe_query(TypeId::of::<u8>()),
                Some(Resolvability::None),
                "the outer probe is restored after the inner scope"
            );
        });
    }
}
