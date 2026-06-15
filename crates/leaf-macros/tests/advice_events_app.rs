//! The macro→leaf_core ROUNDTRIP integration tests for unit 6/6's advice / event /
//! scheduling / app surfaces `[mac-advice-events-main]`.
//!
//! A SEPARATE crate that USES the thin macros on sample items, then asserts at
//! runtime that each macro-emitted const row reached its frozen `linkme` slice with
//! the right `ContractId`/order/metadata — the proof the thin-macro pipeline closes
//! end-to-end (the macro is the only authorised producer of these rows).
//!
//! PROOF GATE (cross-crate, re-export): this crate has NO `linkme` dep — the
//! emitted rows reach their frozen slices through leaf-core's `pub use linkme;` via
//! `#[::leaf_core::linkme::distributed_slice(...)]` + `#[linkme(crate =
//! ::leaf_core::linkme)]` (see `roundtrip.rs`). The richer runtime descriptors
//! (`ListenerDescriptor.adapter`/`event_type`, the live `AdvisorDescriptor`) bind to
//! resolved host beans at refresh — leaf-boot's concern; this unit emits the
//! anti-DCE identity rows + the const pairing metadata.

// The annotated listener/scheduled fns + the catalog struct are discovered purely
// by their link-collected identity rows (their bodies are bound by leaf-boot at
// refresh), so they are legitimately uncalled in this in-process roundtrip test.
#![allow(dead_code)]

use leaf_core::{
    ADVISORS, CATALOGS, COMPONENTS, EVENT_LISTENERS, FAILURE_ANALYZERS, RESOURCES, SCHEDULED,
};
use leaf_macros::{
    advisable, aspect, catalog, component, event_listener, failure_analyzer, holder, runner,
    scheduled, transactional_event_listener,
};

/// The module-qualified contract a macro mints for `ident` in THIS module (the
/// `module_path!()::ident` identity the const initializer builds at the use site).
fn contract_here(ident: &str) -> leaf_core::ContractId {
    leaf_core::ContractId::of(&format!("{}::{}", module_path!(), ident))
}

// ───────────────────────────── #[event_listener] ────────────────────────────

/// A plain inline event listener (a free fn).
#[event_listener]
fn on_user_created() {}

/// A transactional listener deferring to the after-commit phase, with a condition.
#[transactional_event_listener(phase = "after_commit", condition = "event.active")]
fn on_order_placed() {}

#[test]
fn event_listener_reaches_the_event_listeners_slice() {
    // The #[event_listener] emitted an EventListenerRow into the frozen slice.
    let found = EVENT_LISTENERS
        .iter()
        .any(|r| r.contract == contract_here("on_user_created"));
    assert!(found, "#[event_listener] must emit an EventListenerRow");
}

#[test]
fn the_inline_listener_defers_to_none() {
    // A plain #[event_listener] fires inline: its phase pairing const is None.
    assert_eq!(__leaf_listener_phase_on_user_created, None);
}

#[test]
fn the_transactional_listener_carries_its_tx_phase_and_condition() {
    // #[transactional_event_listener(phase = "after_commit")] defers to AfterCommit
    // and records the condition-presence slot.
    assert_eq!(
        __leaf_listener_phase_on_order_placed,
        Some(leaf_core::TxPhase::AfterCommit)
    );
    // Compare the const flag to a runtime `true` so the assertion is a real check
    // (asserting the const directly is a const-folded no-op clippy flags).
    assert_eq!(__leaf_listener_has_condition_on_order_placed, std::hint::black_box(true));
    // Its identity row is in the slice too.
    assert!(EVENT_LISTENERS
        .iter()
        .any(|r| r.contract == contract_here("on_order_placed")));
}

// ─────────────────────────────── #[scheduled] ───────────────────────────────

/// A cron-scheduled cleanup task (a free fn).
#[scheduled(cron = "0 0 * * * *")]
fn cleanup() {}

/// A fixed-rate poller.
#[scheduled(fixed_rate = 5000, initial_delay = 100)]
fn poll() {}

#[test]
fn scheduled_reaches_the_scheduled_slice_with_a_to_row_identity() {
    // #[scheduled] emits a ScheduledMethodDescriptor + its .to_row() ScheduledRow.
    // The row's contract is the BEAN identity (module::cleanup).
    let found = SCHEDULED
        .iter()
        .any(|r| r.contract == contract_here("cleanup"));
    assert!(found, "#[scheduled] must emit a ScheduledRow");
}

