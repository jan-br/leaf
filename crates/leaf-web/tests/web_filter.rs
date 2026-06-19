//! Integration test `[web-web-filter]` — the Task-T4 `#[web_filter]` STEREOTYPE PROOF:
//! a real `#[web_filter]` bean is collected by the container into `Vec<Ref<dyn
//! WebFilter>>` and RUNS in the dispatcher's around-advice chain — end to end, with NO
//! hand-written `Provider`/`Descriptor` and NO `#[configuration]` + `#[bean(provides =
//! "dyn WebFilter")]` holder workaround (the dogfood claim the `#[web_filter]`
//! stereotype delivers).
//!
//! This proves the `#[web_filter]` codegen against the REAL leaf-web types (the token
//! tests in `leaf-codegen` prove the emitted `provides[]` shape; this proves it
//! type-checks, is collected, and runs):
//!
//! 1. **`#[web_filter]` is a `dyn WebFilter` bean.** `#[web_filter] struct AccessLog;`
//!    is a `#[component]`-equivalent bean that `provides` the `dyn ::leaf_web::WebFilter`
//!    view — collected by `Vec<Ref<dyn WebFilter>>` with NO `#[configuration]` holder
//!    (the exact `#[runner]`/`#[control_advice]`-struct provides-a-view shape).
//! 2. **The filter runs in the chain.** It wraps the terminal: it records the request
//!    line (around-advice seam) AND short-circuits a blocked path, proving it actually
//!    executes inside the `FilterChain` the dispatcher assembles.

use std::cell::RefCell;
use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_boot::App;
use leaf_core::{BoxFuture, Injectable, LeafError, Ref, ResolveCtx};
use leaf_macros::web_filter;
use leaf_web::testing::MockServer;
use leaf_web::{Dispatcher, Handler, Next, Request, Response, Route, WebFilter};

// The filter records every request line here so the test can assert it ran in the
// chain. THREAD-LOCAL: each `#[test]` runs on its own thread and drives its dispatches
// synchronously, so the tests never see each other's entries.
thread_local! {
    static ACCESS_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

// ─────────────────────────── the `#[web_filter]` bean (dogfood) ───────────────────
//
// A `#[web_filter]` is an ordinary `#[component]`-family bean providing the
// `dyn WebFilter` view — NO `#[configuration]` + `#[bean(provides = ..)]` holder. The
// user supplies the behaviour in a separate `#[async_impl] impl WebFilter` block.

/// An access-log + gate filter: records the request line, then either short-circuits a
/// `/blocked` request with `403` or continues the chain. The `order` lives on this
/// user-written trait impl (a struct stereotype cannot inject a method into it).
#[web_filter]
struct AccessLog;

#[leaf_macros::async_impl]
impl WebFilter for AccessLog {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        ACCESS_LOG.with(|log| log.borrow_mut().push(format!("{} {}", req.method(), req.path())));
        if req.path() == "/blocked" {
            return Ok(Response::new(StatusCode::FORBIDDEN));
        }
        next.run(req).await
    }

    fn order(&self) -> i32 {
        10
    }
}

// ─────────────────────────── one `dyn Route` (the terminal) ──────────────────────
//
// Stage-1-style hand-written handler bean — the lone legitimate `#[cfg(test)]` impl
// style; the BEAN that publishes it is a real `#[configuration]` `#[bean]` row.

struct PingRoute;

struct PingHandler;
impl Handler for PingHandler {
    fn handle<'a>(&'a self, _req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move { Ok(Response::ok().with_body(Bytes::from_static(b"pong"))) })
    }
}

impl Route for PingRoute {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &str {
        "/ping"
    }
    fn handler(&self) -> &dyn Handler {
        &PingHandler
    }
}

#[leaf_macros::component]
struct RouteBeans;

impl RouteBeans {
    fn new() -> Self {
        RouteBeans
    }
}

impl Default for RouteBeans {
    fn default() -> Self {
        RouteBeans::new()
    }
}

#[leaf_macros::configuration]
impl RouteBeans {
    #[bean(name = "pingRoute", provides = "dyn ::leaf_web::Route")]
    fn ping_route(&self) -> PingRoute {
        PingRoute
    }
}

// ──────────────────────────── the assembly helpers ───────────────────────────────

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// Freeze an engine over the auto-collected rows, resolve the routes + filters by
/// COLLECTION + BY-TRAIT injection, and build the dispatcher — the same container
/// assembly the real `WebServerRunner` performs.
fn assemble_dispatcher() -> Dispatcher {
    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[bean] row")
        .into_builder()
        .freeze()
        .expect("the web-filter registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    let routes: Vec<Ref<dyn Route>> =
        block(<Vec<Ref<dyn Route>> as Injectable>::inject(&cx)).expect("routes resolve");
    let filters: Vec<Ref<dyn WebFilter>> =
        block(<Vec<Ref<dyn WebFilter>> as Injectable>::inject(&cx)).expect("filters resolve");

    // The headline: the `#[web_filter]` bean was collected with NO `#[configuration]`
    // holder — the stereotype's `provides[] = dyn WebFilter` view did the work.
    assert_eq!(filters.len(), 1, "the #[web_filter] bean was collected as a dyn WebFilter");

    Dispatcher::new(
        routes.into_iter().map(Ref::into_arc).collect(),
        filters.into_iter().map(Ref::into_arc).collect(),
        vec![],
    )
}

fn get(path: &str) -> Request {
    Request::new(Method::GET, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
}

// ──────────────────────────────── the proof ──────────────────────────────────────

#[test]
fn a_web_filter_bean_is_collected_and_runs_in_the_chain() {
    ACCESS_LOG.with(|log| log.borrow_mut().clear());
    let server = MockServer::new(Arc::new(assemble_dispatcher()));

    // (a) A normal request flows THROUGH the filter to the terminal route.
    let ok = block(server.handle(get("/ping")));
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.body_bytes(), b"pong".as_slice());

    // (b) The filter short-circuits a /blocked request WITHOUT reaching the terminal —
    //     proving it actually executes inside the chain.
    let blocked = block(server.handle(get("/blocked")));
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    // The `#[web_filter]` bean ran (around-advice seam) for every dispatch.
    ACCESS_LOG.with(|log| {
        assert_eq!(
            *log.borrow(),
            vec!["GET /ping".to_string(), "GET /blocked".to_string()],
            "the container-collected #[web_filter] wrapped every dispatch"
        );
    });
}
