//! `RedisAutoConfig` — the representative `#[auto_config]`-equivalent: an
//! `AUTO_CONFIGS` row at [`CandidateRole::FALLBACK`](leaf_core::CandidateRole)
//! contributing the Redis-backed `Arc<dyn CacheManager>` bean, guarded by a const
//! [`CondExpr`] (`OnProperty(leaf.redis.enabled)` AND
//! `OnMissingBean(RedisCacheManager)`).
//!
//! ## Why hand-built (not the `#[auto_config]` macro)
//!
//! The thin `#[auto_config]` macro emits a `Descriptor` whose provider yields
//! `Self` (the annotated struct), and whose `#[conditional]` guard can only
//! reference the struct's OWN self-type for `OnMissingBean`. A Spring
//! `@Configuration` contributes a *different* bean (here `Arc<dyn CacheManager>`)
//! from a `@Bean` method — which the v1 struct-only macro cannot express. So this
//! crate hand-writes the SAME const artifacts the macro/config-codegen emit (the
//! `AUTO_CONFIGS` `Descriptor` at `FALLBACK`, its `ProviderSeed`, the `Provider`,
//! the `SEED_PAIRINGS` JOIN, and the `GUARD_PAIRINGS` + `CONDITIONS` guard rows) —
//! proving the data-driven auto-config contract end to end without depending on the
//! macro crate. This IS the representative integration pattern: contribute DATA +
//! Providers, never an Engine/kernel strategy.
//!
//! ## The back-off contract
//!
//! The guard's `OnMissingBean(RedisCacheManager)` makes the auto-config back off
//! when a bean of that concrete type is already present (the soft override: a user
//! `CacheManager` supersedes it). leaf-boot's `run_autoconfig` evaluates the guard
//! over the GROWING definition set in the Register sub-pass; the `OnProperty` leaf
//! additionally requires `leaf.redis.enabled` to be present-and-not-`false`.
//!
//! NOTE (honest deferral): leaf-boot's `BuilderProbe` keys back-off on the
//! candidate's CONCRETE `self_type` (the in-process exact-match key), so the
//! `OnMissingBean` here matches a user bean of type `RedisCacheManager`. A user
//! `CacheManager` of a DIFFERENT concrete type (e.g. leaf-cache's
//! `InMemoryCacheManager`) does not yet trip this probe — distributed-by-`dyn`-view
//! back-off is a `provides[]`-aware-probe concern tracked for a later unit. The
//! `provides[]` row below already declares the `dyn CacheManager` view so consumers
//! resolve `Arc<dyn CacheManager>`.

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    Attr, BoxFuture, CondExpr, ConditionId, ContractId, Descriptor, LeafError, Published,
    ResolveCtx,
};

use crate::client::RedisClient;
use crate::manager::RedisCacheManager;
use crate::properties::{RedisProperties, ENABLED_PROPERTY};

/// The stable contract path of the Redis auto-config's contributed cache-manager
/// bean.
pub const REDIS_CACHE_MANAGER_CONTRACT: &str = "leaf_redis::redisCacheManager";

/// The declared name of the contributed `Arc<dyn CacheManager>` bean (Spring's
/// `cacheManager` identity preserved).
pub const REDIS_CACHE_MANAGER_BEAN: &str = "cacheManager";

// ───────────────────────── the const CondExpr guard ─────────────────────────

/// The canonical `OnProperty` condition-kind FQN — the SAME input leaf-conditions
/// mints its [`ConditionId`] from, so this leaf resolves to that runtime impl.
const ON_PROPERTY_FQN: &str = "leaf::condition::OnProperty";
/// The canonical `OnMissingBean` condition-kind FQN.
const ON_MISSING_BEAN_FQN: &str = "leaf::condition::OnMissingBean";

/// Mint a [`ConditionId`] from a kind FQN exactly as leaf-conditions does
/// (`contract_hash` truncated to the dense `u32` space) — reproducible cross-build.
const fn cond_id(fqn: &str) -> ConditionId {
    ConditionId(leaf_core::contract_hash(fqn) as u32)
}

