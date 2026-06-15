//! `run_autoconfig` — the `exclude > back-off > default` ladder over `AUTO_CONFIGS`.
//!
//! conditions-autoconfig (phase3/05) `auto-configuration`: the thin `App<Resolve>`
//! Register-sub-pass orchestrator over the dedicated `AUTO_CONFIGS` channel. It
//! runs ONE cold synchronous pass, AFTER `seal_environment` + all user defs:
//!
//! 1. **kill-switch** — if `leaf.enable-autoconfiguration == false`, the whole
//!    batch is skipped before exclusions (the global off-switch).
//! 2. **exclude** — a candidate whose [`leaf_core::ContractId`] is in the
//!    [`ExclusionSet`] mints NO bean (records `Exclusion`, never enters back-off).
//! 3. **back-off** — the candidate's guard (typically an `OnMissingBean`) is
//!    evaluated over the GROWING definition set; a miss records `Negative`.
//! 4. **default** — a surviving candidate registers at
//!    [`leaf_core::CandidateRole::FALLBACK`] (the soft override: a user bean of
//!    the same contract transparently supersedes), INCREMENTALLY so each later
//!    candidate's `OnMissingBean`/`OnSingleCandidate`
//!    [`DefinitionProbe`](leaf_conditions::DefinitionProbe) sees it.
//!
//! Registration is incremental against a [`BuilderProbe`] mirroring the
//! candidate-resolver's "unique" verdict over the (user + so-far-registered)
//! definitions — the SAME primary/fallback policy injection runs, so there is one
//! definition of "unambiguous".

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use leaf_core::{
    CandidateRole, ConditionRecord, ConditionReport, ConditionReportClass, ContractId, Descriptor,
    Env, LeafError, Provider, ProviderSeed, RegistryBuilder, ReportSink, Resolvability,
};

use leaf_conditions::DefinitionProbe;

use crate::conditions::{CollectingSink, GuardPairing};

/// The relaxed env list-property carrying name-based exclusions.
const EXCLUDE_PROPERTY: &str = "leaf.autoconfigure.exclude";
/// The kill-switch property (the global auto-configuration off-switch).
const ENABLE_PROPERTY: &str = "leaf.enable-autoconfiguration";

/// The merged auto-config exclusion set (auto-config-ordering): the three-source
/// merge (typed `exclude=[ContractId]`, `exclude_name` strings, and the
/// `leaf.autoconfigure.exclude` env list), all normalized to [`ContractId`].
#[derive(Clone, Debug, Default)]
pub struct ExclusionSet {
    excluded: HashSet<ContractId>,
}

impl ExclusionSet {
    /// An empty exclusion set.
    #[must_use]
    pub fn new() -> Self {
        ExclusionSet::default()
    }

    /// Merge the three exclusion sources into one [`ContractId`]-keyed set: the
    /// compile-known typed `exclude=[..]`, the `exclude_name` strings (hashed to
    /// `ContractId`), and the relaxed `leaf.autoconfigure.exclude` env list.
    #[must_use]
    pub fn merge(typed: &[ContractId], names: &[&str], env: &Env) -> Self {
        let mut excluded: HashSet<ContractId> = typed.iter().copied().collect();
        for n in names {
            excluded.insert(ContractId::of(n));
        }
        if let Some(rv) = leaf_core::PropertyResolver::get(env, EXCLUDE_PROPERTY) {
            for name in rv.raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                excluded.insert(ContractId::of(name));
            }
        }
        ExclusionSet { excluded }
    }

    /// Add one excluded contract.
    pub fn insert(&mut self, c: ContractId) {
        self.excluded.insert(c);
    }

    /// Whether `c` is excluded.
    #[must_use]
    pub fn contains(&self, c: ContractId) -> bool {
        self.excluded.contains(&c)
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.excluded.is_empty()
    }
}

