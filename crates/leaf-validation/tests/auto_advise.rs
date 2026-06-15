//! THE leaf-validation AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[component]` + `#[advisable]` service with a `@Valid`-argument method is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure validation advisor leaf-validation ships AUTO-WIRES (a bad arg
//! SHORT-CIRCUITS with aggregated `ConstraintViolations` as a `ValidationError`
//! BEFORE the real method runs, at `VALIDATE_ORDER` = OUTERMOST), PLUS the
//! binder-side `ValidationBindHandler` adapter running JSR validation at config bind.
//!
//! What is user code (the NATURAL `#[validated]` annotation — NO `#[aspect]`, NO
//! hand-written `ADVISOR_PAIRINGS` row):
//! - a `#[component]` + `#[advisable]` `SignupService` whose `create(req: CreateUser)`
//!   method carries `#[validated]` and takes a `@Valid` argument — the ADVISED bean;
//! - a `CreateUser` arg type whose constraints come from `#[derive(Validate)]` (the
//!   constraint-derive); the `#[validated]` ADVISOR macro is what the run test
//!   exercises, and the derive supplies the `ValidateInto` it runs;
//! - a `#[config_properties(prefix = "app", validate)]` + `#[derive(Validate)]`
//!   `PoolProps` bean whose `max-connections` field carries a `min(1)` constraint —
//!   proving FACE 3 (config-binding validation) end-to-end through the REAL
//!   macro-emitted C2 bind thunk (the `validate` flag's bind→validate gate).
//!
//! The `#[validated]` annotation on the `#[advisable]`-impl `create` method is what the
//! impl-block macro lowers to the const `ADVISOR_PAIRINGS` row (the validation advisor
//! keyed by the bean's `TypeId`, binding the single-`@Valid`-arg validator over
//! `CreateUser` — the method's first arg type) — so `Application::run` AUTO-COLLECTS the
//! validation advisor with NO `.with_advisors`.
//!
//! ## Driving the AUTO-INSTALLED chain with real args
//!
//! `RunningApp::invoke_advised` now carries the REAL typed args on `Call.args` (the
//! advised-arg ABI — each arg is `Clone + Send + Sync + 'static`), so the method-arg
//! validation advisor reads the concrete `@Valid` arg straight off the call: this
//! test drives the AUTO-INSTALLED chain end-to-end THROUGH `invoke_advised` — a valid
//! arg reaches the real body, a bad arg SHORT-CIRCUITS with aggregated violations
//! before the body runs. (The earlier take-once-cell + empty-`Call.args` ABI gap is
//! dissolved.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{ContractId, ErasedArgs, ErrorKind, LeafError, MethodKey};
// `#[validated]` is NOT imported: it is a per-method MARKER the `#[advisable]` impl
// macro STRIPS + lowers (the impl-block macro owns the row), so — exactly like
// `#[bean]` inside `#[configuration] impl` — it needs no import.
use leaf_macros::{advisable, config_properties, register_component, Validate};

// ─────────────────────────── the @Valid argument type ───────────────────────

/// The `@Valid` argument: a signup request. Constraints travel WITH the type — now
/// via `#[derive(Validate)]` (the constraint-derive, which emits the SAME
/// `not_empty`/`range` cascade the prior hand `impl ValidateInto` wrote: this test's
/// unchanged short-circuit + aggregated-violation assertions prove the derive
/// reproduces the hand behaviour byte-for-byte). `name` must be non-empty; `age` must
/// be in `[0, 150]`.
#[derive(Debug, Clone, Validate)]
struct CreateUser {
    #[validate(not_empty)]
    name: String,
    #[validate(range(min = 0, max = 150))]
    age: i64,
}

// ───────────────────────── the advised service bean ─────────────────────────

/// A service whose `create` method is VALIDATED (advised by the validation advisor):
/// the `@Valid` `req` is checked before the body runs. The `ran` flag is OWNED bean
/// state proving whether the real body executed (`register_component!` so the atomic
/// field is owned state, not a field-injected dep — the resilience-test precedent).
#[derive(Debug)]
struct SignupService {
    ran: AtomicBool,
}
register_component!(SignupService);

