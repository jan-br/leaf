//! The STOREFRONT REST PROOF (Task 14) — the headline end-to-end milestone: the
//! umbrella-only storefront, with the `web` capability feature, serves REAL HTTP from
//! its domain services through `#[rest_controller]` beans, a `#[component]` `WebFilter`,
//! and a `#[control_advice]` — with ZERO hand-written `Route`/`Provider`/`Handler`/
//! `CacheManager` impls. Everything is a stereotype bean; the server assembles itself
//! from the container and serves.
//!
//! This boots the WHOLE storefront app (the same `leaf::bootstrap` entry `#[leaf::main]`
//! drives) on a dedicated OS thread — the `WebServerRunner` BLOCKS on `serve` (the Spring
//! `WebServer` model), so `run()` never returns; we probe the live socket from the test
//! thread with a real `reqwest` HTTP client.
//!
//! It proves, over real HTTP:
//!   1. `GET /products/COFFEE` → 200 JSON `{sku,name,price_cents}` resolved via
//!      `CatalogService` (the cacheable price lookup) + `ProductRepository`.
//!   2. `GET /products/NOPE` → 404 — the unknown SKU's `LeafError` is mapped by the
//!      `#[control_advice]` (`StorefrontErrors`), not the default floor.
//!   3. `POST /orders` with a JSON body → 200 with the created order (via `OrderService`,
//!      the `#[transactional]` place-order path).
//!   4. The access-log `WebFilter` recorded both requests (its counter, exposed for the
//!      proof, advanced) — the around-advice seam ran.
#![cfg(feature = "web")]

// The umbrella-only facade alias the web macros' absolute `::leaf_web::` paths resolve
// against (this integration-test crate's only leaf dependency is `leaf`; a binary-crate
// root gets this auto-emitted by `#[leaf::main]`, a test names it by hand — the same
// `leaf_core`/`leaf_cache`/`leaf_tx` alias pattern). A SOURCE alias of the one `leaf`
// dep, not a new Cargo dependency.
extern crate leaf as leaf_web;

// Link the storefront LIBRARY's bean rows (the controllers/filter/advice + the domain
// services + the startup runner) into this test binary: a sibling test crate only sees
// the package's LIB, and its `linkme` rows reach the link graph only if the lib is
// referenced. This pins the storefront's beans so the booted app actually has routes.
use storefront as _;

use std::time::Duration;

/// Grab a currently-free localhost port (bind ephemeral, read it back, drop).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Poll a raw TCP connect until the embedded server is accepting connections (the runner
/// binds asynchronously inside the boot task).
async fn wait_until_up(port: u16) {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the storefront web server never came up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_storefront_serves_its_domain_over_real_http() {
    let port = free_port();

    // Boot the whole storefront on a dedicated OS thread with its own runtime: the
    // `WebServerRunner` blocks on `serve`, so `run()` does not return; we probe the live
    // socket from the test thread. The umbrella's `web`-gated force-link pins the bundle.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build the boot runtime");
        rt.block_on(async move {
            let _running = leaf::bootstrap("storefront")
                .run(
                    leaf::RunInputs::new()
                        .with_args([
                            format!("--leaf.web.server.port={port}"),
                            "--app.name=storefront".to_string(),
                        ])
                        .into(),
                    leaf::boot::RunOverlay::none(),
                )
                .await;
        });
    });

    wait_until_up(port).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // 1. GET /products/COFFEE → 200 JSON from CatalogService + ProductRepository.
    let resp = client.get(format!("{base}/products/COFFEE")).send().await.expect("GET COFFEE");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "a known SKU is 200");
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "the @ResponseBody policy set the JSON content-type"
    );
    let body: serde_json::Value = resp.json().await.expect("JSON body");
    assert_eq!(body["sku"], "COFFEE");
    assert_eq!(body["name"], "Bag of Coffee");
    assert_eq!(body["price_cents"], 1299);

    // 2. GET /products/NOPE → 404 via the #[control_advice] (unknown SKU mapping).
    let resp = client.get(format!("{base}/products/NOPE")).send().await.expect("GET NOPE");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND, "an unknown SKU is 404 via advice");

    // 3. POST /orders with a JSON body → 200 with the created order (via OrderService).
    let resp = client
        .post(format!("{base}/orders"))
        .json(&serde_json::json!({ "sku": "COFFEE", "qty": 2 }))
        .send()
        .await
        .expect("POST /orders");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "a placed order is 200");
    let order: serde_json::Value = resp.json().await.expect("JSON order");
    assert_eq!(order["sku"], "COFFEE");
    assert_eq!(order["qty"], 2);
    assert_eq!(order["total_cents"], 2598, "2 x 1299c");

    // 4. The access-log filter recorded both requests — probe the dedicated endpoint the
    //    filter's counter is exposed through (the around-advice seam ran on every call).
    let resp = client.get(format!("{base}/_access_count")).send().await.expect("GET _access_count");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let count: i64 = resp.text().await.expect("count body").trim().parse().expect("a number");
    assert!(count >= 3, "the access-log filter ran on every request (>=3 so far), got {count}");
}