/// One auto-config candidate: its const [`Descriptor`], the [`ProviderSeed`] that
/// BUILDS its `Provider`, and the optional back-off guard tree.
///
/// The macro emits the `Descriptor` into `AUTO_CONFIGS` at
/// [`CandidateRole::FALLBACK`] and the `__leaf_seed_<Ident>` / `__leaf_guard_<Ident>`
/// consts beside it; the binary pairs them into this row (the same JOIN as the
/// `assembly` seed pairing + the `conditions` guard pairing).
#[derive(Clone, Copy)]
pub struct AutoConfigCandidate {
    /// The candidate's const definition row.
    pub descriptor: Descriptor,
    /// The const fn-pointer building the candidate's `Provider`.
    pub seed: ProviderSeed,
    /// The back-off guard (`None` = unconditional, registers at Fallback always).
    pub guard: Option<&'static leaf_core::CondExpr>,
}

impl AutoConfigCandidate {
    /// Build a candidate row from its definition, seed, and optional guard.
    #[must_use]
    pub fn new(
        descriptor: Descriptor,
        seed: ProviderSeed,
        guard: Option<&'static leaf_core::CondExpr>,
    ) -> Self {
        AutoConfigCandidate { descriptor, seed, guard }
    }
}

impl std::fmt::Debug for AutoConfigCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoConfigCandidate")
            .field("contract", &self.descriptor.contract)
            .finish_non_exhaustive()
    }
}

/// The incremental no-instantiation candidate probe over the GROWING definition
/// set (the `OnBean` family + the auto-config back-off both read it).
///
/// Tracks, per `TypeId`, the candidate count and whether any candidate is
/// NON-fallback (a user/plain bean). The verdict mirrors the candidate-resolver:
/// a lone candidate resolves `Unique`; a non-fallback among several wins
/// (`Unique`); several with no clear winner is `Ambiguous`; none is `None`. So
/// `OnMissingBean(T)` (`!= Unique`) backs off exactly when T already resolves.
#[derive(Default)]
pub struct BuilderProbe {
    /// `self_type` → (total candidates, non-fallback count).
    by_type: std::sync::Mutex<HashMap<TypeId, (u16, u16)>>,
}

impl BuilderProbe {
    /// An empty probe (no definitions registered yet).
    #[must_use]
    pub fn new() -> Self {
        BuilderProbe::default()
    }

    /// Seed the probe with an already-registered definition (a user bean lifted
    /// before the auto-config pass).
    pub fn observe(&self, self_type: TypeId, role: CandidateRole) {
        if let Ok(mut g) = self.by_type.lock() {
            let e = g.entry(self_type).or_insert((0, 0));
            e.0 = e.0.saturating_add(1);
            if !role.is_fallback() {
                e.1 = e.1.saturating_add(1);
            }
        }
    }
}

impl DefinitionProbe for BuilderProbe {
    fn would_resolve_unique(&self, ty: TypeId) -> Resolvability {
        let g = match self.by_type.lock() {
            Ok(g) => g,
            Err(_) => return Resolvability::None,
        };
        match g.get(&ty).copied() {
            None | Some((0, _)) => Resolvability::None,
            Some((1, _)) => Resolvability::Unique(0),
            // Several candidates: a single non-fallback (the user override) wins.
            Some((_, 1)) => Resolvability::Unique(0),
            Some((n, _)) => Resolvability::Ambiguous(n),
        }
    }
}

/// The product of [`run_autoconfig`]: the count of registered survivors + the
/// frozen [`ConditionReport`] of every back-off/exclusion verdict.
#[derive(Debug)]
pub struct AutoConfigOutcome {
    /// The number of auto-config candidates that registered (at Fallback).
    pub registered: usize,
    /// The frozen report (Exclusion / Negative / Positive rows for the batch).
    pub report: ConditionReport,
}

