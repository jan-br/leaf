//! Integration test `[web-mock-dispatch]` — the STAGE-1 DI-ASSEMBLY PROOF: the leaf
//! web layer assembles itself FROM THE CONTAINER and serves a request with NO hyper.
//!
//! This is the headline of Task 7. It proves two things at once:
//!
//! 1. **The abstraction is backend-free.** A request flows through the ordered
//!    [`WebFilter`] chain → the matched [`Route`]'s [`Handler`] → the
//!    [`ControlAdvice`] error chain → a [`Response`], driven by an in-memory
//!    [`MockServer`] that holds nothing but a [`Dispatcher`]. There is no hyper,
//!    no tokio runtime, no socket — the whole engine runs on `futures::block_on`.
//!
//! 2. **The container-assembly shape works end-to-end.** The routes, the filter,
//!    and the advice are ORDINARY `#[component]`/`#[bean]` beans (dogfood — no
//!    hand-written `Provider`/`Descriptor`). The dispatcher is built from
//!    `Vec<Ref<dyn Route>>` / `Vec<Ref<dyn WebFilter>>` / `Vec<Ref<dyn ControlAdvice>>`
//!    resolved by COLLECTION + BY-TRAIT injection out of a frozen engine (the same
//!    `App::from_slices` → freeze → resolve path leaf-boot's `WebServerRunner` will
//!    use in Stage 3). No central registry, no codegen-time wiring.
//!
//! A struct `#[component]` cannot itself declare a `provides` dyn-view, so each bean
//! is published as its `dyn _` view via the `#[configuration]` + `#[bean(provides =
//! "dyn …")]` idiom — exactly how `leaf-serde`'s JSON converter and the storefront's
//! `PricingRule`s expose their views. That is the idiomatic, dogfooded registration.

use std::cell::RefCell;
use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_boot::App;
use leaf_core::{BoxFuture, ErrorKind, Injectable, LeafError, Ref, ResolveCtx};
use leaf_macros::{component, configuration};
use leaf_web::testing::MockServer;
use leaf_web::{
    ControlAdvice, Dispatcher, Handler, Next, Request, Response, Route, WebFilter,
};

// ─────────────────────────── the access log (a per-test sink) ────────────────────
//
// The filter records every request line here, so the test can assert it ran in the
// chain (the around-advice seam) for each request it saw. It is a THREAD-LOCAL: every
// `#[test]` runs on its own thread and drives its dispatches synchronously
// (`futures::block_on`) on that thread, so the two tests (which share one process and
// one `WebFilter` bean *type*) never see each other's entries — no cross-test races.

thread_local! {
    static ACCESS_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

// ─────────────────────────── two `dyn Route` handler beans ──────────────────────
//
// PRODUCTION routes come from the controller macro (Stage 2); here — Stage 1, no
// macro yet — the handlers are the lone legitimate hand-written `#[cfg(test)]`-style
// impls, but the BEANS that publish them are real `#[component]`/`#[bean]` rows.

/// `GET /products/{sku}` → echoes the captured `sku` (proves path-param extraction +
/// a successful route response flowing back out through the filter chain).
struct ProductRoute;

struct ProductHandler;
impl Handler for ProductHandler {
    fn handle<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            let sku = req.path_param("sku").unwrap_or("?").to_owned();
            Ok(Response::ok().with_body(Bytes::from(format!("product:{sku}"))))
        })
    }
}

impl Route for ProductRoute {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &str {
        "/products/{sku}"
    }
    fn handler(&self) -> &dyn Handler {
        &ProductHandler
    }
}

/// `GET /boom` → always `Err` with a `ValidationError` kind, so a test can prove the
/// `ControlAdvice` chain (not the default 500) maps it.
struct BoomRoute;

struct BoomHandler;
impl Handler for BoomHandler {
    fn handle<'a>(&'a self, _req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move { Err(LeafError::new(ErrorKind::ValidationError)) })
    }
}

impl Route for BoomRoute {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &str {
        "/boom"
    }
    fn handler(&self) -> &dyn Handler {
        &BoomHandler
    }
}

// ─────────────────────────── one `dyn WebFilter` bean ────────────────────────────

/// An access-log filter (around-advice): records the request line then continues the
/// chain. A real `#[component]`-published `dyn WebFilter`, written with `#[async_impl]`
/// (the lone Stage-1 hand-written impl style).
struct AccessLogFilter;

#[leaf_macros::async_impl]
impl WebFilter for AccessLogFilter {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        let line = format!("{} {}", req.method(), req.path());
        ACCESS_LOG.with(|log| log.borrow_mut().push(line));
        next.run(req).await
    }
}

// ─────────────────────────── one `dyn ControlAdvice` bean ───────────────────────

/// Maps a `ValidationError` (raised by `/boom`) to `422 Unprocessable Entity` — proves
/// a container-contributed advice claims the error before the default 500.
struct ValidationAdvice;

impl ControlAdvice for ValidationAdvice {
    fn handle(&self, err: &LeafError, _req: &Request) -> Option<Response> {
        (err.kind == ErrorKind::ValidationError)
            .then(|| Response::new(StatusCode::UNPROCESSABLE_ENTITY))
    }
}

// ── the `#[configuration]` holder publishing each bean as its `dyn _` view ────────
//
// A struct stereotype takes no `provides`; the `#[configuration]` + `#[bean(provides =
// "dyn …")]` factory is leaf's idiom for a concrete bean that publishes a `dyn` view
// (the same shape leaf-serde's `JsonConverterConfig` and the storefront's
// `PricingRules` use). Collection injection then gathers every `dyn Route` /
// `dyn WebFilter` / `dyn ControlAdvice` provider.

