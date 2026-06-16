//! `TxAutoConfig` — the DEFAULT transaction-manager `#[auto_config]` integration: an
//! [`AUTO_CONFIGS`](leaf_core::AUTO_CONFIGS) row at
//! [`CandidateRole::FALLBACK`](leaf_core::CandidateRole) contributing the in-memory
//! `Arc<dyn TransactionManager>` bean (Spring's `TransactionAutoConfiguration` with a
//! `@Bean transactionManager()` method), guarded by `OnMissingBean(InMemoryTransactionManager)`.
//!
//! ## Why a default manager bean
//!
//! `#[transactional(manager = M)]` resolves its manager by `BeanKey::ByType(TypeId::of::<M>())`
//! — there is no default manager type the advisor falls back to. Before this auto-config an
//! app had to hand-write a registered `TransactionManager` bean (e.g. a `LocalTransactionManager`
//! newtype wrapping leaf-tx's [`InMemoryTransactionManager`]) just so `#[transactional(manager =
//! …)]` had a named, registered manager to demarcate against. This auto-config registers
//! leaf-tx's own [`InMemoryTransactionManager`] as the `"transactionManager"` bean at
//! `FALLBACK`, so an app writes `#[transactional(manager = InMemoryTransactionManager)]` with
//! NO wrapper bean — the cache precedent (`leaf_redis::autoconfig::RedisAutoConfig`) applied
//! to transactions.
//!
//! ## The `#[auto_config] impl` form (Spring's @AutoConfiguration + @Bean methods)
//!
//! `#[auto_config] impl TxAutoConfig { #[bean(name = "transactionManager", provides =
//! "dyn TransactionManager")] #[conditional(on_missing_bean(InMemoryTransactionManager))] fn
//! transaction_manager(&self) -> InMemoryTransactionManager { .. } }` emits the SAME const
//! artifacts a hand-built auto-config would — the `AUTO_CONFIGS` [`Descriptor`] at `FALLBACK`
//! (carrying the `dyn TransactionManager` provides[] view + the `"transactionManager"` declared
//! name), its [`ProviderSeed`] + `SEED_PAIRINGS` JOIN, and the `#[conditional]` guard + its
//! `GUARD_PAIRINGS` + `CONDITIONS` anchors — all keyed on the ONE contributed contract
//! (`module_path!()::transaction_manager`). The holder [`TxAutoConfig`] is a managed
//! `#[component]` (the `&self` receiver the `#[bean]` method reads — singleton-correct).
//!
//! ## The back-off contract
//!
//! Unlike the cache auto-config, this carries NO `on_property` leaf — transactions need no
//! feature flag, so the default manager participates whenever leaf-tx is linked. Its
//! `OnMissingBean(InMemoryTransactionManager)` makes the auto-config back off when a bean of
//! that concrete type is already present, and the `FALLBACK` candidate role lets a user
//! `TransactionManager` of a different concrete type supersede it by role. leaf-boot's
//! `run_autoconfig` evaluates the guard over the GROWING definition set in the Register
//! sub-pass.
//!
//! NOTE (honest deferral — `dyn`-view back-off): like the cache auto-config, leaf-boot's
//! `BuilderProbe` keys back-off on the candidate's CONCRETE `self_type`, so the `OnMissingBean`
//! here matches a user bean of type `InMemoryTransactionManager`. A user `TransactionManager`
//! of a DIFFERENT concrete type does not yet trip this probe (it supersedes by role instead) —
//! distributed-by-`dyn`-view back-off is the same `provides[]`-aware-probe concern tracked for
//! a later unit. The `provides[]` row already declares the `dyn TransactionManager` view so
//! consumers (the advisor's `make_interceptor`) resolve `Arc<dyn TransactionManager>`.

use leaf_core::{CondExpr, ContractId, Descriptor, ProviderSeed};

use crate::manager::InMemoryTransactionManager;

/// The declared name of the contributed `Arc<dyn TransactionManager>` bean (Spring's
/// `transactionManager` identity preserved).
pub const TRANSACTION_MANAGER_BEAN: &str = "transactionManager";

/// The stable contract path of the tx auto-config's contributed transaction-manager bean —
/// the contract the `#[auto_config] impl` macro mints from the `module_path!()::<method>` of
/// the `transaction_manager` `#[bean]` method (the ONE contract the `Descriptor`, the
/// `SeedPairingRow`, and the `GuardPairingRow` share).
pub const TRANSACTION_MANAGER_CONTRACT: &str = "leaf_tx::autoconfig::transaction_manager";

