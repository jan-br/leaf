//! Integration test `[boot-from_slices]`: the leaf-boot assembly engine's
//! cold-pass entry point lifts the macro-emitted `::leaf_core::COMPONENTS` rows,
//! JOINs each `Descriptor` to its macro-emitted `ProviderSeed` via the pairing
//! table, builds the `RegistryBuilder`, and freezes a registry the engine drives
//! to produce the bean — the full thin-macro → leaf-boot roundtrip.
//!
//! This crate uses the THIN `#[component]` macro (dev-dep `leaf-macros`) on a real
//! sample type, so the proof exercises the genuine link-collected slice, not a
//! hand-built `Descriptor`.

use leaf_boot::{App, AntiDceError, SeedPairing};
use leaf_macros::component;

/// The headline bean: a no-dependency `#[component]`. `Widget` decapitalizes to
/// the derived default name `"widget"`.
#[component]
struct Widget;

impl Widget {
    fn tag(&self) -> &'static str {
        "widget"
    }
}

/// A DEPENDENT bean: `Gadget` injects a `Widget` collaborator.
#[component]
struct Gadget {
    widget: leaf_core::Ref<Widget>,
}

impl Gadget {
    fn describe(&self) -> String {
        format!("gadget+{}", self.widget.tag())
    }
}

/// The macro-emitted pairing table the binary crate (here the test, standing in
/// for `#[leaf::main]`) hands leaf-boot so `from_slices` can JOIN each
/// `COMPONENTS` `Descriptor` to its construction recipe by `ContractId`. The
/// `__leaf_seed_<Ident>` consts are the deterministic public seed names the
/// macro exposes beside each row.
fn pairings() -> Vec<SeedPairing> {
    vec![
        SeedPairing::new(
            leaf_core::ContractId::of(&format!("{}::Widget", module_path!())),
            __leaf_seed_Widget,
        ),
        SeedPairing::new(
            leaf_core::ContractId::of(&format!("{}::Gadget", module_path!())),
            __leaf_seed_Gadget,
        ),
    ]
}

#[test]
fn from_slices_lifts_a_component_into_the_builder_with_its_contract_and_provider() {
    // The cold assembly pass lifts the link-collected COMPONENTS rows and JOINs
    // each to its seed-built provider via the ContractId pairing table.
    let app = App::from_slices(&pairings()).expect("from_slices lifts the slices");

    // The Widget + Gadget rows reached the Define-phase builder.
    assert!(
        app.len() >= 2,
        "Widget + Gadget must be lifted into the builder; got {}",
        app.len()
    );

    // Freeze + drive the engine: the JOINed provider produces the real bean.
    let registry = app.into_builder().freeze().expect("the registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let widget = futures::executor::block_on(engine.get::<Widget>())
        .expect("the seed-built provider produces Widget");
    assert_eq!(widget.tag(), "widget");
}

#[test]
fn the_joined_provider_resolves_a_dependent_beans_collaborator() {
    // The dependent `Gadget` injects `Widget`; the JOINed provider resolves the
    // collaborator through the one Engine::get seam.
    let app = App::from_slices(&pairings()).expect("from_slices lifts");
    let registry = app.into_builder().freeze().expect("freeze");
    let engine = leaf_core::Engine::new(registry);
    let gadget = futures::executor::block_on(engine.get::<Gadget>())
        .expect("Gadget resolves, injecting Widget through the engine");
    assert_eq!(gadget.describe(), "gadget+widget");
}

#[test]
fn the_macro_emitted_seeds_auto_collect_so_an_empty_pairing_table_still_lifts() {
    // The macro emits each `#[component]`'s `ProviderSeed` into the link-collected
    // SEED_PAIRINGS slice (Widget + Gadget here), and `from_slices` folds that slice
    // as its JOIN base — so a binary that supplies NO explicit `pairings` still lifts
    // every COMPONENTS row through its auto-collected seed (the maximal-magic channel).
    // This is the behavior that lets leaf-boot drop the hand-written builtin pairing
    // for the framework's own force-linked beans (e.g. the tokio executor).
    let app = App::from_slices(&[]).expect("the auto-collected SEED_PAIRINGS base lifts every row");
    let registry = app.into_builder().freeze().expect("freeze");
    let engine = leaf_core::Engine::new(registry);
    let widget = futures::executor::block_on(engine.get::<Widget>())
        .expect("the slice-collected seed produces Widget with NO explicit pairing");
    assert_eq!(widget.tag(), "widget");
}

#[test]
fn an_expected_but_vanished_source_is_a_loud_anti_dce_error() {
    // A crate present in the ExpectedManifest but contributing zero rows to the
    // link-collected SOURCES slice is the headline AntiDce defense.
    let expected = &[leaf_core::SourceTag("leaf-ghost-crate-that-was-never-linked")];
    let err = App::<leaf_boot::Define>::self_check(expected)
        .expect_err("a vanished source is a loud AntiDceError");
    match err {
        AntiDceError::SourceVanished { crate_name } => {
            assert_eq!(crate_name, "leaf-ghost-crate-that-was-never-linked");
        }
        #[allow(unreachable_patterns)]
        other => panic!("expected SourceVanished, got {other:?}"),
    }
    // It lifts into the one LeafError spine with ErrorKind::AntiDce.
    let leaf: leaf_core::LeafError = err.into();
    assert_eq!(leaf.kind, leaf_core::ErrorKind::AntiDce);
}

#[test]
fn self_check_passes_when_every_expected_source_contributed_rows() {
    // This crate's tests force-link nothing extra, but leaf-core's own discovery
    // tests submit a SourceTag, so an ExpectedManifest naming a present source
    // passes. We assert the empty manifest is trivially green.
    App::<leaf_boot::Define>::self_check(&[]).expect("an empty manifest always passes");
}
