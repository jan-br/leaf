//! `RedisAutoConfig` — the representative `#[auto_config]` integration: an
//! `AUTO_CONFIGS` row at [`CandidateRole::FALLBACK`](leaf_core::CandidateRole)
//! contributing the Redis-backed `Arc<dyn CacheManager>` bean (Spring's
//! `RedisAutoConfiguration` with a `@Bean cacheManager()` method), guarded by
//! `OnProperty(leaf.redis.enabled)` AND `OnMissingBean(RedisCacheManager)`.
//!
//! ## The `#[auto_config] impl` form (Spring's @AutoConfiguration + @Bean methods)
//!
//! `#[auto_config] impl RedisAutoConfig { #[bean(name = "cacheManager", provides =
//! "dyn CacheManager")] #[conditional(..)] fn cache_manager(&self) ->
//! RedisCacheManager { .. } }` emits the SAME const artifacts a hand-built
//! auto-config would — the `AUTO_CONFIGS` [`Descriptor`] at `FALLBACK` (carrying the
//! `dyn CacheManager` provides[] view + the `"cacheManager"` declared name), its
//! [`ProviderSeed`](leaf_core::ProviderSeed) + `SEED_PAIRINGS` JOIN, and the
//! `#[conditional]` guard + its `GUARD_PAIRINGS` + `CONDITIONS` anchors — all keyed on
//! the ONE contributed contract (`module_path!()::cache_manager`) so leaf-boot's
//! `Descriptor.contract == SeedPairingRow.contract == GuardPairingRow.contract` JOIN
//! finds them. The holder [`RedisAutoConfig`] is a managed `#[component]` (the `&self`
//! receiver each `#[bean]` method reads — singleton-correct).
//!
//! The LIVE socket I/O ([`RedisClient::open`](crate::client::RedisClient::open)) stays
//! hand-written inside the `#[bean]` factory body the macro calls — only the const
//! REGISTRATION scaffolding is macro-emitted (the ecosystem boundary).
//!
//! ## The back-off contract
//!
//! The guard's `OnMissingBean(RedisCacheManager)` makes the auto-config back off when a
//! bean of that concrete type is already present (the soft override: a user
//! `CacheManager` supersedes it). leaf-boot's `run_autoconfig` evaluates the guard over
//! the GROWING definition set in the Register sub-pass; the `OnProperty` leaf
//! additionally requires `leaf.redis.enabled` to be present-and-not-`false`.
//!
//! NOTE (honest deferral — `dyn`-view back-off): leaf-boot's `BuilderProbe` keys
//! back-off on the candidate's CONCRETE `self_type`, so the `OnMissingBean` here matches
//! a user bean of type `RedisCacheManager`. A user `CacheManager` of a DIFFERENT concrete
//! type does not yet trip this probe — distributed-by-`dyn`-view back-off is a
//! `provides[]`-aware-probe concern tracked for a later unit. The `provides[]` row
//! already declares the `dyn CacheManager` view so consumers resolve `Arc<dyn CacheManager>`.
//!
//! NOTE (honest deferral — env-bound props): the `#[bean]` factory opens the (lazy)
//! `RedisClient` from DEFAULT [`RedisProperties`]; threading the env-bound props into the
//! factory is the config-bind seam the binary supplies (the `RedisProperties::from_env`
//! projection is ready, and the default URL never fails to open — URL validation only).

use leaf_core::{CondExpr, ContractId, Descriptor, ProviderSeed};

use crate::client::RedisClient;
use crate::manager::RedisCacheManager;
use crate::properties::RedisProperties;

/// The declared name of the contributed `Arc<dyn CacheManager>` bean (Spring's
/// `cacheManager` identity preserved).
pub const REDIS_CACHE_MANAGER_BEAN: &str = "cacheManager";

/// The stable contract path of the Redis auto-config's contributed cache-manager
/// bean — the contract the `#[auto_config] impl` macro mints from the
/// `module_path!()::<method>` of the `cache_manager` `#[bean]` method (the ONE
/// contract the `Descriptor`, the `SeedPairingRow`, and the `GuardPairingRow` share).
pub const REDIS_CACHE_MANAGER_CONTRACT: &str = "leaf_redis::autoconfig::cache_manager";

// ───────────────────────── the #[auto_config] holder ─────────────────────────

/// The auto-configuration HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` block below contributes the cache manager from a `#[bean]`
/// method that reads this holder as its `&self` receiver.
#[leaf_macros::component]
pub struct RedisAutoConfig;

impl RedisAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        RedisAutoConfig
    }
}

impl Default for RedisAutoConfig {
    fn default() -> Self {
        RedisAutoConfig::new()
    }
}

// ───────────────────── the @bean-method cache-manager contribution ─────────────