// ───────────────────────── the #[auto_config] holder ─────────────────────────

/// The auto-configuration HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` block below contributes the transaction manager from a `#[bean]`
/// method that reads this holder as its `&self` receiver.
#[leaf_macros::component]
pub struct TxAutoConfig;

impl TxAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        TxAutoConfig
    }
}

impl Default for TxAutoConfig {
    fn default() -> Self {
        TxAutoConfig::new()
    }
}

// ──────────────────── the @bean-method transaction-manager contribution ─────────

/// The `@Bean`-method contribution: `transaction_manager()` produces the concrete
/// [`InMemoryTransactionManager`] exposed as `dyn TransactionManager` (the `provides[]`
/// view), named `"transactionManager"`, into `AUTO_CONFIGS` at `FALLBACK`, gated by
/// `OnMissingBean(InMemoryTransactionManager)`.
#[leaf_macros::auto_config]
impl TxAutoConfig {
    /// Build the default in-memory transaction manager (the factory body the macro calls).
    /// A real datastore manager (leaf-sqlx-tx, …) is an ordinary bean that supersedes this
    /// `FALLBACK` default; this is the safe no-op stand-in so `#[transactional(manager =
    /// InMemoryTransactionManager)]` resolves with no hand-written wrapper bean.
    #[bean(
        name = "transactionManager",
        provides = "dyn ::leaf_core::TransactionManager"
    )]
    #[conditional(on_missing_bean(InMemoryTransactionManager))]
    fn transaction_manager(&self) -> InMemoryTransactionManager {
        InMemoryTransactionManager::new()
    }
}

// ───────────────────────── thin compatibility aliases ────────────────────────
//
// The macro emits the contributed bean's `ProviderSeed` as `__leaf_seed_transaction_manager`
// and its back-off guard as `__leaf_guard_transaction_manager` (keyed off the METHOD ident),
// and submits the `Descriptor` into the `AUTO_CONFIGS` slice. These thin aliases preserve a
// stable public surface over the macro-emitted artifacts AND act as the anti-DCE anchors that
// path-reference the macro-emitted statics (so the row reaches the slice even under
// `--gc-sections`), mirroring leaf-redis's `REDIS_CACHE_MANAGER_SEED` / `REDIS_AUTO_CONFIG_GUARD`.

/// The const [`ProviderSeed`] leaf-boot's `run_autoconfig` invokes ONCE to mint the
/// manager's `Provider` (the macro-emitted seed).
pub const TRANSACTION_MANAGER_SEED: ProviderSeed = __leaf_seed_transaction_manager;

/// The const back-off guard for the default transaction manager: it registers at `FALLBACK`
/// IFF no `InMemoryTransactionManager` bean already exists (the macro-emitted `#[conditional]`
/// guard tree — a single `OnMissingBean` leaf, no `OnProperty`).
pub static TX_AUTO_CONFIG_GUARD: CondExpr = __leaf_guard_transaction_manager;

