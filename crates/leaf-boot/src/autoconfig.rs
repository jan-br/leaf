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
//! The batch is visited in [`OrderHint`](leaf_core::OrderHint) order (an `order`
//! sort + a `before`/`after`/`*_name` topological refinement — see
//! [`order_candidates`]) BEFORE the incremental loop, so a later-ordered
//! auto-config's guard sees earlier ones' registrations.
//!
//! Registration is incremental against a [`BuilderProbe`] mirroring the
//! candidate-resolver's "unique" verdict over the (user + so-far-registered)
//! definitions — the SAME primary/fallback policy injection runs, so there is one
//! definition of "unambiguous". The probe is `provides[]`-AWARE: it indexes each
//! definition's concrete `self_type` AND every `dyn Trait` view it provides, so an
//! `on_missing_bean(dyn V)` backs off when ANY bean of the view `V` is present
//! (Spring's `@ConditionalOnMissingBean(Interface)`).

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
    /// The auto-config-ordering hint ([`leaf_core::OrderHint::DEFAULT`] when unset) — the
    /// [`run_autoconfig`] batch is sorted by it BEFORE the incremental loop so a
    /// later-ordered candidate's guard sees earlier ones' registrations.
    pub order: leaf_core::OrderHint,
}

impl AutoConfigCandidate {
    /// Build a candidate row from its definition, seed, and optional guard, at the
    /// default order ([`leaf_core::OrderHint::DEFAULT`]).
    #[must_use]
    pub fn new(
        descriptor: Descriptor,
        seed: ProviderSeed,
        guard: Option<&'static leaf_core::CondExpr>,
    ) -> Self {
        AutoConfigCandidate {
            descriptor,
            seed,
            guard,
            order: leaf_core::OrderHint::DEFAULT,
        }
    }

