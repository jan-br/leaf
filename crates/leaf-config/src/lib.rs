//! `leaf-config` — the config-data engine over leaf-core's `Env`/`PropertySource`/
//! `PropertyResolver` ABI (environment-config phase3/06).
//!
//! This crate is the CONCRETE value-shape engine that fills leaf-core's frozen
//! environment ABI. leaf-core owns the read seam ([`leaf_core::Env`] /
//! [`leaf_core::PropertyResolver`]), the raw source trait
//! ([`leaf_core::PropertySource`] / [`leaf_core::MapPropertySource`]), the mutate
//! vocabulary ([`leaf_core::EnvBuilder`]), the relaxed-binding uniform fold
//! ([`leaf_core::uniform_key`] / [`leaf_core::env_var_to_canonical`]), the
//! `${...}` scanner ([`leaf_core::resolve_lenient`] /
//! [`leaf_core::resolve_strict`]), and the coercion seam
//! ([`leaf_core::FromConfigValue`]). This crate reuses ALL of those — it redefines
//! nothing — and adds:
//!
//! - the [`ConfigDataLoader`] SPI + a synchronous [`SyncConfigDataLoader`] facet,
//!   with the concrete [`JsonLoader`] (serde_json), [`YamlLoader`] (yaml-rust2,
//!   the maintained YAML 1.2 parser — `serde_yaml`/`serde_yml`/`libyml` are all
//!   deprecated), [`ConfigTreeLoader`] (`configtree:`), and [`EnvVarLoader`]
//!   (`env:`) format loaders;
//! - the ONE canonical key-segment [`flatten`](crate::flatten::flatten) shared by
//!   JSON + YAML (depth-first, `[index]` array segments, null-as-absent);
//! - the declarative [`PrecedenceRung`] ladder + the [`Contribution`] /
//!   [`ConfigDataPlan`] plan model and its stable comparator;
//! - the [`plan_sync`] / [`apply`] plan-then-apply engine (the async/sync
//!   bisection: IO holds no stack borrow, the fold holds no `.await`);
//! - the [`PlaceholderEngine`] — `${...}` resolution over the layered stack,
//!   delegating typed coercion to [`leaf_core::FromConfigValue`].
//!
//! ## Honest deferrals to leaf-boot
//!
//! Per environment-config phase3/06, the WHOLE pass is the body of
//! `App<Define>::seal_environment().await` — which lives in leaf-boot. So:
//! - the genuinely-async planner (remote sources, the `spring.config.import`
//!   worklist traversal, `CondExpr` document-activation filtering against the
//!   frozen [`leaf_core::ActiveProfiles`]) is leaf-boot's; this crate provides
//!   the deterministic LOCAL planner ([`plan_sync`]) + the pure applier.
//!   NOTE: the activation `IllegalActivationDocument` hard-rule and import-cycle
//!   idempotency are document-activation concerns layered THERE.
//! - the interned [`leaf_core::OriginStore`] / [`leaf_core::OriginId`] dense
//!   layer now lives in the leaf-core ABI, and the FILE loaders (JSON/YAML)
//!   stamp a fine `Origin::File { path, line }` per value: YAML carries the
//!   precise source line (via yaml-rust2's `Marker`), JSON degrades gracefully
//!   to a path-only file origin (no per-value line). The env/config-tree loaders
//!   keep the coarse [`leaf_core::Origin::Native`] carrier (no line to stamp).
//!   FOLLOW-UP: threading the load-local `OriginStore` to the error RENDERER
//!   (so a config error resolves a value's `OriginId` to its file:line) is a
//!   small leaf-boot `seal_environment` hook, deferred to keep this unit local.
//! - cloud-platform detection / SAJ transport population / the `@PropertySource`
//!   `App<Resolve>` contribution step are leaf-boot's (they sequence around this
//!   crate's loaders + rungs).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod configdata;
pub mod engine;
pub mod error;
pub mod flatten;
pub mod loader;
pub mod placeholder_engine;
pub mod precedence;

// ── curated re-exports: the flat config-engine surface ──

pub use configdata::{
    illegal_activation_error, illegal_activation_key, is_document_active, DocControl,
    ACTIVATE_ON_PROFILE, IMPORT_KEY, RESERVED_ACTIVATION_KEYS,
};

pub use error::{ConfigDataError, ConfigDataErrorKind, ConfigDataLocation, LocationScheme};

pub use flatten::{flatten, FlatEntry, Node, OriginSpec};

pub use loader::{
    ConfigDataLoader, ConfigTreeLoader, EnvVarLoader, JsonLoader, LoadCtx, LoadedDocument,
    SyncConfigDataLoader, YamlLoader,
};

pub use precedence::{ConfigDataPlan, Contribution, PrecedenceRung};

pub use engine::{apply, plan_sync, PlanItem};

pub use placeholder_engine::{LayeredLookup, PlaceholderEngine};
