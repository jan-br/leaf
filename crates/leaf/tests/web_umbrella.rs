//! THE UMBRELLA WEB PROOF — a real leaf WEB application written against ONLY the
//! `leaf` umbrella crate with the `web` capability feature (phase3 TOPOLOGY "Starters
//! & BOM": "the blessed path", extended to the web stack).
//!
//! It proves the `web` capability delivers a working HTTP layer from the umbrella's
//! public surface ALONE — `use leaf::prelude::*;` brings the web stereotypes +
//! extractor types into scope, and the controller/advice macros' ABSOLUTE
//! `::leaf_web::` paths resolve through the umbrella's facade alias
//! (`extern crate leaf as leaf_web;`, auto-emitted by `#[leaf::main]` / written at the
//! binary-crate root) + the umbrella's root re-exports of the macro-referenced leaf-web
//! surface. This mirrors the `::leaf_cache::` / `::leaf_tx::` alias proof already
//! shipped for the cross-cutting concerns — the SAME pattern, extended to the web macros.
//!
//! Step 1 of Task 13: this test FAILS to compile until (a) the prelude re-exports the
//! web macros + extractor types, (b) the umbrella re-exports the `::leaf_web::` macro
//! surface at its root, and (c) the facade alias `extern crate leaf as leaf_web;` binds
//! `leaf_web` to the one `leaf` dependency.
#![cfg(feature = "web")]

// The umbrella-only facade alias: an annotation's ABSOLUTE `::leaf_web::` paths resolve
// against the consuming crate's extern prelude (its direct deps) — and this test crate's
// only leaf dependency is `leaf`. `#[leaf::main]` auto-emits this from a binary-crate
// root; an integration test has no `#[leaf::main]`, so (like the `leaf_core`/`leaf_cache`/
// `leaf_tx` aliases a hand-written entry writes) it names the one alias the web macros
// need. This is a SOURCE alias of the existing `leaf` dep, NOT a new Cargo dependency.
extern crate leaf as leaf_web;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use leaf::prelude::*;
use serde::{Deserialize, Serialize};

/// The process-wide counter the [`AccessLog`] filter advances on every request and the
/// `GET /_filter_count` probe reads back — observable demo state so the test can PROVE the
/// around-advice ran end-to-end over real HTTP (not just a bare TCP connect).
static FILTER_COUNT: AtomicI64 = AtomicI64::new(0);

// ─────────────────────────── the user's web beans ───────────────────────────
//
// EVERYTHING is a stereotype bean reached through the umbrella + the prelude glob;
// this test crate names ONLY the `leaf` dependency (plus serde for the DTO derives,
// the data-format vocabulary a JSON app owns).

/// The JSON response DTO the rest-controller serializes via the injected converter.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct ProductDto {
    sku: String,
    name: String,
    price_cents: u32,
}

/// A `#[rest_controller]` (a `@Component`-family bean) — its request-mapping methods
/// lower to generated `Route` beans via the controller-impl iterator. Reached as a
/// prelude macro; its generated route bean's `::leaf_web::Route`/`Handler`/`Request`/
/// `Response`/`HttpMessageConverter`/`FromRequest`/`IntoResponse` paths resolve through
/// the `leaf_web` facade alias + the umbrella's root re-exports.
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
    /// `GET /products/{sku}` — the `Path<String>` arg resolves via `FromRequest`; the
    /// DTO is serialized by the rest-controller `@ResponseBody` policy.
    #[get("/products/{sku}")]
    async fn get(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {
        let Path(sku) = sku;
        Ok(ProductDto { name: format!("Product {sku}"), sku, price_cents: 1299 })
    }

    /// `POST /products/{sku}/touch` — a second mapping (a different verb macro) proving
    /// `#[post]` resolves through the facade too; the `Path<String>` arg rides the same
    /// `FromRequest` seam.
    #[post("/products/{sku}/touch")]
    async fn touch(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {
        let Path(sku) = sku;
        Ok(ProductDto { name: format!("Touched {sku}"), sku, price_cents: 0 })
    }

    /// `GET /_filter_count` — a plaintext probe exposing the access-log filter's counter,
    /// so the test can PROVE the `WebFilter` around-advice ran on every request.
    #[get("/_filter_count")]
    async fn filter_count(&self) -> Result<i64, LeafError> {
        Ok(FILTER_COUNT.load(Ordering::SeqCst))
    }
}

/// A `#[web_filter]` `WebFilter` reached through the prelude's `web_filter`/`WebFilter`/
/// `Next` re-exports + `#[async_impl]` desugaring — proving the extension-bean surface
/// flows through the umbrella too. `#[web_filter]` is the one-annotation stereotype: a
/// `#[component]` that ALSO publishes the `dyn ::leaf_web::WebFilter` view the server
/// collects (no hand-rolled marker-trait + wrong-view registration). It counts every
/// request so the test can assert the around-advice ran end-to-end.
#[web_filter]
#[derive(Default)]
struct AccessLog;

#[async_impl]
impl WebFilter for AccessLog {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        FILTER_COUNT.fetch_add(1, Ordering::SeqCst);
        next.run(req).await
    }
}

