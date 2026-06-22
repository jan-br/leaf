//! `leaf::prelude` — the one glob a leaf application imports.
//!
//! `use leaf::prelude::*;` brings into scope every annotation macro PLUS the core
//! types a user needs to write beans, run the app, and handle faults — so the user
//! never names a hyphenated crate (`leaf-core`/`leaf-macros`/`leaf-boot`) directly.
//!
//! ## What's in scope
//!
//! - **Stereotype + bean macros:** [`component`], [`service`], [`repository`],
//!   [`controller`], [`configuration`], [`bean`], [`register_component`].
//! - **Config + value binding:** [`config_properties`], [`value`], [`BindTarget`],
//!   [`converter`].
//! - **Conditions + auto-config:** [`conditional`], [`profile`], [`auto_config`],
//!   [`import`].
//! - **By-trait injection:** [`injectable`] (on a `trait Foo`, make `dyn Foo` an
//!   injectable view — resolvable as `Ref<dyn Foo>` and collectible as
//!   `Vec<Ref<dyn Foo>>`).
//! - **Declarative advice / AOP + cross-cutting concerns:** [`advisable`],
//!   [`aspect`], [`advice`], [`pointcut`], [`transactional`],
//!   [`cacheable`], [`cache_put`], [`cache_evict`], [`validated`], [`retryable`],
//!   [`concurrency_limit`].
//! - **Events + scheduling + resources:** [`event_listener`],
//!   [`transactional_event_listener`], [`scheduled`], [`resource`], [`catalog`].
//! - **The application entry:** [`main`] (so `#[leaf::main]` works under the glob,
//!   though the canonical spelling stays `#[leaf::main]`), [`runner`],
//!   [`failure_analyzer`].
//! - **The handle currency + deferral family:** [`Ref`], [`Lookup`], [`LazyRef`],
//!   [`Inject`].
//! - **The run engine:** [`Application`], [`Runner`].
//! - **The error spine:** [`LeafError`].
//! - **The advice-manager traits** (the SPIs a user implements to back a concern):
//!   [`CacheManager`], [`TransactionManager`].
//! - **The bootstrap bridge:** [`bootstrap`] (the default-runtime run entry).
//! - **The web surface** (only with the `web` capability feature): the HTTP
//!   stereotypes + request-mapping attrs `rest_controller` / `get` / `post` / `put` /
//!   `delete` / `route`, the around-advice `web_filter` stereotype, the long-running
//!   `keep_alive` lifecycle stereotype, and the global-error
//!   `control_advice` / `exception_handler` macros, plus the leaf-web types a
//!   handler/filter/advice names — `Request` /
//!   `Response` / `IntoResponse`, the `FromRequest` extractors `Path` / `Query` /
//!   `Json` / `Header` / `Extension`, and the `WebFilter` / `Next` / `ControlAdvice`
//!   extension traits. (`controller` is in the always-on stereotype list above.)

// ── every annotation macro (the maximal-magic surface, charter §2.10) ──
#[doc(no_inline)]
pub use leaf_macros::{
    advice, advisable, aspect, async_impl, auto_config, bean, cache_evict, cache_put, cacheable, catalog,
    component, concurrency_limit, conditional, config_properties, configuration, controller,
    converter, event_listener, failure_analyzer, import, injectable, main, pointcut, profile,
    register_component, repository, resource, retryable, runner, scheduled, service, transactional,
    transactional_event_listener, validated, value, BindTarget,
};

// ── the handle currency + the honest-visible deferral family ──
//
// `Ref<T>` is the ONE shared-handle sugar; `Lookup`/`LazyRef`/`Inject` are the
// resolve-on-demand deferral handles (the only construction-cycle break).
#[doc(no_inline)]
pub use leaf_core::{Inject, LazyRef, Lookup, Ref};

// ── the error spine ──
#[doc(no_inline)]
pub use leaf_core::LeafError;

// ── the run engine + the runner SPI ──
#[doc(no_inline)]
pub use leaf_boot::Application;
#[doc(no_inline)]
pub use leaf_core::Runner;

// ── the advice-manager traits (the concern-backing SPIs a user implements) ──
#[doc(no_inline)]
pub use leaf_core::{CacheManager, TransactionManager};

// ── the default-runtime bootstrap bridge ──
#[doc(no_inline)]
pub use crate::runtime::bootstrap;

// ── the WEB capability surface (present iff the `web` feature pulled the bundle) ──
//
// The HTTP transport stereotypes + the request-mapping method attrs + the global-error
// macro, plus the leaf-web types a controller/filter/advice names in its signatures
// (`Request`/`Response`/`IntoResponse`, the `FromRequest` extractors `Path`/`Query`/
// `Json`/`Header`/`Extension`, and the `WebFilter`/`Next`/`ControlAdvice` extension traits).
// Brought in flat so `use leaf::prelude::*;` is the whole web surface — the same maximal-
// magic shape the cross-cutting concerns already have. `#[controller]` is in the always-on
// macro glob above (it predates this stack as a bare marker); the rest land here.
//
// The macros emit ABSOLUTE `::leaf_web::` paths that resolve through the umbrella's facade
// alias (`extern crate leaf as leaf_web;`) + the root re-exports above; these prelude names
// are what the USER writes.
#[cfg(feature = "web")]
#[doc(no_inline)]
pub use leaf_macros::{
    control_advice, delete, exception_handler, get, keep_alive, post, put, rest_controller, route,
    web_filter,
};
#[cfg(feature = "web")]
#[doc(no_inline)]
pub use leaf_starter_web::leaf_web::{
    http, ControlAdvice, Extension, FromRequest, Header, IntoResponse, IntoResponseWith, Json, Next,
    Path, Query, Request, Response, ResponseEntity, WebFilter,
};

// ── the gRPC capability surface (present iff the `grpc` feature pulled the bundle) ──
//
// The #[grpc_controller] stereotype (the gRPC twin of the `web`-gated `rest_controller`/
// `web_filter` exports) + the leaf-grpc types a controller method names in its signatures
// (`Status`/`Code` for the error model, `Streaming` for the streaming shapes). The macro
// emits absolute `::leaf_grpc::` paths resolved through the umbrella's `grpc`-gated root
// re-exports + the `extern crate leaf as leaf_grpc;` facade alias; these prelude names are
// what the USER writes.
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_macros::grpc_controller;
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_starter_grpc::leaf_grpc::{Code, Status, Streaming};
