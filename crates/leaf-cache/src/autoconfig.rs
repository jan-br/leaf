//! `CacheAutoConfig` — the DEFAULT cache-manager `#[auto_config]` integration: an
//! [`AUTO_CONFIGS`](leaf_core::AUTO_CONFIGS) row at
//! [`CandidateRole::FALLBACK`](leaf_core::CandidateRole) contributing the in-memory
//! `Arc<dyn CacheManager>` bean (Spring's `CacheAutoConfiguration` with a
//! `@Bean cacheManager()` method), guarded by `OnMissingBean(dyn CacheManager)` and
//! ordered LAST among the cache auto-configs (Spring's `@AutoConfigureAfter` for the
//! simple-cache default).
//!
//! ## Why a default manager bean
//!
//! `#[cacheable(manager = M)]` resolves its manager by `BeanKey::ByType(TypeId::of::<M>())`
//! — there is no default manager type the advisor falls back to. Before this auto-config an
//! app had to hand-write a registered `CacheManager` bean (a newtype wrapping leaf-cache's
//! [`InMemoryCacheManager`]) just so `#[cacheable(manager = …)]` had a named, registered
//! manager to cache against. This auto-config registers leaf-cache's own
//! [`InMemoryCacheManager`] as the `"cacheManager"` bean at `FALLBACK`, so an app writes
//! `#[cacheable(… manager = InMemoryCacheManager)]` with NO wrapper bean — the same model
//! leaf-tx's `TxAutoConfig` applies to transactions (and the in-memory peer of the
//! `leaf_redis::autoconfig::RedisAutoConfig` Redis-backed default).
//!
//! ## The `#[auto_config] impl` form (Spring's @AutoConfiguration + @Bean methods)
//!
//! `#[auto_config] impl CacheAutoConfig { #[bean(name = "cacheManager", provides =
//! "dyn CacheManager")] #[conditional(on_missing_bean(dyn CacheManager))] fn
//! cache_manager(&self) -> InMemoryCacheManager { .. } }` emits the SAME const artifacts a
//! hand-built auto-config would — the `AUTO_CONFIGS` [`Descriptor`] at `FALLBACK` (carrying
//! the `dyn CacheManager` provides[] view + the `"cacheManager"` declared name), its
//! [`ProviderSeed`] + `SEED_PAIRINGS` JOIN, and the `#[conditional]` guard + its
//! `GUARD_PAIRINGS` + `CONDITIONS` anchors — all keyed on the ONE contributed contract
//! (`module_path!()::cache_manager`). The holder [`CacheAutoConfig`] is a managed
//! `#[component]` (the `&self` receiver the `#[bean]` method reads — singleton-correct).
//!
//! ## The back-off contract
//!
//! Unlike the Redis cache auto-config (`leaf_redis::autoconfig::RedisAutoConfig`), this
//! carries NO `on_property` leaf — the in-memory default needs no feature flag, so it
//! participates whenever leaf-cache is linked. Its `OnMissingBean(dyn CacheManager)` is the
//! `provides[]`-aware VIEW back-off: it backs off when ANY `CacheManager` bean is already
//! present — the Redis-backed `RedisCacheManager`, or a user's hand-rolled manager of ANY
//! concrete type — because leaf-boot's `BuilderProbe` indexes each bean's `provides[]` view
//! TypeIds (the `dyn CacheManager` view), not just the concrete `self_type`. leaf-boot's
//! `run_autoconfig` evaluates the guard over the GROWING definition set in the Register
//! sub-pass.
//!
//! ## Ordering: the in-memory default registers LAST
//!
//! Because both this default and the specific cache auto-configs (redis's) now back off on
//! the SAME `dyn CacheManager` VIEW, the one that registers FIRST contributes the view and
//! wins; whoever runs later sees the view and backs off. So this in-memory default declares
//! a LATE [`OrderHint`] ([`CACHE_DEFAULT_ORDER`]) via the
//! [`AUTO_CONFIG_ORDERS`](leaf_core::AUTO_CONFIG_ORDERS) channel — Spring's
//! `@AutoConfigureAfter(...)` for the simple-cache default. It orders AFTER its peers
//! WITHOUT naming any (no leaf-redis coupling): a specific manager (redis, when enabled)
//! registers first and this in-memory default backs off to it; with no specific manager
//! active, this default registers and provides the `dyn CacheManager` view itself.

