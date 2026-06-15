//! The `seal()` transition + the [`WiringPlan`] wave plan ([`order_batch`]) — the
//! `App<Resolve> → App<Wired>` edge (container-lifecycle phase3/13; registry-core
//! phase3/01).
//!
//! ## `seal()` — the one irreversible freeze
//!
//! [`seal`](crate::App::<crate::Resolve>::seal) is the `markBeanAsCreated`
//! analogue: it consumes the `App<Resolve>`'s mutable [`RegistryBuilder`] into the
//! immutable, dense-[`BeanId`](leaf_core::BeanId) [`Registry`](leaf_core::Registry)
//! (the slot-indexed `OnceCell` singleton store, both indices, the alias overlay,
//! the `ContractId` collision guard). The typestate makes a post-seal edit
//! unrepresentable — the builder is consumed and the `App<Wired>` holds the frozen
//! registry. Conditions/auto-config/exclusions already ran in `App<Resolve>`; the
//! frozen snapshot is what `validate()` and `Context::refresh()` read.
//!
//! ## `order_batch` — the 3-pass wave partition + cycle detect
//!
//! [`order_batch`] computes the [`WiringPlan`] — the partition of the eager beans
//! into ordered [`Wave`]s such that **every mandatory edge lands in a strictly-
//! earlier wave** (the WAVE-PARTITION INVARIANT the R5 eager wave-instantiation
//! relies on: intra-wave beans are independent, so a wave can be built inside one
//! `JoinSet` scope). The dependency graph folds together TWO edge kinds:
//!
//! 1. **Mandatory construction edges** — a bean's [`InjectionPlan`] construction
//!    points ([`PointKind::Bean`](leaf_core::PointKind) at [`Arity::Single`](leaf_core::Arity))
//!    resolved to a concrete provider via the [`Selector`](leaf_core::Selector):
//!    `A`-needs-`B` means `B` is in a strictly-earlier wave than `A`.
//! 2. **`@DependsOn` forced-ordering edges** — `descriptor.meta.depends_on`
//!    (resolved by [`ContractId`](leaf_core::ContractId) to a `BeanId`): a
//!    `@DependsOn` target is likewise folded into a strictly-earlier wave.
//!
//! The three passes:
//!
//! - **Pass 1 — graph build**: resolve each eager bean's mandatory construction
//!   edges (via the Selector over the frozen candidate sets) + its `@DependsOn`
//!   targets into a `BeanId → [BeanId]` dependency adjacency + an indegree count.
//!   `Optional`/`Collection`/`Map`/deferral/`Value` points contribute NO edge (a
//!   deferral edge is the sanctioned cycle break; absence is tolerated).
//! - **Pass 2 — layered topological partition (Kahn)**: peel off the beans whose
//!   dependencies are all already placed, wave by wave, each wave sorted by the one
//!   [`cmp_chain`](leaf_core::cmp_chain) (RoleTier-first) then dense `BeanId` for a
//!   fully deterministic order.
//! - **Pass 3 — cycle detect**: any bean never peeled is on a cycle. A
//!   construction-edge cycle is [`ErrorKind::CircularDependency`](leaf_core::ErrorKind)
//!   carrying the path + the "convert one edge to `LazyRef`" hint; a pure
//!   `@DependsOn` cycle is [`ErrorKind::DependsOnCycle`](leaf_core::ErrorKind).

use std::collections::HashMap;

use leaf_core::{
    cmp_chain, Arity, BeanId, Cand, CandidateSet, Cause, ChainKey, ErrorKind, InjectionPlan,
    LeafError, PointKind, Registry, Resolved, RoleTier, Selector,
};

// ─────────────────────────── the WiringPlan ─────────────────────────────────

/// One eager-instantiation wave: a set of beans with NO mandatory edge between
/// them (intra-wave independence), so the R5 driver builds them inside ONE
/// structured-concurrency `JoinSet` scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wave {
    /// The beans in this wave, in the deterministic `cmp_chain`/`BeanId` order.
    pub beans: Vec<BeanId>,
}