/// The contributed `AUTO_CONFIGS` [`Descriptor`] for the default transaction manager (looked
/// up from the macro-emitted `AUTO_CONFIGS` slice row by its contributed contract) — at
/// `FALLBACK`, on the SEPARATE auto-config channel, carrying the `dyn TransactionManager` view.
#[must_use]
pub fn transaction_manager_descriptor() -> Descriptor {
    *leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.contract == ContractId::of(TRANSACTION_MANAGER_CONTRACT))
        .expect("the #[auto_config] transaction_manager Descriptor must reach the AUTO_CONFIGS slice")
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_conditions::ConditionKind;
    use leaf_core::{CandidateRole, ResolveCtx};
    use std::any::TypeId;

    #[test]
    fn descriptor_is_a_fallback_auto_config_with_the_transaction_manager_view() {
        let d = transaction_manager_descriptor();
        assert_eq!(
            d.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user bean supersedes it"
        );
        assert_eq!(d.role, leaf_core::Role::Application);
        // The product is the CONCRETE InMemoryTransactionManager (the method return type).
        assert_eq!(d.self_type, TypeId::of::<InMemoryTransactionManager>());
        assert_eq!(d.declared_name, Some(TRANSACTION_MANAGER_BEAN));
        // It provides the dyn TransactionManager view (the advisor resolves
        // Arc<dyn TransactionManager> through it).
        assert!(
            d.provides
                .iter()
                .any(|r| r.view == TypeId::of::<dyn leaf_core::TransactionManager>()),
            "the auto-config must declare the dyn TransactionManager view"
        );
    }

    #[test]
    fn the_auto_config_rides_the_separate_auto_configs_channel_not_components() {
        let contract = ContractId::of(TRANSACTION_MANAGER_CONTRACT);
        let autos = leaf_core::collect_slice(&leaf_core::AUTO_CONFIGS);
        assert!(
            autos.iter().any(|r| r.contract == contract),
            "the default transaction manager must be an AUTO_CONFIGS row"
        );
        let comps = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            !comps.iter().any(|r| r.contract == contract),
            "the auto-config contribution must NOT be in COMPONENTS"
        );
    }

    #[test]
    fn the_auto_config_descriptor_has_a_paired_seed_on_the_same_contract() {
        // THE ALIGNMENT: the SeedPairingRow keys on the SAME contributed contract as the
        // Descriptor (so leaf-boot's JOIN finds the construction recipe).
        let contract = ContractId::of(TRANSACTION_MANAGER_CONTRACT);
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == contract),
            "the AUTO_CONFIGS row must have a paired ProviderSeed on its contract"
        );
    }

    #[test]
    fn the_guard_is_a_bare_on_missing_bean_leaf() {
        // A single comma-less #[conditional(on_missing_bean(..))] lowers to a BARE leaf
        // (not wrapped in All([..]) the way the comma-separated redis form is).
        match &TX_AUTO_CONFIG_GUARD {
            CondExpr::Leaf(id, _) => assert_eq!(
                *id,
                leaf_conditions::OnMissingBean::ID,
                "the lone guard leaf is OnMissingBean"
            ),
            other => panic!("expected a bare OnMissingBean Leaf; got {other:?}"),
        }
    }

    #[test]
    fn the_guard_defers_to_the_register_sub_pass_via_the_on_missing_bean_leaf() {
        // An OnMissingBean leaf forces the guard to evaluate at Register (it must see the
        // growing definition set).
        assert_eq!(
            leaf_conditions::OnMissingBean::SUB,
            leaf_core::SubPhase::Register,
            "OnMissingBean is a Register-sub-pass kind (sees the growing set)"
        );
    }

    #[test]
    fn the_guard_is_paired_and_anchored_in_the_slices() {
        // The guard is paired by the contributed contract (the SAME as the Descriptor), and
        // one CONDITIONS anti-DCE anchor rides for the referenced kind.
        let contract = ContractId::of(TRANSACTION_MANAGER_CONTRACT);
        let guards = leaf_core::collect_slice(&leaf_core::GUARD_PAIRINGS);
        assert!(
            guards.iter().any(|r| r.contract == contract),
            "the guard must be paired by the auto-config's contributed contract"
        );
        let conds = leaf_core::collect_slice(&leaf_core::CONDITIONS);
        assert!(
            conds.iter().any(|r| r.contract == ContractId::of("leaf::condition::OnMissingBean")),
            "an OnMissingBean anti-DCE anchor must be present"
        );
    }

    #[test]
    fn the_provider_publishes_a_shared_transaction_manager() {
        // The macro-emitted provider resolves the holder (the `&self` receiver) then calls
        // transaction_manager() — so it needs the holder registered. Drive the real engine
        // over the holder + the contributed bean and assert the published bean is the
        // concrete InMemoryTransactionManager, published Shared (a singleton).
        let mut builder = leaf_core::RegistryBuilder::new();
        let holder = leaf_core::COMPONENTS
            .iter()
            .find(|d| d.declared_name == Some("txAutoConfig"))
            .copied()
            .expect("the holder is a COMPONENTS row");
        builder
            .register(holder, __leaf_seed_TxAutoConfig())
            .expect("the holder registers");
        builder
            .register(transaction_manager_descriptor(), TRANSACTION_MANAGER_SEED())
            .expect("the contributed transaction manager registers");
        let registry = builder.freeze().expect("freezes");
        let engine = leaf_core::Engine::new(registry);
        let mgr = futures::executor::block_on(engine.get::<InMemoryTransactionManager>())
            .expect("the contributed InMemoryTransactionManager resolves");
        // It is a fresh manager (zeroed counters — a live TransactionManager bean).
        assert_eq!(mgr.begins(), 0);

        let provider = engine.registry().provider(
            engine
                .registry()
                .by_contract(ContractId::of(TRANSACTION_MANAGER_CONTRACT))
                .unwrap(),
        );
        let cx = ResolveCtx::for_engine(&engine);
        let published = futures::executor::block_on(provider.provide(&cx)).expect("provides");
        assert!(published.is_shared(), "a singleton transaction manager publishes Shared");
    }
}
