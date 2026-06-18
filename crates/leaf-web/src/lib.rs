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
//! - [`Handler`] / [`Route`] / [`RouteTable`] — the dispatch unit, the
//!   `(method, path-pattern)` registration the server collects, and the matcher
//!   that resolves a concrete request path to a route + captured params.
//! - [`WebFilter`] / [`Next`] / [`FilterChain`] / [`Terminal`] — the around-advice
//!   seam: ordered filters wrap the request, each continuing via `Next::run` or
//!   short-circuiting, with the [`Terminal`] (route dispatch) at the bottom.
//! - [`HttpMessageConverter`] — the content-negotiation seam: serialize a handler
//!   return into a body / deserialize a body into a typed value, keyed by
//!   content-type. The JSON impl is a `#[component]` bean in `leaf-serde`; leaf-web
//!   names no serde data format (only the `erased-serde` object-safety boundary).
//! - [`FromRequest`] + [`Path`] / [`Query`] / [`Json`] / [`Header`] / [`State`] —
//!   the argument-extraction seam: each controller-method parameter resolves from
//!   the [`Request`] via its STRUCTURAL extractor type (the codegen dispatches on
//!   shape, never a type name). `Path<String>`, `Query<HashMap>` and the whole-
//!   `Request` extractor land here; the serde-backed reads (`Json<T>` body,
//!   `Query<T>`) ride the injected `HttpMessageConverter` and the DI-collaborator
//!   `State<T>` rides the handler's captured ctx — both resolved by the controller
//!   codegen, so leaf-web names no serde format here.
//!
//! Later stages add `ControlAdvice` and the `WebServer`/`Dispatcher` — all
//! backend-free, assembled from the container via collection + by-trait injection.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod content;
pub mod extract;
pub mod filter;
pub mod handler;
pub mod request;
pub mod response;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag
// in the link-collected `SOURCES` slice so a binary that lists leaf-web in its
// ExpectedManifest (the `web` capability bundle) can tell "linked-but-zero-rows"
// from "never-linked". The package name (dashes) is the join string.
leaf_core::declare_source!("leaf-web");

pub use content::HttpMessageConverter;
pub use extract::{FromRequest, Header, Json, Path, Query, State};
pub use filter::{FilterChain, Next, Terminal, WebFilter};
pub use handler::{Handler, PathParams, Route, RouteMatch, RouteTable};
pub use request::Request;
pub use response::{IntoResponse, Response};
