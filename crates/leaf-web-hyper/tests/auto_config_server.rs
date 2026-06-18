//! Integration test `[web-hyper-auto-config]` — the STAGE-3 / Task-12 AUTO-CONFIG +
//! SERVER-RUN PROOF: a REAL leaf app boots, and PURELY from the link-collected slices
//! (no hand-wiring) it
//!
//! 1. registers the hyper backend as the FALLBACK `dyn ::leaf_web::WebServer`
//!    (`HyperServerAutoConfig`, this crate's `#[auto_config]` bean, gated by
//!    `OnMissingBean(dyn WebServer)`), and
//! 2. runs the leaf-web `WebServerRunner` (a `#[runner]` bean) which resolves
//!    `Ref<dyn WebServer>` + `Vec<Ref<dyn Route>>` + `Vec<Ref<dyn WebFilter>>` +
//!    `Vec<Ref<dyn ControlAdvice>>` + `Ref<ServerProperties>` FROM THE CONTAINER, builds
//!    the `Dispatcher`, and serves (it blocks on `serve`, the Spring `WebServer` model:
//!    the runner thread keeps the process serving until shutdown) —
//!
//! so a `#[rest_controller]`'s generated `Route` answers a REAL HTTP request. Every bean
//! is auto-collected; there is NO `.with_runner`, NO hand-built `Dispatcher`, NO manual
//! `serve` call. (The OnMissingBean back-off — a user `dyn WebServer` superseding the
//! FALLBACK — is proven by the `autoconfig.rs` unit tests driving the `run_autoconfig`
//! ladder, the same place leaf-cache/leaf-tx prove their defaults' back-off.)
//!
//! The ONE legitimate hand-written production trait impl in this stage is
//! `HyperServer: WebServer` (proven by `tests/serves_http.rs`); here the controller is a
//! `#[rest_controller]` and the server-default/runner are auto-config/`#[runner]` beans —
//! zero hand-rolled `Route`/`Provider`/`Handler`/`WebServer` in the run path.

use std::sync::Arc;
use std::time::Duration;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::LeafError;
use leaf_macros::rest_controller;
use leaf_web::Path;
use serde::Serialize;

// ─────────────────────────── the controller bean + its DTO ───────────────────────
//
// A `#[rest_controller]` is an ordinary `#[component]`-family bean; its request-mapping
// method becomes a `Route` bean via the controller-impl iterator. ZERO hand-written
// Route/Handler/Provider — the dogfood claim, end to end over real HTTP.

/// The JSON response DTO the handler returns; the rest-controller policy serializes it.
#[derive(Serialize, PartialEq, Debug)]
struct PingDto {
    msg: String,
}

/// A trivial controller bean (no collaborators needed for the boot proof).
#[rest_controller]
struct PingController;

impl PingController {
    fn new() -> Self {
        PingController
    }
}

impl Default for PingController {
    fn default() -> Self {
        PingController::new()
    }
}

#[rest_controller]
impl PingController {
    /// `GET /ping/{who}` — the `Path<String>` arg resolves via `FromRequest`; the DTO is
    /// serialized to JSON by the rest-controller `@ResponseBody` policy.
    #[get("/ping/{who}")]
    async fn ping(&self, who: Path<String>) -> Result<PingDto, LeafError> {
        let Path(who) = who;
        Ok(PingDto { msg: format!("pong {who}") })
    }
}

// ──────────────────────────────── test helpers ───────────────────────────────────

/// Grab a currently-free localhost port (bind ephemeral, read it back, drop) — the
/// standard "serve on a known port and tell the client where" pattern.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Poll a raw TCP connect until the embedded server is accepting connections (the runner
/// spawns + binds asynchronously inside the boot task). A bare connect — not an HTTP
/// request — so the readiness probe never reaches the dispatcher.
async fn wait_until_up(port: u16) {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the embedded web server never came up");
}

// ─────────────────────────────────── the proof ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn booting_an_app_auto_serves_the_rest_controller_over_real_http() {
    // Force-link the contributing crates' bean rows so their linkme `AUTO_CONFIGS` /
    // `COMPONENTS` rows reach the slices (anti-DCE): leaf-serde's JSON converter (the
    // `dyn HttpMessageConverter` the generated route field-injects) and leaf-web-hyper's
    // FALLBACK `dyn WebServer` auto-config (the embedded server the runner serves on).
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
    let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();

    let port = free_port();

    // Boot the WHOLE app on a DEDICATED OS thread with its own runtime: the run pipeline
    // auto-collects the `#[auto_config]` FALLBACK `dyn WebServer` + the `WebServerRunner`
    // + the generated route. The `WebServerRunner` BLOCKS on `serve` (the Spring
    // `WebServer` model — the runner keeps the process serving), so `Application::run()`
    // does not return (and its future is not `Send`, so it cannot ride `tokio::spawn`);
    // we run it on its own thread and probe the live socket from the test thread. We bind
    // the port via the `leaf.web.server.port` config key the `ServerProperties`
    // `@ConfigurationProperties` bean reads.
    let boot = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build the boot runtime");
        let spawner: Arc<dyn leaf_core::Spawner> =
            Arc::new(leaf_tokio::TokioExecutionFacility::new());
        rt.block_on(async move {
            let _running = Application::new()
                .with_name("web-auto-config")
                .with_spawner(spawner)
                .run(
                    SealInputs::new().with_args([format!("--leaf.web.server.port={port}")]),
                    RunOverlay::none(),
                )
                .await;
        });
    });

    wait_until_up(port).await;

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // The generated Route answers a REAL HTTP request: GET /ping/leaf → 200 JSON.
    let resp = client.get(format!("{base}/ping/leaf")).send().await.expect("GET /ping/leaf");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "the rest-controller @ResponseBody policy set the converter content-type"
    );
    assert_eq!(resp.text().await.expect("body"), r#"{"msg":"pong leaf"}"#);

    // An unmatched route is the dispatcher's default 404 (it never errors out at the edge).
    let resp = client.get(format!("{base}/nope")).send().await.expect("GET /nope");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // The `WebServerRunner` blocks on `serve`, so the boot thread runs the embedded server
    // until the process ends — the Spring `WebServer` model. The boot thread is detached
    // (we never join it); dropping the JoinHandle leaves it parked on the accept loop, torn
    // down when the test process exits.
    drop(boot);
}
