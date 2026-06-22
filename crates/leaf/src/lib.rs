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
//! - **Dep-hidden capability features** (`Cargo.toml`): `redis` → `dep:leaf-starter-redis`,
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
//!   its `linkme` rows survive Layer-0 DCE) + the const [`expected_manifest`](crate::forcelink::expected_manifest) over
//!   the enabled participating set (the expected-vs-found self-check anchor).
//! - **The default runtime** ([`runtime`]). The base always pulls leaf-tokio as the
//!   default [`ExecutionFacility`]; [`bootstrap`](fn@bootstrap)
//!   installs its ambient store + supplies its [`Spawner`] to a
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
// needs each concern-crate root to be a SOURCE alias of the one `leaf` dep:
//
// ```ignore
// extern crate leaf as leaf_core;   // ::leaf_core::X  → leaf::X (re-exported below)
// extern crate leaf as leaf_cache;  // ::leaf_cache::X → leaf::X (the #[cacheable] root)
// extern crate leaf as leaf_tx;     // ::leaf_tx::X    → leaf::X (the #[transactional] root)
// ```
//
// The user does NOT write these: `#[leaf::main]` AUTO-EMITS them from the binary crate
// root (alongside its `force_link!()` anchor — see leaf_codegen::emit_main), so the
// blessed-path entry is ONLY `use leaf::prelude::*;` + beans + `#[leaf::main]`. Crate-
// root `extern crate` is visible crate-wide, so every module's macro-emitted
// `::leaf_core::` path resolves. `extern crate <dep> as <name>` is a SOURCE alias of an
// existing direct dependency (the `leaf` umbrella), NOT a new Cargo dependency — so the
// downstream still names exactly ONE `leaf` dependency. For each alias to satisfy the
// macro paths, the umbrella re-exports the macro-referenced surface of each crate AT ITS
// ROOT (below): `leaf::Descriptor` (= leaf-core's), `leaf::linkme`, `leaf::CacheOp`,
// `leaf::TxPointcut`, … — so `::leaf_core::Descriptor` resolves to `leaf::Descriptor`,
// etc. This is the maximal-magic umbrella-only DX completed. (A non-`#[leaf::main]`
// entry — a hand-written `#[tokio::main]` — still writes the three aliases by hand.)
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
    build_cache_interceptor, build_cache_interceptor_view, cache_order_key, unit_key_fn,
    CacheKeyFn, CacheOp, CachePointcut, CacheRule, InMemoryCacheManager,
};
#[doc(hidden)]
pub use leaf_tx::{
    make_transaction_interceptor_for, make_transaction_interceptor_for_view, tx_order_key,
    InMemoryTransactionManager, TxPointcut,
};

// The leaf-web macro surface the `#[controller]`/`#[rest_controller]`/`#[control_advice]`
// annotations emit `::leaf_web::` paths into — re-exported AT THE UMBRELLA ROOT so the
// facade alias `extern crate leaf as leaf_web;` (auto-emitted by `#[leaf::main]`) resolves
// `::leaf_web::Route` to `leaf::Route`, exactly like the `leaf_cache`/`leaf_tx` aliases.
// Only the EXACT symbols the web macros reference are re-exported here (an explicit list,
// not a glob — `leaf-web`'s `Next`/`Request`/… share names with other crates a glob could
// clash on); the broader user-facing web surface is reached via the `web` prelude block.
// Present iff the `web` capability feature pulled the bundle in.
#[cfg(feature = "web")]
#[doc(hidden)]
pub use leaf_starter_web::leaf_web::{
    ControlAdvice, ControllerKind, ExtractCtx, FromRequest, FromRequestParts, Handler,
    HttpMessageConverter, IntoResponse, IntoResponseWith, Next, Request, Response, ResponseEntity,
    Route, WebFilter,
};
// The neutral HTTP value vocabulary leaf-web re-exports — re-exported AT THE UMBRELLA ROOT
// too so the controller codegen's `::leaf_web::http::Method::GET` verb token (+ the
// `CONTENT_TYPE` header) resolves through the `leaf_web` facade alias (`::leaf_web::http`
// → `leaf::http`). An umbrella-only app reaches `http` through the one `leaf` dep — never
// naming `http` directly — exactly as `::leaf_core::`/`::leaf_cache::` route their external
// types through a leaf-* crate the alias points at.
#[cfg(feature = "web")]
#[doc(hidden)]
pub use leaf_starter_web::leaf_web::http;