/// The differently-typed `@Bean`-method contribution: `cache_manager()` produces the
/// concrete [`RedisCacheManager`] exposed as `dyn CacheManager` (the `provides[]`
/// view), named `"cacheManager"`, into `AUTO_CONFIGS` at `FALLBACK`, gated by
/// `OnProperty(leaf.redis.enabled)` AND `OnMissingBean(RedisCacheManager)`.
#[leaf_macros::auto_config]
impl RedisAutoConfig {
    /// Build the Redis-backed cache manager (the ecosystem factory body the macro
    /// calls). It opens a LAZY [`RedisClient`] from the DEFAULT properties — URL
    /// validation only, no socket — and publishes the shared [`RedisCacheManager`].
    ///
    /// The default URL is a known-valid Redis connection URL, so `open` cannot fail
    /// here (the env-bound props threading is the deferred config-bind seam); the
    /// `.expect` documents that invariant rather than widening the macro to a fallible
    /// `#[bean]` return.
    #[bean(name = "cacheManager", provides = "dyn ::leaf_core::CacheManager")]
    #[conditional(
        on_property("leaf.redis.enabled"),
        on_missing_bean(RedisCacheManager)
    )]
    fn cache_manager(&self) -> RedisCacheManager {
        let client = RedisClient::open(RedisProperties::default())
            .expect("the default Redis URL is always a valid (lazy) connection URL");
        RedisCacheManager::new(client)
    }
}

// ───────────────────────── thin compatibility aliases ────────────────────────
//
// The macro emits the contributed bean's `ProviderSeed` as `__leaf_seed_cache_manager`
// and its back-off guard as `__leaf_guard_cache_manager` (keyed off the METHOD ident),
// and submits the `Descriptor` into the `AUTO_CONFIGS` slice. These thin aliases
// preserve the crate's historical public surface over the macro-emitted artifacts.

/// The const [`ProviderSeed`](leaf_core::ProviderSeed) leaf-boot's `run_autoconfig`
/// invokes ONCE to mint the manager's `Provider` (the macro-emitted seed).
pub const REDIS_CACHE_MANAGER_SEED: ProviderSeed = __leaf_seed_cache_manager;

/// The const back-off guard for the Redis cache manager: it registers at `FALLBACK`
/// IFF `leaf.redis.enabled` is set AND no `RedisCacheManager` bean already exists (the
/// macro-emitted `#[conditional]` guard tree).
pub static REDIS_AUTO_CONFIG_GUARD: CondExpr = __leaf_guard_cache_manager;

