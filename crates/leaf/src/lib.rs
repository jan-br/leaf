//! `leaf` — the UMBRELLA / facade crate: the BOM coordination point + the single
//! dependency a downstream application names (phase3 TOPOLOGY "Starters & BOM").
//!
//! A real leaf app adds ONE dependency — `leaf = { version = "…", features =
//! ["redis", "web"] }` — writes `use leaf::prelude::*;`, and has the whole
//! framework: the annotation macros, the kernel handle/error/run types, and (per
//! enabled capability feature) the wired integration crates. This crate is the
//! unique DAG SINK: it depends on the starters + the optional integration set, and
//! NOTHING depends on it (wiring the other way would be a hard Cargo cycle).
//!
//! ## What this crate owns
//!
//! - **The dual BOM downstream half.** The version-pinned umbrella IS the BOM
//!   surrogate — picking ONE umbrella version transitively pins the aligned set of
//!   every starter/integration crate it re-exports (Cargo has no native BOM, so the
//!   internal `[workspace.dependencies]` half + this version-pinned umbrella are the
//!   two halves).
//! - **Dep-hidden capability features** ([`Cargo.toml`]): `redis` → `dep:leaf-starter-redis`,
//!   `web` → `dep:leaf-starter-web`. The feature names are the capability vocabulary;
//!   the `dep:` prefix hides the optional crate so the ONLY public names are the
//!   capabilities. Enabling a feature pulls the starter (→ its integration crate +
//!   runtime peer) into the force-link / `ExpectedManifest` PARTICIPATING SET.
//! - **The [`prelude`].** `use leaf::prelude::*;` re-exports every annotation macro
//!   plus the core types a user needs (the handle currency, the run engine, the
//!   error spine, the advice-manager traits).
//! - **The force-link + `ExpectedManifest` seam** ([`forcelink`]). The umbrella owns
//!   the binary-crate anti-DCE codegen: the [`force_link!`] macro the app's `main`
//!   invokes (so an enabled capability's integration crate is path-referenced and
//!   its `linkme` rows survive Layer-0 DCE) + the const [`expected_manifest`] over
//!   the enabled participating set (the expected-vs-found self-check anchor).
//! - **The default runtime** ([`runtime`]). The base always pulls leaf-tokio as the
//!   default [`ExecutionFacility`](leaf_core::ExecutionFacility); [`bootstrap`]
//!   installs its ambient store + supplies its [`Spawner`](leaf_core::Spawner) to a
//!   ready-to-run [`Application`](leaf_boot::Application).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod forcelink;
pub mod prelude;
pub mod runtime;

// ── the FACADE-PATH re-export surface (`__rexport`) ──
//
// The annotation macros emit ABSOLUTE crate-root paths (`::leaf_core::…`,
// `::leaf_cache::…`, `::leaf_tx::…`) — the single-kernel-invariant thin-macro rule
// (TOOLKIT §discovery). An absolute `::crate` path resolves ONLY against the
// consuming crate's EXTERN PRELUDE (its direct Cargo deps), which a re-export cannot
// inject. So an umbrella-ONLY downstream (one `leaf` dependency, the blessed path)
// makes those roots resolve with a one-line-per-root source alias in its crate root:
//
// ```ignore
// extern crate leaf as leaf_core;   // ::leaf_core::X  → leaf::X (re-exported below)
// extern crate leaf as leaf_cache;  // ::leaf_cache::X → leaf::X (the #[cacheable] root)
// extern crate leaf as leaf_tx;     // ::leaf_tx::X    → leaf::X (the #[transactional] root)
// ```
//
// `extern crate <dep> as <name>` is a SOURCE alias of an existing direct dependency
// (the `leaf` umbrella), NOT a new Cargo dependency — so the downstream still names
// exactly ONE `leaf` dependency. For each alias to satisfy the macro paths, the
// umbrella re-exports the macro-referenced surface of each crate AT ITS ROOT (below):
// `leaf::Descriptor` (= leaf-core's), `leaf::linkme`, `leaf::CacheOp`,
// `leaf::TxPointcut`, … — so `::leaf_core::Descriptor` resolves to `leaf::Descriptor`,
// etc. This is the maximal-magic umbrella-only DX completed.
#[doc(hidden)]
pub use leaf_core::*;

