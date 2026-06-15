//! Integration test `[boot-component-run-gating]`: the END-TO-END proof that a
//! plain `#[component]` bean's `#[conditional(..)]` / `#[profile(..)]` guard is
//! ENFORCED when the app boots through the real `Application::run` pipeline.
//!
//! The gap this guards against (the component analogue of the auto-config one in
//! `auto_config_run_gating.rs`): `from_slices` registers EVERY `COMPONENTS`
//! descriptor UNCONDITIONALLY, and the run pipeline EVALUATED the component guards
//! (`app.route_conditions(&self.guards)`) but DISCARDED the `matched` verdict — so a
//! force-linked `#[component]`'s `OnProperty`/`OnProfile` guard was decorative
//! end-to-end. This drives the WHOLE `Application::run` walk over the link-collected
//! slices alone and asserts the guard actually gates registration:
//!
//! - (a) `comp.enabled` UNSET     → the guarded component is ABSENT (it backs off);
//! - (b) `comp.enabled=true`      → the guarded component IS present + resolves;
//! - (c) `#[profile]` ACTIVE      → the profiled component IS present;
//! - (d) `#[profile]` INACTIVE    → the profiled component is ABSENT (it backs off).
//!
//! An UNGUARDED `#[component]` (the control) must be present in every case.
//!
//! (`#[profile]` gating note: a prior attr-key mismatch — the codegen emitted the
//! profile expression under `"profiles"` while the runtime `OnProfile` reads `"expr"`
//! — made every `#[profile]` guard vacuously-active. That is now fixed: the codegen
//! emits the expression under `"expr"`, so case (d) below is enforceable.)

#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::ContractId;
use leaf_macros::{component, conditional, profile};

// ───────────────────────── the gated, force-linked components ─────────────────────

/// A property-gated `#[component]`: registered only when `comp.enabled=true`. The
/// `#[conditional]` lowers to an `OnProperty` guard auto-collected into
/// `GUARD_PAIRINGS` keyed by this struct's `ContractId`.
#[component]
#[conditional(on_property("comp.enabled", having_value = "true"))]
struct PropGated;
impl PropGated {
    fn new() -> Self {
        PropGated
    }
}

/// A profile-gated `#[component]`: registered only when the `eu` profile is active.
#[component]
#[profile("eu")]
struct ProfileGated;
impl ProfileGated {
    fn new() -> Self {
        ProfileGated
    }
}

/// An UNGUARDED `#[component]` — the control bean that must ALWAYS be present (it
/// proves the prune touches only guarded-and-unmatched contracts).
#[component]
struct AlwaysOn;
impl AlwaysOn {
    fn new() -> Self {
        AlwaysOn
    }
}

fn prop_gated_contract() -> ContractId {
    ContractId::of(&format!("{}::PropGated", module_path!()))
}
fn profile_gated_contract() -> ContractId {
    ContractId::of(&format!("{}::ProfileGated", module_path!()))
}
fn always_on_contract() -> ContractId {
    ContractId::of(&format!("{}::AlwaysOn", module_path!()))
}

/// The force-linked tokio runtime handle (the only non-annotation input).
fn spawner() -> Arc<dyn leaf_core::Spawner> {
    Arc::new(leaf_tokio::TokioExecutionFacility::new())
}

// ─────────────────────────────── the gating proofs ────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_component_backs_off_when_the_gate_property_is_unset() {
    // (a) comp.enabled UNSET → the force-linked component must BACK OFF. Against the
    // pre-fix code this FAILS: from_slices registered the COMPONENTS row regardless of
    // the guard and the run pipeline discarded the `matched` verdict, so the bean was
    // in the context even with the property unset.
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("comp-app")
        .with_spawner(spawner())
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app runs to Ready");

    let registry = running.context().engine().registry();
    assert!(
        registry.by_contract(prop_gated_contract()).is_none(),
        "comp.enabled unset → the guarded component must NOT be registered (it backs off)"
    );
    assert!(
        registry.by_contract(always_on_contract()).is_some(),
        "the unguarded control component must ALWAYS be registered"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_component_registers_when_the_gate_property_is_set() {
    // (b) comp.enabled=true → the guard matches → the component is present + resolves.
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("comp-app")
        .with_spawner(spawner())
        .run(
            SealInputs::new().with_args(["--comp.enabled=true"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    let registry = running.context().engine().registry();
    assert!(
        registry.by_contract(prop_gated_contract()).is_some(),
        "comp.enabled=true → the guarded component IS registered (the guard matched)"
    );
    let bean = running.context().get::<PropGated>().await.expect("the component resolves");
    let _ = &*bean;
    assert!(
        registry.by_contract(always_on_contract()).is_some(),
        "the unguarded control component must ALWAYS be registered"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profiled_component_backs_off_when_the_profile_is_inactive() {
    // (d) the `eu` profile is NOT active → the profile-gated component must BACK OFF.
    // Against the pre-fix attr-key mismatch this FAILED: the OnProfile guard read an
    // absent `"expr"` attr and early-returned matched=true (vacuously active), so the
    // component was present regardless of the active profiles. With the codegen now
    // emitting the profile expression under `"expr"`, the guard evaluates `eu` against
    // the (empty) active set, fails to match, and the prune removes the component.
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("comp-app")
        .with_spawner(spawner())
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app runs to Ready");

    let registry = running.context().engine().registry();
    assert!(
        registry.by_contract(profile_gated_contract()).is_none(),
        "the `eu` profile is inactive → the profile-gated component must back off"
    );
    assert!(
        registry.by_contract(always_on_contract()).is_some(),
        "the unguarded control component must ALWAYS be registered"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profiled_component_registers_when_the_profile_is_active() {
    // (c) `eu` profile active → the profile-gated component is present (the guard
    // matches, so the prune leaves it in the builder).
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("comp-app")
        .with_spawner(spawner())
        .run(
            SealInputs::new().with_args(["--leaf.profiles.active=eu"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    let registry = running.context().engine().registry();
    assert!(
        registry.by_contract(profile_gated_contract()).is_some(),
        "`eu` profile active → the profile-gated component IS registered"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}
