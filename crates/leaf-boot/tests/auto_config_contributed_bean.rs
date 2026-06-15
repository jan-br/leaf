//! Integration test `[boot-auto-config-contributed-bean]`: the CLEAN, NON-REDIS proof
//! that `#[auto_config] impl Holder { #[bean] fn .. }` contributes a DIFFERENTLY-TYPED
//! bean (Spring's `@AutoConfiguration`-with-`@Bean`-method shape) into the SEPARATE
//! `AUTO_CONFIGS` channel at `CandidateRole::FALLBACK`, resolvable as an `Arc<dyn Svc>`
//! dyn-view, with a `#[conditional(..)]` back-off guard — driven through the REAL
//! leaf-boot `run_autoconfig` ladder (the public `Application::run_autoconfig` gating
//! path the redis crate's `auto_config_ladder` also exercises).
//!
//! It proves the macro capability independent of redis:
//! 1. the macro emits the contributed product into `AUTO_CONFIGS` at FALLBACK with the
//!    `dyn Greeter` provides[] view (a bean DIFFERENT from the holder struct);
//! 2. the three load-bearing contracts ALIGN: `Descriptor.contract ==
//!    SeedPairingRow.contract == GuardPairingRow.contract` for the contributed bean;
//! 3. the candidate built from the macro artifacts REGISTERS at FALLBACK when the
//!    `OnProperty` gate is set and no user bean exists, and resolves as `Arc<dyn Greeter>`;
//! 4. it BACKS OFF (the soft override) when a user bean of the contributed type is
//!    already present (the `OnMissingBean` leaf).
//!
//! NOTE on the run path: a macro-emitted guarded auto-config rides the `AUTO_CONFIGS`
//! distributed slice, but the GATING `exclude > back-off > default` ladder is leaf-boot's
//! `run_autoconfig` (driven here from the macro-emitted Descriptor + seed + guard — the
//! same JOIN leaf-boot performs over the slices). The bare `from_slices` lift registers
//! a slice row unconditionally; the ladder is the path that evaluates the back-off, so
//! this drives it directly (exactly like the redis integration test).

#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::any::TypeId;
use std::sync::Arc;

use leaf_boot::{run_autoconfig, AutoConfigCandidate, ExclusionSet};
use leaf_core::{
    ActiveProfiles, CandidateRole, ContractId, Descriptor, Env, EnvBuilder, MapPropertySource,
    RegistryBuilder,
};
use leaf_macros::auto_config;

// ─────────────────────────── the contributed dyn service ─────────────────────────

/// The differently-typed service the auto-config contributes (a `dyn Svc` view, NOT
/// the holder struct) — the clean analogue of redis's `Arc<dyn CacheManager>`.
trait Greeter: Send + Sync {
    fn greet(&self) -> String;
}

/// The concrete product the holder's `#[bean]` method builds.
#[derive(Debug)]
struct PoliteGreeter {
    who: String,
}

impl Greeter for PoliteGreeter {
    fn greet(&self) -> String {
        format!("hello, {}", self.who)
    }
}

// ───────────────────────────── the auto-config holder ────────────────────────────

/// The auto-configuration HOLDER (a unit struct registered as a `#[component]` so the
/// container manages it; its `#[bean]` method reads it as `&self`). The holder is a
/// plain bean — the CONTRIBUTION is the method's product.
#[leaf_macros::component]
struct GreetingAutoConfig;

impl GreetingAutoConfig {
    fn new() -> Self {
        GreetingAutoConfig
    }
}

/// THE DIFFERENTLY-TYPED CONTRIBUTION: `#[auto_config] impl` whose `#[bean]` method
/// contributes a `dyn Greeter` (named "greeter") into `AUTO_CONFIGS` at FALLBACK,
/// gated by `OnProperty(greeting.enabled)` AND `OnMissingBean(PoliteGreeter)` (the
/// soft-override back-off).
#[auto_config]
impl GreetingAutoConfig {
    #[bean(name = "greeter", provides = "dyn Greeter")]
    #[conditional(
        on_property("greeting.enabled", having_value = "true"),
        on_missing_bean(PoliteGreeter)
    )]
    fn greeter(&self) -> PoliteGreeter {
        PoliteGreeter { who: "leaf".into() }
    }
}

// The macro emits, beside the contributed Descriptor:
//   * `__leaf_seed_greeter`   — the ProviderSeed building the contributed bean,
//   * `__leaf_guard_greeter`  — the back-off CondExpr guard,
// both keyed off the METHOD ident (the contributed-bean pairing key).

/// The contributed bean's stable contract (`module_path!()::greeter`) — the JOIN key
/// the Descriptor, the seed pairing, AND the guard pairing all share.
fn greeter_contract() -> ContractId {
    ContractId::of(&format!("{}::greeter", module_path!()))
}

/// Find the contributed `AUTO_CONFIGS` Descriptor by its derived name.
fn contributed_descriptor() -> Descriptor {
    *leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.declared_name == Some("greeter"))
        .expect("the contributed bean must reach the AUTO_CONFIGS slice")
}