impl Wave {
    /// The number of beans in this wave.
    #[must_use]
    pub fn len(&self) -> usize {
        self.beans.len()
    }

    /// `true` iff the wave holds no beans.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.beans.is_empty()
    }
}

/// The frozen eager-instantiation wave plan (container-lifecycle R5): the eager
/// beans partitioned into ordered [`Wave`]s honoring the WAVE-PARTITION
/// INVARIANT (every mandatory edge → a strictly-earlier wave).
///
/// Computed by [`order_batch`] over the frozen [`Registry`] + the per-bean
/// [`InjectionPlan`]s. The teardown LIFO is the reverse wave-order (a
/// `@DependsOn` target tears down after its dependent).
#[derive(Clone, Debug, Default)]
pub struct WiringPlan {
    waves: Vec<Wave>,
}

impl WiringPlan {
    /// The ordered waves (earliest first).
    #[must_use]
    pub fn waves(&self) -> &[Wave] {
        &self.waves
    }

    /// The number of waves.
    #[must_use]
    pub fn wave_count(&self) -> usize {
        self.waves.len()
    }

    /// `true` iff the plan has no waves (no eager beans).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waves.is_empty()
    }

    /// The total number of beans across all waves.
    #[must_use]
    pub fn bean_count(&self) -> usize {
        self.waves.iter().map(Wave::len).sum()
    }

    /// The 0-based wave index a bean was placed in (or `None` if absent).
    #[must_use]
    pub fn wave_of(&self, bean: BeanId) -> Option<usize> {
        self.waves
            .iter()
            .position(|w| w.beans.contains(&bean))
    }
}

// ─────────────────────── the per-bean plan lookup ───────────────────────────

/// How [`order_batch`] obtains a bean's const [`InjectionPlan`] (the
/// construction edges it folds into the dependency graph).
///
/// The macro emits the plan beside the `Descriptor`; until a `Descriptor` carries
/// an injection-plan field, the wave planner consults this resolver (defaulting to
/// [`InjectionPlan::EMPTY`] — the no-collaborator POJO case). leaf-boot's run
/// engine / the validate inputs install a richer resolver keyed by `BeanId`.
pub type PlanLookup<'a> = &'a dyn Fn(BeanId) -> InjectionPlan;

// ─────────────────────────── order_batch ────────────────────────────────────

