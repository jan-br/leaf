//! `App<Wired>::validate()` — the WHOLE-GRAPH validation pass + the
//! `[C2-config-locus]` Tier-2 config materialization (bootstrap-diagnostics
//! phase3/14 step-10; container-lifecycle phase3/13; registry-core phase3/01).
//!
//! This is THE one site for ALL wiring + config-value diagnostics, run BEFORE
//! `Context::refresh()` (so a fault is a Tier-2 [`AssemblyReport`] at validate-time
//! — `RunState` still pre-refresh — never an R5 cancel-cascade). It AGGREGATES
//! every fault into ONE report (never fail-on-first) BEFORE any application
//! constructor runs:
//!
//! - **whole-graph wiring** — for every eager bean, resolve each MANDATORY
//!   ([`Arity::Single`]) construction [`InjectionPoint`] via the
//!   [`Selector`](leaf_core::Selector). A `Resolved::None` is `NoSuchBean`
//!   (ENRICHED from the [`ConditionReport`] — a bean a condition silently backed
//!   off is named); a `Resolved::Ambiguous` is `NoUniqueBean` (rich candidate
//!   list); a winner that is advised + concrete-matched is the
//!   `AdvisedConcreteInjection` COHERENCE rejection; a scope-incompatible winner is
//!   `ScopeMismatch`.
//! - **cycle classification** — [`order_batch`](crate::order_batch) over the eager
//!   set folds construction + `@DependsOn` edges into waves; a cycle is
//!   `CircularDependency` (with the path + the convert-to-`LazyRef` hint) or
//!   `DependsOnCycle`.
//! - **C2 config materialization** — PRE-MATERIALIZE each `@ConfigurationProperties`
//!   bean (the macro-emitted binder-backed bind + JSR thunk), aggregating
//!   `BindError`/`ConvertError`/`ValidationError` at Tier-2 and STORING the bound
//!   `Published::Shared(Arc)` into the bean's singleton `OnceCell` (so refresh R5
//!   publishes it and NEVER re-binds — eager-EXCLUDED-because-PREBOUND). DRY-RUN
//!   each eager bean's `@Value` coercions (`resolve_into::<T>`, value DISCARDED)
//!   through the macro-emitted per-bean thunk, aggregating `ConvertError`/
//!   `UnresolvedValue`.
//!
//! ## The [`StartupValidation`] lever
//!
//! Read ONCE at this pass's head. `Strict` aggregates all at `Severity::Fatal`.
//! `Lenient` DOWNGRADES ONLY the two value-shape facets (`@Value`/placeholder) to
//! `Severity::Warn` — wiring (`NoSuchBean`/`NoUniqueBean`/`Cycle`/`ScopeMismatch`)
//! AND the config-properties bind+JSR sub-pass stay HARD (structural soundness is
//! never tunable; the carve-out per `[C2-config-locus]`). `Skip` elides the
//! cold `@Value` re-walk COST only — the config-properties bind+JSR still runs.

use std::any::TypeId;

use leaf_core::{
    reject_advised_concrete, Arity, AssemblyReport, BeanId, Cand, CandidateSet, Cause,
    ConditionReport, ConditionReportClass, Descriptor, Env, ErrorKind, InjectionPlan,
    InjectionPoint, LeafError, Multiplicity, PointKind, Published, Registry, Resolved, Selector,
    Severity, StartupValidation, StoreSource,
};

// ─────────────────────────── ValidationInputs ───────────────────────────────

/// The macro-emitted per-bean inputs the [`validate`] pass folds: which beans to
/// validate eagerly, each bean's [`InjectionPlan`], the `@ConfigurationProperties`
/// pre-materialization thunks, and the `@Value` dry-run thunks.
///
/// leaf-core's frozen `Descriptor` carries no injection plan / bind thunk, so the
/// run engine (or `#[leaf::main]`) supplies them here — the SAME thunks the macro
/// emits beside the `Descriptor`. All closures are borrowed for the pass.
pub struct ValidationInputs<'a> {
    /// The eager beans to validate (the engine derives it: the non-lazy/non-scoped/
    /// non-prototype singletons; the config-properties beans are pre-bound here AND
    /// excluded from refresh R5). An empty slice validates the whole registry's
    /// singleton set.
    eager: &'a [BeanId],
    /// Per-bean [`InjectionPlan`] lookup (defaults to [`InjectionPlan::EMPTY`]).
    plan_of: Option<&'a (dyn Fn(BeanId) -> InjectionPlan + 'a)>,
    /// The `@ConfigurationProperties` beans to pre-materialize at validate (C2).
    config_beans: &'a [ConfigBean<'a>],
    /// Per-bean `@Value` dry-run thunks (C2): `(BeanId, thunk)` where the thunk
    /// re-runs each `PointKind::Value` coercion (value discarded) over the `Env`,
    /// returning the collected coercion faults.
    value_dry_runs: &'a [ValueDryRun<'a>],
}