#[test]
fn the_scheduled_descriptor_carries_the_parsed_trigger_spec() {
    // The public pairing const carries the parsed const TriggerSpec.
    let desc = __leaf_scheduled_cleanup_invoke;
    assert_eq!(desc.spec, leaf_core::TriggerSpec::Cron("0 0 * * * *"));
    let poll = __leaf_scheduled_poll_invoke;
    match poll.spec {
        leaf_core::TriggerSpec::FixedRate { period, initial_delay } => {
            assert_eq!(period, std::time::Duration::from_millis(5000));
            assert_eq!(initial_delay, std::time::Duration::from_millis(100));
        }
        other => panic!("expected a FixedRate spec, got {other:?}"),
    }
}

// ─────────────────────────────── #[aspect] ──────────────────────────────────

/// An aspect bean: a #[component] that ALSO emits an ADVISORS identity row.
#[aspect(order = 50)]
struct AuditAspect;

impl AuditAspect {
    fn new() -> Self {
        AuditAspect
    }
}

// An `#[aspect]` struct IS the interceptor (the auto-collected ADVISOR_PAIRINGS
// `make_interceptor` resolves it + upcasts to `Arc<dyn Interceptor>`).
impl leaf_core::Interceptor for AuditAspect {
    fn intercept<'a>(
        &'a self,
        call: &'a leaf_core::Call<'a>,
        mut next: leaf_core::Next<'a>,
    ) -> leaf_core::BoxFuture<'a, Result<leaf_core::ErasedRet, leaf_core::AdviceError>> {
        Box::pin(async move { next.proceed(call).await })
    }
}

#[test]
fn aspect_reaches_the_advisors_slice_and_components_slice() {
    // The aspect emitted an AdvisorRow into ADVISORS (the advice side)...
    let advisor = ADVISORS.iter().any(|r| r.contract == contract_here("AuditAspect"));
    assert!(advisor, "#[aspect] must emit an AdvisorRow");
    // ...and a Descriptor into COMPONENTS (the aspect IS a registered bean).
    let component = leaf_core::COMPONENTS
        .iter()
        .any(|d| d.declared_name == Some("auditAspect"));
    assert!(component, "#[aspect] must also register the aspect bean");
}

#[test]
fn the_aspect_chain_order_pairing_const_carries_the_explicit_order() {
    // The explicit `order = 50` rides the public chain-order pairing const
    // (Annotation-sourced, so it beats an Implicit floor at equal value).
    assert_eq!(__leaf_advisor_AuditAspect.value, 50);
    assert_eq!(__leaf_advisor_AuditAspect.source, leaf_core::OrderSource::Annotation);
}

// ─────────────────────────────── #[advisable] ───────────────────────────────

/// An advisable bean (a #[component] PROXY TARGET) carrying a marker the proxy plan's
/// `annotated::<A>()` pointcut can match.
#[advisable]
struct OrderService;

impl OrderService {
    fn new() -> Self {
        OrderService
    }
}

#[test]
fn advisable_emits_a_per_bean_join_points_spec_pairing_const() {
    // The headline proxy-join-point closure: an #[advisable] bean emits a PUBLIC
    // ::leaf_core::BeanJoinPointsSpec pairing const (the const twin of BeanJoinPoints)
    // carrying its bean_type + a reference to its OWN flat AnnotationMetadata — the
    // per-bean data leaf-boot's ProxyPlan::freeze runs pointcuts over. The bean is
    // ALSO a registered COMPONENTS bean (the proxy target is a normal bean).
    assert!(
        COMPONENTS.iter().any(|d| d.declared_name == Some("orderService")),
        "#[advisable] must register the proxy-target bean"
    );
    let spec = __leaf_joinpoints_OrderService;
    assert_eq!(
        spec.bean_type,
        std::any::TypeId::of::<OrderService>(),
        "the join-point spec carries the bean's concrete TypeId (the within::<T>() key)"
    );
    // The markers reference is the bean's OWN flat AnnotationMetadata (it carries the
    // @component marker closure — annotated::<A>() reads it).
    let component_marker = leaf_core::MarkerId::of("leaf::Component");
    assert!(
        spec.markers.markers.contains(&component_marker),
        "the join-point spec markers carry the bean's @component marker closure"
    );
    // It reifies into the runtime BeanJoinPoints ProxyPlan::freeze consumes (a struct
    // attr cannot enumerate methods, so the method spec is empty here — the binary /
    // impl-aware form supplies the per-method join points).
    assert!(spec.methods.is_empty(), "a bare #[advisable] struct has no enumerated methods");
    let reified = spec.reify_methods();
    assert!(reified.is_empty());
}

// ─────────────────────────────── #[catalog] ─────────────────────────────────

/// An i18n message catalog.
#[catalog(basename = "messages", locales = ["en", "de"])]
struct AppMessages;

