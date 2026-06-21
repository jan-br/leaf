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
//! - [`HttpMessageConverter`] — the content-type seam: serialize a handler
//!   return into a body / deserialize a body into a typed value, keyed by
//!   content-type. A SINGLE converter is wired today (the JSON impl, a `#[component]`
//!   bean in `leaf-serde`); Accept-based negotiation among several converters is
//!   deferred until a second converter is contributed. leaf-web names no serde data
//!   format (only the `erased-serde` object-safety boundary).
//! - [`FromRequest`] + [`Path`] / [`Query`] / [`Json`] / [`Header`] / [`Extension`] —
//!   the argument-extraction seam: each controller-method parameter resolves from
//!   the [`Request`] via its STRUCTURAL extractor type (the codegen dispatches on
//!   shape, never a type name). `Path<String>`, `Query<HashMap>`, the named
//!   `Header<T>` (its header name from a `#[header("X-Foo")]` attribute), the typed
//!   per-request `Extension<T>` (a value an upstream `WebFilter` attached) and the whole-
//!   `Request` extractor land here; the serde-backed reads (`Json<T>` body, `Query<T>`)
//!   ride the injected `HttpMessageConverter` — resolved by the controller codegen, so
//!   leaf-web names no serde format here. Handler-side DI collaborators are NOT an
//!   argument extractor: a controller field-injects them (`Ref<CatalogService>`), the
//!   sanctioned leaf way — there is no `State<T>`.
//!
//! - [`ControlAdvice`] — the global error-handling seam (Spring's
//!   `@ControllerAdvice`/`@ExceptionHandler`): an ordered chain that maps a
//!   `LeafError` (from a handler/filter/extractor) to a [`Response`], first-match
//!   wins, with a built-in default mapping as the floor.
//! - [`ServerProperties`] / [`WebServer`] / [`Dispatcher`] — the server seam: the
//!   bound address, the pluggable embedded-server bean, and the protocol-agnostic
//!   request engine. The [`Dispatcher`] runs the filter chain → matches a route →
//!   invokes its handler → maps errors via the advice chain, and NEVER errors out
//!   (every failure becomes a response). It is backend-free: a backend converts its
//!   native request to a [`Request`], calls [`Dispatcher::dispatch`], and writes the
//!   [`Response`].
//!
//! All of it is backend-free, assembled from the container via collection +
//! by-trait injection (`Vec<Ref<dyn Route>>` / `Vec<Ref<dyn WebFilter>>` /
//! `Vec<Ref<dyn ControlAdvice>>`). Each concern trait (`Route`/`WebFilter`/
//! `ControlAdvice`/`HttpMessageConverter`) is published as an injectable `dyn` VIEW via
//! `leaf_core::impl_resolve_view!` (the by-trait-injection seam emitted once per trait),
//! so a bean providing the view is resolvable as `Ref<dyn _>` and collectible as
//! `Vec<Ref<dyn _>>` through the SAME path a concrete `Ref<T>` uses.
//!
//! - `testing::MockServer` (the `testing` feature / `cfg(test)`) — the in-memory
//!   pluggability proof: a [`WebServer`] that drives a [`Request`] straight through the
//!   shared [`Dispatcher`] with NO transport, proving the abstraction is backend-free.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod advice;
pub mod body;
pub mod content;
pub mod embedded;
pub mod extract;
pub mod filter;
pub mod handler;
pub mod request;
pub mod response;
pub mod server;

// The in-memory `MockServer` backend (the Stage-1 pluggability proof). A TEST harness,
// gated behind `cfg(test)` (leaf-web's own tests) or the `testing` feature (an external
// consumer / leaf-web's integration tests, which enable it via the self dev-dependency)
// — never production surface.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag
// in the link-collected `SOURCES` slice so a binary that lists leaf-web in its
// ExpectedManifest (the `web` capability bundle) can tell "linked-but-zero-rows"
// from "never-linked". The package name (dashes) is the join string.
leaf_core::declare_source!("leaf-web");

// The neutral HTTP value vocabulary (`Method`/`StatusCode`/`HeaderMap`/`Uri`) leaf-web
// builds on, re-exported so it is reachable AS `leaf_web::http` — the path the
// controller codegen emits (`::leaf_web::http::Method::GET`, the verb token; the
// `CONTENT_TYPE` header name) so an umbrella-only app reaches it through the SAME
// `leaf_web` facade alias the rest of the web macro surface uses, never needing `http`
// as a direct dependency. The `http` types already appear in leaf-web's public API
// (`Route::method` returns `http::Method`), so re-exporting the crate is honest.
#[doc(no_inline)]
pub use http;

pub use advice::ControlAdvice;
pub use body::{Body, Frame};
pub use content::HttpMessageConverter;
pub use extract::{
    ExtractCtx, Extension, FromRequest, FromRequestParts, Header, Json, Path, Query,
};
pub use filter::{FilterChain, Next, Terminal, WebFilter};
pub use handler::{
    ControllerKind, Handler, PathParams, Route, RouteMatch, RouteOutcome, RouteReport, RouteTable,
};
pub use embedded::EmbeddedWebServer;
pub use request::Request;
pub use response::{IntoResponse, IntoResponseWith, Response, ResponseEntity};
pub use server::{Dispatcher, ServerProperties, WebServer};

#[cfg(any(test, feature = "testing"))]
pub use testing::MockServer;