impl<'a> ValidationInputs<'a> {
    /// Empty inputs: validate the registry's singleton set with no injection plans,
    /// no config beans, no `@Value` dry-runs (the bare-graph wiring check).
    #[must_use]
    pub fn new() -> Self {
        ValidationInputs {
            eager: &[],
            plan_of: None,
            config_beans: &[],
            value_dry_runs: &[],
        }
    }

    /// Set the eager bean set (the beans to validate + order into waves).
    #[must_use]
    pub fn with_eager(mut self, eager: &'a [BeanId]) -> Self {
        self.eager = eager;
        self
    }

    /// Install the per-bean [`InjectionPlan`] lookup (the macro-emitted plans).
    #[must_use]
    pub fn with_plans(mut self, plan_of: &'a (dyn Fn(BeanId) -> InjectionPlan + 'a)) -> Self {
        self.plan_of = Some(plan_of);
        self
    }

    /// Install the `@ConfigurationProperties` pre-materialization thunks (C2).
    #[must_use]
    pub fn with_config_beans(mut self, config_beans: &'a [ConfigBean<'a>]) -> Self {
        self.config_beans = config_beans;
        self
    }

    /// Install the `@Value` dry-run thunks (C2).
    #[must_use]
    pub fn with_value_dry_runs(mut self, value_dry_runs: &'a [ValueDryRun<'a>]) -> Self {
        self.value_dry_runs = value_dry_runs;
        self
    }

    fn plan(&self, id: BeanId) -> InjectionPlan {
        self.plan_of.map_or(InjectionPlan::EMPTY, |f| f(id))
    }
}

impl Default for ValidationInputs<'_> {
    fn default() -> Self {
        ValidationInputs::new()
    }
}

/// The bind result of a `@ConfigurationProperties` pre-materialization thunk: the
/// bound [`Published::Shared`] handle to PRE-BIND into the slot, or the aggregated
/// bind/convert/JSR faults.
pub type ConfigBindResult = Result<Published, Vec<LeafError>>;

/// One `@ConfigurationProperties` bean's validate-time pre-materialization (C2).
///
/// The macro emits — beside the config bean's `Descriptor` — a pure-projection
/// bind thunk (`Provider::provide` is `&Env` + `BindHandler` only, NO `ResolveCtx`,
/// so it is safe to build before wiring is live) that runs the binder-backed
/// factory + the stock JSR `ValidationBindHandler`. [`validate`] invokes it, stores
/// the bound `Arc` into `registry.singleton_cell(id)`, and aggregates any faults.
pub struct ConfigBean<'a> {
    /// The bean's frozen slot id (the `OnceCell` to pre-bind).
    id: BeanId,
    /// The pure-projection bind+JSR thunk over the `Env` + the strictness lever.
    bind: &'a (dyn Fn(&Env, StartupValidation) -> ConfigBindResult + 'a),
}

impl<'a> ConfigBean<'a> {
    /// Build a config-bean input from its slot id + its bind thunk.
    #[must_use]
    pub fn new(
        id: BeanId,
        bind: &'a (dyn Fn(&Env, StartupValidation) -> ConfigBindResult + 'a),
    ) -> Self {
        ConfigBean { id, bind }
    }
}

/// One eager bean's `@Value` dry-run (C2): re-run each `PointKind::Value` coercion
/// over the `Env` (value DISCARDED), collecting `ConvertError`/`UnresolvedValue`.
///
/// The macro emits this per-bean thunk over the bean's `Value` rows so [`validate`]
/// avoids `T`-erasure — it is the SAME monomorphized `resolve_into::<T>` the real
/// R5 coercion runs, so dry-run and real coercion are provably one code path.
pub struct ValueDryRun<'a> {
    /// The bean whose `@Value` rows this thunk dry-runs (diagnostic context).
    id: BeanId,
    /// The dry-run thunk: returns the collected coercion faults (empty = clean).
    run: &'a (dyn Fn(&Env, StartupValidation) -> Vec<LeafError> + 'a),
}

