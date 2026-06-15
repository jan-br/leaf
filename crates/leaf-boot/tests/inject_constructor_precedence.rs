//! TASK 5 PROOF-GATE — the `#[inject]` CONSTRUCTOR wiring OVERRIDES the stereotype's
//! struct field-default by `ContractId`.
//!
//! A bean wears a struct stereotype (`#[repository]`, which emits the descriptor + a
//! struct FIELD-DEFAULT seed/plan) AND an `#[advisable] impl` with an `#[inject] fn
//! new()` constructor (which emits a CONSTRUCTOR seed/plan keyed by the SAME
//! `ContractId`). BOTH rows ride `SEED_PAIRINGS`/`INJECTION_PLAN_PAIRINGS`.
//!
//! Before this task the duplicate `SeedPairing` for one `ContractId` was a loud
//! `AntiDce` build-seam error ("more than one SeedPairing"). Task 5 makes the
//! CONSTRUCTOR row win the merge: the bean resolves through the `#[inject]` ctor and
//! the duplicate is suppressed (constructor over field-default, by `ContractId`).

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    collect_slice, ContractId, Ref, INJECTION_PLAN_PAIRINGS, SEED_PAIRINGS,
};
use leaf_macros::{advisable, repository};

// ───────────────────────── the beans (a ctor on each) ────────────────────────

/// A no-collaborator `#[repository]` whose construction is the `#[inject] fn new()`.
#[repository]
#[derive(Debug)]
struct Marker;

#[advisable]
impl Marker {
    #[inject]
    fn new() -> Self {
        Marker
    }
}

/// A `#[repository]` depending on [`Marker`] (an all-`Ref` field set, so the struct
/// field-default ALSO compiles) PLUS an `#[inject] fn new(dep)` constructor. Both the
/// field-default and the ctor wiring rows land for this one `ContractId` — the ctor
/// must win (else the duplicate-pairing build-seam error fires).
#[repository]
#[derive(Debug)]
struct Widget {
    dep: Ref<Marker>,
}

#[advisable]
impl Widget {
    #[inject]
    fn new(dep: Ref<Marker>) -> Self {
        Widget { dep }
    }
}

// ──────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_inject_constructor_wins_over_the_struct_field_default() {
    leaf_tokio::install_ambient_store().ok();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // The run does NOT trip the duplicate-pairing build-seam error: the merge selects
    // the constructor row over the field-default for the SAME ContractId.
    let running = Application::new()
        .with_name("ctor-precedence")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the bean with BOTH a field-default and an #[inject] ctor resolves (ctor wins)");

    // The graph wired through the constructor: Widget injected its Marker dependency.
    let widget = running.context().get::<Widget>().await.expect("Widget resolves via #[inject] ctor");
    // The injected dependency is live (the ctor threaded the resolved Ref<Marker>).
    let _: &Marker = &widget.dep;

    let _marker = running.context().get::<Marker>().await.expect("Marker resolves via #[inject] ctor");

    running.shutdown().await;
}

// ─────────────── the slice-level fact: BOTH rows exist for one contract ───────

#[test]
fn both_the_field_default_and_the_constructor_rows_are_linked_for_one_contract() {
    // The precedence is a RUNTIME merge (constructor wins), NOT a macro-time
    // suppression: the struct stereotype's field-default seed/plan AND the
    // `#[inject]` constructor's seed/plan BOTH ride the slices for the SAME
    // ContractId. Task 5 selects the constructor row at collection time.
    let module = module_path!();
    let widget = ContractId::of(&format!("{module}::Widget"));

    let seed_rows: Vec<_> = collect_slice(&SEED_PAIRINGS)
        .into_iter()
        .filter(|r| r.contract == widget)
        .collect();
    assert_eq!(
        seed_rows.len(),
        2,
        "both the field-default seed and the #[inject]-ctor seed ride SEED_PAIRINGS for one ContractId"
    );
    // EXACTLY one of the two is the constructor row (the precedence flag the merge reads).
    assert_eq!(
        seed_rows.iter().filter(|r| r.from_constructor).count(),
        1,
        "exactly one of the two seed rows is tagged from_constructor"
    );

    let plan_rows: Vec<_> = collect_slice(&INJECTION_PLAN_PAIRINGS)
        .into_iter()
        .filter(|r| r.contract == widget)
        .collect();
    assert_eq!(
        plan_rows.len(),
        2,
        "both the field-default plan and the #[inject]-ctor plan ride INJECTION_PLAN_PAIRINGS"
    );
    assert_eq!(
        plan_rows.iter().filter(|r| r.from_constructor).count(),
        1,
        "exactly one of the two plan rows is tagged from_constructor"
    );
}
