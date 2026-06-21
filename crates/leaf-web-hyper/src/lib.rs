//! `leaf-web-hyper` — the hyper HTTP-server BACKEND for the leaf-web abstractions.
//!
//! This is the ONE crate in the workspace allowed to name hyper/tower/axum. It
//! implements the backend-free leaf-web [`WebServer`](leaf_web::WebServer) trait at
//! the HTTP transport edge: it binds a socket, accepts connections on the leaf-tokio
//! runtime, converts hyper's native `Request`/`Response` to/from the leaf
//! [`Request`](leaf_web::Request)/[`Response`](leaf_web::Response), and drives every
//! request through the shared [`Dispatcher`](leaf_web::Dispatcher) — so nothing above
//! this boundary ever sees hyper.
//!
//! [`HyperServer`] is the swappable default backend; a mock backend
//! (`leaf_web::MockServer`) implements the SAME trait with no transport, proving the
//! abstraction is genuinely backend-free.
//!
//! [`HyperServerAutoConfig`] contributes [`HyperServer`] as the DEFAULT `dyn WebServer` bean
//! — an `#[auto_config]` `FALLBACK` row gated by `OnMissingBean(dyn WebServer)`, so simply
//! linking this crate makes an app serve, while a different backend (or a user `WebServer`)
//! supersedes it by providing the same `dyn ::leaf_web::WebServer` view. The leaf-web
//! [`EmbeddedWebServer`](leaf_web::EmbeddedWebServer) injects whichever server won and serves.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod autoconfig;
pub mod server;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag in the
// link-collected `SOURCES` slice so a binary that lists leaf-web-hyper in its
// ExpectedManifest can tell "linked-but-zero-rows" from "never-linked". The package
// name (dashes) is the author-stable join string.
leaf_core::declare_source!("leaf-web-hyper");

pub use autoconfig::HyperServerAutoConfig;
pub use server::HyperServer;