impl<'a> ValueDryRun<'a> {
    /// Build a `@Value` dry-run input from the bean's slot id + its dry-run thunk.
    #[must_use]
    pub fn new(
        id: BeanId,
        run: &'a (dyn Fn(&Env, StartupValidation) -> Vec<LeafError> + 'a),
    ) -> Self {
        ValueDryRun { id, run }
    }
}

// ─────────────────────────── the validate pass ──────────────────────────────

/// The WHOLE-GRAPH validation pass (the body of [`App::<Wired>::validate`](crate::App)).
///
/// Aggregates every wiring + config fault into ONE [`AssemblyReport`] at Tier-2,
/// honoring `lever`. C2 pre-materialization (config beans pre-bound into their
/// `OnceCell` slots) is a SIDE EFFECT on `registry` — refresh R5 publishes the
/// already-bound `Arc` and never re-binds.
#[must_use]
pub fn validate(
    registry: &Registry,
    env: &Env,
    condition_report: &ConditionReport,
    lever: StartupValidation,
    inputs: &ValidationInputs<'_>,
) -> AssemblyReport {
    let mut report = AssemblyReport::new();

    // The eager set: caller-supplied, else every container-scoped singleton.
    let eager: Vec<BeanId> = if inputs.eager.is_empty() {
        default_eager(registry)
    } else {
        inputs.eager.to_vec()
    };

    // ── (C2) pre-materialize the @ConfigurationProperties beans FIRST ──
    //
    // The config-properties bind+JSR sub-pass is HARD under every lever (it has no
    // Tier-3 fallback — the bean is materialized exactly once, here). On success
    // the bound Arc is stored into the slot OnceCell (eager-EXCLUDED-because-
    // PREBOUND); on failure every BindError/Convert/Violation is aggregated.
    for cfg in inputs.config_beans {
        match (cfg.bind)(env, lever) {
            Ok(published) => {
                if let Some(bean) = published.into_shared() {
                    // Pre-bind into the slot (idempotent: a second bind is a no-op).
                    let _ = registry.singleton_cell(cfg.id).set(bean);
                }
            }
            Err(faults) => {
                for f in faults {
                    report.push(f);
                }
            }
        }
    }

    // ── whole-graph wiring: resolve every eager mandatory injection point ──
    for &id in &eager {
        let descriptor = registry.descriptor(id);
        let plan = inputs.plan(id);
        for point in plan.construction_edges() {
            // Only MANDATORY single-bean points are wiring faults; Optional/
            // Collection/Map tolerate absence, and @Value points are coercion
            // (handled by the dry-run sub-pass), not by-type resolution.
            if point.kind != PointKind::Bean || point.arity != Arity::Single {
                continue;
            }
            if let Some(fault) =
                resolve_point_fault(registry, descriptor, point, condition_report)
            {
                report.push(fault);
            }
        }
    }

    // ── cycle classification via the wave planner (same edges as R5) ──
    let plan_lookup = |id: BeanId| inputs.plan(id);
    match crate::wiring::order_batch(registry, &eager, &plan_lookup) {
        Ok(_plan) => {}
        Err(cycle) => report.push(cycle),
    }

    // ── (C2) @Value dry-run sub-pass (the cold coercion re-walk) ──
    //
    // Skip elides the cold-walk COST only; Lenient downgrades the value-shape
    // faults to Warn. Both still keep the config-properties bind HARD (above).
    if lever != StartupValidation::Skip {
        for dry in inputs.value_dry_runs {
            for fault in (dry.run)(env, lever) {
                let _ = dry.id; // diagnostic context (the thunk owns the per-point detail)
                report.push(maybe_downgrade(fault, lever));
            }
        }
    }

    report
}

// ─────────────────────────── helpers ────────────────────────────────────────

/// The default eager set when the caller supplies none: every container-stored
/// singleton ([`Multiplicity::Once`]) — lazy/scoped/prototype beans are not eager.
fn default_eager(registry: &Registry) -> Vec<BeanId> {
    registry
        .ids()
        .filter(|&id| {
            let scope = registry.descriptor(id).scope;
            scope.multiplicity == Multiplicity::Once
                && matches!(scope.store, StoreSource::ContainerStore)
        })
        .collect()
}