/// The `OnProperty(leaf.redis.enabled)` leaf attrs. The runtime `OnProperty` reads
/// the `"name"` attr (multi-name = ALL must pass); present-and-not-`false` enables.
static ON_PROPERTY_ATTRS: &[Attr] = &[Attr::Str("name", ENABLED_PROPERTY)];

/// The `OnMissingBean(RedisCacheManager)` leaf attrs. The runtime `OnMissingBean`
/// probes the `"type"` attr against the growing definition set.
static ON_MISSING_BEAN_ATTRS: &[Attr] =
    &[Attr::Type("type", TypeId::of::<RedisCacheManager>())];

/// The two guard leaves: `OnProperty(leaf.redis.enabled)` AND
/// `OnMissingBean(RedisCacheManager)`.
static GUARD_LEAVES: &[CondExpr] = &[
    CondExpr::Leaf(cond_id(ON_PROPERTY_FQN), ON_PROPERTY_ATTRS),
    CondExpr::Leaf(cond_id(ON_MISSING_BEAN_FQN), ON_MISSING_BEAN_ATTRS),
];

/// The const back-off guard for `RedisAutoConfig`: it registers the Redis cache
/// manager at `FALLBACK` IFF `leaf.redis.enabled` is set AND no `RedisCacheManager`
/// bean already exists. An `OnBean`-family leaf (the `OnMissingBean`) defers the
/// whole guard to the Register sub-pass (it must see the growing set).
pub static REDIS_AUTO_CONFIG_GUARD: CondExpr = CondExpr::All(GUARD_LEAVES);

// The GUARD_PAIRINGS submission keyed by the auto-config's contract — so leaf-boot's
// condition routing JOINs the guard with NO hand-assembled `.with_guards`.
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::GUARD_PAIRINGS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_AUTO_CONFIG_GUARD_PAIRING: leaf_core::GuardPairingRow =
    leaf_core::GuardPairingRow {
        contract: ContractId::of(REDIS_CACHE_MANAGER_CONTRACT),
        guard: &REDIS_AUTO_CONFIG_GUARD,
    };

// The anti-DCE CONDITIONS anchors — one per referenced kind, keyed by the kind FQN
// (the same belt-and-suspenders anchor `#[conditional]` emits; the live impls are
// force-linked from leaf-conditions in the binary).
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::CONDITIONS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_AUTO_CONFIG_COND_ANCHOR_PROPERTY: leaf_core::ConditionRow =
    leaf_core::ConditionRow {
        contract: ContractId::of(ON_PROPERTY_FQN),
        marker: leaf_core::MarkerId::of(ON_PROPERTY_FQN),
    };

#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::CONDITIONS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_AUTO_CONFIG_COND_ANCHOR_MISSING_BEAN: leaf_core::ConditionRow =
    leaf_core::ConditionRow {
        contract: ContractId::of(ON_MISSING_BEAN_FQN),
        marker: leaf_core::MarkerId::of(ON_MISSING_BEAN_FQN),
    };

// ─────────────────────── the AUTO_CONFIGS Fallback row ───────────────────────

/// The flat const annotation table at [`CandidateRole::FALLBACK`](leaf_core::CandidateRole)
/// — the auto-config soft override (a user bean of the same type/contract wins).
static REDIS_CACHE_MANAGER_META: leaf_core::AnnotationMetadata = leaf_core::AnnotationMetadata {
    qualifiers: &[],
    markers: &[],
    depends_on: &[],
    candidate_role: leaf_core::CandidateRole::FALLBACK,
    autowire_candidate: true,
};

/// The `dyn CacheManager` injectable view this bean provides — so a consumer
/// resolving `Arc<dyn CacheManager>` finds it (the upcast is identity over the
/// erased `Arc`, like every macro-emitted `provides[]` row).
static REDIS_CACHE_MANAGER_PROVIDES: &[leaf_core::TypeRow] = &[leaf_core::TypeRow {
    view: TypeId::of::<dyn leaf_core::CacheManager>(),
    upcast: |bean: leaf_core::ErasedBean| -> leaf_core::ErasedBean { bean },
}];