/// The 3-pass wave partition + cycle detect over the dependency graph
/// (container-lifecycle R5; auto-config-ordering).
///
/// `eager` is the set of beans to instantiate eagerly (the engine derives it: the
/// non-lazy/non-scoped/non-prototype singletons minus the config beans `validate()`
/// pre-bound). `plan_of` yields each bean's [`InjectionPlan`]. The dependency
/// graph folds:
///
/// 1. each bean's MANDATORY construction edges ([`PointKind::Bean`] at
///    [`Arity::Single`]) resolved via the [`Selector`] to a concrete provider, and
/// 2. each bean's `@DependsOn` targets (`descriptor.meta.depends_on`).
///
/// # Errors
/// [`ErrorKind::CircularDependency`] (with the cycle path + the LazyRef hint) for a
/// construction-edge cycle; [`ErrorKind::DependsOnCycle`] for a pure `@DependsOn`
/// cycle. A construction edge resolving to a missing/ambiguous bean is NOT an error
/// here — it has no edge (the whole-graph `validate()` pass reports those richly).
pub fn order_batch(
    registry: &Registry,
    eager: &[BeanId],
    plan_of: PlanLookup<'_>,
) -> Result<WiringPlan, LeafError> {
    // The eager set as a membership test (only edges WITHIN the eager set order
    // waves; an edge to a lazy/absent bean does not gate a wave).
    let in_eager: std::collections::HashSet<BeanId> = eager.iter().copied().collect();

    // Pass 1 — build the dependency adjacency (bean → the beans it depends on) +
    // record which edges are construction edges (for the cycle-class split).
    let mut deps: HashMap<BeanId, Vec<BeanId>> = HashMap::with_capacity(eager.len());
    // depends_on-only edges (the @DependsOn forced-ordering set), for the cycle
    // classification: a cycle made of ONLY these is a DependsOnCycle.
    let mut construction_dep: std::collections::HashSet<(BeanId, BeanId)> =
        std::collections::HashSet::new();

    for &id in eager {
        let mut edges: Vec<BeanId> = Vec::new();

        // (1) mandatory construction edges via the Selector.
        let plan = plan_of(id);
        for point in plan.construction_edges() {
            if point.arity != Arity::Single {
                // Optional/Collection/Map tolerate absence and never gate a wave.
                continue;
            }
            if point.kind != PointKind::Bean {
                // A @Value point reads Env, not the registry — no bean edge.
                continue;
            }
            let set = candidate_set(registry, point.produced);
            // Resolve the unique provider; only a definite single winner is an
            // ordering edge (ambiguity/absence is the validate() pass's concern).
            if let (Resolved::One(winner), _) = Selector::resolve_one(point, &set)
                && winner.id != id
                && in_eager.contains(&winner.id)
                && !edges.contains(&winner.id)
            {
                edges.push(winner.id);
                construction_dep.insert((id, winner.id));
            }
        }

        // (2) @DependsOn forced-ordering edges (resolved by ContractId).
        for &target in registry.descriptor(id).meta.depends_on {
            let Some(tid) = registry.by_contract(target) else {
                continue;
            };
            if tid != id && in_eager.contains(&tid) && !edges.contains(&tid) {
                edges.push(tid);
            }
        }

        deps.insert(id, edges);
    }

    // Pass 2 — layered topological partition (Kahn over the "depends-on" graph).
    // A bean is ready when ALL its deps are already placed in an earlier wave.
    let mut placed: std::collections::HashSet<BeanId> = std::collections::HashSet::new();
    let mut waves: Vec<Wave> = Vec::new();
    let mut remaining: Vec<BeanId> = eager.to_vec();

    while !remaining.is_empty() {
        let mut wave: Vec<BeanId> = remaining
            .iter()
            .copied()
            .filter(|id| deps[id].iter().all(|d| placed.contains(d)))
            .collect();

        if wave.is_empty() {
            // Pass 3 — nothing became ready: the remaining beans are all on a
            // cycle. Classify + report (construction cycle vs @DependsOn cycle).
            return Err(cycle_error(registry, &remaining, &deps, &construction_dep));
        }

        // Deterministic intra-wave order: cmp_chain (RoleTier-first) then BeanId.
        wave.sort_by(|a, b| {
            cmp_chain(&chain_key(registry, *a), &chain_key(registry, *b))
                .then_with(|| a.0.cmp(&b.0))
        });

        for &id in &wave {
            placed.insert(id);
        }
        remaining.retain(|id| !placed.contains(id));
        waves.push(Wave { beans: wave });
    }

    Ok(WiringPlan { waves })
}

// ─────────────────────────── helpers ────────────────────────────────────────

/// Build the [`CandidateSet`] for a produced `TypeId` from the frozen registry's
/// candidate slice — one [`Cand`] read-view per candidate slot.
fn candidate_set(registry: &Registry, produced: std::any::TypeId) -> CandidateSet<'_> {
    let mut set = CandidateSet::new();
    for &cid in registry.candidates(produced) {
        let d = registry.descriptor(cid);
        let mut cand = Cand::new(cid, d.declared_name.unwrap_or(""));
        cand.role = d.meta.candidate_role;
        cand.autowire_candidate = d.meta.autowire_candidate;
        // A concrete self_type match (vs a declared dyn-view) drives the
        // advised-concrete coherence check at validate; here it is informational.
        cand.concrete_match = d.self_type == produced;
        cand.markers = d.meta.markers;
        set.push(cand);
    }
    set
}