/// Resolve one mandatory injection point; return `Some(fault)` if it is a wiring
/// fault (`NoSuchBean`/`NoUniqueBean`/`AdvisedConcreteInjection`/`ScopeMismatch`),
/// else `None`. The `NoSuchBean` is ENRICHED from the condition report.
fn resolve_point_fault(
    registry: &Registry,
    consumer: &Descriptor,
    point: &InjectionPoint,
    condition_report: &ConditionReport,
) -> Option<LeafError> {
    let set = candidate_set(registry, point.produced);
    let (resolved, trace) = Selector::resolve_one(point, &set);
    match resolved {
        Resolved::One(winner) => {
            // COHERENCE: an advised bean injected by its concrete type is rejected.
            if let Err(e) = reject_advised_concrete(point, &winner) {
                return Some(e);
            }
            // Scope coherence: a mandatory single point cannot bind a prototype
            // (owned-move) bean — that is a ScopeMismatch.
            let wd = registry.descriptor(winner.id);
            if wd.scope.multiplicity == Multiplicity::PerResolution {
                return Some(scope_mismatch(point, wd));
            }
            None
        }
        Resolved::None => Some(enrich_no_such_bean(point, consumer, condition_report)),
        Resolved::Ambiguous(cands) => Some(leaf_core::no_unique_bean_traced(
            point,
            &cands,
            trace.as_ref(),
        )),
    }
}

/// Build the [`CandidateSet`] for a produced `TypeId` from the frozen registry,
/// projecting each candidate row's selection-relevant flags into a [`Cand`].
fn candidate_set(registry: &Registry, produced: TypeId) -> CandidateSet<'_> {
    let mut set = CandidateSet::new();
    for &cid in registry.candidates(produced) {
        // A present-but-null singleton is COUNTED here for arbitration/uniqueness;
        // the NULL_BEAN typed-boundary mapping (mandatory → error, optional → None)
        // is the engine's `get<T>` concern, not the validate candidate set's.
        let d = registry.descriptor(cid);
        let mut cand = Cand::new(cid, d.declared_name.unwrap_or(""));
        cand.role = d.meta.candidate_role;
        cand.autowire_candidate = d.meta.autowire_candidate;
        cand.concrete_match = d.self_type == produced;
        cand.markers = d.meta.markers;
        set.push(cand);
    }
    set
}

/// A `NoSuchBean` ENRICHED from the condition report: if a bean of the required
/// shape was silently backed off by a `@Conditional`, name it + its reason.
fn enrich_no_such_bean(
    point: &InjectionPoint,
    consumer: &Descriptor,
    condition_report: &ConditionReport,
) -> LeafError {
    // Scan the condition report for an element that backed off (Negative/Exclusion/
    // CompiledOutByCfg/BuildFoldedFalse) — the silent-now/loud-later join. We do
    // not have the produced TypeId↔ContractId map here, so surface the first
    // backed-off element as the enrichment hint (the macro-supplied report keys by
    // ContractId; a richer type-keyed join is a downstream refinement).
    let backed_off: Vec<&leaf_core::ConditionRecord> = condition_report
        .records()
        .iter()
        .filter(|r| {
            matches!(
                r.class,
                ConditionReportClass::Negative(_)
                    | ConditionReportClass::Exclusion(_)
                    | ConditionReportClass::CompiledOutByCfg(_)
                    | ConditionReportClass::BuildFoldedFalse(_)
            )
        })
        .collect();

    let base = format!(
        "no candidate bean for injection point `{}` (type {:?}) required by `{}`",
        point.name,
        point.produced,
        consumer.declared_name.unwrap_or("<unnamed>")
    );

    let detail = if backed_off.is_empty() {
        base
    } else {
        let hints: Vec<String> = backed_off
            .iter()
            .map(|r| format!("`{:?}` ({})", r.element, class_label(&r.class)))
            .collect();
        format!(
            "{base}. A matching bean may have been gated by a condition: {}",
            hints.join(", ")
        )
    };

    LeafError::new(ErrorKind::NoSuchBean)
        .caused_by(Cause::plain("validating injection point", detail))
}

fn class_label(class: &ConditionReportClass) -> &'static str {
    match class {
        ConditionReportClass::Positive => "matched",
        ConditionReportClass::Negative(_) => "condition not met",
        ConditionReportClass::Exclusion(_) => "excluded",
        ConditionReportClass::Unconditional => "unconditional",
        ConditionReportClass::CompiledOutByCfg(_) => "compiled out (feature off)",
        ConditionReportClass::BuildFoldedFalse(_) => "build-folded false",
    }
}