    /// Build a candidate row carrying an explicit [`OrderHint`](leaf_core::OrderHint)
    /// (the auto-config-ordering data the [`run_autoconfig`] batch sort reads).
    #[must_use]
    pub fn with_order(
        descriptor: Descriptor,
        seed: ProviderSeed,
        guard: Option<&'static leaf_core::CondExpr>,
        order: leaf_core::OrderHint,
    ) -> Self {
        AutoConfigCandidate { descriptor, seed, guard, order }
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
    /// before the auto-config pass), indexing its concrete `self_type` ONLY.
    pub fn observe(&self, self_type: TypeId, role: CandidateRole) {
        self.observe_with_views(self_type, &[], role);
    }

    /// Seed the probe with a definition, indexing its concrete `self_type` AND
    /// every `dyn Trait` VIEW it provides — so a `provides[]`-aware `OnMissingBean`
    /// (`on_missing_bean(dyn V)`) probes the view TypeId and finds the bean
    /// (Spring's `@ConditionalOnMissingBean(Interface)`).
    ///
    /// Each view counts as one candidate FOR THAT VIEW under the same
    /// total/non-fallback accounting as `self_type`, so the resolver's
    /// primary/fallback verdict ([`would_resolve_unique`](Self::would_resolve_unique))
    /// is identical whether queried by concrete type or by view.
    pub fn observe_with_views(&self, self_type: TypeId, provides: &[TypeId], role: CandidateRole) {
        if let Ok(mut g) = self.by_type.lock() {
            for ty in std::iter::once(&self_type).chain(provides) {
                let e = g.entry(*ty).or_insert((0, 0));
                e.0 = e.0.saturating_add(1);
                if !role.is_fallback() {
                    e.1 = e.1.saturating_add(1);
                }
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

    // The incremental probe, seeded with the already-registered user defs (the
    // seed-probe carries each user bean's concrete type AND its provides[] views as
    // separate entries — see `assembly::component_seed_probe` — so a `dyn`-view
    // back-off sees a user bean of a differently-named concrete type).
    let probe = Arc::new(BuilderProbe::new());
    for (ty, role) in seed_probe {
        probe.observe(*ty, *role);
    }

    // ── auto-config ordering (auto-config-ordering): process candidates in
    // OrderHint order BEFORE the incremental loop, so a later-ordered candidate's
    // guard sees earlier ones' registrations. The sort is `order` then the
    // before/after topological refinement (the kill-switch/exclude/back-off ladder
    // is unchanged; only the visitation ORDER is fixed here).
    let ordered = order_candidates(candidates);

    let mut registered = 0usize;

    for &c in &ordered {
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
            // Index the survivor's concrete self_type AND its provides[] views, so a
            // later candidate's `on_missing_bean(dyn V)` sees the view it contributes.
            let views: Vec<TypeId> = c.descriptor.provides.iter().map(|r| r.view).collect();
            probe.observe_with_views(
                c.descriptor.self_type,
                &views,
                c.descriptor.meta.candidate_role,
            );
            registered += 1;
        }
    }

    Ok(AutoConfigOutcome { registered, report: sink.freeze() })
}

/// Order the auto-config batch (auto-config-ordering): a stable sort by
/// [`OrderHint::order`](leaf_core::OrderHint) (lower = earlier), then a stable
/// topological refinement honoring the `before`/`after` (typed [`ContractId`]) and
/// `before_name`/`after_name` (resolved against the candidate declared-name index)
/// edges. A cycle degrades gracefully to the `order`-sorted sequence (the edges are
/// a refinement, never a hard failure — Spring treats unsatisfiable ordering as a
/// best-effort hint).
fn order_candidates(candidates: &[AutoConfigCandidate]) -> Vec<&AutoConfigCandidate> {
    // The `order`-sorted base sequence (stable: equal-order keeps definition order).
    let mut base: Vec<&AutoConfigCandidate> = candidates.iter().collect();
    base.sort_by_key(|c| c.order.order);

    // No edges anywhere → the `order`-sorted sequence IS the answer (the common case).
    let has_edges = candidates.iter().any(|c| {
        !c.order.before.is_empty()
            || !c.order.after.is_empty()
            || !c.order.before_name.is_empty()
            || !c.order.after_name.is_empty()
    });
    if !has_edges {
        return base;
    }

    // Resolve the before/after edges to indices into `base`, then Kahn topo-sort
    // with the `order`-sorted sequence as the deterministic tie-break.
    let by_contract: HashMap<ContractId, usize> =
        base.iter().enumerate().map(|(i, c)| (c.descriptor.contract, i)).collect();
    let by_name: HashMap<&str, usize> = base
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.descriptor.declared_name.map(|n| (n, i)))
        .collect();

    let n = base.len();
    // `edge[a]` holds the set of nodes that must come AFTER `a` (a → b: a before b).
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    let add_edge = |from: usize, to: usize, adj: &mut Vec<Vec<usize>>, indeg: &mut Vec<usize>| {
        if from != to && !adj[from].contains(&to) {
            adj[from].push(to);
            indeg[to] += 1;
        }
    };

    for (i, c) in base.iter().enumerate() {
        // `before` targets must come AFTER this candidate → edge i → target.
        for t in c.order.before {
            if let Some(&j) = by_contract.get(t) {
                add_edge(i, j, &mut adj, &mut indeg);
            }
        }
        for name in c.order.before_name {
            if let Some(&j) = by_name.get(name) {
                add_edge(i, j, &mut adj, &mut indeg);
            }
        }
        // `after` targets must come BEFORE this candidate → edge target → i.
        for t in c.order.after {
            if let Some(&j) = by_contract.get(t) {
                add_edge(j, i, &mut adj, &mut indeg);
            }
        }
        for name in c.order.after_name {
            if let Some(&j) = by_name.get(name) {
                add_edge(j, i, &mut adj, &mut indeg);
            }
        }
    }

    // Kahn's algorithm with a `base`-order ready queue (smallest base index first →
    // the `order` sort is the deterministic tie-break among ready nodes).
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut out: Vec<&AutoConfigCandidate> = Vec::with_capacity(n);
    let mut emitted = vec![false; n];
    while !ready.is_empty() {
        ready.sort_unstable();
        let i = ready.remove(0);
        emitted[i] = true;
        out.push(base[i]);
        for &j in &adj[i] {
            indeg[j] -= 1;
            if indeg[j] == 0 {
                ready.push(j);
            }
        }
    }
    // A cycle leaves some nodes unemitted: append them in `base` order (graceful
    // degradation — the edges were an unsatisfiable hint, not a hard contract).
    if out.len() < n {
        for (i, c) in base.iter().enumerate() {
            if !emitted[i] {
                out.push(c);
            }
        }
    }
    out
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

    // The const upcast fn the dyn-Greeter provides[] view carries (re-erases the
    // bean's Arc through the dyn view); the body is never invoked in these probe
    // tests (the seed never runs), so a panic documents that.
    fn greeter_upcast(_b: leaf_core::ErasedBean) -> leaf_core::ErasedBean {
        unreachable!("the greeter upcast is never invoked in the probe-only tests")
    }

    // A descriptor carrying explicit provides[] view rows (so the incremental loop
    // indexes the survivor's views).
    fn auto_descriptor_with_views(
        name: &'static str,
        contract: &str,
        provides: &'static [leaf_core::TypeRow],
    ) -> Descriptor {
        Descriptor { provides, ..auto_descriptor(name, contract) }
    }

    fn candidate_with_views(
        name: &'static str,
        contract: &str,
        views: &[(TypeId, leaf_core::UpcastFn)],
        guard: Option<&'static CondExpr>,
    ) -> AutoConfigCandidate {
        let rows: &'static [leaf_core::TypeRow] = Box::leak(
            views
                .iter()
                .map(|(view, upcast)| leaf_core::TypeRow { view: *view, upcast: *upcast })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let descriptor = auto_descriptor_with_views(name, contract, rows);
        AutoConfigCandidate::new(descriptor, auto_seed, guard)
    }

    // A candidate carrying an explicit OrderHint (the ordering-enforcement fixture).
    fn candidate_ordered(
        name: &'static str,
        contract: &str,
        guard: Option<&'static CondExpr>,
        order: i32,
    ) -> AutoConfigCandidate {
        let hint = leaf_core::OrderHint { order, ..leaf_core::OrderHint::DEFAULT };
        AutoConfigCandidate::with_order(auto_descriptor(name, contract), auto_seed, guard, hint)
    }

    // Flatten `(self_type, views, role)` seed entries into the `(TypeId, role)`
    // seed-probe tuples `run_autoconfig` consumes — one for the self_type plus one
    // per provides[] view (exactly how `component_seed_probe` lifts user beans).
    fn seed_with_views(
        beans: &[(TypeId, Vec<TypeId>, CandidateRole)],
    ) -> Vec<(TypeId, CandidateRole)> {
        let mut out = Vec::new();
        for (self_ty, views, role) in beans {
            out.push((*self_ty, *role));
            for v in views {
                out.push((*v, *role));
            }
        }
        out
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

    // ── a trait VIEW + a bean providing it (the dyn-view back-off fixture) ──
    trait Greeter: Send + Sync {}
    #[derive(Debug)]
    struct InMemoryGreeter;
    impl Greeter for InMemoryGreeter {}

    // An OnMissingBean(dyn Greeter) back-off guard — the VIEW target.
    fn on_missing_dyn_greeter() -> &'static CondExpr {
        let attrs: AttrSlice =
            Box::leak(Box::new([Attr::Type("type", TypeId::of::<dyn Greeter>())]));
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

    // ── view-aware probe (provides[]-aware OnMissingBean) ────────────────────

    #[test]
    fn observe_with_views_indexes_each_provides_view_typeid() {
        // The probe must resolve a VIEW TypeId (a `dyn Trait` the bean provides),
        // not just the concrete self_type — Spring's @ConditionalOnMissingBean(Iface).
        let probe = BuilderProbe::new();
        probe.observe_with_views(
            TypeId::of::<InMemoryGreeter>(),
            &[TypeId::of::<dyn Greeter>()],
            CandidateRole::NORMAL,
        );
        // The concrete type resolves...
        assert!(probe.would_resolve_unique(TypeId::of::<InMemoryGreeter>()).is_unique());
        // ...and so does the declared dyn-view.
        assert!(
            probe.would_resolve_unique(TypeId::of::<dyn Greeter>()).is_unique(),
            "a provides[] view TypeId must be probe-resolvable"
        );
    }

    #[test]
    fn observe_keeps_the_self_type_only_indexing_unchanged() {
        // ADDITIVE: the plain `observe` (no views) still indexes self_type only.
        let probe = BuilderProbe::new();
        probe.observe(TypeId::of::<InMemoryGreeter>(), CandidateRole::NORMAL);
        assert!(probe.would_resolve_unique(TypeId::of::<InMemoryGreeter>()).is_unique());
        assert!(
            !probe.would_resolve_unique(TypeId::of::<dyn Greeter>()).is_unique(),
            "no views were observed, so the dyn-view must NOT resolve"
        );
    }

    #[test]
    fn a_user_bean_providing_dyn_view_makes_on_missing_bean_dyn_view_back_off() {
        // A user bean of a DIFFERENT concrete type that PROVIDES `dyn Greeter` makes
        // an `on_missing_bean(dyn Greeter)`-guarded auto-config back off (the headline
        // provides[]-aware back-off: redis CacheManager overrides the in-memory default).
        let mut builder = RegistryBuilder::new();
        let cands =
            [candidate("autoPool", "test::AutoConfig", Some(on_missing_dyn_greeter()))];
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            // A user bean whose self_type is InMemoryGreeter and which PROVIDES dyn Greeter.
            &seed_with_views(&[(
                TypeId::of::<InMemoryGreeter>(),
                vec![TypeId::of::<dyn Greeter>()],
                CandidateRole::NORMAL,
            )]),
        )
        .expect("runs");
        assert_eq!(
            out.registered, 0,
            "OnMissingBean(dyn Greeter) backs off: a bean providing that view wins"
        );
        let rec = out.report.lookup(ContractId::of("test::AutoConfig")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
    }

    #[test]
    fn on_missing_bean_dyn_view_registers_when_no_provider_of_the_view_present() {
        // No bean provides the view → OnMissingBean(dyn Greeter) matches → the default
        // registers (the in-memory default applies when nothing overrides it).
        let mut builder = RegistryBuilder::new();
        let cands =
            [candidate("autoPool", "test::AutoConfig", Some(on_missing_dyn_greeter()))];
        let out = run_autoconfig(
            &cands,
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(out.registered, 1, "no provider of the view → the default applies");
    }

    #[test]
    fn an_auto_config_providing_dyn_view_makes_a_later_dyn_view_back_off_incrementally() {
        // The incremental case: a FIRST auto-config that PROVIDES `dyn Greeter` makes a
        // SECOND auto-config guarded by `on_missing_bean(dyn Greeter)` back off — proving
        // the incremental register indexes the survivor's provides[] views.
        let mut builder = RegistryBuilder::new();
        let first = candidate_with_views(
            "greeterA",
            "test::AutoConfigA",
            &[(TypeId::of::<dyn Greeter>(), greeter_upcast)],
            None,
        );
        let second =
            candidate("greeterB", "test::AutoConfigB", Some(on_missing_dyn_greeter()));
        let out = run_autoconfig(
            &[first, second],
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(
            out.registered, 1,
            "the first registers (providing the view); the second backs off seeing the view"
        );
        let rec = out.report.lookup(ContractId::of("test::AutoConfigB")).unwrap();
        assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
    }

    // ── auto-config ordering enforcement ─────────────────────────────────────

    #[test]
    fn ordering_makes_a_late_ordered_candidate_see_an_earlier_one() {
        // Two candidates of the SAME type given in REVERSE order priority: the one with
        // the SMALLER `order` must register first so the larger-ordered one's
        // OnMissingBean sees it and backs off — even though it is listed FIRST in input.
        let mut builder = RegistryBuilder::new();
        // Listed first, but ordered LATER (order = 10): its OnMissingBean must see the
        // earlier-ordered unconditional candidate and back off.
        let late = candidate_ordered(
            "autoPoolLate",
            "test::AutoConfigLate",
            Some(on_missing_autopool()),
            10,
        );
        // Listed second, but ordered EARLIER (order = -10): unconditional, registers first.
        let early = candidate_ordered("autoPoolEarly", "test::AutoConfigEarly", None, -10);
        let out = run_autoconfig(
            &[late, early],
            &env_with(&[]),
            &mut builder,
            &ExclusionSet::new(),
            &leaf_core::ActiveProfiles::default(),
            &[],
        )
        .expect("runs");
        assert_eq!(
            out.registered, 1,
            "the earlier-ordered candidate registers first; the later one backs off seeing it"
        );
        // The EARLY (unconditional) one registered; the LATE one backed off.
        let early_rec = out.report.lookup(ContractId::of("test::AutoConfigEarly")).unwrap();
        assert!(matches!(early_rec.class, ConditionReportClass::Unconditional));
        let late_rec = out.report.lookup(ContractId::of("test::AutoConfigLate")).unwrap();
        assert!(
            matches!(late_rec.class, ConditionReportClass::Negative(_)),
            "the later-ordered candidate must see the earlier registration and back off"
        );
    }
}