#[test]
fn catalog_reaches_the_catalogs_slice_with_its_descriptor() {
    let found = CATALOGS.iter().any(|r| r.contract == contract_here("AppMessages"));
    assert!(found, "#[catalog] must emit a CatalogRow");
    // The richer descriptor pairing const carries the basename + locales.
    assert_eq!(__leaf_catalog_AppMessages.basename, "messages");
    assert_eq!(__leaf_catalog_AppMessages.locales, &["en", "de"]);
}

// ─────────────────────────────── #[holder] ──────────────────────────────────

/// An ambient context key declared via `#[holder]` — the same shape leaf-i18n's
/// `LocaleKey`/`LOCALE` migrate onto. `inherit` policy + a derived `LOCALE` accessor
/// (`LocaleKey` -> SCREAMING_SNAKE-with-`Key`-stripped).
#[holder(name = "locale", policy = inherit, value = leaf_core::Locale)]
pub struct LocaleKey;

#[test]
fn holder_emits_the_cxkey_impl_and_accessor() {
    use leaf_core::{CxKey, Propagation};
    // The trait-const path resolves to the args (NO linkme row — a CxKey is plain data).
    assert_eq!(<LocaleKey as CxKey>::NAME, "locale");
    assert_eq!(<LocaleKey as CxKey>::POLICY, Propagation::Inherit);
    // The derived accessor static exposes the same facts.
    assert_eq!(LOCALE.name(), "locale");
    assert_eq!(LOCALE.policy(), Propagation::Inherit);
}

#[test]
fn holder_accessor_round_trips_through_scope_and_get() {
    use futures::executor::block_on;
    use leaf_core::Locale;
    // No ambient binding => None.
    assert!(LOCALE.get().is_none());
    // Inside a holder scope => the bound value.
    let seen = block_on(LOCALE.scope(Locale::new("fr-FR"), async { LOCALE.get() }));
    assert_eq!(seen.map(|l| l.tag().to_string()), Some("fr-FR".to_string()));
    // Restored after the scope.
    assert!(LOCALE.get().is_none());
}

// ─────────────────────────────── #[resource] ────────────────────────────────

/// A compiled-in classpath resource — its bytes are this very source file. The
/// `include_bytes!` path resolves RELATIVE to this source file's own directory, so
/// the bare filename references this file.
#[leaf_macros::resource("advice_events_app.rs")]
const SELF_SOURCE: &[u8];

#[test]
fn resource_reaches_the_resources_slice_and_binds_the_bytes() {
    let found = RESOURCES.iter().any(|r| r.location == "advice_events_app.rs");
    assert!(found, "#[resource] must emit a ResourceRow at its location");
    // The user const is bound to the compiled-in bytes.
    assert!(!SELF_SOURCE.is_empty(), "the resource const carries the include_bytes! data");
    // The ResourceEntry pairing const exposes the same path + a bytes accessor.
    assert_eq!(__leaf_resource_SELF_SOURCE.logical_path, "advice_events_app.rs");
    assert_eq!((__leaf_resource_SELF_SOURCE.bytes_fn)(), SELF_SOURCE);
}

// ─────────────────────────────── #[runner] ──────────────────────────────────

/// A runner bean: a #[component] that ALSO declares the dyn Runner upcast view, so
/// the run pipeline collects it from the Runner contract.
#[runner]
struct MigrateRunner;

impl MigrateRunner {
    fn new() -> Self {
        MigrateRunner
    }
}

impl leaf_core::Runner for MigrateRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf_core::ApplicationArguments,
    ) -> leaf_core::BoxFuture<'a, Result<(), leaf_core::LeafError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn runner_reaches_components_and_declares_the_runner_upcast_view() {
    // A #[runner] is a COMPONENTS row (so it is a registered, resolvable bean)...
    let desc = COMPONENTS
        .iter()
        .find(|d| d.declared_name == Some("migrateRunner"))
        .expect("#[runner] must register the runner bean");
    // ...whose provides[] declares the dyn Runner view (the run pipeline's key).
    let runner_view = std::any::TypeId::of::<dyn leaf_core::Runner>();
    assert!(
        desc.provides.iter().any(|row| row.view == runner_view),
        "#[runner] must declare the dyn Runner upcast view"
    );
}

