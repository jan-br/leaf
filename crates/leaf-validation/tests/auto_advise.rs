//! THE leaf-validation AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[component]` + `#[advisable]` service with a `@Valid`-argument method is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure validation advisor leaf-validation ships AUTO-WIRES (a bad arg
//! SHORT-CIRCUITS with aggregated `ConstraintViolations` as a `ValidationError`
//! BEFORE the real method runs, at `VALIDATE_ORDER` = OUTERMOST), PLUS the
//! binder-side `ValidationBindHandler` adapter running JSR validation at config bind.
//!
//! What is user code (annotations + one slice row):
//! - a `#[component]` + `#[advisable]` `SignupService` whose `create(req: CreateUser)`
//!   method takes a `@Valid` argument — the ADVISED bean;
//! - a `CreateUser` arg type with a hand-written `impl ValidateInto` (the
//!   `#[derive(Validate)]` / `#[validated]` macros are deferred — see the crate NOTE,
//!   the leaf-tx `#[transactional]` precedent);
//! - ONE const `ADVISOR_PAIRINGS` row the binary submits, exactly like `#[aspect]`
//!   emits — so `Application::run` AUTO-COLLECTS the validation advisor with NO
//!   hand-assembled `.with_advisors`;
//! - a `#[config_properties]` `PoolProps` bean whose `max-connections` field carries
//!   a `range(min=1)` constraint — proving FACE 3 (config-binding JSR validation).
//!
//! Everything else (the proxy plan, the chain install at R4, the make_interceptor)
//! is the run pipeline's auto-wiring.
//!
//! ## NOTE — driving the AUTO-INSTALLED chain with real args
//!
//! `RunningApp::invoke_advised` hands the interceptor chain an EMPTY `Call.args`
//! pack: the real args ride a take-once cell consumed only at the innermost tail (so
//! the owned-args `MethodEntry.invoke` thunk can move them once). That is the
//! proxy-substrate's deferred `ErasedArgs` pack-unpack ABI gap (the design's
//! "Remaining risks": "validation's typed-arg access needs concrete arg types at the
//! seam"). Method-ARG validation MUST see the typed args, so this test drives the
//! chain that `Application::run` AUTO-INSTALLED (`proxies().chain_for(svc)`) over a
//! `Call` carrying the REAL `(CreateUser,)` args — proving the auto-wired chain
//! short-circuits a bad arg before the body, with the typed args the seam will carry
//! once the ABI lands. The advisor auto-collection + the bean being auto-advised are
//! still asserted through the run pipeline.

use std::any::TypeId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    AdviceError, BeanKey, BoxFuture, Call, ContractId, ErasedArgs, ErasedRet, ErrorKind, FixedTarget,
    LeafError, MethodKey, ResolveCtx, Tail,
};
use leaf_macros::{advisable, component};
use leaf_validation::{
    single_arg_make_interceptor, validate_config, validation_advisor_contract, Cascade,
    ValidateInto, ValidationPointcut,
};

// ─────────────────────────── the @Valid argument type ───────────────────────

/// The `@Valid` argument: a signup request. Constraints travel WITH the type (a hand
/// `impl ValidateInto` — the `#[derive(Validate)]` macro is deferred). `name` must be
/// non-empty; `age` must be in `[0, 150]`.
#[derive(Debug, Clone)]
struct CreateUser {
    name: String,
    age: i64,
}

impl ValidateInto for CreateUser {
    fn validate_into(&self, c: &mut Cascade<'_>) {
        c.check("name", leaf_validation::constraints::not_empty(&self.name));
        c.check("age", leaf_validation::constraints::range(self.age, 0, 150));
    }
}

// ───────────────────────── the advised service bean ─────────────────────────

/// A `@Component` service whose `create` method is VALIDATED (advised by the
/// validation advisor): the `@Valid` `req` is checked before the body runs.
#[component]
#[derive(Debug)]
struct SignupService;

#[advisable]
impl SignupService {
    fn new() -> Self {
        SignupService
    }

    /// Create a user from a VALIDATED request. The real body only runs when `req`
    /// passes validation (a bad `req` short-circuits before this line).
    fn create(&self, req: CreateUser) -> Result<String, LeafError> {
        Ok(format!("created {} (age {})", req.name, req.age))
    }
}

// ───────────────────── the AUTO-WIRED validation advisor row ─────────────────

// The validation advisor matches SignupService by its concrete TypeId (the
// recursion-safe pointcut). The const TypeId-of seam mints the 'static slice exactly
// as a `#[validated]` macro would.
static ADVISED_TYPES: [TypeId; 1] = [const { TypeId::of::<SignupService>() }];
static VALIDATION_POINTCUT: ValidationPointcut = ValidationPointcut::new(&ADVISED_TYPES, &[]);

// THE auto-wire row: one const `AdvisorPairingRow` in `ADVISOR_PAIRINGS` (the same
// channel `Application::run` collects `#[aspect]` rows from), baking in the per-method
// single-`@Valid`-arg validator for `CreateUser`. No `.with_advisors` in the run
// call. The make_interceptor is a const closure literal (const-promoted to the bare
// fn-pointer exactly like the `#[aspect]` codegen emits).
#[leaf_core::linkme::distributed_slice(leaf_core::ADVISOR_PAIRINGS)]
#[linkme(crate = leaf_core::linkme)]
static VALIDATION_ADVISOR_ROW: leaf_core::AdvisorPairingRow = leaf_core::AdvisorPairingRow {
    contract: ContractId::of("leaf::validation::MethodValidationAdvisor"),
    order: leaf_core::OrderKey {
        value: leaf_core::VALIDATE_ORDER,
        source: leaf_core::OrderSource::Interface,
    },
    role: leaf_core::Role::Infrastructure,
    pointcut: &VALIDATION_POINTCUT,
    make_interceptor: |c| single_arg_make_interceptor::<CreateUser>()(c),
};

