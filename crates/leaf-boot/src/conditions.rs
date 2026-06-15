//! `route_conditions` — the `App<Resolve>` Parse-then-Register condition driver.
//!
//! conditions-autoconfig (phase3/05): the runtime-tier [`leaf_core::CondExpr`]
//! leaves are evaluated over the sealed [`leaf_core::Env`] in TWO sub-passes:
//!
//! - **Parse** — guards whose inferred [`CondExpr::phase`] is `Parse` (no
//!   `OnBean` family leaf). They read ONLY the sealed `Env`/`ActiveProfiles`, so
//!   they decide independent of the growing definition set.
//! - **Register** — guards with any `OnBean`-family leaf
//!   ([`SubPhase::Register`]). They must see the growing definition set, so they
//!   run with the per-assembly
//!   [`DefinitionProbe`](leaf_conditions::DefinitionProbe) installed in
//!   the ambient scope the family impls read.
//!
//! Every verdict is recorded into ONE [`leaf_core::ConditionReport`] over the
//! six-class [`leaf_core::ConditionReportClass`] taxonomy: a match is `Positive`,
//! a miss is `Negative(reason)` (NOT an error — the silent-now/loud-later bridge),
//! an unconditional element is `Unconditional`, a build-folded-false leaf is
//! `BuildFoldedFalse`. An unresolved `ConditionId` is the loud `ConditionError`.
//!
//! ## The guard JOIN
//!
//! The frozen `Descriptor.meta` ([`leaf_core::AnnotationMetadata`]) carries NO
//! `CondExpr` field, so the macro emits the guard tree as a public
//! `__leaf_guard_<Ident>` const beside each gated element and leaf-boot completes
//! the `Descriptor → CondExpr` pairing HERE — exactly like the `ProviderSeed`
//! JOIN in [`crate::assembly`]. A [`GuardPairing`] is one such pairing row.

use std::any::TypeId;
use std::sync::Arc;
use std::sync::Mutex;

use leaf_core::{
    evaluate, ActiveProfiles, CondExpr, ConditionCtx, ConditionId, ConditionRecord, ConditionReport,
    ConditionReportClass, ContractId, Env, LeafError, LeafOutcome, ReasonMsg, ReportSink,
    SubPhase, UNCONDITIONAL,
};

use leaf_conditions::{
    resolve as resolve_condition, with_probe, ConditionKind, DefinitionProbe, OnBean, OnMissingBean,
    OnSingleCandidate,
};

/// The Register-phase kind ids (the `OnBean` family). A bare
/// [`CondExpr::Leaf`]'s [`CondExpr::phase`] reads `Parse` because it cannot
/// introspect the opaque [`ConditionId`] (documented in leaf-core); leaf-boot
/// owns the sub-pass sequencing, so it refines the per-leaf `SUB` by consulting
/// these known Register-phase kinds — exactly the `ConditionKind::SUB` the design
/// says the framework computes from each member.
fn is_register_kind(id: ConditionId) -> bool {
    id == OnBean::ID || id == OnMissingBean::ID || id == OnSingleCandidate::ID
}

/// The framework-inferred sub-phase of a guard = `max` over leaves, with each
/// leaf's `SUB` resolved against the known kind catalog (an `OnBean`-family leaf
/// nested anywhere defers the WHOLE guard to `Register`).
#[must_use]
fn guard_phase(guard: &CondExpr) -> SubPhase {
    match guard {
        CondExpr::Const(_) => SubPhase::Parse,
        CondExpr::Leaf(id, _) => {
            if is_register_kind(*id) {
                SubPhase::Register
            } else {
                SubPhase::Parse
            }
        }
        CondExpr::Not(inner) => guard_phase(inner),
        CondExpr::All(children) | CondExpr::Any(children) => children
            .iter()
            .fold(SubPhase::Parse, |acc, c| acc.max(guard_phase(c))),
    }
}

/// One macro-emitted `Descriptor → CondExpr` guard pairing, keyed by the gated
/// element's stable [`ContractId`].
///
/// The binary crate (`#[leaf::main]` / `build.rs`) emits one row per gated
/// element — `GuardPairing { contract, self_type, guard: &crate::__leaf_guard_Ty }`
/// — and [`route_conditions`] JOINs each definition against this table by
/// `contract`. The `guard` is a `&'static CondExpr` because the macro emits a
/// `pub const __leaf_guard_<Ident>: CondExpr` (a const tree).
#[derive(Clone, Copy)]
pub struct GuardPairing {
    /// The gated element's stable cross-build identity (the JOIN + report key).
    pub contract: ContractId,
    /// The element's `TypeId` (the report's fast secondary key), if known.
    pub self_type: Option<TypeId>,
    /// The const guard tree (the macro-emitted `__leaf_guard_<Ident>`).
    pub guard: &'static CondExpr,
}