/// The const `AUTO_CONFIGS` [`Descriptor`] for the Redis cache manager — at
/// `FALLBACK`, on the SEPARATE auto-config channel (so component-scanning over
/// `COMPONENTS` never picks it up), carrying the `dyn CacheManager` view.
pub const REDIS_CACHE_MANAGER_DESCRIPTOR: Descriptor = Descriptor {
    contract: ContractId::of(REDIS_CACHE_MANAGER_CONTRACT),
    self_type: TypeId::of::<RedisCacheManager>(),
    provides: REDIS_CACHE_MANAGER_PROVIDES,
    declared_name: Some(REDIS_CACHE_MANAGER_BEAN),
    aliases: &[],
    scope: leaf_core::ScopeDef::SINGLETON,
    role: leaf_core::Role::Application,
    meta: &REDIS_CACHE_MANAGER_META,
    parent: None,
    origin: leaf_core::Origin::Native { crate_name: Some("leaf-redis") },
};

// The link-time element: submit the const Descriptor into the SEPARATE AUTO_CONFIGS
// slice via the SAME `::leaf_core::linkme` path the config-codegen emits.
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_CACHE_MANAGER_ELEMENT: Descriptor = REDIS_CACHE_MANAGER_DESCRIPTOR;

/// The [`Provider`](leaf_core::Provider) that constructs the Redis cache manager:
/// it opens a (lazy) [`RedisClient`] from the resolved properties and publishes the
/// shared `RedisCacheManager` (resolvable as `Arc<dyn CacheManager>` via the view).
pub struct RedisCacheManagerProvider {
    descriptor: Descriptor,
    props: RedisProperties,
}

impl RedisCacheManagerProvider {
    /// Construct the provider with default properties (the seed builds this; the
    /// real env-bound props are threaded by a later config-bind step).
    #[must_use]
    pub fn new() -> Self {
        RedisCacheManagerProvider {
            descriptor: REDIS_CACHE_MANAGER_DESCRIPTOR,
            props: RedisProperties::default(),
        }
    }

    /// Construct the provider over explicit properties (the env-bound path).
    #[must_use]
    pub fn with_properties(props: RedisProperties) -> Self {
        RedisCacheManagerProvider { descriptor: REDIS_CACHE_MANAGER_DESCRIPTOR, props }
    }
}

impl Default for RedisCacheManagerProvider {
    fn default() -> Self {
        RedisCacheManagerProvider::new()
    }
}

impl leaf_core::Provider for RedisCacheManagerProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    fn provide<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<Published, LeafError>> {
        let props = self.props.clone();
        Box::pin(async move {
            let client = RedisClient::open(props)?;
            Ok(Published::shared_value(RedisCacheManager::new(client)))
        })
    }
}

/// The const [`ProviderSeed`](leaf_core::ProviderSeed) leaf-boot's `run_autoconfig`
/// invokes ONCE to mint the manager's `Provider` when the candidate survives the
/// back-off ladder.
pub const REDIS_CACHE_MANAGER_SEED: leaf_core::ProviderSeed =
    || Arc::new(RedisCacheManagerProvider::new());

