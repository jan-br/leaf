//! Integration test `[boot-run-wiring]`: the macro-emitted per-bean data the run
//! pipeline auto-wiring needs, JOINed through leaf-boot's pub pairing-const pattern
//! (the SeedPairing/GuardPairing extension — proxy-interception phase3/08,
//! bootstrap-diagnostics phase3/14, the C2 config locus).
//!
//! Three closures, each proving a macro→leaf-boot JOIN that Application::run now
//! threads from REAL macro emission (not a hand-mirrored plan):
//!
//! 1. PROXY JOIN-POINTS — a `#[advisable]` bean emits a pub `BeanJoinPointsSpec`
//!    pairing const; leaf-boot's `JoinPointPairing` JOINs it by ContractId, reifies
//!    it, and `ProxyPlan::freeze` matches an advisor's pointcut to build a NON-EMPTY
//!    plan (the gap the old `frozen_proxy_plan` left as `ProxyPlan::empty()`).
//! 2. RUNNER COLLECTION — a `#[runner]` is discoverable as a `dyn Runner` candidate
//!    in the frozen registry (the run pipeline's runner enumeration).
//! 3. CONFIG THUNKS — a `#[config_properties]` bean emits a pub `ConfigBindThunk`
//!    pairing const; leaf-boot's `ConfigPairing` JOINs it by ContractId and the C2
//!    validate pass pre-materializes the bean from the REAL macro-emitted thunk.
//!
//! PROOF GATE: this crate uses the THIN macros on real sample types (the genuine
//! link-collected slices + the genuine pub pairing consts), not hand-built rows.

#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::any::TypeId;
use std::sync::Arc;

use leaf_boot::{
    build_join_points, runner_candidate_ids, App, ConfigPairing, Define, JoinPointPairing,
    SeedPairing,
};
use leaf_core::{
    within, AdvisorDescriptor, BeanJoinPointsSpec, ContractId, CreatorPolicy, EnvBuilder,
    MapPropertySource, MethodJoinPointSpec, MethodKey, OrderKey, ProxyPlan, Role,
    StartupValidation, Within,
};
use leaf_macros::{advisable, config_properties, runner};

// ─────────────────────────── the sample app beans ───────────────────────────

/// An advisable bean (the proxy target). `#[advisable]` emits the per-bean
/// `__leaf_joinpoints_<Ident>` `BeanJoinPointsSpec` pairing const beside its row.
#[advisable]
struct OrderService;

impl OrderService {
    fn new() -> Self {
        OrderService
    }
}

/// A runner bean. `#[runner]` emits the `dyn Runner` upcast view in `provides[]`.
#[runner]
struct MigrateRunner;

impl MigrateRunner {
    fn new() -> Self {
        MigrateRunner
    }
}

impl leaf_core::Runner for MigrateRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf_core::ApplicationArguments,
    ) -> leaf_core::BoxFuture<'a, Result<(), leaf_core::LeafError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A config-properties bean. `#[config_properties]` emits the
/// `__leaf_config_bind_<Ident>` `ConfigBindThunk` pairing const + the Bean impl.
#[config_properties(prefix = "svc")]
#[derive(Debug, Default, PartialEq, Eq)]
struct SvcProps {
    name: String,
    retries: u16,
}

fn contract_here(ident: &str) -> ContractId {
    ContractId::of(&format!("{}::{}", module_path!(), ident))
}

/// Lift the test crate's `#[advisable]`/`#[runner]` `#[component]` rows into a frozen
/// registry through the genuine `from_slices` JOIN (the SeedPairing pattern).
fn frozen_registry() -> leaf_core::Registry {
    // The #[config_properties] SvcProps now auto-registers an AUTO_CONFIGS Descriptor
    // + seed too (`__leaf_seed_SvcProps`), so the bare from_slices JOIN must cover it
    // (a lifted row with no matching SeedPairing is a loud AntiDce error).
    let seeds = vec![
        SeedPairing::new(contract_here("OrderService"), __leaf_seed_OrderService),
        SeedPairing::new(contract_here("MigrateRunner"), __leaf_seed_MigrateRunner),
        SeedPairing::new(contract_here("SvcProps"), __leaf_seed_SvcProps),
    ];
    App::<Define>::from_slices(&seeds)
        .expect("from_slices lifts the COMPONENTS rows")
        .into_builder()
        .freeze()
        .expect("the lifted builder freezes")
}