// The cross-cutting-concern crates the natural `#[transactional]` / `#[cacheable]`
// annotations emit `::leaf_tx::` / `::leaf_cache::` paths into. They are leaf-core-only
// crates; the umbrella always carries them so the declarative-advice macros (whose
// markers are in the prelude) actually wire from an umbrella-only app. Only the exact
// symbols the concern macros emit are re-exported at the root (an explicit list, not a
// glob — the two crates share `advisor`/`interceptor`/`manager` MODULE names a glob
// would clash on), so the `extern crate leaf as leaf_cache;` / `as leaf_tx;` aliases
// resolve `::leaf_cache::X` / `::leaf_tx::X` to `leaf::X`.
#[doc(hidden)]
pub use leaf_cache::{
    build_cache_interceptor, cache_order_key, unit_key_fn, CacheKeyFn, CacheOp, CachePointcut,
    CacheRule, InMemoryCacheManager,
};
#[doc(hidden)]
pub use leaf_tx::{
    make_transaction_interceptor_for, tx_order_key, InMemoryTransactionManager, TxPointcut,
};

// ── flat re-exports of the three foundation crates (so `leaf::core`, `leaf::boot`,
// `leaf::macros` reach the full surface without naming the hyphenated crates) ──

/// The kernel ABI crate (the ultra-stable single-copy `leaf-core`): the handle
/// currency, the registry, the injection spine, the error model, the linkme
/// discovery channels. Re-exported so a downstream reaches it as `leaf::core`.
#[doc(no_inline)]
pub use leaf_core as core;

/// The bootstrap / assembly + run-engine crate (`leaf-boot`): the `App<…>`
/// typestate, the `Application` run pipeline, the anti-DCE self-check.
#[doc(no_inline)]
pub use leaf_boot as boot;

/// The thin proc-macro crate (`leaf-macros`): every annotation macro. Re-exported
/// so the macros are reachable as `leaf::macros::component` etc.; the [`prelude`]
/// brings them into scope flat.
#[doc(no_inline)]
pub use leaf_macros as macros;

/// `#[leaf::main]` — the binary-crate entrypoint macro, re-exported at the crate root
/// so the canonical `#[leaf::main]` spelling resolves (the prelude also brings it into
/// scope under the glob).
#[doc(no_inline)]
pub use leaf_macros::main;

/// The default async runtime (`tokio`), re-exported so an umbrella-only app can build
/// the executor it owns (`leaf::tokio::runtime::Builder`) or drive a `block_on` test
/// without naming `tokio` as a direct dependency — the umbrella provides the runtime.
#[doc(hidden)]
pub use tokio;

/// The default tokio runtime integration (`leaf-tokio`): the
/// [`ExecutionFacility`](leaf_core::ExecutionFacility) /
/// [`AmbientStore`](leaf_core::AmbientStore) impls the base always pulls.
#[doc(no_inline)]
pub use leaf_tokio as tokio_runtime;

// ── the enabled capability starters (dep-hidden; present iff the feature is on) ──

/// The Redis CAPABILITY starter (present iff the `redis` feature is enabled). The
/// `dep:`-hidden dependency edge pulls `leaf-redis` + its tokio runtime peer into
/// the participating set; this re-export lets a downstream reach the capability's
/// surface as `leaf::redis` without naming the starter crate.
#[cfg(feature = "redis")]
#[doc(no_inline)]
pub use leaf_starter_redis as redis;

/// The web STACK starter (present iff the `web` feature is enabled). The
/// `dep:`-hidden dependency edge pulls the curated web bundle into the participating
/// set; this re-export lets a downstream reach the stack as `leaf::web`.
#[cfg(feature = "web")]
#[doc(no_inline)]
pub use leaf_starter_web as web;

// ── the bootstrap bridge (the default-runtime run entry) ──

pub use runtime::{bootstrap, run_main, RunInputs};

/// The error spine ([`leaf_core::LeafError`]), re-exported at the crate root so a
/// downstream names it as `leaf::LeafError` (the `#[leaf::main]` return type) without
/// reaching through the prelude or `leaf::core`.
#[doc(no_inline)]
pub use leaf_core::LeafError;