/// The [`ChainKey`] for a bean (RoleTier from its `Role`, implicit order).
fn chain_key(registry: &Registry, id: BeanId) -> ChainKey {
    let d = registry.descriptor(id);
    ChainKey {
        tier: RoleTier::of(d.role),
        order: leaf_core::OrderKey::implicit(),
        id: d.contract,
    }
}

/// Build the cycle [`LeafError`]: classify (construction vs pure `@DependsOn`),
/// extract a concrete cycle path, and attach the rich narrative + the LazyRef
/// hint for a construction cycle.
fn cycle_error(
    registry: &Registry,
    remaining: &[BeanId],
    deps: &HashMap<BeanId, Vec<BeanId>>,
    construction_dep: &std::collections::HashSet<(BeanId, BeanId)>,
) -> LeafError {
    let cycle = find_cycle(remaining, deps).unwrap_or_else(|| remaining.to_vec());
    let names: Vec<String> = cycle
        .iter()
        .map(|id| registry.descriptor(*id).declared_name.unwrap_or("<unnamed>").to_string())
        .collect();
    let path = format!("{} → {}", names.join(" → "), names.first().cloned().unwrap_or_default());

    // A cycle is a CONSTRUCTION cycle iff at least one of its consecutive edges is
    // a construction (mandatory bean) edge; otherwise it is a pure @DependsOn cycle.
    let mut is_construction = false;
    for w in cycle.windows(2) {
        if construction_dep.contains(&(w[0], w[1])) {
            is_construction = true;
            break;
        }
    }
    if !is_construction && cycle.len() >= 2 {
        // close the ring edge (last → first)
        let (last, first) = (cycle[cycle.len() - 1], cycle[0]);
        if construction_dep.contains(&(last, first)) {
            is_construction = true;
        }
    }

    if is_construction {
        LeafError::new(ErrorKind::CircularDependency).caused_by(Cause::plain(
            "ordering eager beans",
            format!(
                "constructor-injection cycle: {path}. Single-phase construction has no \
                 early-reference break — convert one edge to a `LazyRef<T>` (or `Lookup<T>`/\
                 `Inject<T>`) deferral handle to remove its construction-time edge."
            ),
        ))
    } else {
        LeafError::new(ErrorKind::DependsOnCycle).caused_by(Cause::plain(
            "ordering eager beans",
            format!("@DependsOn ordering cycle: {path}. Remove one @DependsOn edge to break it."),
        ))
    }
}

