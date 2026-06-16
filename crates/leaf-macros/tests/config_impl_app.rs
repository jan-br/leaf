//! The macro→leaf_core ROUNDTRIP integration tests for the IMPL-BLOCK forms
//! `#[configuration] impl Cfg { #[bean] fn .. }` and `#[aspect] impl A {
//! #[advice] fn .. }` (configuration-classes phase3/05; aspect-model phase3/08+09).
//!
//! These CLOSE the v1 gap: a `#[bean]` on a config-class METHOD and an `#[advice]`/
//! `#[pointcut]` on an aspect METHOD were previously a loud `compile_error!` because
//! a proc-macro ATTR on a single method cannot emit the sibling const/`static` row.
//! The design's Rust-idiomatic answer is an IMPL-BLOCK macro that iterates the impl's
//! methods and emits ONE const row per method — exercised here end-to-end.
//!
//! PROOF GATE (cross-crate, re-export): this crate has NO `linkme` dep — the rows
//! reach their frozen slices through leaf-core's `pub use linkme;` (see `roundtrip.rs`).

#![allow(dead_code)]

use leaf_macros::{aspect, configuration};

// ════════════════ #[configuration] impl Cfg { #[bean] fn .. } ════════════════

/// The CONFIG bean: a unit struct (the config holds shared state read via `&self`).
/// It is registered as a `#[component]` so the container manages it as a singleton;
/// each `#[bean]` method below resolves it (the receiver) as the managed instance.
#[leaf_macros::component]
struct AppConfig;

impl AppConfig {
    fn new() -> Self {
        AppConfig
    }
    /// Shared config state the `#[bean]` methods read through `&self`.
    fn seed(&self) -> u32 {
        10
    }
}

/// The two produced beans.
#[derive(Debug, PartialEq)]
struct Pool {
    size: u32,
}

#[derive(Debug, PartialEq)]
struct Repo {
    pool_size: u32,
}

// The headline: TWO `#[bean]` methods on the config impl. Each threads `&self` (the
// managed config singleton) plus its injected collaborators. `repo` injects `Pool`
// as a PARAMETER (the singleton-correct route — NOT an intra-config `self.pool()`
// self-call, which would be a compile_error!).
#[configuration]
impl AppConfig {
    #[bean]
    fn pool(&self) -> Pool {
        Pool { size: self.seed() * 2 }
    }

    #[bean]
    fn repo(&self, pool: leaf_core::Ref<Pool>) -> Repo {
        Repo { pool_size: pool.size }
    }
}

/// Find a macro-emitted descriptor in the frozen `COMPONENTS` slice by derived name.
fn component_named(name: &str) -> leaf_core::Descriptor {
    *leaf_core::COMPONENTS
        .iter()
        .find(|d| d.declared_name == Some(name))
        .unwrap_or_else(|| panic!("`{name}` must roundtrip through ::leaf_core::COMPONENTS"))
}

#[test]
fn a_configuration_impl_with_two_bean_methods_emits_two_component_descriptors() {
    // The headline closure: two #[bean] methods => two COMPONENTS Descriptor rows,
    // each named off its method (decapitalized) and producing the method's return
    // type. (`pool`/`repo` are already lowercase, so the derived names are unchanged.)
    let pool = component_named("pool");
    let repo = component_named("repo");

    // The product types are the method return types.
    assert_eq!(pool.self_type, std::any::TypeId::of::<Pool>());
    assert_eq!(repo.self_type, std::any::TypeId::of::<Repo>());

    // Each method's contract is module-qualified on the METHOD ident.
    assert_eq!(
        pool.contract,
        leaf_core::ContractId::of(&format!("{}::pool", module_path!()))
    );
    assert_eq!(
        repo.contract,
        leaf_core::ContractId::of(&format!("{}::repo", module_path!()))
    );
}