#[advisable]
impl SignupService {
    fn new() -> Self {
        SignupService { ran: AtomicBool::new(false) }
    }

    /// Create a user from a VALIDATED request. The `#[validated]` annotation auto-wires
    /// the validation advisor over this method's first arg type (`CreateUser`), keyed by
    /// the bean's `TypeId`. The real body only runs when `req` passes validation (a bad
    /// `req` short-circuits before this line) — it flips `ran` so the test can assert a
    /// bad arg never reached it.
    #[validated]
    fn create(&self, req: CreateUser) -> Result<String, LeafError> {
        self.ran.store(true, Ordering::SeqCst);
        Ok(format!("created {} (age {})", req.name, req.age))
    }

    /// `true` iff the real `create` body has run.
    fn ran(&self) -> bool {
        self.ran.load(Ordering::SeqCst)
    }
}

// ───────────────── FACE 3: a @ConfigurationProperties with range(min=1) ──────

/// A `@ConfigurationProperties` bean whose `max-connections` carries `min(1)`
/// (FACE 3 — the config-binding JSR validation half). It is BOTH
/// `#[config_properties(prefix = "app", validate)]` (so the macro-emitted C2 bind
/// thunk runs JSR validation over the bound value) AND `#[derive(Validate)]` (the
/// constraint-derive supplying the `ValidateInto` the thunk calls). Its `Default`
/// yields a VALID `max_connections` (1) so the empty-env C2 validate during the
/// `Application::run` test above binds the default-filled bean clean — the failing
/// case is driven explicitly through the thunk with a bad env value below.
#[config_properties(prefix = "app", validate)]
#[derive(Debug, Validate)]
struct PoolProps {
    #[validate(min = 1)]
    max_connections: i64,
}

impl Default for PoolProps {
    fn default() -> Self {
        // A valid default (>= 1): the empty-env C2 validate (the sibling run test)
        // binds + validates this default-filled bean WITHOUT a fault.
        PoolProps { max_connections: 1 }
    }
}

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_validated_bean_auto_advises_through_run_and_a_bad_arg_short_circuits() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::SignupService"));

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but annotations + the one slice row.
    let running = Application::new()
        .with_name("signup-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // The validation advisor row auto-collected from the `#[validated]` annotation: a
    // per-method-unique row whose contract is the validation family base @ the
    // module-qualified `Bean::method`.
    let create_contract = ContractId::of(&format!(
        "leaf::validation::MethodValidationAdvisor@{module}::SignupService::create"
    ));
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == create_contract),
        "the validation Infrastructure advisor row auto-collected from the #[validated] annotation"
    );

    // The service is AUTOMATICALLY advised (the proxy installed at R4).
    let svc_id = running
        .context()
        .engine()
        .registry()
        .by_contract(service_contract)
        .expect("SignupService in registry");
    assert!(
        running.is_advised(svc_id),
        "the #[advisable] bean is AUTOMATICALLY advised by the auto-collected validation advisor"
    );

    let method = MethodKey::of("SignupService::create");
    let svc = running.context().get::<SignupService>().await.expect("SignupService resolves");

    // ── BAD arg FIRST: short-circuits with AGGREGATED violations, the body NEVER runs ──
    // (Done before the valid call so `ran()` cleanly proves the body did not fire.)
    let bad = running
        .invoke_advised(
            svc_id,
            method,
            // BOTH fields invalid → both violations aggregated.
            ErasedArgs::pack((CreateUser { name: "".into(), age: 999 },)),
        )
        .await
        .expect_err("a bad @Valid arg SHORT-CIRCUITS with a framework ValidationError");
    let leaf = bad.into_leaf_error();
    assert_eq!(leaf.kind, ErrorKind::ValidationError, "an aggregated ValidationError");
    assert_eq!(
        leaf.chain.len(),
        2,
        "BOTH violations aggregated by the auto-wired chain (collect-all, not first-fail)"
    );
    let details: Vec<String> = leaf.chain.iter().map(|c| c.detail.to_string()).collect();
    assert!(details.iter().any(|d| d.contains("name: validation.not_empty")), "the name violation");
    assert!(details.iter().any(|d| d.contains("age: validation.range")), "the age violation");
    assert!(!svc.ran(), "the real body NEVER ran (short-circuited before proceed)");

    // ── VALID arg: invoke_advised carries the typed arg to the real body ──
    // The args ride `Call.args` (the advised-arg ABI), so the validation advisor reads
    // the @Valid `CreateUser` straight off the call and proceeds to the auto-installed
    // method-table thunk (the REAL `create` body).
    let ok = running
        .invoke_advised(
            svc_id,
            method,
            ErasedArgs::pack((CreateUser { name: "Jan".into(), age: 39 },)),
        )
        .await
        .expect("the valid arg proceeds through the auto-installed validation chain");
    let ok_ret: Result<String, LeafError> = ok.unpack().expect("the Result<String,_> return");
    assert_eq!(ok_ret.expect("Ok"), "created Jan (age 39)", "the real body ran on a valid arg");
    assert!(svc.ran(), "the body ran (validation passed → proceed → the real create body)");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}

