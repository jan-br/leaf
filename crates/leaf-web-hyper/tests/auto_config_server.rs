//! Integration test `[web-hyper-auto-config]` — the AUTO-CONFIG + LIFECYCLE PROOF: a REAL
//! leaf app boots IN-PROCESS, and PURELY from the link-collected slices (no hand-wiring) it
//!
//! 1. registers the hyper backend as the FALLBACK `dyn ::leaf_web::WebServer`
//!    (`HyperServerAutoConfig`, this crate's `#[auto_config]` bean, gated by
//!    `OnMissingBean(dyn WebServer)`), and
//! 2. collects the leaf-web `EmbeddedWebServer` (a `#[keep_alive]` bean, NOT a `#[runner]`)
//!    which resolves the backend, the route/filter/advice collections, and the
//!    `ServerProperties` FROM THE CONTAINER, builds the `Dispatcher`, and SERVES on a
//!    SPAWNED lifecycle task (it binds, latches readiness via `ctx.on_ready`, parks on the
//!    shutdown signal, then drains) — so `Application::run()` RETURNS once Ready instead of
//!    blocking on the accept loop.
//!
//! so a `#[rest_controller]`'s generated `Route` answers a REAL HTTP request. Every bean is
//! auto-collected; there is NO `.with_runner`, NO hand-built `Dispatcher`, NO manual `serve`.
//! (The OnMissingBean back-off — a user `dyn WebServer` superseding the FALLBACK — is proven
//! by the `autoconfig.rs` unit tests driving the `run_autoconfig` ladder.)
//!
//! It ALSO proves the production lifecycle: readiness reaches AcceptingTraffic WHILE the
//! socket actually serves (the `on_ready` latch fires inside the backend after `bind`), a
//! plain `#[runner]` alongside the server is NOT starved (the server is off the runner
//! stream by construction), and shutdown drains cleanly to `Closed`.
//!
//! The ONE legitimate hand-written production trait impl in this stage is
//! `HyperServer: WebServer` (proven by `tests/serves_http.rs`); here the controller is a
//! `#[rest_controller]` and the server-default is an auto-config / `#[keep_alive]` bean —
//! zero hand-rolled `Route`/`Provider`/`Handler`/`WebServer` in the run path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::LeafError;
use leaf_macros::{rest_controller, runner};
use leaf_web::Path;
use serde::Serialize;

/// A process-wide flag a PLAIN `#[runner]` bean (alongside the embedded web server) sets
/// when it runs — the second-runner-not-starved proof. The embedded server is no longer a
/// `#[runner]` (it is a `#[keep_alive]` off the runner stream), so a real runner can never
/// be starved by it.
static PLAIN_RUNNER_RAN: AtomicBool = AtomicBool::new(false);

/// A plain `#[runner]` bean that sets [`PLAIN_RUNNER_RAN`]. It must run to completion in the
/// readiness-gate window even though a long-running web server is in the same app — because
/// the server is OFF the runner stream (a `#[keep_alive]`), it cannot block this runner.
#[runner]
struct PlainRunner;

impl PlainRunner {
    fn new() -> Self {
        PlainRunner
    }
}

impl Default for PlainRunner {
    fn default() -> Self {
        PlainRunner::new()
    }
}

#[leaf_macros::async_impl]
impl leaf_core::Runner for PlainRunner {
    async fn run(&self, _args: &leaf_core::ApplicationArguments) -> Result<(), LeafError> {
        PLAIN_RUNNER_RAN.store(true, Ordering::SeqCst);
        Ok(())
    }
}

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

/// Poll a raw TCP connect until the embedded server is accepting connections (the keep-alive
/// spawns + binds asynchronously on its lifecycle task). A bare connect — not an HTTP
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

/// Force-link the contributing crates' bean rows so their linkme `AUTO_CONFIGS` /
/// `COMPONENTS` rows reach the slices (anti-DCE): leaf-serde's JSON converter (the
/// `dyn HttpMessageConverter` the generated route field-injects) and leaf-web-hyper's
/// FALLBACK `dyn WebServer` auto-config (the embedded server the keep-alive serves on).
fn force_link() {
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
    let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
}