use leaf_core::{CondExpr, ContractId, Descriptor, OrderHint, OrderPairingRow, ProviderSeed};

use crate::manager::InMemoryCacheManager;

/// The declared name of the contributed `Arc<dyn CacheManager>` bean (Spring's
/// `cacheManager` identity preserved — the canonical name a `#[cacheable]` consumer
/// resolves by-type against, shared with any specific manager that supersedes it).
pub const CACHE_MANAGER_BEAN: &str = "cacheManager";

/// The LATE auto-config order for the in-memory default (Spring's `@AutoConfigureAfter`
/// for the simple-cache default): a large positive `i32` so this default sorts AFTER the
/// specific cache auto-configs (which keep [`OrderHint::DEFAULT`]'s `0`). It names no
/// peer — it only declares its own "register last" intent — so there is no leaf-redis
/// (or any other backend) coupling.
pub const CACHE_DEFAULT_ORDER: i32 = 10_000;

/// The stable contract path of the cache auto-config's contributed cache-manager bean —
/// the contract the `#[auto_config] impl` macro mints from the `module_path!()::<method>` of
/// the `cache_manager` `#[bean]` method (the ONE contract the `Descriptor`, the
/// `SeedPairingRow`, and the `GuardPairingRow` share).
pub const CACHE_MANAGER_CONTRACT: &str = "leaf_cache::autoconfig::cache_manager";

// ───────────────────────── the #[auto_config] holder ─────────────────────────

/// The auto-configuration HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` block below contributes the cache manager from a `#[bean]`
/// method that reads this holder as its `&self` receiver.
#[leaf_macros::component]
pub struct CacheAutoConfig;

impl CacheAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        CacheAutoConfig
    }
}

impl Default for CacheAutoConfig {
    fn default() -> Self {
        CacheAutoConfig::new()
    }
}

// ──────────────────── the @bean-method cache-manager contribution ─────────────

/// The `@Bean`-method contribution: `cache_manager()` produces the concrete
/// [`InMemoryCacheManager`] exposed as `dyn CacheManager` (the `provides[]` view), named
/// `"cacheManager"` (the canonical name, shared with any specific manager that supersedes
/// it — see [`CACHE_MANAGER_BEAN`]), into `AUTO_CONFIGS` at `FALLBACK`, gated by
/// `OnMissingBean(dyn CacheManager)` (the `provides[]`-aware VIEW back-off) and ordered
/// LAST among the cache auto-configs.
#[leaf_macros::auto_config]
impl CacheAutoConfig {
    /// Build the default in-memory cache manager (the factory body the macro calls). A real
    /// backend's manager (the Redis-backed `RedisCacheManager`, Caffeine, …) is an ordinary
    /// bean (or an earlier-ordered auto-config) that supersedes this `FALLBACK` default via
    /// the `dyn CacheManager` view; this is the safe in-memory stand-in so
    /// `#[cacheable(manager = InMemoryCacheManager)]` resolves with no hand-written wrapper
    /// bean.
    #[bean(name = "cacheManager", provides = "dyn ::leaf_core::CacheManager")]
    #[conditional(on_missing_bean(dyn ::leaf_core::CacheManager))]
    fn cache_manager(&self) -> InMemoryCacheManager {
        InMemoryCacheManager::new()
    }
}

// ───────────────────── the late auto-config ordering hint ─────────────────────

