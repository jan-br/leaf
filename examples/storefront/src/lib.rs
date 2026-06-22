//! The storefront's beans, as a LIBRARY crate — so they are linkable both by the thin
//! `main.rs` binary (which adds `#[leaf::main]` + the anti-DCE force-link) AND by the
//! integration tests under `tests/` (which boot the app and probe it over real HTTP).
//!
//! A binary-only crate's modules are invisible to its integration tests, and crucially
//! its bean `linkme` rows never reach a test binary's link graph — so the storefront's
//! domain + web beans live HERE, in the lib, where both targets link them.
//!
//! This stays umbrella-only: the ONLY dependency is `leaf` (plus `serde`, the JSON
//! data-format vocabulary a web app owns). The macro-emitted absolute
//! `::leaf_core::`/`::leaf_cache::`/`::leaf_tx::`/`::leaf_web::` paths resolve through the
//! facade SOURCE aliases below — the same aliases `#[leaf::main]` auto-emits at a binary
//! root, written here by hand because a library has no `#[leaf::main]` (mirroring the
//! `leaf_core`/`leaf_cache`/`leaf_tx`/`leaf_web` alias pattern the umbrella proofs use).

// The umbrella-only facade aliases: bind each concern-crate root the annotation macros
// emit absolute paths against (`::leaf_core::`/`::leaf_cache::`/`::leaf_tx::`/`::leaf_web::`)
// to the one `leaf` dependency. SOURCE aliases of the `leaf` dep, NOT new Cargo deps.
#[allow(unused_extern_crates)]
extern crate leaf as leaf_core;
#[allow(unused_extern_crates)]
extern crate leaf as leaf_cache;
#[allow(unused_extern_crates)]
extern crate leaf as leaf_tx;
#[cfg(feature = "web")]
#[allow(unused_extern_crates)]
extern crate leaf as leaf_web;
#[cfg(feature = "grpc")]
#[allow(unused_extern_crates)]
extern crate leaf as leaf_grpc;

pub mod catalog;
pub mod order;
pub mod platform;
pub mod pricing;
/// The HTTP REST surface (the `web` capability feature): `#[rest_controller]` endpoints
/// over the catalog + order services, an access-log `WebFilter`, and a `#[control_advice]`
/// error mapping. Present iff the `web` feature is enabled.
#[cfg(feature = "web")]
pub mod web;
/// The gRPC surface (the `grpc` capability feature): a `#[grpc_controller]` over the
/// catalog domain (the same `CatalogService`/`ProductRepository` the HTTP controllers use),
/// served over H2 on the SAME embedded server. Present iff the `grpc` feature is enabled.
#[cfg(feature = "grpc")]
pub mod grpc;
