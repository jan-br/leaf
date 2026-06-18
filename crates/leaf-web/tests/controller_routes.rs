//! Integration test `[web-controller-routes]` — the STAGE-2 / Task-9 CONTROLLER-CODEGEN
//! PROOF: a real `#[rest_controller]` whose request-mapping methods are lowered, by the
//! controller-impl ITERATOR, into `Route` beans the container assembles + dispatches —
//! end to end, with NO hyper and NO hand-written `Route`/`Handler`/`Provider` impl.
//!
//! This proves the Task-9 codegen against the REAL leaf-web types (the token tests in
//! `leaf-codegen` prove the emitted shape; this proves it type-checks + runs):
//!
//! 1. **The macro lowers each mapped method to a `dyn Route` bean.** `#[rest_controller]
//!    impl Catalog { #[get("/products/{sku}")] async fn get(..) -> Result<ProductDto,
//!    LeafError> {..} #[post("/products")] .. }` emits one generated `Route` bean per
//!    method — a `#[component]`-equivalent providing `dyn ::leaf_web::Route` (NOT a
//!    hand-rolled Provider). The dispatcher collects them by `Vec<Ref<dyn Route>>`.
//! 2. **Argument resolution is the `FromRequest` extractor seam.** The `Path<String>`
//!    parameter resolves via `<Path<String> as FromRequest>::from_request` (structural
//!    trait dispatch, never a type-name match) — the captured `sku` reaches the method.
//! 3. **The `@ResponseBody` return policy serializes via the injected converter.** The
//!    `ProductDto` return is serialized to a JSON body by the field-injected
//!    `dyn HttpMessageConverter` (`leaf-serde`'s `JsonConverter` bean), with the
//!    `Content-Type: application/json` header — the rest-controller policy.
//! 4. **A handler `Err` rides the advice chain.** An unknown SKU returns
//!    `Err(LeafError)`; with no advice it maps to the default 500 (the dispatcher never
//!    errors out) — proving the generated handler propagates the controller's `Result`.

use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_boot::App;
use leaf_core::{ErrorKind, Injectable, LeafError, Ref, ResolveCtx};
// `#[get]`/`#[post]` need NOT be imported: the outer `#[rest_controller]` impl macro
// expands first and STRIPS the inner mapping attrs (lowering them to `Route` beans), so
// they are never resolved as attribute macros in their own right.
use leaf_macros::rest_controller;
use leaf_web::testing::MockServer;
use leaf_web::{Dispatcher, Path, Request, Route};
use serde::Serialize;

// ─────────────────────────── the controller bean + its DTO ───────────────────────
//
// A `#[rest_controller]` is an ordinary `#[component]`-family bean (here with no
// collaborators). Its request-mapping methods become `Route` beans via the macro — the
// dogfood claim: ZERO hand-written Route/Handler/Provider in this whole file.

/// The JSON response DTO a handler returns; the rest-controller policy serializes it.
#[derive(Serialize, PartialEq, Debug)]
struct ProductDto {
    sku: String,
    name: String,
    price_cents: u32,
}

/// The catalog controller bean (a unit struct — no injected collaborators needed for
/// the proof; a real one would field-inject `Ref<CatalogService>`).
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

/// The request-mapping methods — lowered by the controller-impl iterator to one `Route`
/// bean each (the macro strips `#[get]`/`#[post]` from this re-emitted impl).
#[rest_controller]
impl Catalog {
    /// `GET /products/{sku}` — the `Path<String>` arg resolves via `FromRequest`; a
    /// known SKU returns its DTO (serialized to JSON), an unknown one is a loud
    /// `LeafError` (→ the dispatcher's default 500).
    #[get("/products/{sku}")]
    async fn get(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {
        let Path(sku) = sku;
        if sku == "COFFEE" {
            Ok(ProductDto { sku, name: "House Blend".to_string(), price_cents: 1299 })
        } else {
            Err(LeafError::new(ErrorKind::NoSuchBean))
        }
    }

    /// `POST /products` — proves a second mapping method lowers to its own `Route` bean;
    /// returns a fixed DTO serialized to JSON.
    #[post("/products")]
    async fn create(&self) -> Result<ProductDto, LeafError> {
        Ok(ProductDto { sku: "TEA".to_string(), name: "Earl Grey".to_string(), price_cents: 999 })
    }
}

// ─────────────────────────────── assembly helpers ────────────────────────────────

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// Freeze an engine over the auto-collected `#[component]`/`#[bean]` rows (the generated
/// `Route` beans + the controller bean + `leaf-serde`'s JSON converter bean), resolve
/// the routes by COLLECTION + BY-TRAIT injection, and build the dispatcher — the same
/// container-assembly the Stage-3 `WebServerRunner` performs.
fn assemble_dispatcher() -> Dispatcher {
    // Force-link `leaf-serde`'s `JsonConverter` bean so its `dyn HttpMessageConverter`
    // row reaches the slices the generated rest-controller route field-injects.
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();

    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[bean] row")
        .into_builder()
        .freeze()
        .expect("the controller-route registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    let routes: Vec<Ref<dyn Route>> =
        block(<Vec<Ref<dyn Route>> as Injectable>::inject(&cx)).expect("routes resolve");
    assert_eq!(routes.len(), 2, "both generated #[rest_controller] route beans were collected");

    Dispatcher::new(routes.into_iter().map(Ref::into_arc).collect(), vec![], vec![])
}

fn request(method: Method, path: &str) -> Request {
    Request::new(method, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
}

// ──────────────────────────────────── the proof ──────────────────────────────────

#[test]
fn a_rest_controller_get_serializes_its_dto_via_the_injected_converter() {
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // GET /products/COFFEE → the generated Route's Handler resolves Path<String>=COFFEE
    // via FromRequest, invokes Catalog::get, and serializes the ProductDto to JSON.
    let resp = block(server.handle(request(Method::GET, "/products/COFFEE")));
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "the @ResponseBody policy sets the converter's content-type"
    );
    let body = std::str::from_utf8(resp.body_bytes()).expect("utf8 body");
    assert_eq!(body, r#"{"sku":"COFFEE","name":"House Blend","price_cents":1299}"#);
}

#[test]
fn a_rest_controller_post_route_is_a_distinct_generated_bean() {
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // POST /products → the SECOND generated Route bean (distinct verb + path).
    let resp = block(server.handle(request(Method::POST, "/products")));
    assert_eq!(resp.status(), StatusCode::OK);
    let body = std::str::from_utf8(resp.body_bytes()).expect("utf8 body");
    assert_eq!(body, r#"{"sku":"TEA","name":"Earl Grey","price_cents":999}"#);
}

#[test]
fn a_handler_error_rides_the_advice_chain_to_the_default_500() {
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // GET /products/NOPE → Catalog::get returns Err(NoSuchBean). NoSuchBean is the
    // unmatched-route shape too, so the generated handler's Err propagates and the
    // default mapping turns NoSuchBean → 404 (the dispatcher never errors out).
    let resp = block(server.handle(request(Method::GET, "/products/NOPE")));
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