/// Find ONE concrete cycle among the remaining (unplaced) beans by a DFS that
/// follows dependency edges back into the remaining set.
fn find_cycle(remaining: &[BeanId], deps: &HashMap<BeanId, Vec<BeanId>>) -> Option<Vec<BeanId>> {
    let live: std::collections::HashSet<BeanId> = remaining.iter().copied().collect();
    let mut stack: Vec<BeanId> = Vec::new();
    let mut on_stack: std::collections::HashSet<BeanId> = std::collections::HashSet::new();
    let mut visited: std::collections::HashSet<BeanId> = std::collections::HashSet::new();

    fn dfs(
        node: BeanId,
        deps: &HashMap<BeanId, Vec<BeanId>>,
        live: &std::collections::HashSet<BeanId>,
        stack: &mut Vec<BeanId>,
        on_stack: &mut std::collections::HashSet<BeanId>,
        visited: &mut std::collections::HashSet<BeanId>,
    ) -> Option<Vec<BeanId>> {
        stack.push(node);
        on_stack.insert(node);
        visited.insert(node);
        if let Some(edges) = deps.get(&node) {
            for &next in edges {
                if !live.contains(&next) {
                    continue;
                }
                if on_stack.contains(&next) {
                    // Found the back-edge: slice the stack from `next` onward.
                    let start = stack.iter().position(|&x| x == next).unwrap_or(0);
                    return Some(stack[start..].to_vec());
                }
                if !visited.contains(&next)
                    && let Some(c) = dfs(next, deps, live, stack, on_stack, visited)
                {
                    return Some(c);
                }
            }
        }
        stack.pop();
        on_stack.remove(&node);
        None
    }

    for &id in remaining {
        if !visited.contains(&id)
            && let Some(c) = dfs(id, deps, &live, &mut stack, &mut on_stack, &mut visited)
        {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::sync::Arc;

    use leaf_core::{
        AnnotationMetadata, BeanKey, BoxFuture, ContractId, Descriptor, InjectionPoint, Origin,
        Provider, Published, RegistryBuilder, ResolveCtx, Role, ScopeDef,
    };

    // ── test fixtures ──────────────────────────────────────────────────────────

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

    fn desc(
        contract: &str,
        name: &'static str,
        self_type: TypeId,
        depends_on: &'static [ContractId],
    ) -> Descriptor {
        // Leak a per-descriptor AnnotationMetadata so depends_on can vary per bean.
        let meta: &'static AnnotationMetadata = Box::leak(Box::new(AnnotationMetadata {
            depends_on,
            ..AnnotationMetadata::EMPTY
        }));
        Descriptor {
            contract: ContractId::of(contract),
            self_type,
            provides: &[],
            declared_name: Some(name),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    fn register(b: &mut RegistryBuilder, d: Descriptor) -> BeanId {
        b.register(d, Arc::new(StubProvider(d))).expect("register")
    }

    // Distinct marker types so each bean has a distinct self_type (no by-type
    // ambiguity in the construction-edge resolution).
    #[derive(Debug)]
    struct TA;
    #[derive(Debug)]
    struct TB;
    #[derive(Debug)]
    struct TC;

    // ── @DependsOn forces wave order ────────────────────────────────────────────

    #[test]
    fn depends_on_forces_the_target_into_a_strictly_earlier_wave() {
        // A @DependsOn B  ⇒  B in an earlier wave than A.
        static B_CONTRACT: ContractId = ContractId::of("w::B");
        static A_DEPS: &[ContractId] = &[B_CONTRACT];

        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "a", TypeId::of::<TA>(), A_DEPS));
        let id_b = register(&mut b, desc("w::B", "b", TypeId::of::<TB>(), &[]));
        let reg = b.freeze().expect("freeze");

        let plan = order_batch(&reg, &[id_a, id_b], &|_| InjectionPlan::EMPTY).expect("ordered");

        let wave_a = plan.wave_of(id_a).expect("a placed");
        let wave_b = plan.wave_of(id_b).expect("b placed");
        assert!(wave_b < wave_a, "B (a @DependsOn target) is strictly earlier: b={wave_b} a={wave_a}");
        assert_eq!(plan.bean_count(), 2);
    }

    #[test]
    fn independent_beans_share_one_wave() {
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "a", TypeId::of::<TA>(), &[]));
        let id_b = register(&mut b, desc("w::B", "b", TypeId::of::<TB>(), &[]));
        let reg = b.freeze().expect("freeze");
        let plan = order_batch(&reg, &[id_a, id_b], &|_| InjectionPlan::EMPTY).expect("ordered");
        assert_eq!(plan.wave_count(), 1, "independent beans are one wave");
        assert_eq!(plan.waves()[0].len(), 2);
    }

    // ── a mandatory construction edge forces wave order ─────────────────────────

    #[test]
    fn a_mandatory_construction_edge_forces_wave_order() {
        // A's InjectionPlan has a single mandatory point of B's concrete type ⇒ B
        // earlier than A.
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "a", TypeId::of::<TA>(), &[]));
        let id_b = register(&mut b, desc("w::B", "b", TypeId::of::<TB>(), &[]));
        let reg = b.freeze().expect("freeze");

        let a = id_a;
        let plan = order_batch(&reg, &[id_a, id_b], &|id| {
            if id == a {
                InjectionPlan { points: point_of::<TB>("b") }
            } else {
                InjectionPlan::EMPTY
            }
        })
        .expect("ordered");

        assert!(plan.wave_of(id_b).unwrap() < plan.wave_of(id_a).unwrap());
    }

    // ── a constructor cycle is CircularDependency with the path + LazyRef hint ──

    #[test]
    fn a_constructor_cycle_is_reported_with_the_path_and_the_lazyref_hint() {
        // A needs B, B needs A (both mandatory construction edges) ⇒ cycle.
        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "alpha", TypeId::of::<TA>(), &[]));
        let id_b = register(&mut b, desc("w::B", "beta", TypeId::of::<TB>(), &[]));
        let reg = b.freeze().expect("freeze");

        let a = id_a;
        let err = order_batch(&reg, &[id_a, id_b], &|id| {
            if id == a {
                InjectionPlan { points: point_of::<TB>("b") }
            } else {
                InjectionPlan { points: point_of::<TA>("a") }
            }
        })
        .expect_err("a constructor cycle is loud");

        assert_eq!(err.kind, ErrorKind::CircularDependency);
        let msg = err.to_string();
        assert!(msg.contains("alpha"), "names the cycle: {msg}");
        assert!(msg.contains("beta"), "names the cycle: {msg}");
        assert!(msg.contains("LazyRef"), "carries the convert-to-LazyRef hint: {msg}");
        assert!(msg.contains("→"), "renders the path: {msg}");
    }

    fn point_of<T: 'static>(name: &'static str) -> &'static [InjectionPoint] {
        Box::leak(Box::new([InjectionPoint::single(TypeId::of::<T>(), name)]))
    }

    // ── a pure @DependsOn cycle is DependsOnCycle ───────────────────────────────

    #[test]
    fn a_pure_depends_on_cycle_is_a_depends_on_cycle() {
        static A_C: ContractId = ContractId::of("w::A");
        static B_C: ContractId = ContractId::of("w::B");
        static A_DEPS: &[ContractId] = &[B_C];
        static B_DEPS: &[ContractId] = &[A_C];

        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "a", TypeId::of::<TA>(), A_DEPS));
        let id_b = register(&mut b, desc("w::B", "b", TypeId::of::<TB>(), B_DEPS));
        let reg = b.freeze().expect("freeze");

        let err = order_batch(&reg, &[id_a, id_b], &|_| InjectionPlan::EMPTY)
            .expect_err("a @DependsOn cycle is loud");
        assert_eq!(err.kind, ErrorKind::DependsOnCycle);
        assert!(err.to_string().contains("@DependsOn"), "got: {err}");
    }

    // ── three-bean chain ⇒ three waves in order ────────────────────────────────

    #[test]
    fn a_chain_a_to_b_to_c_yields_three_ordered_waves() {
        static C_C: ContractId = ContractId::of("w::C");
        static B_C: ContractId = ContractId::of("w::B");
        static B_DEPS: &[ContractId] = &[C_C];
        static A_DEPS: &[ContractId] = &[B_C];

        let mut b = RegistryBuilder::new();
        let id_a = register(&mut b, desc("w::A", "a", TypeId::of::<TA>(), A_DEPS));
        let id_b = register(&mut b, desc("w::B", "b", TypeId::of::<TB>(), B_DEPS));
        let id_c = register(&mut b, desc("w::C", "c", TypeId::of::<TC>(), &[]));
        let reg = b.freeze().expect("freeze");

        let plan = order_batch(&reg, &[id_a, id_b, id_c], &|_| InjectionPlan::EMPTY).expect("ok");
        assert_eq!(plan.wave_count(), 3);
        assert!(plan.wave_of(id_c).unwrap() < plan.wave_of(id_b).unwrap());
        assert!(plan.wave_of(id_b).unwrap() < plan.wave_of(id_a).unwrap());
        // sanity: BeanKey resolution still works on the frozen registry.
        assert!(reg.contains(&BeanKey::ByContract(C_C)));
    }
}