/// The [`SeedPairingRow`](leaf_core::SeedPairingRow) JOINing the `AUTO_CONFIGS`
/// descriptor to its seed (the anti-DCE per-bean JOIN — an unconstructible
/// auto-config must never silently vanish).
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_CACHE_MANAGER_SEED_PAIRING: leaf_core::SeedPairingRow =
    leaf_core::SeedPairingRow {
        contract: ContractId::of(REDIS_CACHE_MANAGER_CONTRACT),
        seed: REDIS_CACHE_MANAGER_SEED,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{CandidateRole, ConditionKind, Provider};

    #[test]
    fn descriptor_is_a_fallback_auto_config_with_the_cache_manager_view() {
        let d = &REDIS_CACHE_MANAGER_DESCRIPTOR;
        assert_eq!(
            d.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user bean supersedes it"
        );
        assert_eq!(d.role, leaf_core::Role::Application);
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
        let autos = leaf_core::collect_slice(&leaf_core::AUTO_CONFIGS);
        assert!(
            autos.iter().any(|r| r.contract == ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)),
            "the Redis cache manager must be an AUTO_CONFIGS row"
        );
        let comps = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            !comps.iter().any(|r| r.contract == ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)),
            "the auto-config must NOT be in COMPONENTS (component-scanning never picks it up)"
        );
    }

    #[test]
    fn the_auto_config_descriptor_has_a_paired_seed() {
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)),
            "the AUTO_CONFIGS row must have a paired ProviderSeed (anti-DCE JOIN)"
        );
    }

    #[test]
    fn the_guard_is_property_and_missing_bean_gated() {
        // The guard tree is All([OnProperty, OnMissingBean]) — both leaves present.
        match &REDIS_AUTO_CONFIG_GUARD {
            CondExpr::All(children) => {
                assert_eq!(children.len(), 2, "OnProperty AND OnMissingBean");
                assert!(matches!(children[0], CondExpr::Leaf(id, _) if id == cond_id(ON_PROPERTY_FQN)));
                assert!(
                    matches!(children[1], CondExpr::Leaf(id, _) if id == cond_id(ON_MISSING_BEAN_FQN))
                );
            }
            other => panic!("expected All([..]); got {other:?}"),
        }
    }

    #[test]
    fn the_guard_leaf_ids_match_the_leaf_conditions_kind_ids() {
        // The hand-minted ids MUST equal leaf-conditions' ConditionKind::ID so the
        // leaves resolve to the runtime impls (the cross-crate ID contract).
        assert_eq!(cond_id(ON_PROPERTY_FQN), leaf_conditions::OnProperty::ID);
        assert_eq!(cond_id(ON_MISSING_BEAN_FQN), leaf_conditions::OnMissingBean::ID);
    }

    #[test]
    fn the_guard_defers_to_the_register_sub_pass_via_the_on_missing_bean_leaf() {
        // An OnBean-family leaf forces the whole guard to evaluate at Register (it
        // must see the growing definition set). The structural phase folds Parse over
        // bare leaves; leaf-conditions wraps the OnMissingBean leaf so the binary
        // defers it — here we assert the leaf id is the OnBean-family member.
        assert_eq!(
            leaf_conditions::OnMissingBean::SUB,
            leaf_core::SubPhase::Register,
            "OnMissingBean is a Register-sub-pass kind (sees the growing set)"
        );
    }

    #[test]
    fn the_guard_is_paired_and_anchored_in_the_slices() {
        let guards = leaf_core::collect_slice(&leaf_core::GUARD_PAIRINGS);
        assert!(
            guards.iter().any(|r| r.contract == ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)),
            "the guard must be paired by the auto-config's contract"
        );
        let conds = leaf_core::collect_slice(&leaf_core::CONDITIONS);
        assert!(
            conds.iter().any(|r| r.contract == ContractId::of(ON_PROPERTY_FQN)),
            "an OnProperty anti-DCE anchor must be present"
        );
        assert!(
            conds.iter().any(|r| r.contract == ContractId::of(ON_MISSING_BEAN_FQN)),
            "an OnMissingBean anti-DCE anchor must be present"
        );
    }

    #[test]
    fn the_provider_publishes_a_shared_cache_manager() {
        let p = RedisCacheManagerProvider::new();
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provides");
        assert!(published.is_shared(), "a singleton cache manager publishes Shared");
        let erased = published.into_shared().unwrap();
        assert!(
            erased.downcast::<RedisCacheManager>().is_ok(),
            "the published bean is the concrete RedisCacheManager"
        );
    }
}
