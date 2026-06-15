//! The macro→leaf_core ROUNDTRIP integration test `[mac-component]`.
//!
//! This is a SEPARATE crate that USES the thin `#[component]` macro on a sample
//! type, then asserts at runtime that the macro-emitted `::leaf_core::Descriptor`
//! reached the frozen `::leaf_core::COMPONENTS` `linkme` slice with the right
//! `contract` + `derive_default_name("greeter")`, and that its `ProviderSeed`
//! builds a `Provider` that the engine drives to produce the bean. This proves the
//! thin-macro pipeline end-to-end and CLOSES leaf-core's "Descriptor.self_type
//! emitted by a macro" boundary (the macro is the only authorised producer of a
//! `COMPONENTS` row).
//!
//! PROOF GATE (cross-crate, re-export): this test crate has NO `linkme` dependency
//! — only `leaf-core` + `leaf-macros` (check the `[dev-dependencies]`). That it
//! compiles and the roundtrip passes proves a `#[component]` user needs only
//! leaf-core/leaf-macros. The `#[component]` macro emits
//! `#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]` +
//! `#[linkme(crate = ::leaf_core::linkme)]`, reaching the slice through leaf-core's
//! `pub use linkme;`: the attribute macro DOES resolve by its fully-qualified
//! re-export path on stable, and the `crate =` override redirects linkme's runtime
//! types (`DistributedSlice`/`__private`/`Void`) there too. The override is the
//! load-bearing piece — without it the element expansion emits a bare `::linkme::…`
//! runtime path and fails with `E0433: cannot find linkme in the crate root` (the
//! exact failure a prior pass mistook for "the re-export does not resolve").

use leaf_macros::component;

/// The headline bean: a no-dependency `#[component]`. `Greeter` decapitalizes to
/// the derived default name `"greeter"`.
#[component]
struct Greeter;

impl Greeter {
    fn new() -> Self {
        Greeter
    }
    fn greet(&self) -> &'static str {
        "hello"
    }
}

/// A DEPENDENT bean: `Loud` injects a `Greeter` collaborator (the field is the
/// injection point; its type is resolved through the one `Engine::get` seam).
#[component]
struct Loud {
    greeter: leaf_core::Ref<Greeter>,
}

impl Loud {
    fn new(greeter: leaf_core::Ref<Greeter>) -> Self {
        Loud { greeter }
    }
    fn shout(&self) -> String {
        self.greeter.greet().to_uppercase()
    }
}

/// Find a macro-emitted descriptor in the frozen `COMPONENTS` slice by its derived
/// canonical name.
fn descriptor_named(name: &str) -> leaf_core::Descriptor {
    *leaf_core::COMPONENTS
        .iter()
        .find(|d| d.declared_name == Some(name))
        .unwrap_or_else(|| panic!("`{name}` must roundtrip through ::leaf_core::COMPONENTS"))
}

#[test]
fn component_descriptor_reaches_the_components_slice_with_contract_and_derived_name() {
    // The macro derived the name via Spring's decapitalize (`Greeter` -> `greeter`)
    // at expansion, so the const row carries a ready `&'static str`.
    let greeter = descriptor_named("greeter");
    assert_eq!(greeter.declared_name, Some("greeter"));

    // The row mints its stable cross-build identity from the author-stable identity
    // path the macro builds from `module_path!()` + the ident.
    let expected = leaf_core::ContractId::of(&format!("{}::Greeter", module_path!()));
    assert_eq!(
        greeter.contract, expected,
        "the macro-emitted contract_id must match contract_hash(module::Ident)"
    );

    // The role/scope axes default to Application/Singleton (a plain @component).
    assert_eq!(greeter.role, leaf_core::Role::Application);

    // The flattened meta carries the COMPONENT marker (every stereotype is in
    // COMPONENTS transitively because meta.markers contains COMPONENT).
    assert!(
        greeter
            .meta
            .markers
            .contains(&leaf_core::MarkerId::of("leaf::Component")),
        "the @component meta must carry the COMPONENT marker"
    );
}

#[test]
fn derived_name_matches_leaf_core_derive_default_name() {
    // The macro's derived name MUST equal leaf-core's canonical `derive_default_name`
    // (Spring's decapitalize) — closing the macro↔core naming-rule boundary.
    let greeter = descriptor_named("greeter");
    assert_eq!(
        greeter.declared_name.map(str::to_string),
        Some(leaf_core::derive_default_name("Greeter").into_owned())
    );
}

#[test]
fn provider_seed_builds_a_provider_that_produces_the_bean() {
    // Pair the macro-emitted descriptor (from COMPONENTS) with a provider built by
    // its ProviderSeed, freeze a one-bean registry, and drive the engine to produce
    // the bean — the full macro→leaf_core construction roundtrip.
    let greeter = descriptor_named("greeter");
    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(greeter, leaf_macros_test_seeds::greeter_seed()())
        .expect("registering the macro-emitted descriptor");
    let registry = builder.freeze().expect("a one-bean registry freezes");
    let engine = leaf_core::Engine::new(registry);

    let produced = futures::executor::block_on(engine.get::<Greeter>())
        .expect("the ProviderSeed-built Provider produces the Greeter bean");
    assert_eq!(produced.greet(), "hello");
}

#[test]
fn a_dependent_bean_resolves_its_collaborator_through_the_engine() {
    // The dependent `Loud` injects `Greeter`. Registering BOTH macro-emitted rows
    // and driving the engine proves the generated Provider resolves the collaborator
    // through the one Engine::get seam (the ResolveCtx engine back-ref).
    let greeter = descriptor_named("greeter");
    let loud = descriptor_named("loud");
    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(greeter, leaf_macros_test_seeds::greeter_seed()())
        .expect("greeter registers");
    builder
        .register(loud, leaf_macros_test_seeds::loud_seed()())
        .expect("loud registers");
    let registry = builder.freeze().expect("the two-bean registry freezes");
    let engine = leaf_core::Engine::new(registry);

    let produced = futures::executor::block_on(engine.get::<Loud>())
        .expect("Loud resolves, injecting Greeter through the engine");
    assert_eq!(produced.shout(), "HELLO");
}

/// The macro must expose each bean's `ProviderSeed` under a deterministic public
/// path so a hand-written assembly (here the test, standing in for leaf-boot's
/// pairing pass) can pair the `COMPONENTS` descriptor with its construction recipe.
mod leaf_macros_test_seeds {
    pub fn greeter_seed() -> leaf_core::ProviderSeed {
        crate::__leaf_seed_Greeter
    }
    pub fn loud_seed() -> leaf_core::ProviderSeed {
        crate::__leaf_seed_Loud
    }
}
