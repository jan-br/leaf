//! The macro→leaf_core PAIRING-CHANNEL roundtrip `[mac-pairing-slices]`.
//!
//! A SEPARATE crate that USES the thin macros on sample items, then asserts at
//! runtime that EACH per-bean wiring pairing the macro emits beside a bean's
//! `Descriptor` (the `__leaf_seed_`/`__leaf_guard_`/`__leaf_joinpoints_`/
//! `__leaf_methods_`/`__leaf_runner_upcast_`/`__leaf_config_bind_`/injection-plan
//! consts) is ALSO auto-collected into the matching `linkme` distributed-slice in
//! leaf-core — so a normal annotated app wires itself with NO hand-assembled
//! `.with_seeds`/`.with_guards`/… (those `.with_*` builders STAY as escape hatches
//! that ADD to the slice-collected set, but are not required).
//!
//! This is the COMPONENTS/AUTO_CONFIGS auto-collect substrate, extended to every
//! pairing channel. The element rows are const-compatible TWINS of the leaf-boot
//! `*Pairing` structs (each carries `ContractId` + the fn-ptr/&'static-ref/data),
//! defined in leaf-core so the macro can `#[distributed_slice(::leaf_core::<SLICE>)]`
//! into them cross-crate — exactly the COMPONENTS submission pattern.
//!
//! PROOF GATE (cross-crate, re-export): this crate has NO `linkme` dep — the rows
//! reach their frozen slices through leaf-core's `pub use linkme;` via
//! `#[::leaf_core::linkme::distributed_slice(...)]` + `#[linkme(crate =
//! ::leaf_core::linkme)]` (see `roundtrip.rs`).

#![allow(dead_code)]

use leaf_core::{
    collect_slice, AdvisorPairingRow, ConfigBindPairingRow, ContractId, GuardPairingRow,
    InjectionPlanPairingRow, JoinPointPairingRow, MethodTablePairingRow, RunnerPairingRow,
    SeedPairingRow, ADVISOR_PAIRINGS, CONFIG_BIND_PAIRINGS, GUARD_PAIRINGS, INJECTION_PLAN_PAIRINGS,
    JOINPOINT_PAIRINGS, METHOD_TABLE_PAIRINGS, RUNNER_PAIRINGS, SEED_PAIRINGS,
};
use leaf_macros::{advisable, aspect, component, config_properties, conditional, runner};

/// The module-qualified contract a macro mints for `ident` in THIS module.
fn contract_here(ident: &str) -> ContractId {
    ContractId::of(&format!("{}::{}", module_path!(), ident))
}

// ───────────────────────────── #[component] → SEED ──────────────────────────

/// A plain no-dependency component: its `__leaf_seed_Widget` ProviderSeed must be
/// auto-collected into `SEED_PAIRINGS` keyed by the bean's ContractId.
#[component]
struct Widget;

impl Widget {
    fn new() -> Self {
        Widget
    }
}