impl GuardPairing {
    /// Build a guard pairing from a gated element's identity + its guard tree.
    #[must_use]
    pub fn new(contract: ContractId, self_type: Option<TypeId>, guard: &'static CondExpr) -> Self {
        GuardPairing { contract, self_type, guard }
    }
}

impl std::fmt::Debug for GuardPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardPairing")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

/// A cold-path accumulating [`ReportSink`] — appends each [`ConditionRecord`]
/// behind a `Mutex` (the cold assembly pass is single-threaded, so the lock is
/// never contended; it satisfies the `Send + Sync` bound the borrowed
/// `&dyn ReportSink` carries through `ConditionCtx`). Frozen into a
/// [`ConditionReport`] at the end of the pass.
#[derive(Default)]
pub struct CollectingSink {
    records: Mutex<Vec<ConditionRecord>>,
}

impl CollectingSink {
    /// A fresh empty sink.
    #[must_use]
    pub fn new() -> Self {
        CollectingSink::default()
    }

    /// Freeze the accumulated records into the keyed [`ConditionReport`].
    #[must_use]
    pub fn freeze(self) -> ConditionReport {
        ConditionReport::from_records(self.records.into_inner().unwrap_or_default())
    }
}

impl ReportSink for CollectingSink {
    fn record(&self, rec: ConditionRecord) {
        if let Ok(mut g) = self.records.lock() {
            g.push(rec);
        }
    }
}

/// Whether a guard is the vacuously-true unconditional guard (`All([])`) — the
/// value an element with no `#[conditional]` carries.
#[must_use]
fn is_unconditional(guard: &CondExpr) -> bool {
    matches!(guard, CondExpr::All(children) if children.is_empty())
}

/// Evaluate ONE guard against the sealed `Env`, returning whether it matched and
/// recording the verdict class into `sink`.
///
/// The resolver is leaf-conditions' catalog (the production path force-links the
/// `CONDITIONS` channel; this drives the same impls). `OnProfile` reads the
/// sealed [`ActiveProfiles`] threaded onto the [`ConditionCtx`] via
/// [`ConditionCtx::with_profiles`]; an `OnBean`-family leaf still reads the
/// ambient [`DefinitionProbe`](leaf_conditions::DefinitionProbe) the caller
/// installs for the Register sub-pass.
///
/// # Errors
/// A [`LeafError`] (`ConditionError`) iff a leaf's [`ConditionId`] is unresolved
/// (the anti-DCE "condition family not force-linked" guard) — never a silent
/// pass-all. A condition that simply does not match is `Ok(false)`, recorded
/// `Negative`.
fn evaluate_guard(
    pairing: &GuardPairing,
    env: &Env,
    sink: &dyn ReportSink,
    profiles: &ActiveProfiles,
) -> Result<bool, LeafError> {
    let guard = pairing.guard;

    if is_unconditional(guard) {
        sink.record(ConditionRecord {
            element: pairing.contract,
            self_type: pairing.self_type,
            class: ConditionReportClass::Unconditional,
            leaves: Box::new([]),
        });
        return Ok(true);
    }

    let ctx = ConditionCtx::new(env, sink).with_profiles(profiles);
    let outcome = evaluate(guard, &ctx, &|id: ConditionId| resolve_condition(id))?;

    let class = if outcome.matched {
        ConditionReportClass::Positive
    } else if let CondExpr::Const(false) = guard {
        // A build-folded `false` leaf decided at build.
        ConditionReportClass::BuildFoldedFalse(folded_false_id(guard))
    } else {
        ConditionReportClass::Negative(outcome.reason.clone())
    };

    let leaves = top_leaf(guard, &outcome.reason);
    sink.record(ConditionRecord {
        element: pairing.contract,
        self_type: pairing.self_type,
        class,
        leaves,
    });
    Ok(outcome.matched)
}

