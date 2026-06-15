//! Integration test `[boot-resolve-walk]`: the `App<Define> → App<Resolve>` walk
//! over GENUINE link-collected `#[component]` rows.
//!
//! Drives the real thin-macro discovery slice through `from_slices` → the 5f
//! `seal_environment` fence → `route_conditions` (Parse/Register) → the
//! `run_autoconfig` ladder, proving the unit-2 engines wire up end-to-end against
//! a real `Descriptor`/`ProviderSeed` JOIN (not a hand-built row) and that:
//!
//! - command-line env precedence beats a config-file value;
//! - a `#[conditional]`-style property guard gates a definition;
//! - `exclude(ContractId)` removes an auto-config candidate before back-off;
//! - a user `@Component` beats an auto-config Fallback of the same contract
//!   (the soft override via the auto-config back-off probe).

use std::any::TypeId;

use leaf_boot::{
    run_autoconfig, App, AutoConfigCandidate, Define, ExclusionSet, GuardPairing, ImportLocation,
    SealInputs, SeedPairing,
};
use leaf_core::{
    Attr, AttrSlice, CandidateRole, CondExpr, ContractId, PropertyResolver, StartupValidation,
};
use leaf_macros::component;

/// A real `#[component]` lifted through the genuine `COMPONENTS` linkme slice.
#[component]
struct Widget;

impl Widget {
    fn new() -> Self {
        Widget
    }
}

fn pairings() -> Vec<SeedPairing> {
    vec![SeedPairing::new(
        ContractId::of(&format!("{}::Widget", module_path!())),
        __leaf_seed_Widget,
    )]
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

#[test]
fn define_to_resolve_seals_env_with_command_line_precedence() {
    // The real lifted builder transitions through the 5f fence; the command-line
    // value beats the config-file value (cmdline > file precedence).
    let json = leaf_config::JsonLoader;
    let loaders: [&dyn leaf_config::ConfigDataLoader; 1] = [&json];
    let _ = loaders; // (the default loader set drives the inline doc below)

    let app = App::<Define>::from_slices(&pairings()).expect("lift the real Widget row");
    let resolve = block_on(app.seal_environment(
        SealInputs::new()
            .with_args(["--server.port=8080"])
            .with_import(ImportLocation::inline(
                "application.json",
                leaf_config::PrecedenceRung::ConfigDataFile {
                    group: 0,
                    profile_specific: false,
                    external: false,
                },
                r#"{"server":{"port":"1111"}}"#,
            )),
        Vec::new(),
    ))
    .expect("seal_environment");

    assert_eq!(
        resolve.env().get("server.port").unwrap().raw,
        "8080",
        "command-line precedence beats the config file"
    );
    assert_eq!(resolve.settings().startup_validation, StartupValidation::Strict);
}

#[test]
fn a_property_guard_gates_a_definition_through_the_typestate() {
    use leaf_conditions::{ConditionKind, OnProperty};
    static ATTRS: &[Attr] = &[Attr::Str("name", "feature.x")];
    static GUARD: CondExpr = CondExpr::Leaf(OnProperty::ID, ATTRS);

    // feature.x present → matched.
    let app = App::<Define>::from_slices(&pairings()).expect("lift");
    let mut on = block_on(app.seal_environment(
        SealInputs::new().with_args(["--feature.x=true"]),
        Vec::new(),
    ))
    .expect("seal");
    let g = GuardPairing::new(ContractId::of("app::Gated"), None, &GUARD);
    let matched = on.route_conditions(&[g]).expect("routes");
    assert!(matched.contains(&ContractId::of("app::Gated")));

    // feature.x absent → backs off (NOT matched), recorded in the report.
    let app = App::<Define>::from_slices(&pairings()).expect("lift");
    let mut off = block_on(app.seal_environment(SealInputs::new(), Vec::new())).expect("seal");
    let g = GuardPairing::new(ContractId::of("app::Gated"), None, &GUARD);
    let matched = off.route_conditions(&[g]).expect("routes (a miss is not an error)");
    assert!(!matched.contains(&ContractId::of("app::Gated")));
    let report = off.condition_report();
    assert!(report.lookup(ContractId::of("app::Gated")).is_some());
}

// ── a hand-built auto-config candidate of a contract a user bean can supersede ─

#[derive(Debug)]
struct Pool;

struct PoolProvider(leaf_core::Descriptor);
impl leaf_core::Provider for PoolProvider {
    fn descriptor(&self) -> &leaf_core::Descriptor {
        &self.0
    }
    fn provide<'a>(
        &'a self,
        _cx: &'a leaf_core::ResolveCtx<'a>,
    ) -> leaf_core::BoxFuture<'a, Result<leaf_core::Published, leaf_core::LeafError>> {
        Box::pin(async { Ok(leaf_core::Published::shared_value(Pool)) })
    }
}

