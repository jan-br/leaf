//! `leaf-redis` — the REPRESENTATIVE ecosystem integration crate: the pattern every
//! binding follows (leaf-router, leaf-json, leaf-sqlx-tx, cache backends, …).
//!
//! TOPOLOGY (phase3/03): an integration crate depends on **leaf-core** (plus a
//! runtime/3rd-party lib — here `redis` + `leaf-tokio`), **never the umbrella**, and
//! it CONTRIBUTES DATA + Providers, **never an Engine impl or kernel strategy**. It
//! participates only when the binary force-links it (the `leaf-starter-redis`
//! two-gate activation), and it WIRES only when its runtime `CondExpr` guard matches
//! AND it loses to no user bean (the Fallback soft-override).
//!
//! ## What this crate ships
//!
//! - [`RedisAutoConfig`](autoconfig) — the `#[auto_config] impl` holder modelling
//!   Spring's `RedisAutoConfiguration`: its `#[bean]` `cache_manager` method
//!   contributes the Redis-backed cache manager (concrete `RedisCacheManager`, exposed
//!   as `dyn CacheManager`) into [`AUTO_CONFIGS`](leaf_core::AUTO_CONFIGS) as a
//!   [`Descriptor`](leaf_core::Descriptor) at
//!   [`CandidateRole::FALLBACK`](leaf_core::CandidateRole)
//!   ([`redis_cache_manager_descriptor`]), with its macro-emitted
//!   [`ProviderSeed`](leaf_core::ProviderSeed) ([`REDIS_CACHE_MANAGER_SEED`]) and the
//!   back-off guard [`REDIS_AUTO_CONFIG_GUARD`] = `OnProperty(leaf.redis.enabled)` AND
//!   `OnMissingBean(RedisCacheManager)`. leaf-boot's `run_autoconfig` runs the
//!   `exclude > back-off > default` ladder over it. The LIVE `RedisClient::open` socket
//!   factory stays hand-written inside the `#[bean]` body; only the const registration
//!   scaffolding is macro-emitted.
//! - [`RedisClient`] — the `Role::Infrastructure` connection-factory bean
//!   ([`REDIS_CLIENT_DESCRIPTOR`]), contributed into
//!   [`COMPONENTS`](leaf_core::COMPONENTS) the SAME way leaf-tokio contributes its
//!   `applicationTaskExecutor`.
//! - [`RedisCacheManager`] / [`RedisCache`] — the Redis-backed
//!   [`CacheManager`](leaf_core::CacheManager) bridging to leaf-cache's ABI.
//! - [`RedisProperties`] — the relaxed-bound `leaf.redis.*` connection config.
//!
//! ## Testing without a live server
//!
//! Every test here drives the WIRING (the AUTO_CONFIGS/COMPONENTS rows, the seed
//! pairings, the guard tree, the back-off ladder via leaf-boot's `run_autoconfig`,
//! the Provider shapes, the `Cache`/`CacheManager` ABI in-process) — NONE touch the
//! network. The single live-Redis round-trip is gated behind the `live-redis`
//! feature (see [`live`]) and `#[ignore]`d, with a clear note.
//!
//! ## Honest deferrals (NOTE)
//!
//! - **Value serialization.** leaf-core's `StoredValue` is `Box<dyn Any>` (no serde
//!   bound), so a typed value cannot yet round-trip THROUGH a Redis socket. The
//!   manager serves the typed value round-trip in-process while the backend owns the
//!   namespaced durable key set; the value-bytes serialization is a `leaf-serde`
//!   concern, deferred (see [`manager`]).
//! - **`dyn`-view back-off.** leaf-boot's `BuilderProbe` keys `OnMissingBean` on the
//!   candidate's concrete `self_type`, so the auto-config backs off against a user
//!   `RedisCacheManager`, not yet against an arbitrary `dyn CacheManager` of a
//!   different concrete type (a `provides[]`-aware-probe concern; see [`autoconfig`]).
//! - **Env-bound props at construction.** The provider/seed open with default
//!   `RedisProperties`; threading the env-bound props into the seed is the config-bind
//!   step the binary supplies (the `RedisProperties::from_env` projection is ready).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod autoconfig;
pub mod client;
pub mod live;
pub mod manager;
pub mod properties;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag in
// the link-collected `SOURCES` slice so the binary's expected-vs-found self-check
// can tell "linked-but-zero-rows" from "never-linked". A force-linked-but-zero-
// contributing leaf-redis (its AUTO_CONFIGS row GC'd) becomes a loud `SourceVanished`
// naming it rather than a silent missing auto-config. The package name (dashes) is
// the author-stable string the umbrella's ExpectedManifest joins on.
leaf_core::declare_source!("leaf-redis");

// ── the flat integration surface ──

pub use autoconfig::{
    redis_cache_manager_descriptor, RedisAutoConfig, REDIS_AUTO_CONFIG_GUARD,
    REDIS_CACHE_MANAGER_BEAN, REDIS_CACHE_MANAGER_CONTRACT, REDIS_CACHE_MANAGER_SEED,
};
pub use client::{
    RedisClient, RedisClientProvider, REDIS_CLIENT_BEAN, REDIS_CLIENT_CONTRACT,
    REDIS_CLIENT_DESCRIPTOR, REDIS_CLIENT_SEED,
};
pub use manager::{RedisCache, RedisCacheManager};
pub use properties::{
    RedisProperties, DEFAULT_URL, ENABLED_PROPERTY, KEY_PREFIX_PROPERTY, PREFIX, URL_PROPERTY,
};