/// The contributed `AUTO_CONFIGS` [`Descriptor`] for the Redis cache manager (looked up
/// from the macro-emitted `AUTO_CONFIGS` slice row by its `"cacheManager"` name) — at
/// `FALLBACK`, on the SEPARATE auto-config channel, carrying the `dyn CacheManager` view.
#[must_use]
pub fn redis_cache_manager_descriptor() -> Descriptor {
    *leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.contract == ContractId::of(REDIS_CACHE_MANAGER_CONTRACT))
        .expect("the #[auto_config] cache_manager Descriptor must reach the AUTO_CONFIGS slice")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use leaf_core::{CandidateRole, ConditionKind, ResolveCtx};

    #[test]
    fn descriptor_is_a_fallback_auto_config_with_the_cache_manager_view() {
        let d = redis_cache_manager_descriptor();
        assert_eq!(
            d.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user bean supersedes it"
        );
        assert_eq!(d.role, leaf_core::Role::Application);
        // The product is the CONCRETE RedisCacheManager (the method return type).
        assert_eq!(d.self_type, TypeId::of::<RedisCacheManager>());
        assert_eq!(d.declared_name, Some(REDIS_CACHE_MANAGER_BEAN));
        // It provides the dyn CacheManager view (consumers resolve Arc<dyn CacheManager>).
        assert!(
            d.provides.iter().any(|r| r.view == TypeId::of::<dyn leaf_core::CacheManager>()),
            "the auto-config must declare the dyn CacheManager view"
        );
    }

    #[test]
    fn the_auto_config_rides_the_separate_auto_configs_channel_not_components() {
        let contract = ContractId::of(REDIS_CACHE_MANAGER_CONTRACT);
        let autos = leaf_core::collect_slice(&leaf_core::AUTO_CONFIGS);
        assert!(
            autos.iter().any(|r| r.contract == contract),
            "the Redis cache manager must be an AUTO_CONFIGS row"
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
        let contract = ContractId::of(REDIS_CACHE_MANAGER_CONTRACT);
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == contract),
            "the AUTO_CONFIGS row must have a paired ProviderSeed on its contract"
        );
    }

    #[test]
    fn the_guard_is_property_and_missing_bean_gated() {
        // The guard tree is All([OnProperty, OnMissingBean]) — both leaves present (the
        // comma-separated #[conditional] form ANDs them).
        match &REDIS_AUTO_CONFIG_GUARD {
            CondExpr::All(children) => {
                assert_eq!(children.len(), 2, "OnProperty AND OnMissingBean");
                assert!(matches!(children[0], CondExpr::Leaf(id, _) if id == leaf_conditions::OnProperty::ID));
                assert!(
                    matches!(children[1], CondExpr::Leaf(id, _) if id == leaf_conditions::OnMissingBean::ID)
                );
            }
            other => panic!("expected All([..]); got {other:?}"),
        }
    }

    #[test]
    fn the_guard_leaf_ids_match_the_leaf_conditions_kind_ids() {
        // The macro-minted leaf ids MUST equal leaf-conditions' ConditionKind::ID so the
        // leaves resolve to the runtime impls (the cross-crate ID contract).
        match &REDIS_AUTO_CONFIG_GUARD {
            CondExpr::All(children) => {
                let ids: Vec<_> = children
                    .iter()
                    .filter_map(|c| match c {
                        CondExpr::Leaf(id, _) => Some(*id),
                        _ => None,
                    })
                    .collect();
                assert!(ids.contains(&leaf_conditions::OnProperty::ID));
                assert!(ids.contains(&leaf_conditions::OnMissingBean::ID));
            }
            other => panic!("expected All([..]); got {other:?}"),
        }
    }

    #[test]
    fn the_guard_defers_to_the_register_sub_pass_via_the_on_missing_bean_leaf() {
        // An OnBean-family leaf (the OnMissingBean) forces the whole guard to evaluate at
        // Register (it must see the growing definition set).
        assert_eq!(
            leaf_conditions::OnMissingBean::SUB,
            leaf_core::SubPhase::Register,
            "OnMissingBean is a Register-sub-pass kind (sees the growing set)"
        );
    }

    #[test]
    fn the_guard_is_paired_and_anchored_in_the_slices() {
        // The guard is paired by the contributed contract (the SAME as the Descriptor),
        // and one CONDITIONS anti-DCE anchor rides per referenced kind.
        let contract = ContractId::of(REDIS_CACHE_MANAGER_CONTRACT);
        let guards = leaf_core::collect_slice(&leaf_core::GUARD_PAIRINGS);
        assert!(
            guards.iter().any(|r| r.contract == contract),
            "the guard must be paired by the auto-config's contributed contract"
        );
        let conds = leaf_core::collect_slice(&leaf_core::CONDITIONS);
        assert!(
            conds.iter().any(|r| r.contract == ContractId::of("leaf::condition::OnProperty")),
            "an OnProperty anti-DCE anchor must be present"
        );
        assert!(
            conds.iter().any(|r| r.contract == ContractId::of("leaf::condition::OnMissingBean")),
            "an OnMissingBean anti-DCE anchor must be present"
        );
    }

    #[test]
    fn the_provider_publishes_a_shared_cache_manager() {
        // The macro-emitted provider resolves the holder (the `&self` receiver) then
        // calls cache_manager() — so it needs the holder registered. Drive the real
        // engine over the holder + the contributed bean and assert the published bean is
        // the concrete RedisCacheManager.
        use leaf_core::CacheManager;
        let mut builder = leaf_core::RegistryBuilder::new();
        let holder = leaf_core::COMPONENTS
            .iter()
            .find(|d| d.declared_name == Some("redisAutoConfig"))
            .copied()
            .expect("the holder is a COMPONENTS row");
        builder
            .register(holder, __leaf_seed_RedisAutoConfig())
            .expect("the holder registers");
        builder
            .register(redis_cache_manager_descriptor(), REDIS_CACHE_MANAGER_SEED())
            .expect("the contributed cache manager registers");
        let registry = builder.freeze().expect("freezes");
        let engine = leaf_core::Engine::new(registry);
        let mgr = futures::executor::block_on(engine.get::<RedisCacheManager>())
            .expect("the contributed RedisCacheManager resolves");
        // It is a working CacheManager (hands out a named cache via the trait ABI).
        assert!(
            CacheManager::cache(&*mgr, "users").is_some(),
            "the published manager is a live CacheManager"
        );

        // And the provider publishes Shared (a singleton cache manager).
        let provider = engine.registry().provider(
            engine.registry().by_contract(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)).unwrap(),
        );
        let cx = ResolveCtx::for_engine(&engine);
        let published = futures::executor::block_on(provider.provide(&cx)).expect("provides");
        assert!(published.is_shared(), "a singleton cache manager publishes Shared");
    }
}