/// Run the auto-config ladder over `candidates`, registering survivors
/// incrementally into `builder` at [`CandidateRole::FALLBACK`].
///
/// `seed_probe` carries the user/plain definitions already lifted into `builder`
/// (their `(self_type, role)`) so the first candidate's back-off sees them. The
/// kill-switch is read first; exclusions short-circuit before back-off; each
/// survivor's register grows the probe for later candidates.
///
/// # Errors
/// A [`LeafError`] if a guard leaf is unresolvable (anti-DCE), or the builder's
/// loud name/collision guard fires at `register`.
pub fn run_autoconfig(
    candidates: &[AutoConfigCandidate],
    env: &Env,
    builder: &mut RegistryBuilder,
    exclusions: &ExclusionSet,
    profiles: &leaf_core::ActiveProfiles,
    seed_probe: &[(TypeId, CandidateRole)],
) -> Result<AutoConfigOutcome, LeafError> {
    let sink = CollectingSink::new();

    // ── 1. kill-switch (read FIRST, short-circuits the whole batch) ────────────
    if leaf_core::PropertyResolver::get_as::<bool>(env, ENABLE_PROPERTY)
        .ok()
        .flatten()
        == Some(false)
    {
        return Ok(AutoConfigOutcome { registered: 0, report: sink.freeze() });
    }

    // The incremental probe, seeded with the already-registered user defs.
    let probe = Arc::new(BuilderProbe::new());
    for (ty, role) in seed_probe {
        probe.observe(*ty, *role);
    }

    let mut registered = 0usize;

    for c in candidates {
        let contract = c.descriptor.contract;

        // ── 2. exclude (mints no bean, never enters back-off) ──────────────────
        if exclusions.contains(contract) {
            sink.record(ConditionRecord {
                element: contract,
                self_type: Some(c.descriptor.self_type),
                class: ConditionReportClass::Exclusion(contract),
                leaves: Box::new([]),
            });
            continue;
        }

        // ── 3. back-off (the guard over the GROWING set; probe + profiles in) ──
        let matched = match c.guard {
            None => {
                // Unconditional auto-config: always registers (at Fallback).
                sink.record(ConditionRecord {
                    element: contract,
                    self_type: Some(c.descriptor.self_type),
                    class: ConditionReportClass::Unconditional,
                    leaves: Box::new([]),
                });
                true
            }
            Some(guard) => {
                let pairing =
                    GuardPairing::new(contract, Some(c.descriptor.self_type), guard);
                let probe_clone: Arc<dyn DefinitionProbe> = probe.clone();
                crate::conditions::evaluate_guard_in_register(
                    &pairing,
                    env,
                    &sink,
                    profiles,
                    probe_clone,
                )?
            }
        };

        // ── 4. default (register the survivor at Fallback, INCREMENTALLY) ───────
        if matched {
            register_fallback(builder, c)?;
            probe.observe(c.descriptor.self_type, c.descriptor.meta.candidate_role);
            registered += 1;
        }
    }

    Ok(AutoConfigOutcome { registered, report: sink.freeze() })
}