#[test]
fn config_bean_methods_resolve_the_managed_config_and_collaborators_through_the_engine() {
    // The full construction roundtrip: register the config bean + both #[bean]-method
    // beans (pairing each macro-emitted Descriptor with its ProviderSeed, exactly as
    // leaf-boot's pairing pass would), freeze, and drive the engine. The providers
    // resolve the config receiver (`&self` -> managed singleton) + the `Pool`
    // collaborator (`repo`'s parameter) through the one Engine::get seam.
    let mut builder = leaf_core::RegistryBuilder::new();
    builder
        .register(component_named("appConfig"), config_seeds::app_config_seed()())
        .expect("the config bean registers");
    builder
        .register(component_named("pool"), config_seeds::pool_seed()())
        .expect("the pool @bean registers");
    builder
        .register(component_named("repo"), config_seeds::repo_seed()())
        .expect("the repo @bean registers");
    let registry = builder.freeze().expect("the config registry freezes");
    let engine = leaf_core::Engine::new(registry);

    // `pool` reads the MANAGED config singleton (`seed = 10`) → `size = 20`.
    let pool = futures::executor::block_on(engine.get::<Pool>())
        .expect("the pool @bean method produces Pool from the managed config");
    assert_eq!(pool.size, 20);

    // `repo` injects the SAME managed `Pool` as a parameter (singleton-correct).
    let repo = futures::executor::block_on(engine.get::<Repo>())
        .expect("the repo @bean method injects the managed Pool");
    assert_eq!(repo.pool_size, 20);
}

/// The macro exposes each `#[bean]` method's `ProviderSeed` under the deterministic
/// `__leaf_seed_<method>` public path (keyed on the METHOD ident), so a hand-written
/// assembly (here standing in for leaf-boot's pairing pass) can pair the descriptor
/// with its construction recipe.
mod config_seeds {
    pub fn app_config_seed() -> leaf_core::ProviderSeed {
        crate::__leaf_seed_AppConfig
    }
    pub fn pool_seed() -> leaf_core::ProviderSeed {
        crate::__leaf_seed_pool
    }
    pub fn repo_seed() -> leaf_core::ProviderSeed {
        crate::__leaf_seed_repo
    }
}

// ═══════════════ #[aspect] impl A { #[advice] / #[pointcut] fn .. } ═══════════

/// The aspect bean (registered by `#[aspect]` on the STRUCT — the bean carrier so
/// advice can inject collaborators).
#[aspect]
struct AuditAspect;

impl AuditAspect {
    fn new() -> Self {
        AuditAspect
    }
}

// An `#[aspect]` struct IS the interceptor: the macro's auto-collected ADVISOR_PAIRINGS
// `make_interceptor` resolves the aspect bean + upcasts it to `Arc<dyn Interceptor>`.
#[leaf_macros::async_impl]
impl leaf_core::Interceptor for AuditAspect {
    async fn intercept(
        &self,
        call: &leaf_core::Call<'_>,
        mut next: leaf_core::Next<'_>,
    ) -> Result<leaf_core::ErasedRet, leaf_core::AdviceError> {
        next.proceed(call).await
    }
}

// The per-method advice form: `#[aspect]` on the IMPL block iterates the
// `#[advice]`/`#[pointcut]` methods and emits ONE AdvisorRow per method.
#[aspect]
impl AuditAspect {
    #[advice(around, order = 100)]
    fn time(&self) {}

    #[advice(before, order = 50)]
    fn log(&self) {}

    #[pointcut]
    fn tx_methods(&self) {}
}

fn contract_here(ident: &str) -> leaf_core::ContractId {
    leaf_core::ContractId::of(&format!("{}::{}", module_path!(), ident))
}

#[test]
fn an_aspect_impl_emits_one_advisor_row_per_advice_method() {
    // The headline AOP closure: each #[advice]/#[pointcut] METHOD emits one
    // AdvisorRow into the frozen ADVISORS slice, keyed `<Aspect>_<method>`.
    for method in ["AuditAspect_time", "AuditAspect_log", "AuditAspect_tx_methods"] {
        assert!(
            leaf_core::ADVISORS
                .iter()
                .any(|r| r.contract == contract_here(method)),
            "the `{method}` advice method must emit an AdvisorRow"
        );
    }
}

#[test]
fn each_advice_methods_chain_order_rides_its_own_pairing_const() {
    // The explicit per-method `order = N` rides the per-method chain-order pairing
    // const (Annotation-sourced, so it beats an Implicit floor at equal value).
    assert_eq!(__leaf_advisor_AuditAspect_time.value, 100);
    assert_eq!(
        __leaf_advisor_AuditAspect_time.source,
        leaf_core::OrderSource::Annotation
    );
    assert_eq!(__leaf_advisor_AuditAspect_log.value, 50);
}

#[test]
fn the_aspect_bean_is_still_registered_as_a_component() {
    // The #[aspect] on the STRUCT registers the aspect bean (so advice can inject
    // collaborators); the impl-block form adds the per-method advisor rows beside it.
    assert!(
        leaf_core::COMPONENTS
            .iter()
            .any(|d| d.declared_name == Some("auditAspect")),
        "the #[aspect] struct must still register the aspect bean"
    );
}
