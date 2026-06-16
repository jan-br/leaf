//! Integration tests for the remaining stereotype + bean macros: each is a real
//! use of the macro on a sample type, asserting the macro-emitted
//! `::leaf_core::Descriptor` reached `COMPONENTS` with the right transitive
//! `meta.markers` closure, and (for `#[bean]`/`register_component!`) that the
//! `ProviderSeed` builds a `Provider` the engine drives to the bean.
//!
//! PROOF GATE (cross-crate, re-export): see `roundtrip.rs` — this crate has NO
//! `linkme` dep; the rows reach `COMPONENTS` through leaf-core's `pub use linkme;`
//! via `#[::leaf_core::linkme::distributed_slice(...)]` + `#[linkme(crate = ...)]`.

use leaf_macros::{bean, configuration, controller, register_component, repository, service};

// ── @service / @repository / @controller / @configuration stereotypes ──

#[service]
struct UserService;

#[repository]
struct UserRepo;

#[controller]
struct UserController;

#[configuration]
struct AppConfig;

fn descriptor_named(name: &str) -> leaf_core::Descriptor {
    *leaf_core::COMPONENTS
        .iter()
        .find(|d| d.declared_name == Some(name))
        .unwrap_or_else(|| panic!("`{name}` must roundtrip through COMPONENTS"))
}

fn has_marker(d: &leaf_core::Descriptor, path: &str) -> bool {
    d.meta.markers.contains(&leaf_core::MarkerId::of(path))
}

#[test]
fn each_stereotype_emits_its_marker_and_transitively_component() {
    // Every stereotype is a @component (one-hop meta-edge), so each row's flattened
    // meta.markers carries BOTH its own marker AND COMPONENT — the default scan
    // include filter matches every stereotype transitively.
    for (name, marker) in [
        ("userService", "leaf::Service"),
        ("userRepo", "leaf::Repository"),
        ("userController", "leaf::Controller"),
        ("appConfig", "leaf::Configuration"),
    ] {
        let d = descriptor_named(name);
        assert!(has_marker(&d, marker), "{name} must carry {marker}");
        assert!(
            has_marker(&d, "leaf::Component"),
            "{name} must transitively carry COMPONENT"
        );
        // The stereotype axis does not change Role (orthogonal); all are Application.
        assert_eq!(d.role, leaf_core::Role::Application);
    }
}

// ── @bean factory function ──

struct Clock {
    label: &'static str,
}

// The `#[bean]` macro emits `impl ::leaf_core::Bean for Clock {}` itself, so the
// product type is engine-resolvable without a hand-written marker impl.
#[bean]
fn system_clock() -> Clock {
    Clock { label: "system" }
}

#[test]
fn a_bean_factory_fn_reaches_components_and_builds_its_product() {
    // The @bean fn registers its RETURN type as a bean named off the fn ident, and
    // the seed-built provider invokes the fn to produce the product.
    let d = descriptor_named("system_clock");
    assert!(has_marker(&d, "leaf::Component"));

    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(d, __leaf_seed_system_clock())
        .expect("register the @bean row");
    let engine = leaf_core::Engine::new(builder.freeze().expect("freezes"));
    let clock =
        futures::executor::block_on(engine.get::<Clock>()).expect("the @bean fn produces a Clock");
    assert_eq!(clock.label, "system");
}

// ── `#[repository(constructor = <path>)]` — the state-holding referenced-ctor path ──
//
// A REAL macro expansion (not a token-string assertion): the `constructor = …` arg
// makes the stereotype emit a `::leaf_core::construct_with(OrderRepo::new, __cx)`
// provider that builds the bean THROUGH the referenced constructor. This proves the
// generated `construct_with` call typechecks against the per-arity `InjectableCtor`
// impls AND that a non-`Injectable` state field (`AtomicI64`) compiles — impossible
// on the field-injection default. The field is NEVER lowered as an injection point.

use std::sync::atomic::{AtomicI64, Ordering};

#[repository(constructor = OrderRepo::new)]
struct OrderRepo {
    next_id: AtomicI64,
}
impl OrderRepo {
    fn new() -> Self {
        OrderRepo { next_id: AtomicI64::new(1) }
    }
    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

#[test]
fn a_referenced_constructor_repository_resolves_and_carries_its_state() {
    // The state-holding repository wears `#[repository(constructor = OrderRepo::new)]`
    // (the `register_component!` replacement for a stateful bean). Its seed-built
    // provider awaits `construct_with(OrderRepo::new, __cx)` — a zero-injected-param
    // constructor that SEEDS state — and the resolved bean's state survives + behaves
    // (two `next_id()` calls return increasing ids).
    let d = descriptor_named("orderRepo");
    assert!(has_marker(&d, "leaf::Repository"));
    assert!(has_marker(&d, "leaf::Component"));

    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(d, __leaf_seed_OrderRepo())
        .expect("register the referenced-constructor row");
    let engine = leaf_core::Engine::new(builder.freeze().expect("freezes"));
    let repo = futures::executor::block_on(engine.get::<OrderRepo>())
        .expect("the referenced-constructor repository resolves");
    // The constructor seeded `next_id` at 1; the state survived construction through
    // the stereotype and increments across calls.
    assert_eq!(repo.next_id(), 1);
    assert_eq!(repo.next_id(), 2);

    // The runtime dependency plan (read at assembly) is empty — a zero-injected-param
    // constructor has no construction edges.
    assert!(__leaf_ctor_deps_OrderRepo().is_empty());
}

// ── register_component!(Concrete) — the generic escape hatch ──

struct Wrapper<T> {
    #[allow(dead_code)]
    inner: T,
}
impl<T> Wrapper<T> {
    fn new() -> Self
    where
        T: Default,
    {
        Wrapper { inner: T::default() }
    }
}

// `register_component!` emits `impl ::leaf_core::Bean for Wrapper<u32> {}` itself.
register_component!(Wrapper<u32>);

#[test]
fn register_component_registers_a_concrete_instantiation() {
    // The escape hatch: a concrete monomorphization is a normal COMPONENTS row,
    // named off the leading ident (`Wrapper` -> `wrapper`), built via `<Ty>::new()`.
    let d = descriptor_named("wrapper");
    assert!(has_marker(&d, "leaf::Component"));

    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(d, __leaf_seed_Wrapper())
        .expect("register the concrete row");
    let engine = leaf_core::Engine::new(builder.freeze().expect("freezes"));
    let w = futures::executor::block_on(engine.get::<Wrapper<u32>>())
        .expect("the concrete instantiation resolves");
    assert_eq!(w.inner, 0);
}