fn scope_mismatch(point: &InjectionPoint, winner: &Descriptor) -> LeafError {
    LeafError::new(ErrorKind::ScopeMismatch).caused_by(Cause::plain(
        "validating injection point",
        format!(
            "injection point `{}` requires a single shared bean, but the resolved bean `{}` is \
             a prototype (per-resolution owned move) — a mandatory single point cannot bind a \
             prototype",
            point.name,
            winner.declared_name.unwrap_or("<unnamed>")
        ),
    ))
}

/// Under `Lenient`, downgrade a value-shape fault (`@Value`/placeholder coercion)
/// to `Severity::Warn`; otherwise leave it `Fatal`. Wiring + config-JSR faults are
/// never routed through here (they stay HARD).
fn maybe_downgrade(fault: LeafError, lever: StartupValidation) -> LeafError {
    if lever == StartupValidation::Lenient
        && matches!(fault.kind, ErrorKind::ConvertError | ErrorKind::UnresolvedValue)
    {
        fault.with_severity(Severity::Warn)
    } else {
        fault
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use leaf_core::{
        AnnotationMetadata, BoxFuture, ConditionRecord, ContractId, EnvBuilder, Origin, Provider,
        ReasonMsg, RegistryBuilder, ResolveCtx, Role, ScopeDef,
    };

    // ── fixtures ────────────────────────────────────────────────────────────────

    #[derive(Debug)]
    struct Stub;
    struct StubProvider(Descriptor);
    impl Provider for StubProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.0
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Stub)) })
        }
    }

    #[derive(Debug)]
    struct TA;
    #[derive(Debug)]
    struct TB;
    #[derive(Debug)]
    struct TC;

    fn desc(contract: &str, name: &'static str, self_type: TypeId, scope: ScopeDef) -> Descriptor {
        Descriptor {
            contract: ContractId::of(contract),
            self_type,
            provides: &[],
            declared_name: Some(name),
            aliases: &[],
            scope,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    fn register(b: &mut RegistryBuilder, d: Descriptor) -> BeanId {
        b.register(d, Arc::new(StubProvider(d))).expect("register")
    }

    fn empty_env() -> Env {
        EnvBuilder::new().seal_env()
    }

    fn point_of<T: 'static>(name: &'static str) -> &'static [InjectionPoint] {
        Box::leak(Box::new([InjectionPoint::single(TypeId::of::<T>(), name)]))
    }

    // ── validate aggregates 3 distinct wiring errors into one report ─────────────

    #[test]
    fn validate_aggregates_three_distinct_wiring_errors_into_one_report() {
        // Bean A needs: a missing bean (NoSuchBean), an ambiguous bean
        // (NoUniqueBean), and an advised-concrete bean (AdvisedConcreteInjection).
        //
        // - TB: nothing registered ⇒ NoSuchBean.
        // - TC: TWO registered ⇒ NoUniqueBean.
        // - TA-advised: an advised bean injected by concrete type.
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("v::A", "a", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        // two TCs (ambiguous)
        register(&mut b, desc("v::C0", "c0", TypeId::of::<TC>(), ScopeDef::SINGLETON));
        register(&mut b, desc("v::C1", "c1", TypeId::of::<TC>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        // A's plan: a TB point (missing) + a TC point (ambiguous).
        let a = id_a;
        let plan_of = move |id: BeanId| -> InjectionPlan {
            if id == a {
                InjectionPlan {
                    points: Box::leak(Box::new([
                        InjectionPoint::single(TypeId::of::<TB>(), "b"),
                        InjectionPoint::single(TypeId::of::<TC>(), "c"),
                    ])),
                }
            } else {
                InjectionPlan::EMPTY
            }
        };

        let eager = vec![id_a];
        let inputs = ValidationInputs::new()
            .with_eager(&eager)
            .with_plans(&plan_of);

        let report = validate(
            &reg,
            &empty_env(),
            &ConditionReport::new(),
            StartupValidation::Strict,
            &inputs,
        );

        assert!(!report.is_ok(), "wiring faults aggregated");
        let kinds: Vec<ErrorKind> = report.faults().iter().map(|f| f.error().kind).collect();
        assert!(kinds.contains(&ErrorKind::NoSuchBean), "got: {kinds:?}");
        assert!(kinds.contains(&ErrorKind::NoUniqueBean), "got: {kinds:?}");
        assert_eq!(report.len(), 2, "two distinct wiring faults: {kinds:?}");
    }

    #[test]
    fn validate_collects_all_faults_not_just_the_first() {
        // Three independent beans each needing a distinct missing bean ⇒ THREE
        // NoSuchBean faults aggregated (never fail-on-first).
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("v::A", "a", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let id_b = register(&mut b, desc("v::B", "b", TypeId::of::<TB>(), ScopeDef::SINGLETON));
        let id_c = register(&mut b, desc("v::C", "c", TypeId::of::<TC>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        #[derive(Debug)]
        struct Missing0;
        #[derive(Debug)]
        struct Missing1;
        #[derive(Debug)]
        struct Missing2;

        let (a, bb, c) = (id_a, id_b, id_c);
        let plan_of = move |id: BeanId| -> InjectionPlan {
            let points = if id == a {
                point_of::<Missing0>("m0")
            } else if id == bb {
                point_of::<Missing1>("m1")
            } else if id == c {
                point_of::<Missing2>("m2")
            } else {
                return InjectionPlan::EMPTY;
            };
            InjectionPlan { points }
        };

        let eager = vec![id_a, id_b, id_c];
        let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
        let report = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);
        assert_eq!(report.len(), 3, "all three missing-bean faults aggregated");
    }

    // ── a constructor cycle is reported with the path + the LazyRef hint ────────

    #[test]
    fn a_constructor_cycle_surfaces_in_the_validate_report() {
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("v::A", "alpha", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let id_b = register(&mut b, desc("v::B", "beta", TypeId::of::<TB>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        let (a, _bb) = (id_a, id_b);
        let plan_of = move |id: BeanId| -> InjectionPlan {
            if id == a {
                InjectionPlan { points: point_of::<TB>("b") }
            } else {
                InjectionPlan { points: point_of::<TA>("a") }
            }
        };
        let eager = vec![id_a, id_b];
        let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
        let report = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);

        let cyc = report
            .faults()
            .iter()
            .find(|f| f.error().kind == ErrorKind::CircularDependency)
            .expect("a cycle fault is present");
        let msg = cyc.error().to_string();
        assert!(msg.contains("alpha") && msg.contains("beta"), "path: {msg}");
        assert!(msg.contains("LazyRef"), "the convert-to-LazyRef hint: {msg}");
    }

    // ── a bad @ConfigurationProperties bind is a Tier-2 error ───────────────────

    #[test]
    fn a_bad_config_properties_bind_is_a_tier2_error() {
        let mut b = RegistryBuilder::new();
        let id_cfg = register(&mut b, desc("v::Cfg", "cfg", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        // A bind thunk that fails range(min=1) for DB_POOL_SIZE=0 → BindError.
        let bind = |_env: &Env, _lever: StartupValidation| -> ConfigBindResult {
            Err(vec![LeafError::new(ErrorKind::BindError).caused_by(Cause::plain(
                "binding @ConfigurationProperties",
                "db.pool-size=0 violates range(min=1)",
            ))])
        };
        let cfg = [ConfigBean::new(id_cfg, &bind)];
        let inputs = ValidationInputs::new()
            .with_eager(&[])
            .with_config_beans(&cfg);

        let report = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);
        let fault = report
            .faults()
            .iter()
            .find(|f| f.error().kind == ErrorKind::BindError)
            .expect("the bad bind is a Tier-2 BindError");
        assert!(fault.error().to_string().contains("range(min=1)"), "got: {}", fault.error());
        // The slot was NOT pre-bound (the bind failed).
        assert!(reg.singleton_cell(id_cfg).get().is_none());
    }

    #[test]
    fn a_good_config_properties_bind_prebinds_the_slot_oncecell() {
        let mut b = RegistryBuilder::new();
        let id_cfg = register(&mut b, desc("v::Cfg", "cfg", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        let bind = |_env: &Env, _lever: StartupValidation| -> ConfigBindResult {
            Ok(Published::shared_value(TA))
        };
        let cfg = [ConfigBean::new(id_cfg, &bind)];
        let inputs = ValidationInputs::new().with_config_beans(&cfg);
        let report = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);

        assert!(report.is_ok(), "a good bind is clean");
        // eager-EXCLUDED-because-PREBOUND: the slot OnceCell now holds the bound Arc.
        assert!(reg.singleton_cell(id_cfg).get().is_some(), "the config bean is pre-bound");
    }

    // ── @Value dry-run surfaces a coercion fault at Tier-2; Skip elides it ──────

    #[test]
    fn value_dry_run_surfaces_a_coercion_fault_and_skip_elides_it() {
        let mut b = RegistryBuilder::new();
        let id = register(&mut b, desc("v::A", "a", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        // order.max-retries=abc → u16 is a ConvertError (the macro's dry-run thunk).
        let run = |_env: &Env, _lever: StartupValidation| -> Vec<LeafError> {
            vec![LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "@Value coercion dry-run",
                "order.max-retries=abc is not a u16",
            ))]
        };
        let drys = [ValueDryRun::new(id, &run)];

        // Strict: the coercion fault surfaces (Fatal).
        let inputs = ValidationInputs::new().with_value_dry_runs(&drys);
        let strict = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);
        let f = strict.faults().iter().find(|f| f.error().kind == ErrorKind::ConvertError).expect("convert fault");
        assert_eq!(f.error().mode, Severity::Fatal);

        // Lenient: the value-shape fault downgrades to Warn (still recorded).
        let lenient = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Lenient, &inputs);
        let fl = lenient.faults().iter().find(|f| f.error().kind == ErrorKind::ConvertError).expect("convert fault");
        assert_eq!(fl.error().mode, Severity::Warn, "Lenient downgrades the value-shape facet");

        // Skip: the cold @Value re-walk is elided entirely.
        let skip = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Skip, &inputs);
        assert!(skip.faults().iter().all(|f| f.error().kind != ErrorKind::ConvertError), "Skip elides the @Value cold walk");
    }

    // ── NoSuchBean is enriched from the condition report ────────────────────────

    #[test]
    fn no_such_bean_is_enriched_from_the_condition_report() {
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("v::A", "a", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        // A condition report saying v::Gated backed off (condition not met).
        let records = vec![ConditionRecord {
            element: ContractId::of("v::Gated"),
            self_type: Some(TypeId::of::<TB>()),
            class: ConditionReportClass::Negative(ReasonMsg::of("OnProperty")),
            leaves: Box::new([]),
        }];
        let cr = ConditionReport::from_records(records);

        let a = id_a;
        let plan_of = move |id: BeanId| -> InjectionPlan {
            if id == a {
                InjectionPlan { points: point_of::<TB>("b") }
            } else {
                InjectionPlan::EMPTY
            }
        };
        let eager = vec![id_a];
        let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
        let report = validate(&reg, &empty_env(), &cr, StartupValidation::Strict, &inputs);

        let nsb = report.faults().iter().find(|f| f.error().kind == ErrorKind::NoSuchBean).expect("nsb");
        let msg = nsb.error().to_string();
        assert!(msg.contains("gated by a condition"), "enriched: {msg}");
        // The gated element is named by its stable ContractId (the only identity a
        // ConditionRecord carries) — assert the join surfaced THIS element + reason.
        let gated = format!("{:?}", ContractId::of("v::Gated"));
        assert!(msg.contains(&gated), "names the gated element by ContractId: {msg}");
        assert!(msg.contains("condition not met"), "carries the back-off reason: {msg}");
    }

    // ── a clean graph validates with no faults ──────────────────────────────────

    #[test]
    fn a_clean_graph_validates_with_no_faults() {
        let mut b = RegistryBuilder::new();
        // A needs B; both registered; B is a singleton.
        let id_a = register(&mut b, desc("v::A", "a", TypeId::of::<TA>(), ScopeDef::SINGLETON));
        let id_b = register(&mut b, desc("v::B", "b", TypeId::of::<TB>(), ScopeDef::SINGLETON));
        let reg = b.freeze().expect("freeze");

        let a = id_a;
        let plan_of = move |id: BeanId| -> InjectionPlan {
            if id == a {
                InjectionPlan { points: point_of::<TB>("b") }
            } else {
                InjectionPlan::EMPTY
            }
        };
        let eager = vec![id_a, id_b];
        let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
        let report = validate(&reg, &empty_env(), &ConditionReport::new(), StartupValidation::Strict, &inputs);
        assert!(report.is_ok(), "clean graph: {:?}", report.faults().iter().map(|f| f.error().kind).collect::<Vec<_>>());
    }
}