static FALLBACK_META: leaf_core::AnnotationMetadata = leaf_core::AnnotationMetadata {
    qualifiers: &[],
    markers: &[],
    depends_on: &[],
    candidate_role: CandidateRole::FALLBACK,
    autowire_candidate: true,
};

fn auto_descriptor() -> leaf_core::Descriptor {
    leaf_core::Descriptor {
        contract: ContractId::of("app::PoolAutoConfig"),
        self_type: TypeId::of::<Pool>(),
        provides: &[],
        declared_name: Some("autoPool"),
        aliases: &[],
        scope: leaf_core::ScopeDef::SINGLETON,
        role: leaf_core::Role::Application,
        meta: &FALLBACK_META,
        parent: None,
        origin: leaf_core::Origin::Native { crate_name: Some("test") },
    }
}

fn auto_seed() -> std::sync::Arc<dyn leaf_core::Provider> {
    std::sync::Arc::new(PoolProvider(auto_descriptor()))
}

fn on_missing_pool() -> &'static CondExpr {
    use leaf_conditions::{ConditionKind, OnMissingBean};
    let attrs: AttrSlice = Box::leak(Box::new([Attr::Type("type", TypeId::of::<Pool>())]));
    Box::leak(Box::new(CondExpr::Leaf(OnMissingBean::ID, attrs)))
}

#[test]
fn exclude_removes_an_auto_config_candidate_before_back_off() {
    let app = App::<Define>::from_slices(&pairings()).expect("lift");
    let mut resolve = block_on(app.seal_environment(
        SealInputs::new().with_args(["--leaf.autoconfigure.exclude=app::PoolAutoConfig"]),
        Vec::new(),
    ))
    .expect("seal");

    let excl = ExclusionSet::merge(&[], &[], resolve.env());
    let cands = [AutoConfigCandidate::new(auto_descriptor(), auto_seed, None)];
    let before = resolve.len();
    let n = resolve.run_autoconfig(&cands, &excl).expect("ladder runs");
    assert_eq!(n, 0, "the excluded candidate mints no bean");
    assert_eq!(resolve.len(), before, "the builder did not grow");
}

#[test]
fn a_user_component_beats_an_auto_config_fallback_of_the_same_contract() {
    // A user @Component of type Pool is in the inventory (non-fallback); the
    // auto-config's OnMissingBean back-off sees it and the Fallback default does
    // NOT register — the user bean transparently supersedes.
    let app = App::<Define>::from_slices(&pairings()).expect("lift");
    let inventory = vec![(TypeId::of::<Pool>(), CandidateRole::NORMAL)];
    let mut resolve =
        block_on(app.seal_environment(SealInputs::new(), inventory)).expect("seal");

    let cands = [AutoConfigCandidate::new(auto_descriptor(), auto_seed, Some(on_missing_pool()))];
    let n = resolve.run_autoconfig(&cands, &ExclusionSet::new()).expect("ladder runs");
    assert_eq!(n, 0, "OnMissingBean backs off: the user @Component wins");

    // With NO user bean present, the same auto-config DOES register (the default).
    let app = App::<Define>::from_slices(&pairings()).expect("lift");
    let mut resolve = block_on(app.seal_environment(SealInputs::new(), Vec::new())).expect("seal");
    let cands = [AutoConfigCandidate::new(auto_descriptor(), auto_seed, Some(on_missing_pool()))];
    let n = resolve.run_autoconfig(&cands, &ExclusionSet::new()).expect("ladder runs");
    assert_eq!(n, 1, "no user bean → the auto-config default applies");
}

#[test]
fn the_standalone_run_autoconfig_engine_is_drivable_directly() {
    // The engine is also callable as a free fn (test-slice mode) over an explicit
    // builder + candidate subset.
    let mut builder = leaf_core::RegistryBuilder::new();
    let env = {
        let mut b = leaf_core::EnvBuilder::new();
        b.add_last(std::sync::Arc::new(leaf_core::MapPropertySource::from_pairs(
            "t",
            [("k", "v")],
        )));
        b.seal_env()
    };
    let cands = [AutoConfigCandidate::new(auto_descriptor(), auto_seed, None)];
    let out = run_autoconfig(
        &cands,
        &env,
        &mut builder,
        &ExclusionSet::new(),
        &leaf_core::ActiveProfiles::default(),
        &[],
    )
    .expect("runs");
    assert_eq!(out.registered, 1);
    assert_eq!(builder.len(), 1);
    let _ = Widget::new();
}