/// Evaluate ONE guard in the Register sub-pass with `probe` installed in the
/// ambient scope the `OnBean`-family impls read and `profiles` threaded onto the
/// [`ConditionCtx`] for `OnProfile`, recording the verdict into `sink`. The
/// incremental [`run_autoconfig`] pass calls this per candidate so each guard
/// sees the growing definition set.
///
/// [`run_autoconfig`]: crate::run_autoconfig
///
/// # Errors
/// A [`LeafError`] (`ConditionError`) iff a leaf's [`ConditionId`] is unresolved.
pub(crate) fn evaluate_guard_in_register(
    pairing: &GuardPairing,
    env: &Env,
    sink: &dyn ReportSink,
    profiles: &ActiveProfiles,
    probe: Arc<dyn DefinitionProbe>,
) -> Result<bool, LeafError> {
    with_probe(probe, || evaluate_guard(pairing, env, sink, profiles))
}

/// A best-effort top-level leaf breakdown for the report (the full per-leaf
/// trace is a later condition-report enrichment; this records the deciding
/// leaf's id + reason when the guard is a single leaf).
fn top_leaf(guard: &CondExpr, reason: &ReasonMsg) -> Box<[LeafOutcome]> {
    match guard {
        CondExpr::Leaf(id, _) => Box::new([LeafOutcome {
            id: *id,
            matched: false,
            reason: reason.clone(),
        }]),
        _ => Box::new([]),
    }
}

/// A placeholder `ConditionId` for a const-folded guard (the `Const(false)` case
/// carries no id; use the unconditional sentinel's tier marker).
fn folded_false_id(_guard: &CondExpr) -> ConditionId {
    ConditionId(0)
}