#[test]
fn runner_emits_the_upcast_thunk_that_rewraps_the_erased_bean() {
    // The headline: #[runner] emits the per-runner upcast thunk the run pipeline pairs
    // by ContractId — `ErasedBean -> Option<Arc<dyn Runner>>` (the RunnerPairing the
    // auto-wire test previously hand-wrote). It downcasts the concrete runner + upcasts.
    let bean: leaf_core::ErasedBean = std::sync::Arc::new(MigrateRunner);
    let upcast: Option<std::sync::Arc<dyn leaf_core::Runner>> =
        __leaf_runner_upcast_MigrateRunner(bean);
    assert!(upcast.is_some(), "the upcast thunk re-wraps the concrete runner as Arc<dyn Runner>");
    // A non-runner erased bean yields None (the thunk is type-checked, never a guess).
    let other: leaf_core::ErasedBean = std::sync::Arc::new(42_u32);
    assert!(__leaf_runner_upcast_MigrateRunner(other).is_none());
}

// ─────────────────────── #[advisable] impl (method-aware) ───────────────────

/// The METHOD-AWARE `#[advisable]` form — `#[component]` on the struct (the
/// Descriptor) + `#[advisable]` on the impl, whose `&self` methods are advised join
/// points + transparently-invocable `MethodEntry`s (no duplicate join-points const).
#[component]
struct PricingService;

#[advisable]
impl PricingService {
    fn new() -> Self {
        PricingService
    }

    fn quote(&self, base: i64) -> i64 {
        base * 2
    }
}

#[test]
fn advisable_impl_emits_a_method_table_with_a_working_downcast_thunk() {
    // The impl form emits the per-bean method table (`__leaf_methods_PricingService`)
    // — one downcast-thunk MethodEntry per `&self` method — the const the auto-wire
    // test previously hand-wrote. The thunk downcasts, unpacks the positional arg
    // tuple, calls the real method, and packs the ErasedRet.
    let table: &::leaf_core::MethodTable = __leaf_methods_PricingService;
    let entry = table
        .lookup(::leaf_core::MethodKey::of("PricingService::quote"))
        .expect("the advised method is in the macro-emitted table");

    let bean: ::leaf_core::ErasedBean = std::sync::Arc::new(PricingService);
    let cx = ::leaf_core::ResolveCtx::root();
    let ret = futures::executor::block_on((entry.invoke)(
        &bean,
        ::leaf_core::ErasedArgs::pack((21_i64,)),
        &cx,
    ))
    .expect("the downcast thunk drives the real method");
    assert_eq!(ret.unpack::<i64>().unwrap(), 42, "21 * 2 — the real method ran via the thunk");
}

#[test]
fn advisable_impl_emits_per_method_join_points() {
    // The impl form ALSO enumerates each `&self` method as a join point (the empty
    // struct-form spec is replaced by the method-aware one for the SAME bean type).
    let spec = __leaf_joinpoints_PricingService;
    assert_eq!(spec.bean_type, std::any::TypeId::of::<PricingService>());
    assert_eq!(spec.methods.len(), 1, "one advised `&self` method (the `new` assoc fn is skipped)");
    assert_eq!(spec.methods[0].method, ::leaf_core::MethodKey::of("PricingService::quote"));
    assert_eq!(spec.methods[0].arg_types, &[std::any::TypeId::of::<i64>()]);
    assert_eq!(spec.methods[0].ret_type, std::any::TypeId::of::<i64>());
}

// ───────────────────────────── #[failure_analyzer] ──────────────────────────

/// A failure analyzer: the user writes the impl; the macro wires its discovery.
#[failure_analyzer]
struct AntiDceAnalyzer;

impl leaf_core::FailureAnalyzer for AntiDceAnalyzer {
    fn analyze(
        &self,
        err: &leaf_core::LeafError,
        _ctx: &leaf_core::AnalysisCtx,
    ) -> Option<leaf_core::FailureAnalysis> {
        if err.kind == leaf_core::ErrorKind::AntiDce {
            Some(leaf_core::FailureAnalysis {
                description: "a participating crate contributed zero rows".into(),
                action: "force-link the crate via #[leaf::main]".into(),
                cause: None,
            })
        } else {
            None
        }
    }
}

#[test]
fn failure_analyzer_reaches_the_failure_analyzers_slice_and_runs() {
    // The analyzer is link-collected and drives through the reused error-model SPI.
    let analyzers: Vec<&'static dyn leaf_core::FailureAnalyzer> =
        FAILURE_ANALYZERS.iter().copied().collect();
    let err = leaf_core::LeafError::new(leaf_core::ErrorKind::AntiDce);
    let hit = analyzers
        .iter()
        .find_map(|a| a.analyze(&err, &leaf_core::AnalysisCtx::empty()));
    assert!(
        hit.is_some_and(|h| h.description.contains("zero rows")),
        "the #[failure_analyzer] must be discoverable + run via FAILURE_ANALYZERS"
    );
}