// The leaf-grpc macro surface the #[grpc_controller] codegen emits `::leaf_grpc::` paths
// into — re-exported AT THE UMBRELLA ROOT so the facade alias `extern crate leaf as
// leaf_grpc;` resolves `::leaf_grpc::GrpcRoute` to `leaf::GrpcRoute`, exactly like the
// `leaf_web` re-exports. Only the EXACT symbols the macro references are listed: the
// `GrpcRoute`/`GrpcHandler`/`GrpcControllerKind` registration traits, the disjoint
// `GrpcRecv`/`GrpcSend` framing seams trait resolution picks the call shape from, the
// concrete `ProstCodec` the route field-injects, the `MethodDescriptor` the `path()` const
// reads, plus the user-facing `Code`/`Status`/`Streaming`/`GrpcStatusMapper`/`GrpcCodec`/
// `decode_frames`/`encode_frame`. Present iff the `grpc` capability feature pulled the
// bundle in.
#[cfg(feature = "grpc")]
#[doc(hidden)]
pub use leaf_starter_grpc::leaf_grpc::{
    decode_frames, encode_frame, CallShape, Code, GrpcCodec, GrpcControllerKind, GrpcDispatch,
    GrpcHandler, GrpcRecv, GrpcRoute, GrpcSend, GrpcStatusMapper, MethodDescriptor, ProstCodec,
    Status, Streaming, REFLECTED_FILE_DESCRIPTOR_SETS,
};
// The prost message-codec surface the `leaf-grpc-build`-generated message structs emit
// absolute `::prost::` paths against — re-exported AT THE UMBRELLA ROOT so an umbrella-only
// app's `extern crate leaf as prost;` facade alias resolves `::prost::Message` to
// `leaf::Message` (and `::prost::alloc::*` to `leaf::alloc::*`), exactly like the
// `leaf_web`/`http` root re-exports. `Message` is BOTH the codec trait and its `#[derive]`
// macro (they share the name across namespaces); `alloc` is prost's re-export of the `alloc`
// crate the generated `String`/`Vec` types live in. Only the EXACT symbols the generated
// message code references. Present iff the `grpc` capability feature pulled the bundle in.
#[cfg(feature = "grpc")]
#[doc(hidden)]
pub use leaf_starter_grpc::leaf_grpc::prost::{
    alloc, bytes, encoding, DecodeError, EncodeError, Enumeration, Message, Name, Oneof,
    UnknownEnumValue,
};
// The `dispatch` module the route codegen names (`::leaf_grpc::dispatch::status_response` —
// the early-return when an inbound frame is a malformed `Status`). Re-exported AT THE ROOT
// so the facade alias resolves `::leaf_grpc::dispatch` to `leaf::dispatch`, like `leaf::http`.
#[cfg(feature = "grpc")]
#[doc(hidden)]
pub use leaf_starter_grpc::leaf_grpc::dispatch;
// The `map_first` mapper-chain fold + `LeafError`-mapping helper the dogfood controller calls
// to render a raised domain error through the collection-injected GrpcStatusMapper chain
// (the gRPC ControlAdvice analogue). Reached as `leaf::grpc::leaf_grpc::map_first` too, but
// re-exported here so the facade `::leaf_grpc::map_first` resolves.
#[cfg(feature = "grpc")]
#[doc(hidden)]
pub use leaf_starter_grpc::leaf_grpc::map_first;

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

/// `#[leaf::async_impl]` — write `async fn` in a trait `impl`; desugars each to the
/// `BoxFuture` form leaf's `dyn` traits require (no visible `Box::pin`/lifetimes). Also
/// in the prelude.
#[doc(no_inline)]
pub use leaf_macros::async_impl;

/// `#[grpc_controller]` — the gRPC controller-family stereotype (present iff the `grpc`
/// capability feature is enabled). Re-exported so the canonical `#[grpc_controller]`
/// spelling resolves; the prelude also brings it into scope under the `grpc`-gated glob.
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_macros::grpc_controller;

/// The default async runtime (`tokio`), re-exported so an umbrella-only app can build
/// the executor it owns (`leaf::tokio::runtime::Builder`) or drive a `block_on` test
/// without naming `tokio` as a direct dependency — the umbrella provides the runtime.
#[doc(hidden)]
pub use tokio;

/// The default tokio runtime integration (`leaf-tokio`): the
/// [`ExecutionFacility`] /
/// [`AmbientStore`] impls the base always pulls.
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

/// The gRPC STACK starter (present iff the `grpc` feature is enabled). The `dep:`-hidden
/// edge pulls the gRPC bundle into the participating set; reached as `leaf::grpc`.
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_starter_grpc as grpc;

// ── the bootstrap bridge (the default-runtime run entry) ──

pub use runtime::{bootstrap, run_main, RunInputs};

/// The error spine ([`leaf_core::LeafError`]), re-exported at the crate root so a
/// downstream names it as `leaf::LeafError` (the `#[leaf::main]` return type) without
/// reaching through the prelude or `leaf::core`.
#[doc(no_inline)]
pub use leaf_core::LeafError;