/// Route the runtime-tier conditions over a batch of guards in the Parse then
/// Register sub-passes, returning the set of element [`ContractId`]s whose guard
/// MATCHED plus the frozen [`ConditionReport`].
///
/// Parse-phase guards (no `OnBean` leaf) decide first over the sealed `Env`;
/// Register-phase guards run with `probe`/`profiles` installed so an `OnBean`
/// family leaf sees the (caller-supplied) definition set. A guard's phase is the
/// framework-inferred [`CondExpr::phase`] (`max` over leaves), so the macro never
/// declares it.
///
/// # Errors
/// A [`LeafError`] (`ConditionError`) iff any leaf's [`ConditionId`] is
/// unresolved (anti-DCE), surfaced from the first failing guard.
pub fn route_conditions(
    guards: &[GuardPairing],
    env: &Env,
    profiles: &ActiveProfiles,
    probe: Arc<dyn DefinitionProbe>,
) -> Result<RouteOutcome, LeafError> {
    let sink = CollectingSink::new();
    let mut matched: Vec<ContractId> = Vec::new();

    // ── Parse sub-pass: guards with no OnBean leaf (decide over the sealed Env) ─
    // OnProfile leaves are Parse-phase too; they read the active set straight off
    // the `ConditionCtx` (`evaluate_guard` threads `profiles` in).
    for p in guards {
        if guard_phase(p.guard) == SubPhase::Parse {
            match evaluate_guard(p, env, &sink, profiles) {
                Ok(true) => matched.push(p.contract),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
    }

    // ── Register sub-pass: OnBean-family guards over the growing definition set ─
    let register_result = with_probe(probe, || {
        for p in guards {
            if guard_phase(p.guard) == SubPhase::Register {
                match evaluate_guard(p, env, &sink, profiles) {
                    Ok(true) => matched.push(p.contract),
                    Ok(false) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    });
    register_result?;

    Ok(RouteOutcome { matched, report: sink.freeze() })
}

/// The product of [`route_conditions`]: the matched elements + the frozen report.
#[derive(Debug)]
pub struct RouteOutcome {
    /// The [`ContractId`]s whose guard matched (registration proceeds for these).
    pub matched: Vec<ContractId>,
    /// The frozen condition report (every verdict, keyed by `ContractId`).
    pub report: ConditionReport,
}

impl RouteOutcome {
    /// Whether `contract`'s guard matched.
    #[must_use]
    pub fn is_matched(&self, contract: ContractId) -> bool {
        self.matched.contains(&contract)
    }
}

/// The unconditional guard pointer (re-exported for binaries hand-writing a
/// guard pairing for an unguarded element).
pub const UNCONDITIONAL_GUARD: &CondExpr = &UNCONDITIONAL;

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{Attr, AttrSlice, EnvBuilder, MapPropertySource, Resolvability};
    use leaf_conditions::{OnMissingBean, OnProperty};
    use leaf_core::ConditionKind;

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let src = MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    struct EmptyProbe;
    impl DefinitionProbe for EmptyProbe {
        fn would_resolve_unique(&self, _ty: TypeId) -> Resolvability {
            Resolvability::None
        }
    }
    struct UniqueProbe;
    impl DefinitionProbe for UniqueProbe {
        fn would_resolve_unique(&self, _ty: TypeId) -> Resolvability {
            Resolvability::Unique(0)
        }
    }

    struct Gated;

    // ── const guard trees (the macro-emitted shape) ──
    static ON_PROP_ATTRS: &[Attr] = &[Attr::Str("name", "feature.x")];
    static ON_PROP_GUARD: CondExpr = CondExpr::Leaf(OnProperty::ID, ON_PROP_ATTRS);

    #[test]
    fn an_unconditional_guard_matches_and_records_unconditional() {
        let env = env_with(&[]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), None, UNCONDITIONAL_GUARD);
        let out = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(EmptyProbe))
            .expect("routes");
        assert!(out.is_matched(ContractId::of("x::Bean")));
        let rec = out.report.lookup(ContractId::of("x::Bean")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Unconditional));
    }

    #[test]
    fn a_property_guard_gates_a_definition() {
        // Present → Positive + matched.
        let env = env_with(&[("feature.x", "true")]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), None, &ON_PROP_GUARD);
        let out = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(EmptyProbe))
            .expect("routes");
        assert!(out.is_matched(ContractId::of("x::Bean")));
        let rec = out.report.lookup(ContractId::of("x::Bean")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Positive));
    }

    #[test]
    fn a_property_guard_backs_off_when_absent_recording_negative() {
        // Absent → Negative + NOT matched (silent-now, loud-later).
        let env = env_with(&[]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), None, &ON_PROP_GUARD);
        let out = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(EmptyProbe))
            .expect("routes (a miss is not an error)");
        assert!(!out.is_matched(ContractId::of("x::Bean")));
        let rec = out.report.lookup(ContractId::of("x::Bean")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
    }

    #[test]
    fn a_const_false_guard_records_build_folded_false() {
        static FALSE_GUARD: CondExpr = CondExpr::Const(false);
        let env = env_with(&[]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), None, &FALSE_GUARD);
        let out = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(EmptyProbe))
            .expect("routes");
        assert!(!out.is_matched(ContractId::of("x::Bean")));
        let rec = out.report.lookup(ContractId::of("x::Bean")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::BuildFoldedFalse(_)));
    }

    #[test]
    fn on_missing_bean_runs_in_the_register_subpass_with_the_probe() {
        // An OnMissingBean leaf is Register-phase; with the EMPTY probe it matches
        // (no candidate yet); with a UNIQUE probe it backs off.
        let on_missing_attrs: AttrSlice = Box::leak(Box::new([Attr::Type(
            "type",
            TypeId::of::<Gated>(),
        )]));
        let guard: &'static CondExpr =
            Box::leak(Box::new(CondExpr::Leaf(OnMissingBean::ID, on_missing_attrs)));
        // leaf-boot refines the bare-leaf Parse default to Register via the kind id.
        assert_eq!(
            super::guard_phase(guard),
            SubPhase::Register,
            "OnMissingBean defers to Register"
        );

        let env = env_with(&[]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), Some(TypeId::of::<Gated>()), guard);

        let empty = route_conditions(
            std::slice::from_ref(&g),
            &env,
            &ActiveProfiles::default(),
            Arc::new(EmptyProbe),
        )
        .expect("routes");
        assert!(empty.is_matched(ContractId::of("x::Bean")), "no bean → OnMissingBean matches");

        let present = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(UniqueProbe))
            .expect("routes");
        assert!(
            !present.is_matched(ContractId::of("x::Bean")),
            "a unique bean → OnMissingBean backs off"
        );
    }

    #[test]
    fn an_unresolved_condition_id_is_a_loud_error() {
        static UNKNOWN: CondExpr = CondExpr::Leaf(ConditionId(0xDEAD_BEEF), &[]);
        let env = env_with(&[]);
        let g = GuardPairing::new(ContractId::of("x::Bean"), None, &UNKNOWN);
        let err = route_conditions(&[g], &env, &ActiveProfiles::default(), Arc::new(EmptyProbe))
            .expect_err("an unresolved ConditionId is loud, never a silent pass-all");
        assert_eq!(err.kind, leaf_core::ErrorKind::ConditionError);
    }
}