#[test]
fn component_seed_reaches_the_seed_pairings_slice() {
    let rows: Vec<SeedPairingRow> = collect_slice(&SEED_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Widget"))
        .expect("the #[component] seed must auto-collect into SEED_PAIRINGS");
    // The collected seed builds a real Provider (it IS `__leaf_seed_Widget`).
    let _provider = (mine.seed)();
}

#[test]
fn component_injection_plan_reaches_the_injection_plan_slice() {
    let rows: Vec<InjectionPlanPairingRow> = collect_slice(&INJECTION_PLAN_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Widget"))
        .expect("the #[component] InjectionPlan must auto-collect into INJECTION_PLAN_PAIRINGS");
    // A no-dependency component has an empty plan.
    assert!(mine.plan.points.is_empty(), "Widget injects nothing");
}

// ─────────────────────── #[conditional] → GUARD ─────────────────────────────

/// A conditionally-registered component: its `__leaf_guard_Gated` CondExpr must be
/// auto-collected into `GUARD_PAIRINGS`.
#[component]
#[conditional(on_property("feature.gated", having_value = "true"))]
struct Gated;

impl Gated {
    fn new() -> Self {
        Gated
    }
}

#[test]
fn conditional_guard_reaches_the_guard_pairings_slice() {
    let rows: Vec<GuardPairingRow> = collect_slice(&GUARD_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Gated"))
        .expect("the #[conditional] guard must auto-collect into GUARD_PAIRINGS");
    // The collected guard is a non-trivial CondExpr (not the vacuous All([])).
    assert!(
        !matches!(mine.guard, leaf_core::CondExpr::All(c) if c.is_empty()),
        "the gated guard must carry the on_property leaf"
    );
}

// ─────────────────── #[advisable] → JOINPOINTS + METHODS ─────────────────────

/// An advisable bean with one advised method. The canonical method-aware form —
/// `#[component]` on the struct (the `Descriptor`) + `#[advisable]` on the impl,
/// whose `&self` methods are advised join points + a `MethodEntry` (no duplicate
/// join-points const). The `__leaf_joinpoints_Service` spec and the
/// `__leaf_methods_Service` table must auto-collect into their slices by ContractId.
#[component]
struct Service;

#[advisable]
impl Service {
    fn new() -> Self {
        Service
    }
    fn handle(&self, n: u32) -> u32 {
        n + 1
    }
}

#[test]
fn advisable_joinpoints_reach_the_joinpoint_pairings_slice() {
    let rows: Vec<JoinPointPairingRow> = collect_slice(&JOINPOINT_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Service"))
        .expect("the #[advisable] join-point spec must auto-collect into JOINPOINT_PAIRINGS");
    assert_eq!(
        mine.spec.bean_type,
        std::any::TypeId::of::<Service>(),
        "the spec carries the bean's concrete TypeId"
    );
    // The impl form enumerates the advised method as a join point.
    assert!(
        !mine.spec.methods.is_empty(),
        "the #[advisable] impl form must enumerate Service::handle as a join point"
    );
}

#[test]
fn advisable_method_table_reaches_the_method_table_pairings_slice() {
    let rows: Vec<MethodTablePairingRow> = collect_slice(&METHOD_TABLE_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Service"))
        .expect("the #[advisable] impl method table must auto-collect into METHOD_TABLE_PAIRINGS");
    assert!(
        !mine.table.0.is_empty(),
        "Service::handle must produce a MethodEntry in the table"
    );
}

// ─────────────────────────── #[runner] → RUNNER ─────────────────────────────

/// A runner bean: its `__leaf_runner_upcast_Boot` thunk must auto-collect into
/// `RUNNER_PAIRINGS`.
#[runner]
struct Boot;

impl Boot {
    fn new() -> Self {
        Boot
    }
}

impl leaf_core::Runner for Boot {
    fn run<'a>(
        &'a self,
        _args: &'a leaf_core::ApplicationArguments,
    ) -> leaf_core::BoxFuture<'a, Result<(), leaf_core::LeafError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn runner_upcast_reaches_the_runner_pairings_slice() {
    let rows: Vec<RunnerPairingRow> = collect_slice(&RUNNER_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Boot"))
        .expect("the #[runner] upcast must auto-collect into RUNNER_PAIRINGS");
    // The upcast thunk is the macro-emitted `__leaf_runner_upcast_Boot`.
    let _ = mine.upcast;
}

// ─────────────────────────── #[aspect] → ADVISOR ────────────────────────────

/// An aspect bean — the advisor. `#[aspect]` on the struct emits its
/// `__LEAF_ADVISOR_PAIRING_Auditor` `AdvisorPairingRow` (the const pointcut + the
/// make_interceptor bean bridge) into `ADVISOR_PAIRINGS`, so the run pipeline
/// auto-collects the LIVE advisor with no hand-assembled `.with_advisors`.
#[aspect]
struct Auditor;

impl Auditor {
    fn new() -> Self {
        Auditor
    }
}

// The aspect IS the interceptor (the make_interceptor resolves + upcasts it).
#[leaf_macros::async_impl]
impl leaf_core::Interceptor for Auditor {
    async fn intercept(
        &self,
        call: &leaf_core::Call<'_>,
        mut next: leaf_core::Next<'_>,
    ) -> Result<leaf_core::ErasedRet, leaf_core::AdviceError> {
        next.proceed(call).await
    }
}

#[test]
fn aspect_advisor_pairing_reaches_the_advisor_pairings_slice() {
    let rows: Vec<AdvisorPairingRow> = collect_slice(&ADVISOR_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("Auditor"))
        .expect("the #[aspect] advisor pairing must auto-collect into ADVISOR_PAIRINGS");
    // The row carries the live advice shape (pointcut + make_interceptor) the hand
    // `.with_advisors` table used to supply — both const-emitted by the macro.
    let _ = mine.pointcut;
    let _ = mine.make_interceptor;
    assert_eq!(mine.role, leaf_core::Role::Application, "a user #[aspect] is Application-role");
}

// ─────────────────── #[config_properties] → CONFIG_BIND ──────────────────────

/// A config-properties bean: its `__leaf_config_bind_AppProps` thunk must
/// auto-collect into `CONFIG_BIND_PAIRINGS`.
#[config_properties(prefix = "app")]
#[derive(Default)]
struct AppProps {
    name: String,
}

#[test]
fn config_properties_bind_thunk_reaches_the_config_bind_pairings_slice() {
    let rows: Vec<ConfigBindPairingRow> = collect_slice(&CONFIG_BIND_PAIRINGS);
    let mine = rows
        .iter()
        .find(|r| r.contract == contract_here("AppProps"))
        .expect("the #[config_properties] bind thunk must auto-collect into CONFIG_BIND_PAIRINGS");
    let _ = mine.thunk;
}