/// The auto-config-ordering hint for the in-memory default: a LATE [`OrderHint`] keyed on
/// the contributed contract, submitted into the
/// [`AUTO_CONFIG_ORDERS`](leaf_core::AUTO_CONFIG_ORDERS) channel. leaf-boot's
/// `collect_autoconfig_candidates` JOINs it by `contract` so `run_autoconfig` visits this
/// default AFTER the specific (redis) cache auto-configs — Spring's `@AutoConfigureAfter`
/// for the simple-cache default, with NO named-peer coupling.
#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIG_ORDERS)]
#[linkme(crate = ::leaf_core::linkme)]
pub static CACHE_DEFAULT_ORDER_PAIRING: OrderPairingRow = OrderPairingRow {
    contract: ContractId::of(CACHE_MANAGER_CONTRACT),
    order: OrderHint {
        order: CACHE_DEFAULT_ORDER,
        before: &[],
        after: &[],
        before_name: &[],
        after_name: &[],
    },
};

// ───────────────────────── thin compatibility aliases ────────────────────────
//
// The macro emits the contributed bean's `ProviderSeed` as `__leaf_seed_cache_manager` and
// its back-off guard as `__leaf_guard_cache_manager` (keyed off the METHOD ident), and
// submits the `Descriptor` into the `AUTO_CONFIGS` slice. These thin aliases preserve a
// stable public surface over the macro-emitted artifacts AND act as the anti-DCE anchors
// that path-reference the macro-emitted statics (so the row reaches the slice even under
// `--gc-sections`), mirroring leaf-redis's `REDIS_CACHE_MANAGER_SEED` /
// `REDIS_AUTO_CONFIG_GUARD` and leaf-tx's `TRANSACTION_MANAGER_SEED` / `TX_AUTO_CONFIG_GUARD`.

/// The const [`ProviderSeed`] leaf-boot's `run_autoconfig` invokes ONCE to mint the
/// manager's `Provider` (the macro-emitted seed).
pub const CACHE_MANAGER_SEED: ProviderSeed = __leaf_seed_cache_manager;

/// The const back-off guard for the default cache manager: it registers at `FALLBACK` IFF no
/// `dyn CacheManager` bean already exists (the macro-emitted `#[conditional]` guard tree
/// — a single `OnMissingBean(dyn CacheManager)` VIEW leaf, no `OnProperty`).
pub static CACHE_AUTO_CONFIG_GUARD: CondExpr = __leaf_guard_cache_manager;