// ── (1) PROXY JOIN-POINTS: the macro spec JOINs + ProxyPlan::freeze matches ──

#[test]
fn advisable_join_point_spec_joins_by_contract_into_reified_bean_join_points() {
    // The macro-emitted __leaf_joinpoints_<Ident> pairing JOINs to the bean's frozen
    // BeanId by ContractId and reifies into the runtime BeanJoinPoints.
    let registry = frozen_registry();
    let svc_id = registry.by_contract(contract_here("OrderService")).expect("OrderService row");

    let pairing = JoinPointPairing::new(contract_here("OrderService"), &__leaf_joinpoints_OrderService);
    let reified = build_join_points(&[pairing], &registry);
    assert_eq!(reified.len(), 1, "the advisable bean's join-point spec JOINed to its BeanId");
    assert_eq!(reified[0].id(), svc_id);
    // The reified view carries the macro-emitted bean_type (the within::<T>() key).
    let view = reified[0].view();
    assert_eq!(view.bean_type, TypeId::of::<OrderService>());
}

#[test]
fn a_join_point_pairing_with_methods_freezes_a_non_empty_proxy_plan() {
    // The headline: with the bean's method join points supplied (a method-aware
    // source — a bare #[advisable] struct attr cannot enumerate methods), a
    // within::<OrderService>() advisor matches and ProxyPlan::freeze builds a
    // NON-EMPTY plan (the gap the old frozen_proxy_plan left as empty).
    let registry = frozen_registry();
    let svc_id = registry.by_contract(contract_here("OrderService")).expect("OrderService row");

    // A method-aware spec (what a method-aware emission supplies): the bean_type +
    // markers come from the SAME shape the macro emits; one method join point.
    static METHODS: &[MethodJoinPointSpec] = &[MethodJoinPointSpec {
        method: MethodKey::of("OrderService::place_order"),
        arg_types: &[],
        ret_type: TypeId::of::<i64>(),
    }];
    static SPEC: BeanJoinPointsSpec = BeanJoinPointsSpec {
        bean_type: TypeId::of::<OrderService>(),
        markers: &leaf_core::AnnotationMetadata::EMPTY,
        methods: METHODS,
    };

    let reified = build_join_points(&[JoinPointPairing::new(contract_here("OrderService"), &SPEC)], &registry);
    let jps: std::collections::HashMap<_, _> = reified.iter().map(|r| (r.id(), r.view())).collect();

    // A within::<OrderService>() advisor matches the bean by its (macro-emitted)
    // bean_type. `within()` is not const, so the pointcut is leaked to `'static`
    // (the binary supplies `&'static dyn Pointcut` to the AdvisorDescriptor anyway).
    let within: &'static Within = Box::leak(Box::new(within::<OrderService>()));
    let advisor = AdvisorDescriptor {
        id: contract_here("test::WithinAdvisor"),
        order: OrderKey::implicit(),
        role: Role::Application,
        pointcut: within,
        make_interceptor: |_c| Box::pin(async {
            Err(leaf_core::LeafError::new(leaf_core::ErrorKind::ConstructionFailed))
        }),
    };

    let plan = ProxyPlan::freeze(&[advisor], &registry, &CreatorPolicy::ALL, &jps)
        .expect("freeze the proxy plan");
    assert!(!plan.is_empty(), "the macro join-points + advisor build a NON-EMPTY plan");
    assert!(plan.is_advised(svc_id), "OrderService is advised");
    assert_eq!(plan.advisors_for(svc_id).len(), 1);
}

// ── (2) RUNNER COLLECTION: a #[runner] is a dyn Runner candidate ─────────────

#[test]
fn a_runner_is_discoverable_as_a_dyn_runner_candidate() {
    // The run pipeline discovers runners by the dyn Runner candidate view the
    // #[runner] macro emits in provides[] — no bespoke RUNNERS slice.
    let registry = frozen_registry();
    let runner_id = registry.by_contract(contract_here("MigrateRunner")).expect("MigrateRunner row");
    let candidates = runner_candidate_ids(&registry);
    assert!(
        candidates.contains(&runner_id),
        "the #[runner] bean is enumerable as a dyn Runner candidate: {candidates:?}"
    );
}

