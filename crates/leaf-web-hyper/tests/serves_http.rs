//! Integration test `[web-hyper-serves-http]` — the STAGE-3 / Task-11 BACKEND PROOF:
//! a real [`HyperServer`] binds a socket, accepts connections on the leaf-tokio
//! runtime, and drives REAL HTTP requests through the SAME backend-free
//! [`Dispatcher`](leaf_web::Dispatcher) the mock backend feeds — converting hyper's
//! native request/response to/from the leaf [`Request`]/[`Response`] at the boundary.
//!
//! It proves the transport edge end to end with a real out-of-process HTTP client
//! (`reqwest`), with NO leaf-web-facing type ever naming hyper:
//!
//! 1. **Routing + body.** `GET /ping` reaches its [`Route`] handler → `200 "pong"`.
//! 2. **The filter chain runs around the transport.** A logging [`WebFilter`] records
//!    every request that crosses the boundary (proving filters wrap the hyper edge).
//! 3. **A handler `Err` rides the advice chain through the wire.** `GET /boom` returns
//!    `Err(LeafError)`; a [`ControlAdvice`] maps it to `418` — the mapped status comes
//!    back over real HTTP (the dispatcher never errors out at the socket).
//! 4. **Request bytes survive the boundary.** A `POST /echo` round-trips its body back,
//!    proving the hyper `Incoming` body is collected into the leaf `Request`.
//!
//! The routes/filters/advice here are `#[cfg(test)]` fakes (the ONLY hand-written
//! `Route`/`Handler`/`WebFilter`/`ControlAdvice` impls allowed — they stand in for the
//! controller macro, which is proven elsewhere); the ONE legitimate hand-written
//! production trait impl in this whole stage is `HyperServer: WebServer`, which bridges
//! hyper.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_core::{BoxFuture, LeafError};
use leaf_web::filter::Next;
use leaf_web::handler::{Handler, Route};
use leaf_web::{
    ControlAdvice, Dispatcher, Request, Response, ServerProperties, WebFilter, WebServer,
};

use leaf_web_hyper::HyperServer;

// ── `#[cfg(test)]` fakes (stand-ins for the controller macro) ────────────────────

/// A route whose handler runs a fixed closure-like behaviour, keyed by a tag.
struct FakeRoute {
    method: Method,
    path: &'static str,
    handler: FakeHandler,
}

impl Route for FakeRoute {
    fn method(&self) -> Method {
        self.method.clone()
    }
    fn path(&self) -> &str {
        self.path
    }
    fn handler(&self) -> &dyn Handler {
        &self.handler
    }
}

/// `Ping` → 200 "pong"; `Boom` → an `Err`; `Echo` → echoes the request body; `Slow(d)`
/// sleeps `d` before answering 200 "slow" (the in-flight-drain straggler).
enum FakeHandler {
    Ping,
    Boom,
    Echo,
    Slow(std::time::Duration),
}

impl Handler for FakeHandler {
    fn handle<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            match self {
                FakeHandler::Ping => Ok(Response::ok().with_body(Bytes::from_static(b"pong"))),
                FakeHandler::Boom => {
                    Err(LeafError::new(leaf_core::ErrorKind::ConstructionFailed))
                }
                FakeHandler::Echo => {
                    Ok(Response::ok().with_body(Bytes::copy_from_slice(req.body_bytes())))
                }
                FakeHandler::Slow(d) => {
                    tokio::time::sleep(*d).await;
                    Ok(Response::ok().with_body(Bytes::from_static(b"slow")))
                }
            }
        })
    }
}

/// A filter that records every request path it sees, then continues (proves the chain
/// wraps the hyper transport edge).
struct LogFilter {
    log: Arc<Mutex<Vec<String>>>,
}

#[leaf_macros::async_impl]
impl WebFilter for LogFilter {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        self.log.lock().expect("log lock").push(req.path().to_string());
        next.run(req).await
    }
}