/// The contributed `AUTO_CONFIGS` [`Descriptor`] for the default cache manager (looked up
/// from the macro-emitted `AUTO_CONFIGS` slice row by its contributed contract) — at
/// `FALLBACK`, on the SEPARATE auto-config channel, carrying the `dyn CacheManager` view.
#[must_use]
pub fn cache_manager_descriptor() -> Descriptor {
    *leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.contract == ContractId::of(CACHE_MANAGER_CONTRACT))
        .expect("the #[auto_config] cache_manager Descriptor must reach the AUTO_CONFIGS slice")
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_conditions::ConditionKind;
    use leaf_core::{CandidateRole, ResolveCtx};
    use std::any::TypeId;

    #[test]
    fn descriptor_is_a_fallback_auto_config_with_the_cache_manager_view() {
        let d = cache_manager_descriptor();
        assert_eq!(
            d.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user bean supersedes it"
        );
        assert_eq!(d.role, leaf_core::Role::Application);
        // The product is the CONCRETE InMemoryCacheManager (the method return type).
        assert_eq!(d.self_type, TypeId::of::<InMemoryCacheManager>());
        assert_eq!(d.declared_name, Some(CACHE_MANAGER_BEAN));
        // It provides the dyn CacheManager view (the advisor resolves
        // Arc<dyn CacheManager> through it).
        assert!(
            d.provides
                .iter()
                .any(|r| r.view == TypeId::of::<dyn leaf_core::CacheManager>()),
            "the auto-config must declare the dyn CacheManager view"
        );
    }

    #[test]
    fn the_auto_config_rides_the_separate_auto_configs_channel_not_components() {
        let contract = ContractId::of(CACHE_MANAGER_CONTRACT);
        let autos = leaf_core::collect_slice(&leaf_core::AUTO_CONFIGS);
        assert!(
            autos.iter().any(|r| r.contract == contract),
            "the default cache manager must be an AUTO_CONFIGS row"
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
        let contract = ContractId::of(CACHE_MANAGER_CONTRACT);
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == contract),
            "the AUTO_CONFIGS row must have a paired ProviderSeed on its contract"
        );
    }

    #[test]
    fn the_guard_is_a_bare_on_missing_bean_dyn_view_leaf() {
        // A single comma-less #[conditional(on_missing_bean(dyn ..))] lowers to a BARE
        // leaf (not wrapped in All([..]) the way the comma-separated redis form is),
        // and its `type` Attr targets the `dyn CacheManager` VIEW TypeId — so the
        // back-off fires when ANY CacheManager bean (any concrete type) is present.
        match &CACHE_AUTO_CONFIG_GUARD {
            CondExpr::Leaf(id, attrs) => {
                assert_eq!(
                    *id,
                    leaf_conditions::OnMissingBean::ID,
                    "the lone guard leaf is OnMissingBean"
                );
                let view = attrs
                    .iter()
                    .find_map(|a| match a {
                        leaf_core::Attr::Type("type", t) => Some(*t),
                        _ => None,
                    })
                    .expect("the OnMissingBean leaf carries a `type` Attr");
                assert_eq!(
                    view,
                    TypeId::of::<dyn leaf_core::CacheManager>(),
                    "the back-off targets the dyn CacheManager VIEW, not a concrete type"
                );
            }
            other => panic!("expected a bare OnMissingBean Leaf; got {other:?}"),
        }
    }

    #[test]
    fn the_default_declares_a_late_auto_config_order_on_its_contract() {
        // The in-memory default submits an AUTO_CONFIG_ORDERS row keyed on its
        // contributed contract carrying a LATE order (Spring's @AutoConfigureAfter for
        // the simple-cache default) — so run_autoconfig visits it AFTER the specific
        // (redis) cache auto-configs and it backs off to them via the dyn view.
        let contract = ContractId::of(CACHE_MANAGER_CONTRACT);
        let orders = leaf_core::collect_slice(&leaf_core::AUTO_CONFIG_ORDERS);
        let row = orders
            .iter()
            .find(|r| r.contract == contract)
            .expect("the default cache manager submits a late order pairing on its contract");
        assert_eq!(row.order.order, CACHE_DEFAULT_ORDER, "the declared order is LATE");
        assert!(
            row.order.order > leaf_core::OrderHint::DEFAULT.order,
            "a specific cache auto-config (OrderHint::DEFAULT) sorts before this default"
        );
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
        let contract = ContractId::of(CACHE_MANAGER_CONTRACT);
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
    fn the_provider_publishes_a_shared_cache_manager() {
        // The macro-emitted provider resolves the holder (the `&self` receiver) then calls
        // cache_manager() — so it needs the holder registered. Drive the real engine over
        // the holder + the contributed bean and assert the published bean is the concrete
        // InMemoryCacheManager, published Shared (a singleton).
        use leaf_core::CacheManager;
        let mut builder = leaf_core::RegistryBuilder::new();
        let holder = leaf_core::COMPONENTS
            .iter()
            .find(|d| d.declared_name == Some("cacheAutoConfig"))
            .copied()
            .expect("the holder is a COMPONENTS row");
        builder
            .register(holder, __leaf_seed_CacheAutoConfig())
            .expect("the holder registers");
        builder
            .register(cache_manager_descriptor(), CACHE_MANAGER_SEED())
            .expect("the contributed cache manager registers");
        let registry = builder.freeze().expect("freezes");
        let engine = leaf_core::Engine::new(registry);
        let mgr = futures::executor::block_on(engine.get::<InMemoryCacheManager>())
            .expect("the contributed InMemoryCacheManager resolves");
        // It is a working CacheManager (hands out a named cache via the trait ABI).
        assert!(
            CacheManager::cache(&*mgr, "prices").is_some(),
            "the published manager is a live CacheManager"
        );

        let provider = engine.registry().provider(
            engine
                .registry()
                .by_contract(ContractId::of(CACHE_MANAGER_CONTRACT))
                .unwrap(),
        );
        let cx = ResolveCtx::for_engine(&engine);
        let published = futures::executor::block_on(provider.provide(&cx)).expect("provides");
        assert!(published.is_shared(), "a singleton cache manager publishes Shared");
    }
}
