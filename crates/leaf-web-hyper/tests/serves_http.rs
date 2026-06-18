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

/// `Ping` → 200 "pong"; `Boom` → an `Err`; `Echo` → echoes the request body.
enum FakeHandler {
    Ping,
    Boom,
    Echo,
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

    // Bind the server on a free ephemeral port; serve in a background task (serve runs
    // until shutdown, so it must not block the test).
    let port = free_port();
    let props = ServerProperties { host: "127.0.0.1".to_string(), port };
    let server = Arc::new(HyperServer::new());

    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving = tokio::spawn(async move {
        serve_server.serve(serve_dispatcher, &serve_props).await
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

    // Tear the background server task down (it serves until shutdown otherwise).
    serving.abort();
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