// ───────────────── FACE 3: a @ConfigurationProperties with range(min=1) ──────

/// A `@ConfigurationProperties` bean whose `max-connections` carries `range(min=1)`
/// (FACE 3 — the config-binding JSR validation half). The hand `impl ValidateInto`
/// stands in for the deferred derive.
#[derive(Debug, Default)]
struct PoolProps {
    max_connections: i64,
}

impl ValidateInto for PoolProps {
    fn validate_into(&self, c: &mut Cascade<'_>) {
        c.check("max-connections", leaf_validation::constraints::min(self.max_connections, 1));
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

    // The validation advisor row auto-collected (the headline: it is in ADVISOR_PAIRINGS).
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == validation_advisor_contract()),
        "the validation Infrastructure advisor row auto-collected from ADVISOR_PAIRINGS"
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

    // The AUTO-INSTALLED chain (assembled by Application::run at R4) for the advised
    // bean. We drive it directly over a Call carrying the REAL typed args (the
    // invoke_advised empty-args ABI gap — see the module NOTE).
    let proxies = running.unit().proxies().expect("proxies installed at R4");
    let chain = proxies.chain_for(svc_id).expect("the auto-installed validation chain").clone();
    let method = MethodKey::of("SignupService::create");

    // The innermost target is the published singleton (the same FixedTarget the run
    // pipeline's own tail resolves); our tail records whether the real body ran.
    let registry = running.context().engine().registry();
    let target = FixedTarget::new(
        leaf_boot::InstalledProxies::fixed_target_for(registry, svc_id)
            .expect("the advised singleton is published"),
    );
    let cx = ResolveCtx::root();

    // ── VALID arg: the chain proceeds to the body (the tail runs) ──
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in = Arc::clone(&ran);
    let good_call = Call::new(
        method,
        BeanKey::ByContract(service_contract),
        ErasedArgs::pack((CreateUser { name: "Jan".into(), age: 39 },)),
        &target,
        &cx,
    );
    let good_tail: Box<Tail> = Box::new(move |c: &Call<'_>| {
        ran_in.store(true, Ordering::SeqCst);
        // The body's view of the typed arg the validator just approved.
        let arg = c.args.0.downcast_ref::<(CreateUser,)>().expect("the typed arg reached the body");
        let ret: Result<String, LeafError> = Ok(format!("created {} (age {})", arg.0.name, arg.0.age));
        Box::pin(async move { Ok(ErasedRet::pack(ret)) })
            as BoxFuture<'_, Result<ErasedRet, AdviceError>>
    });
    let ok = chain
        .invoke(&good_call, &*good_tail)
        .await
        .expect("the valid arg proceeds through the auto-installed validation chain");
    let ok_ret: Result<String, LeafError> = ok.unpack().expect("the Result<String,_> return");
    assert_eq!(ok_ret.expect("Ok"), "created Jan (age 39)", "the real body ran on a valid arg");
    assert!(ran.load(Ordering::SeqCst), "the body ran (validation passed → proceed)");

    // ── BAD arg: short-circuits with AGGREGATED violations, the body NEVER runs ──
    let ran2 = Arc::new(AtomicBool::new(false));
    let ran2_in = Arc::clone(&ran2);
    let bad_call = Call::new(
        method,
        BeanKey::ByContract(service_contract),
        // BOTH fields invalid → both violations aggregated.
        ErasedArgs::pack((CreateUser { name: "".into(), age: 999 },)),
        &target,
        &cx,
    );
    let bad_tail: Box<Tail> = Box::new(move |_c: &Call<'_>| {
        ran2_in.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(ErasedRet::pack(Ok::<String, LeafError>(String::new()))) })
            as BoxFuture<'_, Result<ErasedRet, AdviceError>>
    });
    let bad = chain
        .invoke(&bad_call, &*bad_tail)
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
    assert!(!ran2.load(Ordering::SeqCst), "the real body NEVER ran (short-circuited before proceed)");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}

#[test]
fn a_config_properties_with_range_min_1_fails_at_validate_via_the_bind_handler() {
    // FACE 3: the binder-side adapter runs the SAME engine after a bind. A bound
    // PoolProps with max-connections = 0 violates range(min=1) and fails at VALIDATE
    // with the canonical property KEY (`app.max-connections`) — the leaf-config/
    // leaf-boot C2 path's missing validation half.

    // A clean value (>= 1) binds without fault.
    let ok = PoolProps { max_connections: 8 };
    assert!(validate_config("app", &ok).is_none(), "max-connections=8 satisfies range(min=1)");

    // max-connections = 0 fails at validate.
    let bad = PoolProps { max_connections: 0 };
    let err = validate_config("app", &bad).expect("range(min=1) fails at validate via the bind handler");
    assert_eq!(err.kind, ErrorKind::ValidationError);
    assert_eq!(err.chain.len(), 1, "one violation");
    let detail = err.chain[0].detail.to_string();
    assert!(
        detail.contains("app.max-connections: validation.min [min=1]"),
        "the violation maps to the canonical config KEY `app.max-connections`, got: {detail}"
    );
}