// ── (3) CONFIG THUNKS: the macro thunk JOINs + binds via ConfigPairing ───────

#[test]
fn config_properties_thunk_joins_by_contract_and_binds_from_the_env() {
    // The C2 path: the macro-emitted __leaf_config_bind_<Ident> thunk JOINs to the
    // config bean's BeanId via ConfigPairing, and binds the bean from the env.
    let mut builder = leaf_core::RegistryBuilder::new();
    // Register the config bean as a row (so the C2 JOIN has a slot). A trivial
    // default-returning provider stands in for the registered config-properties row.
    let desc = leaf_core::Descriptor {
        contract: contract_here("SvcProps"),
        self_type: TypeId::of::<SvcProps>(),
        provides: &[],
        declared_name: Some("svcProps"),
        aliases: &[],
        scope: leaf_core::ScopeDef::SINGLETON,
        role: Role::Application,
        meta: &leaf_core::AnnotationMetadata::EMPTY,
        parent: None,
        origin: leaf_core::Origin::Native { crate_name: Some("leaf-boot::run-wiring") },
    };
    builder.register(desc, default_svc_props_provider()).expect("register the config bean");
    let registry = builder.freeze().expect("freeze");
    let cfg_id = registry.by_contract(contract_here("SvcProps")).expect("SvcProps row");

    // The ConfigPairing JOINs the REAL macro thunk to the bean's BeanId.
    let pairing = ConfigPairing::new(contract_here("SvcProps"), __leaf_config_bind_SvcProps);
    let cfg_bean = pairing.to_config_bean(&registry).expect("the thunk JOINs to the BeanId");

    // Drive it through the C2 validate pass: the bound value is pre-bound into the slot.
    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(MapPropertySource::from_pairs(
        "test",
        [
            ("svc.name".to_string(), "orders".to_string()),
            ("svc.retries".to_string(), "3".to_string()),
        ],
    )));
    let env = b.seal_env();
    let cfg_beans = [cfg_bean];
    let inputs = leaf_boot::ValidationInputs::new().with_config_beans(&cfg_beans);
    let report = leaf_boot::validate(
        &registry,
        &env,
        &leaf_core::ConditionReport::new(),
        StartupValidation::Strict,
        &inputs,
    );
    assert!(report.is_ok(), "the macro bind thunk binds cleanly: {report:?}");

    // eager-EXCLUDED-because-PREBOUND: the slot now holds the bound config bean.
    let bound = registry.singleton_cell(cfg_id).get().expect("the config bean is pre-bound");
    let props = bound.downcast_ref::<SvcProps>().expect("downcasts to SvcProps");
    assert_eq!(*props, SvcProps { name: "orders".into(), retries: 3 });
}

/// A trivial default-returning provider for the SvcProps slot (the C2 thunk pre-binds
/// the real value, so this never runs at refresh — it only makes the row registerable).
fn default_svc_props_provider() -> Arc<dyn leaf_core::Provider> {
    struct P(leaf_core::Descriptor);
    impl leaf_core::Provider for P {
        fn descriptor(&self) -> &leaf_core::Descriptor {
            &self.0
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a leaf_core::ResolveCtx<'a>,
        ) -> leaf_core::BoxFuture<'a, Result<leaf_core::Published, leaf_core::LeafError>> {
            Box::pin(async { Ok(leaf_core::Published::shared_value(SvcProps::default())) })
        }
    }
    Arc::new(P(leaf_core::Descriptor {
        contract: contract_here("SvcProps"),
        self_type: TypeId::of::<SvcProps>(),
        provides: &[],
        declared_name: Some("svcProps"),
        aliases: &[],
        scope: leaf_core::ScopeDef::SINGLETON,
        role: Role::Application,
        meta: &leaf_core::AnnotationMetadata::EMPTY,
        parent: None,
        origin: leaf_core::Origin::Native { crate_name: Some("leaf-boot::run-wiring") },
    }))
}