/// Boot the WHOLE app IN-PROCESS on the test's own tokio runtime and return the live
/// [`leaf_boot::RunningApp`]. The run pipeline auto-collects the `#[auto_config]` FALLBACK
/// `dyn WebServer` + the `#[keep_alive]` `EmbeddedWebServer` + the generated route, and
/// SPAWNS the keep-alive's `serve` onto the spawner — so `Application::run()` RETURNS (the
/// server runs on a spawned task) instead of blocking. We bind the port via the
/// `leaf.web.server.port` config key the `ServerProperties` bean reads. The tokio
/// drain-sleeper makes the teardown grace-join bounded, exactly like production.
async fn boot_web_app(name: &'static str, port: u16) -> leaf_boot::RunningApp {
    force_link();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
    Application::new()
        .with_name(name)
        .with_spawner(spawner)
        .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
        .run(
            SealInputs::new().with_args([format!("--leaf.web.server.port={port}")]),
            RunOverlay::none(),
        )
        .await
        .expect("the web app boots to Ready")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn booting_an_app_auto_serves_the_rest_controller_over_real_http() {
    let port = free_port();
    // Boot in-process: run() returns once the app reaches Ready (the embedded server serves
    // on its spawned KeepAlive task), and we park it implicitly by holding `running` until
    // we shut it down at the end.
    let running = boot_web_app("web-auto-config", port).await;

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

    // Trigger graceful shutdown (fires the run unit's shutdown signal → the keep-alive
    // drains) and assert clean teardown.
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "clean teardown to Closed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readiness_reaches_accepting_traffic_while_the_socket_actually_serves() {
    // ACCEPTING-TRAFFIC-WHILE-SERVING: the embedded server's `ctx.on_ready` (latched inside
    // the backend AFTER `TcpListener::bind` succeeds) is what flips readiness to
    // AcceptingTraffic. So at the moment readiness is AcceptingTraffic, a real HTTP request
    // must already succeed — proving on_ready flipped WHILE the socket accepts, not merely
    // when the keep-alive task was spawned.
    let port = free_port();
    let running = boot_web_app("web-ready-while-serving", port).await;

    // The socket comes up asynchronously on the spawned serve task; wait for the readiness
    // cell to reach AcceptingTraffic (the on_ready latch), bounded so a regression fails
    // loudly rather than hanging.
    let mut reached = false;
    for _ in 0..400 {
        if running.unit().availability().readiness() == leaf_core::ReadinessState::AcceptingTraffic
        {
            reached = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(reached, "readiness reached AcceptingTraffic (the embedded server's on_ready latch)");

    // AT THIS POINT a real HTTP request must succeed — the socket is genuinely accepting.
    let base = format!("http://127.0.0.1:{port}");
    let resp = reqwest::Client::new()
        .get(format!("{base}/ping/leaf"))
        .send()
        .await
        .expect("a request succeeds the instant readiness is AcceptingTraffic");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "serving WHILE AcceptingTraffic");

    let _ = running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_runner_is_not_starved_by_the_embedded_server() {
    // SECOND-RUNNER-NOT-STARVED: a plain `#[runner]` bean (PlainRunner, which sets
    // PLAIN_RUNNER_RAN) lives alongside the long-running web server. Because the server is
    // a `#[keep_alive]` (off the runner stream) and not a blocking `#[runner]`, the run
    // pipeline's `call_runners` window completes and PlainRunner runs — it cannot be
    // starved by a server that never returns from `Runner::run`.
    PLAIN_RUNNER_RAN.store(false, Ordering::SeqCst);
    let port = free_port();
    let running = boot_web_app("web-runner-not-starved", port).await;

    // run() returned, which means the readiness-gate `call_runners` window already ran to
    // completion — so the plain runner must have fired (the server did NOT block it).
    assert!(
        PLAIN_RUNNER_RAN.load(Ordering::SeqCst),
        "the plain #[runner] ran — the embedded server (a #[keep_alive]) did not starve it"
    );

    // Sanity: the server is still serving (the keep-alive is parked, not consumed by the
    // runner stream).
    wait_until_up(port).await;
    let base = format!("http://127.0.0.1:{port}");
    let resp = reqwest::Client::new().get(format!("{base}/ping/leaf")).send().await.expect("serves");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let _ = running.shutdown().await;
}
