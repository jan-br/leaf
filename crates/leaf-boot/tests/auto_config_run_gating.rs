//! Integration test `[boot-auto-config-run-gating]`: the END-TO-END proof that a
//! force-linked `#[auto_config]` bean's `#[conditional(..)]` guard is ENFORCED when
//! the app boots through the real `Application::run` pipeline (NOT only when the
//! `run_autoconfig` ladder is driven directly with an explicit candidate).
//!
//! The gap this guards against: `from_slices` used to register EVERY `AUTO_CONFIGS`
//! descriptor UNCONDITIONALLY, so a force-linked auto-config's `OnProperty` /
//! `OnMissingBean` guard was decorative end-to-end. This drives the WHOLE
//! `Application::run` walk over the link-collected slices alone (no `.with_autoconfig`)
//! and asserts the guard actually gates registration:
//!
//! - (a) `gate.enabled` UNSET  → the auto-config bean BACKS OFF (not in the context);
//! - (b) `gate.enabled=true`   → the auto-config bean IS present + resolves;
//! - (c) `OnMissingBean`       → a user bean of the same type supersedes the default.

#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::any::TypeId;
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{CandidateRole, ContractId};
use leaf_macros::auto_config;

// ─────────────────────── the gated, force-linked auto-config ──────────────────────

/// The differently-typed product the gated auto-config contributes.
trait Clock: Send + Sync {
    fn now(&self) -> &'static str;
}

#[derive(Debug)]
struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> &'static str {
        "tick"
    }
}

/// The auto-configuration HOLDER (a `#[component]` so the container manages it; the
/// `#[bean]` method reads it as `&self`).
#[leaf_macros::component]
struct ClockAutoConfig;
impl ClockAutoConfig {
    fn new() -> Self {
        ClockAutoConfig
    }
}

/// THE GATED CONTRIBUTION: `#[auto_config] impl` whose `#[bean]` method contributes a
/// `dyn Clock` (named "clock") into `AUTO_CONFIGS` at FALLBACK, gated by
/// `OnProperty(gate.enabled) AND OnMissingBean(SystemClock)` (the soft-override).
#[auto_config]
impl ClockAutoConfig {
    #[bean(name = "clock", provides = "dyn Clock")]
    #[conditional(
        on_property("gate.enabled", having_value = "true"),
        on_missing_bean(SystemClock)
    )]
    fn clock(&self) -> SystemClock {
        SystemClock
    }
}

/// The contributed bean's stable contract (`module_path!()::clock`).
fn clock_contract() -> ContractId {
    ContractId::of(&format!("{}::clock", module_path!()))
}

/// The force-linked tokio runtime handle (the only non-annotation input).
fn spawner() -> Arc<dyn leaf_core::Spawner> {
    Arc::new(leaf_tokio::TokioExecutionFacility::new())
}

// ─────────────────────────────── the gating proofs ────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auto_config_backs_off_when_the_gate_property_is_unset() {
    // (a) gate.enabled UNSET → the force-linked auto-config must BACK OFF. Against the
    // pre-fix code this FAILS: from_slices registered the AUTO_CONFIGS row regardless of
    // the guard, so the bean was in the context even with the property unset.
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("clock-app")
        .with_spawner(spawner())
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app runs to Ready");

    let id = running.context().engine().registry().by_contract(clock_contract());
    assert!(
        id.is_none(),
        "gate.enabled unset → the guarded auto-config bean must NOT be registered (it backs off)"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auto_config_registers_when_the_gate_property_is_set() {
    // (b) gate.enabled=true + no user bean → the guard matches → the auto-config is
    // present and resolves as the concrete product through the slice path.
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("clock-app")
        .with_spawner(spawner())
        .run(
            SealInputs::new().with_args(["--gate.enabled=true"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    let id = running.context().engine().registry().by_contract(clock_contract());
    assert!(
        id.is_some(),
        "gate.enabled=true → the guarded auto-config bean IS registered (the guard matched)"
    );
    let clock = running.context().get::<SystemClock>().await.expect("the clock resolves");
    assert_eq!(clock.now(), "tick", "the real #[bean] factory body produced the bean");

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auto_config_backs_off_when_a_user_bean_of_the_type_is_present() {
    // (c) OnMissingBean: gate.enabled=true but a user bean of the contributed type
    // (SystemClock) already exists (seeded into the back-off probe via the
    // `.with_inventory` escape hatch — the `#[leaf::main]`-supplied inventory path) →
    // the OnMissingBean leaf sees it and the auto-config backs off (the user bean wins).
    leaf_tokio::install_ambient_store().ok();
    let running = Application::new()
        .with_name("clock-app")
        .with_spawner(spawner())
        .with_inventory(vec![(TypeId::of::<SystemClock>(), CandidateRole::NORMAL)])
        .run(
            SealInputs::new().with_args(["--gate.enabled=true"]),
            RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    let id = running.context().engine().registry().by_contract(clock_contract());
    assert!(
        id.is_none(),
        "a user SystemClock present → OnMissingBean backs off the auto-config (user bean wins)"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed);
}
