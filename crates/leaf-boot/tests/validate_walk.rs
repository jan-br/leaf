//! Integration test `[boot-validate-walk]`: the `App<Define> → Resolve → Wired`
//! walk over GENUINE link-collected `#[component]` rows, driving the unit-3
//! `seal()` freeze + the `App<Wired>::validate()` whole-graph pass + the
//! `WiringPlan` wave order.
//!
//! Proves end-to-end over a REAL `Descriptor`/`ProviderSeed` JOIN (not a hand-built
//! row) that:
//!
//! - `seal()` freezes the lifted builder into the immutable dense-`BeanId`
//!   registry, transitioning `App<Resolve> → App<Wired>`;
//! - `validate()` over a clean graph (`Gadget` needs `Widget`, both present) is OK;
//! - a MISSING mandatory dependency aggregates a Tier-2 `NoSuchBean`;
//! - `order_batch` puts a dependency in a strictly-earlier wave;
//! - a bad `@ConfigurationProperties` bind is a Tier-2 `BindError` that PRE-BINDS
//!   nothing, and a good bind pre-binds the slot `OnceCell`.

use std::any::TypeId;

use leaf_boot::{
    order_batch, App, ConfigBean, Define, SealInputs, SeedPairing, ValidationInputs, ValueDryRun,
};
use leaf_core::{
    BeanId, BeanKey, Cause, ContractId, Env, ErrorKind, InjectionPlan, InjectionPoint, Published,
    Ref, StartupValidation,
};
use leaf_macros::component;

/// A no-dependency `#[component]` (the collaborator).
#[component]
struct Widget;

impl Widget {
    fn new() -> Self {
        Widget
    }
}

/// A DEPENDENT `#[component]`: `Gadget` injects a `Widget`.
#[component]
struct Gadget {
    #[allow(dead_code)]
    widget: Ref<Widget>,
}

impl Gadget {
    fn new(widget: Ref<Widget>) -> Self {
        Gadget { widget }
    }
}