/// Register one candidate at [`CandidateRole::FALLBACK`], minting its `Provider`
/// via the seed. The descriptor already carries `FALLBACK` in `meta` (the macro
/// emits `auto_config_role()`); the seed is invoked ONCE here.
fn register_fallback(
    builder: &mut RegistryBuilder,
    c: &AutoConfigCandidate,
) -> Result<(), LeafError> {
    let provider: Arc<dyn Provider> = (c.seed)();
    builder.register(c.descriptor, provider)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{
        AnnotationMetadata, Attr, AttrSlice, BoxFuture, CondExpr, EnvBuilder, MapPropertySource,
        Origin, Provider, Published, ResolveCtx, Role, ScopeDef,
    };
    use leaf_conditions::{OnMissingBean, ConditionKind};

    // ── a probe-able bean type + provider the seed builds ──
    #[derive(Debug)]
    struct AutoPool;

    struct PoolProvider {
        descriptor: Descriptor,
    }
    impl Provider for PoolProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(AutoPool)) })
        }
    }

    // The auto-config descriptor at FALLBACK role.
    static FALLBACK_META: AnnotationMetadata = AnnotationMetadata {
        qualifiers: &[],
        markers: &[],
        depends_on: &[],
        candidate_role: CandidateRole::FALLBACK,
        autowire_candidate: true,
    };

    fn auto_descriptor(name: &'static str, contract: &str) -> Descriptor {
        Descriptor {
            contract: ContractId::of(contract),
            self_type: TypeId::of::<AutoPool>(),
            provides: &[],
            declared_name: Some(name),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &FALLBACK_META,
            parent: None,
            origin: Origin::Native { crate_name: Some("leaf-boot::test") },
        }
    }

    fn auto_seed() -> Arc<dyn Provider> {
        Arc::new(PoolProvider { descriptor: auto_descriptor("autoPool", "test::AutoPool") })
    }

    fn candidate(
        name: &'static str,
        contract: &str,
        guard: Option<&'static CondExpr>,
    ) -> AutoConfigCandidate {
        AutoConfigCandidate::new(auto_descriptor(name, contract), auto_seed, guard)
    }

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let src = MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    // An OnMissingBean(AutoPool) back-off guard.
    fn on_missing_autopool() -> &'static CondExpr {
        let attrs: AttrSlice = Box::leak(Box::new([Attr::Type("type", TypeId::of::<AutoPool>())]));
        Box::leak(Box::new(CondExpr::Leaf(OnMissingBean::ID, attrs)))
    }

    #[test]
    fn an_unconditional_auto_config_registers_at_fallback() {
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", None)];
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 1);
        assert_eq!(builder.len(), 1);
    }

    #[test]
    fn exclude_removes_a_candidate_before_back_off() {
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", None)];
        let mut excl = ExclusionSet::new();
        excl.insert(ContractId::of("test::AutoConfig"));
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &excl,
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 0, "an excluded candidate mints no bean");
        assert_eq!(builder.len(), 0);
        let rec = out.report.lookup(ContractId::of("test::AutoConfig")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Exclusion(_)));
    }

    #[test]
    fn exclude_by_env_list_property() {
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", None)];
        let env = env_with(&[("leaf.autoconfigure.exclude", "test::AutoConfig")]);
        let excl = ExclusionSet::merge(&[], &[], &env);
        let out = run_autoconfig(
            &cands,
            &env,
            &mut builder,
            &excl,
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 0);
    }

    #[test]
    fn a_user_bean_beats_an_auto_config_fallback_via_on_missing_bean() {
        // A user @Component of type AutoPool is already registered (seed the probe
        // with a NON-fallback def of that type). The auto-config's OnMissingBean
        // back-off then sees it and does NOT register — the user bean supersedes.
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", Some(on_missing_autopool()))];
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            // A user (non-fallback) bean of type AutoPool already present.
            &[(TypeId::of::<AutoPool>(), CandidateRole::NORMAL)],
        )
        .expect("runs");
        assert_eq!(out.registered, 0, "OnMissingBean backs off: the user bean wins");
        let rec = out.report.lookup(ContractId::of("test::AutoConfig")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
    }

    #[test]
    fn on_missing_bean_registers_when_no_user_bean_present() {
        // No user bean of that type → OnMissingBean matches → the default registers.
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", Some(on_missing_autopool()))];
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 1, "no user bean → the auto-config default applies");
    }

    #[test]
    fn kill_switch_short_circuits_the_whole_batch() {
        let mut builder = RegistryBuilder::new();
        let cands = [candidate("autoPool", "test::AutoConfig", None)];
        let env = env_with(&[("leaf.enable-autoconfiguration", "false")]);
        let out = run_autoconfig(
            &cands,
            &env,
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 0, "the kill-switch skips the whole batch");
        assert_eq!(builder.len(), 0);
        assert!(out.report.is_empty(), "no exclusion/back-off rows recorded when killed");
    }

    #[test]
    fn incremental_registration_lets_a_later_candidate_see_an_earlier_one() {
        // Two candidates of the SAME type: the first (unconditional) registers at
        // Fallback; the second's OnMissingBean then sees the lone Fallback resolve
        // as Unique and backs off — proving the register is incremental.
        let mut builder = RegistryBuilder::new();
        let first = candidate("autoPoolA", "test::AutoConfigA", None);
        let second = candidate("autoPoolB", "test::AutoConfigB", Some(on_missing_autopool()));
        let out = run_autoconfig(
            &[first, second],
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 1, "the first registers; the second backs off seeing it");
        let rec = out.report.lookup(ContractId::of("test::AutoConfigB")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
    }
}
