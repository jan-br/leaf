//! The hyper [`WebServer`] backend — bind, accept, and drive each request through the
//! shared [`Dispatcher`].
//!
//! [`HyperServer`] is the swappable default HTTP backend. Its [`serve`](HyperServer::serve)
//! binds a [`tokio::net::TcpListener`] per the [`ServerProperties`], accepts
//! connections, and for each request:
//!
//! 1. converts hyper's native `Request<Incoming>` → a leaf [`Request`]
//!    (collecting the streamed body into [`Bytes`] at the edge),
//! 2. calls [`Dispatcher::dispatch`] — the same backend-free request engine the mock
//!    backend feeds — which NEVER errors out,
//! 3. converts the returned leaf [`Response`] → a hyper `Response<Full<Bytes>>` and
//!    writes it.
//!
//! NOTHING the leaf-web abstraction exposes ever names hyper: the conversion lives
//! entirely inside this boundary. The serve body is written as a native `async fn` via
//! [`#[async_impl]`](leaf_macros::async_impl) — no hand-rolled `BoxFuture`/`Box::pin`.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use leaf_core::{Cause, ErrorKind, LeafError};
use leaf_web::server::{Dispatcher, ServerProperties, WebServer};
use leaf_web::{Request, Response};
use tokio::net::TcpListener;

/// The default hyper-backed [`WebServer`].
///
/// Stateless (a unit type): the routing table, filters, and advice live in the
/// [`Dispatcher`] handed to [`serve`](HyperServer::serve), which the container
/// assembles by collection + by-trait injection (the `WebServerRunner`, Task 12).
/// Construct with [`HyperServer::new`]; resolve it as the FALLBACK `dyn WebServer`
/// auto-config bean in production (Task 12).
#[derive(Clone, Copy, Debug, Default)]
pub struct HyperServer;

impl HyperServer {
    /// A new hyper backend. It holds no state — the [`Dispatcher`] is supplied per
    /// [`serve`](HyperServer::serve).
    #[must_use]
    pub fn new() -> Self {
        HyperServer
    }
}

#[leaf_macros::async_impl]
impl WebServer for HyperServer {
    /// Bind per `props` and serve requests through `dispatcher` until the task is
    /// dropped/aborted (it runs the accept loop forever; shutdown is the runtime
    /// tearing the task down).
    async fn serve(
        &self,
        dispatcher: Arc<Dispatcher>,
        props: &ServerProperties,
    ) -> Result<(), LeafError> {
        let addr = format!("{}:{}", props.host, props.port);
        let listener = TcpListener::bind(&addr).await.map_err(|e| bind_error(&addr, &e))?;

        loop {
            // Accept the next connection. An accept error is transient (per-connection,
            // e.g. a fd limit) — log-and-continue would need a logger; here we surface
            // it as a serve-level error only if the listener itself is broken.
            let (stream, _peer) = listener.accept().await.map_err(|e| accept_error(&e))?;
            let io = TokioIo::new(stream);

            // Each connection gets its own clone of the shared dispatcher (an Arc bump)
            // so the spawned service closure is `'static`.
            let conn_dispatcher = Arc::clone(&dispatcher);
            tokio::spawn(async move {
                let service = service_fn(move |hyper_req: hyper::Request<Incoming>| {
                    // Clone again per request so each request future owns its handle.
                    let req_dispatcher = Arc::clone(&conn_dispatcher);
                    async move { serve_one(req_dispatcher, hyper_req).await }
                });

                // hyper-util's auto builder negotiates HTTP/1 (and HTTP/2 if enabled) on
                // the leaf-tokio executor; a connection error is per-connection and does
                // not bring the server down.
                let _ = ConnBuilder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            });
        }
    }
}

/// Handle one hyper request end to end at the boundary: convert in → dispatch →
/// convert out. The dispatcher never errors out, so this is `Infallible` (every failure
/// is already a [`Response`]).
async fn serve_one(
    dispatcher: Arc<Dispatcher>,
    hyper_req: hyper::Request<Incoming>,
) -> Result<hyper::Response<Full<Bytes>>, Infallible> {
    let leaf_req = to_leaf_request(hyper_req).await;
    let leaf_resp = dispatcher.dispatch(leaf_req).await;
    Ok(to_hyper_response(leaf_resp))
}

/// Convert a hyper `Request<Incoming>` into a leaf [`Request`], collecting the streamed
/// body into [`Bytes`] at the edge (the leaf abstraction is body-eager: the whole body
/// is materialized before dispatch).
async fn to_leaf_request(hyper_req: hyper::Request<Incoming>) -> Request {
    let (parts, body) = hyper_req.into_parts();
    // A body-read failure (a truncated/aborted client body) degrades to an empty body
    // rather than poisoning the request — the handler/extractor then sees no bytes and
    // can map that to a 4xx via the advice chain.
    let bytes = body.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
    Request::new(parts.method, parts.uri, parts.headers, bytes)
}

/// Convert a leaf [`Response`] into a hyper `Response<Full<Bytes>>`, copying the status,
/// headers, and body bytes back across the boundary.
fn to_hyper_response(leaf_resp: Response) -> hyper::Response<Full<Bytes>> {
    let body = Bytes::copy_from_slice(leaf_resp.body_bytes());
    let mut builder = hyper::Response::builder().status(leaf_resp.status());
    // Copy every header (the builder's header map is created lazily; `headers_mut`
    // is available once a field is set, which `status` guarantees).
    if let Some(headers) = builder.headers_mut() {
        headers.clone_from(leaf_resp.headers());
    }
    builder
        .body(Full::new(body))
        // The status/headers/body are all already-valid leaf values, so building cannot
        // fail; fall back to a bare 500 if hyper ever rejects them.
        .unwrap_or_else(|_| {
            hyper::Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::new()))
                .expect("a bare 500 always builds")
        })
}

/// The transport-edge bind failure (e.g. the port is in use) as a [`LeafError`].
fn bind_error(addr: &str, err: &std::io::Error) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed)
        .caused_by(Cause::plain("binding the web server", format!("{addr}: {err}")))
}

/// A fatal accept-loop failure (the listener itself is broken) as a [`LeafError`].
fn accept_error(err: &std::io::Error) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed)
        .caused_by(Cause::plain("accepting a web connection", err.to_string()))
}