#[component]
struct WebBeans;

impl WebBeans {
    fn new() -> Self {
        WebBeans
    }
}

impl Default for WebBeans {
    fn default() -> Self {
        WebBeans::new()
    }
}

#[configuration]
impl WebBeans {
    #[bean(name = "productRoute", provides = "dyn ::leaf_web::Route")]
    fn product_route(&self) -> ProductRoute {
        ProductRoute
    }

    #[bean(name = "boomRoute", provides = "dyn ::leaf_web::Route")]
    fn boom_route(&self) -> BoomRoute {
        BoomRoute
    }

    #[bean(name = "accessLogFilter", provides = "dyn ::leaf_web::WebFilter")]
    fn access_log_filter(&self) -> AccessLogFilter {
        AccessLogFilter
    }

    #[bean(name = "validationAdvice", provides = "dyn ::leaf_web::ControlAdvice")]
    fn validation_advice(&self) -> ValidationAdvice {
        ValidationAdvice
    }
}

// ──────────────────────────── the assembly helpers ───────────────────────────────

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// Freeze an engine over the auto-collected `#[component]`/`#[bean]` rows (the
/// `from_slices(&[])` maximal-magic channel — every macro-emitted seed auto-collects),
/// resolve the three `dyn _` collections by injection, and build the dispatcher — the
/// full container-assembly the real `WebServerRunner` performs.
fn assemble_dispatcher() -> Dispatcher {
    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[bean] row")
        .into_builder()
        .freeze()
        .expect("the web-beans registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    // COLLECTION + BY-TRAIT injection: gather EVERY provider of each dyn view.
    let routes: Vec<Ref<dyn Route>> =
        block(<Vec<Ref<dyn Route>> as Injectable>::inject(&cx)).expect("routes resolve");
    let filters: Vec<Ref<dyn WebFilter>> =
        block(<Vec<Ref<dyn WebFilter>> as Injectable>::inject(&cx)).expect("filters resolve");
    let advice: Vec<Ref<dyn ControlAdvice>> =
        block(<Vec<Ref<dyn ControlAdvice>> as Injectable>::inject(&cx)).expect("advice resolve");

    assert_eq!(routes.len(), 2, "both #[bean] dyn Route providers were collected");
    assert_eq!(filters.len(), 1, "the dyn WebFilter provider was collected");
    assert_eq!(advice.len(), 1, "the dyn ControlAdvice provider was collected");

    // The dispatcher takes Arc handles; Ref<dyn _> -> Arc<dyn _> is a cheap unwrap.
    Dispatcher::new(
        routes.into_iter().map(Ref::into_arc).collect(),
        filters.into_iter().map(Ref::into_arc).collect(),
        advice.into_iter().map(Ref::into_arc).collect(),
    )
}

fn get(path: &str) -> Request {
    Request::new(Method::GET, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
}

// ──────────────────────────────── the proof ──────────────────────────────────────

#[test]
fn the_web_layer_assembles_from_the_container_and_serves_with_no_hyper() {
    ACCESS_LOG.with(|log| log.borrow_mut().clear());

    // Build the in-memory backend over the container-assembled dispatcher.
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // (a) A successful route: GET /products/COFFEE flows through the filter chain,
    //     matches the macro-equivalent route, runs the handler, echoes the captured
    //     sku — the whole happy path with no transport.
    let ok = block(server.handle(get("/products/COFFEE")));
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.body_bytes(), b"product:COFFEE".as_slice());

    // (b) A handler error mapped by the CONTAINER-CONTRIBUTED advice: GET /boom raises
    //     a ValidationError, which the resolved ValidationAdvice maps to 422 (not the
    //     default 500) — proving the advice chain assembled from the container.
    let boom = block(server.handle(get("/boom")));
    assert_eq!(boom.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // (c) An unmatched route is the default 404 (the dispatcher never errors out).
    let missing = block(server.handle(get("/nope")));
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // The access-log filter ran in the chain for every dispatched request (the
    // around-advice seam wrapped the terminal each time).
    ACCESS_LOG.with(|log| {
        assert_eq!(
            *log.borrow(),
            vec![
                "GET /products/COFFEE".to_string(),
                "GET /boom".to_string(),
                "GET /nope".to_string(),
            ],
            "the container-collected filter wrapped every dispatch"
        );
    });
}

#[test]
fn the_mock_server_is_a_real_web_server_bean_implementation() {
    // The MockServer implements the leaf `WebServer` trait — the SAME trait the hyper
    // backend (Stage 3) implements — so a `dyn WebServer` can be either. This is the
    // pluggability claim: serving runs to completion on the in-memory backend with no
    // socket (its `serve` returns immediately after capturing the dispatcher).
    let dispatcher = Arc::new(assemble_dispatcher());
    let server = MockServer::new(dispatcher.clone());
    let as_dyn: &dyn leaf_web::WebServer = &server;
    let props = leaf_web::ServerProperties::default();
    block(as_dyn.serve(dispatcher, &props)).expect("the mock server 'serves' without a socket");

    // And direct dispatch still works after a serve (the dispatcher is shared, not
    // consumed).
    let ok = block(server.handle(get("/products/TEA")));
    assert_eq!(ok.body_bytes(), b"product:TEA".as_slice());
}
