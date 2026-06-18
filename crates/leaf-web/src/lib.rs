//! `leaf-web` — the DI-native HTTP transport ABSTRACTIONS (Spring `spring-web`
//! analogue). This crate is the public web API: it defines the leaf web traits +
//! the request/response model, resting on `leaf-core` ONLY.
//!
//! It names NO HTTP-server library. hyper/tower/axum live exclusively in the
//! swappable `leaf-web-hyper` backend crate, which implements these traits at the
//! boundary (a mock backend proves the seam). For the neutral HTTP value
//! vocabulary — `Method` / `StatusCode` / `HeaderMap` / `Uri` — leaf-web reuses
//! the ecosystem-standard `http` crate (server-agnostic, not server internals).
//!
//! ## The pieces
//!
//! - [`Request`] / [`Response`] — leaf types wrapping the `http` primitives + a
//!   [`bytes::Bytes`] body the backend fills at the edge.
//! - [`IntoResponse`] — any handler return that can become a [`Response`]
//!   (`Response`/`StatusCode`/`&str`/`String`/`()`/`Result<T, E>`).
//!
//! Later stages add `Handler`/`Route` (matching), `WebFilter`/`Next`,
//! `FromRequest` extractors, `HttpMessageConverter`, `ControlAdvice`, and the
//! `WebServer`/`Dispatcher` — all backend-free, assembled from the container via
//! collection + by-trait injection.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod request;
pub mod response;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag
// in the link-collected `SOURCES` slice so a binary that lists leaf-web in its
// ExpectedManifest (the `web` capability bundle) can tell "linked-but-zero-rows"
// from "never-linked". The package name (dashes) is the join string.
leaf_core::declare_source!("leaf-web");

pub use request::Request;
pub use response::{IntoResponse, Response};