/// Find the macro-emitted `__leaf_config_bind_<Ident>` thunk for a config bean on the
/// `CONFIG_BIND_PAIRINGS` slice by its module-qualified contract id (the auto-collect
/// substrate the leaf-boot C2 validate sub-pass JOINs over).
fn config_bind_thunk(ident: &str) -> leaf_core::ConfigBindThunk {
    let module = module_path!();
    let contract = ContractId::of(&format!("{module}::{ident}"));
    leaf_core::collect_slice(&leaf_core::CONFIG_BIND_PAIRINGS)
        .iter()
        .find(|r| r.contract == contract)
        .expect("the #[config_properties] bind thunk auto-collected into CONFIG_BIND_PAIRINGS")
        .thunk
}

/// Build a sealed `Env` from raw canonical pairs (the binder reads `app.max-connections`).
fn env_from(pairs: &[(&str, &str)]) -> leaf_core::Env {
    let mut b = leaf_core::EnvBuilder::new();
    b.add_last(Arc::new(leaf_core::MapPropertySource::from_pairs(
        "test",
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
    )));
    b.seal_env()
}

#[test]
fn a_config_properties_validate_flag_fails_through_the_real_bind_thunk() {
    // FACE 3 END-TO-END: the `#[config_properties(prefix = "app", validate)]`
    // macro-emitted C2 bind thunk (NOT a direct `validate_config` call) binds
    // PoolProps from the env AND runs JSR validation over the bound value. A bad env
    // value (`app.max-connections = 0`, violating `min(1)`) drives the thunk to the
    // aggregated `ValidationError` keyed `app.max-connections`. This proves the
    // codegen `validate` flag wires the bind→validate gate the thunk previously
    // SKIPPED (the `validate_config` adapter was tested but never called by the thunk).
    let thunk = config_bind_thunk("PoolProps");

    // A clean env value (>= 1) binds + validates WITHOUT a fault → Ok(Published).
    thunk(&env_from(&[("app.max-connections", "8")]), leaf_core::StartupValidation::Strict)
        .expect("max-connections=8 satisfies min(1): the bind+validate succeeds");

    // The UNBOUND/default arm also validates: an empty env binds the (valid) default.
    thunk(&env_from(&[]), leaf_core::StartupValidation::Strict)
        .expect("the default (max_connections=1) is valid: the default arm binds clean");

    // max-connections = 0 fails JSR validation INSIDE the thunk → Err(vec![verr]).
    let faults = thunk(
        &env_from(&[("app.max-connections", "0")]),
        leaf_core::StartupValidation::Strict,
    )
    .expect_err("max-connections=0 violates min(1): the thunk SHORT-CIRCUITS with the fault");
    assert_eq!(faults.len(), 1, "one aggregated ValidationError from the thunk");
    let err: &LeafError = &faults[0];
    assert_eq!(err.kind, ErrorKind::ValidationError, "the validate flag surfaces a ValidationError");
    assert_eq!(err.chain.len(), 1, "one violation");
    let detail = err.chain[0].detail.to_string();
    assert!(
        detail.contains("app.max-connections: validation.min [min=1]"),
        "the violation maps to the canonical config KEY `app.max-connections`, got: {detail}"
    );
}