fn pairings() -> Vec<SeedPairing> {
    vec![
        SeedPairing::new(
            ContractId::of(&format!("{}::Widget", module_path!())),
            __leaf_seed_Widget,
        ),
        SeedPairing::new(
            ContractId::of(&format!("{}::Gadget", module_path!())),
            __leaf_seed_Gadget,
        ),
    ]
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// Drive the full `Define → Resolve → Wired` walk over the real rows.
fn wired() -> App<leaf_boot::Wired> {
    let app = App::<Define>::from_slices(&pairings()).expect("lift the real rows");
    let resolve =
        block_on(app.seal_environment(SealInputs::new(), Vec::new())).expect("seal_environment");
    resolve.seal().expect("seal freezes the registry")
}

/// The id of a bean by its module-qualified contract path.
fn id_of(app: &App<leaf_boot::Wired>, simple: &str) -> BeanId {
    app.registry()
        .by_contract(ContractId::of(&format!("{}::{simple}", module_path!())))
        .unwrap_or_else(|| panic!("{simple} registered"))
}

#[test]
fn seal_freezes_the_resolve_builder_into_a_wired_registry() {
    let app = wired();
    // The frozen registry holds the real Widget + Gadget rows (plus any framework
    // beans leaf-boot force-links under the default tokio feature).
    assert!(app.len() >= 2, "Widget + Gadget frozen; got {}", app.len());
    assert!(app
        .registry()
        .contains(&BeanKey::ByContract(ContractId::of(&format!(
            "{}::Widget",
            module_path!()
        )))));
    // Debug names the Wired phase.
    let s = format!("{app:?}");
    assert!(s.contains("Wired"), "got: {s}");
}

#[test]
fn validate_a_clean_graph_is_ok_and_gadget_depends_on_widget() {
    let app = wired();
    let widget = id_of(&app, "Widget");
    let gadget = id_of(&app, "Gadget");

    // Gadget's plan: a single mandatory Widget point (what the macro emits).
    let plan_of = move |id: BeanId| -> InjectionPlan {
        if id == gadget {
            InjectionPlan {
                points: Box::leak(Box::new([InjectionPoint::single(
                    TypeId::of::<Widget>(),
                    "widget",
                )])),
            }
        } else {
            InjectionPlan::EMPTY
        }
    };

    let eager = [widget, gadget];
    let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
    let report = app.validate_report(&inputs);
    assert!(
        report.is_ok(),
        "clean graph validates: {:?}",
        report.faults().iter().map(|f| f.error().kind).collect::<Vec<_>>()
    );

    // order_batch: Widget is in a strictly-earlier wave than Gadget.
    let plan = order_batch(app.registry(), &eager, &plan_of).expect("ordered");
    assert!(
        plan.wave_of(widget).unwrap() < plan.wave_of(gadget).unwrap(),
        "Widget builds before Gadget"
    );
}

#[test]
fn a_missing_mandatory_dependency_aggregates_a_tier2_no_such_bean() {
    let app = wired();
    let gadget = id_of(&app, "Gadget");

    // A bean of a type that is NOT registered.
    #[derive(Debug)]
    struct Absent;

    let plan_of = move |id: BeanId| -> InjectionPlan {
        if id == gadget {
            InjectionPlan {
                points: Box::leak(Box::new([InjectionPoint::single(
                    TypeId::of::<Absent>(),
                    "absent",
                )])),
            }
        } else {
            InjectionPlan::EMPTY
        }
    };
    let eager = [gadget];
    let inputs = ValidationInputs::new().with_eager(&eager).with_plans(&plan_of);
    let report = app.validate_report(&inputs);
    assert!(
        report.faults().iter().any(|f| f.error().kind == ErrorKind::NoSuchBean),
        "the missing dependency is a Tier-2 NoSuchBean"
    );
    // validate() (the strict collapse) surfaces it as an Err too.
    assert!(app.validate(&inputs).is_err());
}

#[test]
fn a_bad_config_bind_is_tier2_and_a_good_bind_prebinds_the_slot() {
    let app = wired();
    let widget = id_of(&app, "Widget");

    // A bad bind for the Widget slot → Tier-2 BindError; the slot stays unbound.
    let bad = |_env: &Env, _l: StartupValidation| -> leaf_boot::ConfigBindResult {
        Err(vec![leaf_core::LeafError::new(ErrorKind::BindError).caused_by(
            Cause::plain("binding @ConfigurationProperties", "app.size=0 violates range(min=1)"),
        )])
    };
    let cfg = [ConfigBean::new(widget, &bad)];
    let inputs = ValidationInputs::new().with_config_beans(&cfg);
    let report = app.validate_report(&inputs);
    assert!(report.faults().iter().any(|f| f.error().kind == ErrorKind::BindError));
    assert!(app.registry().singleton_cell(widget).get().is_none(), "a failed bind pre-binds nothing");

    // A good bind pre-binds the slot OnceCell (eager-EXCLUDED-because-PREBOUND).
    let app2 = wired();
    let widget2 = id_of(&app2, "Widget");
    let good = |_env: &Env, _l: StartupValidation| -> leaf_boot::ConfigBindResult {
        Ok(Published::shared_value(Widget))
    };
    let cfg2 = [ConfigBean::new(widget2, &good)];
    let inputs2 = ValidationInputs::new().with_config_beans(&cfg2);
    assert!(app2.validate(&inputs2).is_ok());
    assert!(app2.registry().singleton_cell(widget2).get().is_some(), "the config bean is pre-bound");
}

#[test]
fn a_value_dry_run_fault_aggregates_at_tier2_under_the_default_strict_lever() {
    // The default BootstrapSettings lever is Strict, so a @Value coercion dry-run
    // fault surfaces in the ONE Tier-2 report (Skip/Lenient lever behavior is
    // exercised in the lib unit tests, which can vary the lever directly).
    let app = wired();
    let widget = id_of(&app, "Widget");
    assert_eq!(app.settings().startup_validation, StartupValidation::Strict);

    let run = |_env: &Env, _l: StartupValidation| -> Vec<leaf_core::LeafError> {
        vec![leaf_core::LeafError::new(ErrorKind::ConvertError)
            .caused_by(Cause::plain("@Value dry-run", "order.max-retries=abc is not a u16"))]
    };
    let drys = [ValueDryRun::new(widget, &run)];
    let inputs = ValidationInputs::new().with_value_dry_runs(&drys);

    let report = app.validate_report(&inputs);
    assert!(report.faults().iter().any(|f| f.error().kind == ErrorKind::ConvertError));

    let _ = Widget::new();
    let _ = Gadget::new(Ref::new(Widget));
}