/// Maps the `Boom` handler's `ConstructionFailed` → `418 I'm a teapot` (a distinctive
/// status so the test proves advice — not the default 500 — claimed it over the wire).
struct TeapotAdvice;

impl ControlAdvice for TeapotAdvice {
    fn handle(&self, err: &LeafError, _req: &Request) -> Option<Response> {
        (err.kind == leaf_core::ErrorKind::ConstructionFailed)
            .then(|| Response::new(StatusCode::IM_A_TEAPOT))
    }
}

/// Grab a currently-free localhost port by binding an ephemeral socket and reading the
/// OS-assigned port back (then dropping the listener). The standard test pattern for
/// "serve on a random port and tell the client where" given a `host:port` server API.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hyper_server_serves_real_http_through_the_dispatcher() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));

    let routes: Vec<Arc<dyn Route>> = vec![
        Arc::new(FakeRoute { method: Method::GET, path: "/ping", handler: FakeHandler::Ping }),
        Arc::new(FakeRoute { method: Method::GET, path: "/boom", handler: FakeHandler::Boom }),
        Arc::new(FakeRoute { method: Method::POST, path: "/echo", handler: FakeHandler::Echo }),
    ];
    let filters: Vec<Arc<dyn WebFilter>> = vec![Arc::new(LogFilter { log: log.clone() })];
    let advice: Vec<Arc<dyn ControlAdvice>> = vec![Arc::new(TeapotAdvice)];

    let dispatcher = Arc::new(Dispatcher::new(routes, filters, advice));

    // Bind the server on a free ephemeral port; serve in a background task driven by the
    // KeepAlive lifecycle ctx (bind → latch ready → park on shutdown → drain). serve runs
    // until the shutdown signal fires, so it must not block the test.
    let port = free_port();
    let props = Arc::new(ServerProperties { host: "127.0.0.1".to_string(), port, ..Default::default() });
    let server = Arc::new(HyperServer::new());

    let (signal, trigger) = leaf_core::shutdown_channel();
    let ctx = leaf_core::LifecycleCtx {
        shutdown: signal,
        on_ready: Box::new(|| {}),
        grace: None,
    };
    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving = tokio::spawn(async move {
        serve_server.serve(serve_dispatcher, serve_props, ctx).await
    });

    // Wait for the listener to come up before the client connects. The probe is a raw
    // TCP connect (NOT an HTTP request) so it does not reach the dispatcher / filter —
    // the access-log assertion below must see ONLY the real requests.
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    wait_until_up(port).await;

    // 1. Routing + body: GET /ping → 200 "pong".
    let resp = client.get(format!("{base}/ping")).send().await.expect("GET /ping");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.expect("ping body"), "pong");

    // 3. A handler Err rides the advice chain through the wire: GET /boom → 418.
    let resp = client.get(format!("{base}/boom")).send().await.expect("GET /boom");
    assert_eq!(resp.status(), reqwest::StatusCode::IM_A_TEAPOT);

    // 4. Request bytes survive the boundary: POST /echo round-trips the body.
    let resp = client
        .post(format!("{base}/echo"))
        .body("hello-bytes")
        .send()
        .await
        .expect("POST /echo");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.expect("echo body"), "hello-bytes");

    // An unmatched route is the dispatcher's default 404 (never errors out at the edge).
    let resp = client.get(format!("{base}/nope")).send().await.expect("GET /nope");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 2. The filter ran for every request that crossed the boundary.
    let seen = log.lock().expect("log").clone();
    assert_eq!(seen, vec!["/ping", "/boom", "/echo", "/nope"]);

    // Trigger graceful shutdown and assert clean teardown: the serve future breaks its
    // accept loop, drains the (now-idle) connections, and resolves Ok.
    trigger.fire();
    let outcome = serving.await.expect("serve task joins");
    assert!(outcome.is_ok(), "the server drained and stopped cleanly: {outcome:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversize_request_body_is_413_within_limit_succeeds() {
    // The hyper backend caps the buffered request body at `max_request_body_bytes`
    // (enforced at the transport edge, before the whole body is materialized): a body
    // OVER the cap is rejected with `413 Payload Too Large` without ever allocating it
    // wholesale; a body WITHIN the cap still round-trips through the dispatcher.
    let routes: Vec<Arc<dyn Route>> =
        vec![Arc::new(FakeRoute { method: Method::POST, path: "/echo", handler: FakeHandler::Echo })];
    let dispatcher = Arc::new(Dispatcher::new(routes, vec![], vec![]));

    // A deliberately tiny cap so the test never allocates megabytes.
    let limit = 16usize;
    let port = free_port();
    let props = Arc::new(ServerProperties {
        host: "127.0.0.1".to_string(),
        port,
        max_request_body_bytes: limit,
    });
    let server = Arc::new(HyperServer::new());

    let (signal, trigger) = leaf_core::shutdown_channel();
    let ctx = leaf_core::LifecycleCtx {
        shutdown: signal,
        on_ready: Box::new(|| {}),
        grace: None,
    };
    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving =
        tokio::spawn(async move { serve_server.serve(serve_dispatcher, serve_props, ctx).await });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    wait_until_up(port).await;

    // Within the cap (8 <= 16): the body round-trips → 200.
    let small = "a".repeat(8);
    let resp =
        client.post(format!("{base}/echo")).body(small.clone()).send().await.expect("small POST");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.expect("small echo"), small);

    // Over the cap (64 > 16): rejected with 413 — no panic, no silent truncation.
    let big = "b".repeat(64);
    let resp = client.post(format!("{base}/echo")).body(big).send().await.expect("big POST");
    assert_eq!(resp.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    trigger.fire();
    assert!(serving.await.expect("serve task joins").is_ok(), "clean drain after the 413 edge");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_flight_request_drains_while_a_new_connection_is_refused() {
    // GRACEFUL DRAIN: a request already in flight when shutdown fires must COMPLETE (the
    // graceful tracker waits it out), while a NEW connection arriving after the signal is
    // refused (the accept loop has broken + the listener is dropped). An UNBOUNDED grace
    // (None) drains the straggler cleanly.
    let routes: Vec<Arc<dyn Route>> = vec![Arc::new(FakeRoute {
        method: Method::GET,
        path: "/slow",
        handler: FakeHandler::Slow(std::time::Duration::from_millis(400)),
    })];
    let dispatcher = Arc::new(Dispatcher::new(routes, vec![], vec![]));

    let port = free_port();
    let props = Arc::new(ServerProperties { host: "127.0.0.1".to_string(), port, ..Default::default() });
    let server = Arc::new(HyperServer::new());

    let (signal, trigger) = leaf_core::shutdown_channel();
    let ctx = leaf_core::LifecycleCtx {
        shutdown: signal,
        on_ready: Box::new(|| {}),
        grace: None, // unbounded: the in-flight straggler drains fully.
    };
    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving =
        tokio::spawn(async move { serve_server.serve(serve_dispatcher, serve_props, ctx).await });

    let base = format!("http://127.0.0.1:{port}");
    wait_until_up(port).await;

    // Fire the slow request on its own task; give it a moment to actually reach the handler
    // (so it is genuinely IN FLIGHT) before we trigger shutdown.
    let in_flight = {
        let url = format!("{base}/slow");
        tokio::spawn(async move { reqwest::Client::new().get(url).send().await?.text().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Trigger shutdown WHILE the slow request is mid-flight.
    trigger.fire();

    // A NEW connection after the signal is refused (the listener is dropped on drain). Give
    // the accept loop a beat to break first.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let refused =
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
    assert!(refused.is_err(), "a new connection after shutdown is refused");

    // The in-flight request COMPLETES (drained), not aborted.
    let body = in_flight.await.expect("in-flight task joins").expect("in-flight request completes");
    assert_eq!(body, "slow", "the in-flight request was drained to completion");

    // The serve future resolves cleanly once the straggler drained.
    assert!(serving.await.expect("serve joins").is_ok(), "clean drain");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_too_slow_straggler_is_aborted_past_the_grace_budget() {
    // GRACE-BOUNDED DRAIN: with a finite `ctx.grace`, an in-flight request slower than the
    // budget is ABORTED when the budget elapses (the `tokio::time::timeout` around the
    // graceful shutdown fires), so teardown never hangs. The serve future still resolves Ok
    // (a bounded drain that timed out is not a serve fault — it is the budget doing its job).
    let routes: Vec<Arc<dyn Route>> = vec![Arc::new(FakeRoute {
        method: Method::GET,
        path: "/slow",
        // Far slower than the grace budget below.
        handler: FakeHandler::Slow(std::time::Duration::from_secs(10)),
    })];
    let dispatcher = Arc::new(Dispatcher::new(routes, vec![], vec![]));

    let port = free_port();
    let props = Arc::new(ServerProperties { host: "127.0.0.1".to_string(), port, ..Default::default() });
    let server = Arc::new(HyperServer::new());

    let (signal, trigger) = leaf_core::shutdown_channel();
    let ctx = leaf_core::LifecycleCtx {
        shutdown: signal,
        on_ready: Box::new(|| {}),
        grace: Some(std::time::Duration::from_millis(150)), // bounded: abort the straggler.
    };
    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving =
        tokio::spawn(async move { serve_server.serve(serve_dispatcher, serve_props, ctx).await });

    let base = format!("http://127.0.0.1:{port}");
    wait_until_up(port).await;

    let in_flight = {
        let url = format!("{base}/slow");
        // Read the FULL body so the client only succeeds if the whole response arrives —
        // an aborted connection mid-body is then observably an error, not a partial Ok.
        tokio::spawn(async move {
            let resp = reqwest::Client::new().get(url).send().await?;
            resp.text().await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    // Fire shutdown; the straggler (10s) far exceeds the 150ms budget, so the drain must
    // give up well before the handler would finish.
    let fired_at = std::time::Instant::now();
    trigger.fire();

    // The serve future resolves within (well under) the 10s handler — the grace budget
    // aborted the drain. Bound the join generously to avoid flakiness on a loaded CI box.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), serving)
        .await
        .expect("serve resolves within the grace bound, NOT the 10s handler")
        .expect("serve joins");
    assert!(outcome.is_ok(), "a grace-bounded abort is not a serve fault: {outcome:?}");
    assert!(
        fired_at.elapsed() < std::time::Duration::from_secs(3),
        "the straggler was aborted past the grace budget, not waited out"
    );

    // STRONGER than "serve returned": the in-flight connection was actually TORN DOWN
    // (its spawned task aborted past the grace budget), so the client never gets the full
    // "slow" response — it observes the connection drop as an Err well before the 10s
    // handler would have finished. A leaked (merely-abandoned) connection would instead
    // run to completion and hand back "slow".
    let client_result = tokio::time::timeout(std::time::Duration::from_secs(3), in_flight)
        .await
        .expect("the client observes the torn-down connection promptly, not after 10s")
        .expect("in-flight task joins");
    assert!(
        client_result.is_err(),
        "the aborted straggler's connection was torn down — the client got an error, not the \
         full 'slow' body: {client_result:?}"
    );
    assert!(
        fired_at.elapsed() < std::time::Duration::from_secs(5),
        "the connection was cut shortly past the grace budget, not waited out for 10s"
    );
}

/// Poll a raw TCP connect until the server is accepting connections (the bind happens
/// asynchronously in the spawned serve task). A bare connect — not an HTTP request —
/// so the readiness probe never reaches the dispatcher / access-log filter.
async fn wait_until_up(port: u16) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("hyper server never came up");
}