/// A `#[control_advice]` mapping a `LeafError` to a `Response` — the global error seam,
/// reached as a prelude macro; its generated `impl ::leaf_web::ControlAdvice` resolves
/// through the facade alias.
#[control_advice]
struct Errors;

impl Errors {
    fn new() -> Self {
        Errors
    }
}

impl Default for Errors {
    fn default() -> Self {
        Errors::new()
    }
}

#[control_advice]
impl Errors {
    #[exception_handler]
    fn not_found(&self, _err: &LeafError, _req: &Request) -> Option<Response> {
        None
    }
}

// ─────────────────────────────── the milestone ───────────────────────────────

/// Grab a currently-free localhost port (bind ephemeral, read it back, drop).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Poll a raw TCP connect until the embedded server is accepting connections (the keep-alive
/// binds asynchronously on its spawned lifecycle task).
async fn wait_until_up(port: u16) -> bool {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// A minimal HTTP/1.1 `GET` over a raw `TcpStream` — the umbrella's `leaf` dependency set
/// stays HTTP-client-free (no `reqwest`), so the proof speaks the wire protocol directly.
/// Returns `(status_line, body)`; reads to EOF (the server closes the connection per the
/// `Connection: close` request).
async fn http_get(port: u16, path: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream =
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("connect to the server");
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.expect("write the request");
    stream.flush().await.expect("flush the request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read the response");
    let text = String::from_utf8_lossy(&raw).into_owned();

    let status = text.lines().next().unwrap_or_default().to_string();
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

/// The blessed web path BOOTS + SERVES, umbrella-only: enabling `web` pulls the curated
/// bundle (leaf-web + leaf-web-hyper + leaf-serde) into the force-link set, so PURELY
/// from the link-collected slices the run pipeline
///   (a) registers the generated `Route` beans (the `#[rest_controller]` impl),
///   (b) registers the `#[component]` filter + the `#[control_advice]`,
///   (c) wires the leaf-serde JSON converter (the `dyn HttpMessageConverter` the route
///       field-injects) + the leaf-web-hyper FALLBACK `dyn WebServer`,
///   (d) collects the leaf-web `EmbeddedWebServer` `#[keep_alive]`, which assembles the
///       `Dispatcher` from the container + serves on the bound port from a spawned task —
/// with NO hand-wiring. The whole point: the macro-emitted `::leaf_web::` paths resolved
/// through the umbrella facade alias, and the umbrella `web` feature pulled the real
/// bundle (not the old placeholder).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_umbrella_web_stack_boots_and_serves_through_the_facade() {
    let port = free_port();

    // Boot the WHOLE app IN-PROCESS on the test's runtime: the embedded server is a
    // `#[keep_alive]` that serves on a spawned lifecycle task, so `Application::run()`
    // RETURNS once Ready and we hold the live app. The umbrella's own `web`-gated
    // force-link pins the bundle rlibs onto the link graph (no `force_link!()` in a lib test).
    let running = leaf::bootstrap("web-umbrella")
        .run(
            leaf::RunInputs::new()
                .with_args([format!("--leaf.web.server.port={port}")])
                .into(),
            leaf::boot::RunOverlay::none(),
        )
        .await
        .expect("the umbrella web app boots to Ready");

    assert!(
        wait_until_up(port).await,
        "the umbrella-only web app bound the port — the bundle force-linked + the keep-alive \
         assembled the dispatcher + the FALLBACK hyper backend served"
    );

    // A REAL HTTP request through the dispatcher: `GET /products/COFFEE` resolves the
    // rest-controller route + serializes the DTO — proving the wired stack serves, not just
    // that the port is bound.
    let (status, body) = http_get(port, "/products/COFFEE").await;
    assert!(status.contains("200"), "a mapped route is 200, got status line {status:?}");
    assert!(body.contains("\"sku\":\"COFFEE\""), "the DTO serialized, got body {body:?}");

    // The `#[web_filter]` around-advice ran on that request: probe the filter's counter
    // endpoint and assert it advanced. This is the END-TO-END proof the filter is actually
    // collected as `dyn WebFilter` and invoked — the bare-TCP-connect probe never could.
    let (status, body) = http_get(port, "/_filter_count").await;
    assert!(status.contains("200"), "the probe route is 200, got status line {status:?}");
    let count: i64 = body.trim().parse().expect("the filter count is a number");
    assert!(
        count >= 2,
        "the access-log WebFilter ran on every request (>=2: the COFFEE GET + this probe), \
         got {count}"
    );

    // Trigger graceful shutdown (fires the run unit's shutdown signal → the embedded server
    // keep-alive drains) and assert clean teardown to Closed.
    let report = running.shutdown().await;
    assert_eq!(
        report.run_state,
        leaf::core::RunState::Closed,
        "the umbrella web app drained + tore down cleanly"
    );
}
