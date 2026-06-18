//! Integration test `[web-control-advice]` — the STAGE-2 / Task-10 CONTROL-ADVICE PROOF:
//! a real `#[control_advice]` bean whose `#[exception_handler]` method is lowered, by the
//! control-advice ITERATOR, into a `dyn ControlAdvice` the container collects + the
//! dispatcher consults on a handler `Err` — end to end, with NO hand-written
//! `ControlAdvice`/`Provider` impl (the dogfood claim).
//!
//! This proves the Task-10 codegen against the REAL leaf-web types (the token tests in
//! `leaf-codegen` prove the emitted shape; this proves it type-checks + runs):
//!
//! 1. **The struct form is a `dyn ControlAdvice` bean.** `#[control_advice] struct
//!    StorefrontErrors;` is a `#[component]`-equivalent bean that `provides` the
//!    `dyn ::leaf_web::ControlAdvice` view — collected by `Vec<Ref<dyn ControlAdvice>>`.
//! 2. **The impl form generates `ControlAdvice::handle`.** `#[control_advice] impl
//!    StorefrontErrors { #[exception_handler] fn not_found(&self, e, req) -> Option<Response> }`
//!    lowers to `impl ControlAdvice for StorefrontErrors` whose `handle` delegates to the
//!    method — NO hand-written trait impl.
//! 3. **The dispatcher consults the advice on a handler `Err`.** A `#[rest_controller]`
//!    route returning `Err(ValidationError)` is mapped by the advice to `400`; an error
//!    the advice declines (`NoSuchBean`) falls through to the dispatcher's default 404.

use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_boot::App;
use leaf_core::{ErrorKind, Injectable, LeafError, Ref, ResolveCtx};
use leaf_macros::{control_advice, rest_controller};
use leaf_web::testing::MockServer;
use leaf_web::{ControlAdvice, Dispatcher, Request, Response, Route};

// ─────────────────────── the control-advice bean (dogfood) ───────────────────────
//
// A `#[control_advice]` is an ordinary `#[component]`-family bean providing the
// `dyn ControlAdvice` view; its `#[exception_handler]` method is wired into the
// generated `ControlAdvice::handle` by the impl iterator. ZERO hand-written
// ControlAdvice/Provider impl in this file.

/// The storefront error advice bean (a unit struct — a real one might field-inject a
/// message source). The struct form `provides` the `dyn ControlAdvice` view.
#[control_advice]
struct StorefrontErrors;

impl StorefrontErrors {
    fn new() -> Self {
        StorefrontErrors
    }
}

impl Default for StorefrontErrors {
    fn default() -> Self {
        StorefrontErrors::new()
    }
}

/// The exception handlers — lowered into the generated `ControlAdvice::handle` (the
/// macro strips `#[exception_handler]` from this re-emitted impl). It maps a
/// `ValidationError` to `400` and DECLINES everything else (so the dispatcher's default
/// floor takes over).
#[control_advice]
impl StorefrontErrors {
    #[exception_handler]
    fn bad_request(&self, err: &LeafError, _req: &Request) -> Option<Response> {
        if err.kind == ErrorKind::ValidationError {
            Some(Response::new(StatusCode::BAD_REQUEST))
        } else {
            None
        }
    }
}

// ─────────────────────────── the failing controller route ────────────────────────

/// The catalog controller bean.
#[rest_controller]
struct Catalog;

impl Catalog {
    fn new() -> Self {
        Catalog
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Catalog::new()
    }
}

#[rest_controller]
impl Catalog {
    /// `GET /bad` → a `ValidationError` (the advice maps it to 400).
    #[get("/bad")]
    async fn bad(&self) -> Result<(), LeafError> {
        Err(LeafError::new(ErrorKind::ValidationError))
    }

    /// `GET /missing` → a `NoSuchBean` (the advice declines → default 404).
    #[get("/missing")]
    async fn missing(&self) -> Result<(), LeafError> {
        Err(LeafError::new(ErrorKind::NoSuchBean))
    }
}

// ─────────────────────────────── assembly helpers ────────────────────────────────

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// Freeze an engine over the auto-collected rows, resolve the routes + advice by
/// COLLECTION + BY-TRAIT injection, and build the dispatcher — the same container
/// assembly the Stage-3 `WebServerRunner` performs.
fn assemble_dispatcher() -> Dispatcher {
    // Force-link `leaf-serde`'s `JsonConverter` bean so the generated rest-controller
    // route's `dyn HttpMessageConverter` field injection resolves.
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();

    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[bean] row")
        .into_builder()
        .freeze()
        .expect("the control-advice registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    let routes: Vec<Ref<dyn Route>> =
        block(<Vec<Ref<dyn Route>> as Injectable>::inject(&cx)).expect("routes resolve");
    let advice: Vec<Ref<dyn ControlAdvice>> =
        block(<Vec<Ref<dyn ControlAdvice>> as Injectable>::inject(&cx)).expect("advice resolves");
    assert_eq!(advice.len(), 1, "the generated #[control_advice] bean was collected");

    Dispatcher::new(
        routes.into_iter().map(Ref::into_arc).collect(),
        vec![],
        advice.into_iter().map(Ref::into_arc).collect(),
    )
}

fn request(method: Method, path: &str) -> Request {
    Request::new(method, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
}

// ──────────────────────────────────── the proof ──────────────────────────────────

#[test]
fn a_control_advice_maps_a_handler_error_to_its_status() {
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // GET /bad → Catalog::bad returns Err(ValidationError); the generated
    // StorefrontErrors::handle delegates to bad_request, which maps it to 400.
    let resp = block(server.handle(request(Method::GET, "/bad")));
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn an_error_the_advice_declines_falls_through_to_the_default() {
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // GET /missing → Catalog::missing returns Err(NoSuchBean); the advice declines
    // (returns None), so the dispatcher's default floor maps NoSuchBean → 404.
    let resp = block(server.handle(request(Method::GET, "/missing")));
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