/// The holder `#[component]` Descriptor (a `@bean` method reads it as `&self`, so the
/// container must manage the holder — exactly as a real app registers it from COMPONENTS).
fn holder_descriptor() -> Descriptor {
    *leaf_core::COMPONENTS
        .iter()
        .find(|d| d.declared_name == Some("greetingAutoConfig"))
        .expect("the holder must be a COMPONENTS row")
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

/// Build the one auto-config candidate from the macro-emitted const artifacts
/// (Descriptor + seed + guard) — exactly the JOIN leaf-boot performs over the slices.
fn greeting_candidate() -> AutoConfigCandidate {
    AutoConfigCandidate::new(contributed_descriptor(), __leaf_seed_greeter, Some(&__leaf_guard_greeter))
}

fn run(
    env: &Env,
    seed_probe: &[(TypeId, CandidateRole)],
) -> (usize, RegistryBuilder) {
    let mut builder = RegistryBuilder::new();
    // Register the holder #[component] first (the container manages it; the contributed
    // @bean method resolves it as the `&self` receiver — singleton-correct).
    builder
        .register(holder_descriptor(), __leaf_seed_GreetingAutoConfig())
        .expect("the holder component registers");
    let cands = [greeting_candidate()];
    let out = run_autoconfig(
        &cands,
        env,
        &mut builder,
        &ExclusionSet::new(),
        &ActiveProfiles::default(),
        seed_probe,
    )
    .expect("the ladder runs");
    (out.registered, builder)
}

// ─────────────────────────────── the capability proofs ───────────────────────────

#[test]
fn the_contribution_is_a_fallback_auto_config_carrying_the_dyn_greeter_view() {
    // (1) the headline: the #[bean] method's PRODUCT (a dyn Greeter, DIFFERENT from the
    // holder GreetingAutoConfig) lands in AUTO_CONFIGS at FALLBACK, carrying the
    // dyn-view so a consumer resolving Arc<dyn Greeter> finds it.
    let d = contributed_descriptor();
    assert_eq!(
        d.meta.candidate_role,
        CandidateRole::FALLBACK,
        "an auto-config contribution registers at FALLBACK (a user bean supersedes it)"
    );
    // The product is the CONCRETE PoliteGreeter (the method return type), exposed as a
    // dyn Greeter via provides[] — exactly redis's RedisCacheManager / dyn CacheManager.
    assert_eq!(d.self_type, TypeId::of::<PoliteGreeter>(), "the product is the concrete bean");
    assert!(
        d.provides.iter().any(|r| r.view == TypeId::of::<dyn Greeter>()),
        "the contribution declares the dyn Greeter provides[] view"
    );
    // It must NOT be a COMPONENTS row (component-scanning never picks an auto-config up).
    assert!(
        !leaf_core::COMPONENTS.iter().any(|c| c.declared_name == Some("greeter")),
        "the contribution must not be a COMPONENTS row"
    );
}

#[test]
fn the_three_contracts_align_for_the_contributed_bean() {
    // (2) THE LOAD-BEARING INVARIANT: Descriptor.contract == SeedPairingRow.contract ==
    // GuardPairingRow.contract for the contributed bean, so leaf-boot's JOIN finds the
    // seed AND the guard by the one contract.
    let contract = greeter_contract();
    assert_eq!(contributed_descriptor().contract, contract, "the Descriptor keys on it");

    let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
    assert!(
        seeds.iter().any(|r| r.contract == contract),
        "the SeedPairingRow keys on the SAME contributed contract"
    );

    let guards = leaf_core::collect_slice(&leaf_core::GUARD_PAIRINGS);
    assert!(
        guards.iter().any(|r| r.contract == contract),
        "the GuardPairingRow keys on the SAME contributed contract (the alignment)"
    );
}

#[test]
fn registers_at_fallback_when_enabled_and_unclaimed() {
    // (3) enabled + no user bean → the guard matches → the contribution registers, and
    // resolves as Arc<dyn Greeter> through the dyn-view.
    let (registered, builder) = run(&env_with(&[("greeting.enabled", "true")]), &[]);
    assert_eq!(registered, 1, "enabled + unclaimed → the contributed bean wires");
    assert_eq!(builder.len(), 2, "the holder + the contributed bean");

    // The contributed bean resolves (its real #[bean] factory body ran), AND the dyn
    // Greeter view is indexed so a consumer resolving the trait object finds it.
    let registry = builder.freeze().expect("the lifted builder freezes");
    assert!(
        registry.by_contract(greeter_contract()).is_some(),
        "the contributed bean is registered by its contract"
    );
    let engine = leaf_core::Engine::new(registry);
    let greeter = futures::executor::block_on(engine.get::<PoliteGreeter>())
        .expect("the contributed bean resolves (the real #[bean] factory ran)");
    assert_eq!(greeter.greet(), "hello, leaf", "the real #[bean] factory body produced it");
    // The dyn Greeter view resolves to the SAME contributed bean (the provides[] upcast).
    let erased = futures::executor::block_on(
        engine.get_erased(leaf_core::BeanKey::ByType(TypeId::of::<dyn Greeter>())),
    )
    .expect("the dyn Greeter view resolves to the contributed bean");
    assert!(
        erased.downcast::<PoliteGreeter>().is_ok(),
        "the dyn-view erased bean is the concrete PoliteGreeter"
    );
}

#[test]
fn backs_off_when_the_enable_property_is_unset() {
    // The OnProperty leaf gates participation: absent → the whole guard is Negative.
    let (registered, builder) = run(&env_with(&[]), &[]);
    assert_eq!(registered, 0, "absent enable property → the auto-config backs off");
    assert_eq!(builder.len(), 1, "only the holder; the contributed bean backed off");
}

#[test]
fn a_user_bean_of_the_contributed_type_supersedes_the_auto_config() {
    // (4) the soft override: a user bean of the contributed type already present →
    // OnMissingBean sees it → the auto-config backs off (the user bean wins).
    let (registered, builder) = run(
        &env_with(&[("greeting.enabled", "true")]),
        &[(TypeId::of::<PoliteGreeter>(), CandidateRole::NORMAL)],
    );
    assert_eq!(registered, 0, "OnMissingBean backs off: the user Greeter supersedes");
    assert_eq!(builder.len(), 1, "only the holder; the contributed bean backed off");
}
