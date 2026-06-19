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
use http_body_util::{BodyExt, Full, Limited};
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

        // The request-body cap (bytes) copied out of `props` so each `'static` connection
        // task owns it — the edge enforces it BEFORE the whole body is buffered.
        let max_body = props.max_request_body_bytes;

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
                    async move { serve_one(req_dispatcher, hyper_req, max_body).await }
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
///
/// `max_body` caps the buffered request body at the edge: if the incoming body exceeds
/// it, we SHORT-CIRCUIT with a `413 Payload Too Large` and never reach the dispatcher —
/// the oversize body is never materialized wholesale, so it cannot exhaust memory.
async fn serve_one(
    dispatcher: Arc<Dispatcher>,
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<hyper::Response<Full<Bytes>>, Infallible> {
    match to_leaf_request(hyper_req, max_body).await {
        Ok(leaf_req) => Ok(to_hyper_response(dispatcher.dispatch(leaf_req).await)),
        // The body blew the cap before it was buffered: emit 413 at the edge.
        Err(BodyEdgeError::TooLarge) => {
            Ok(to_hyper_response(Response::new(http::StatusCode::PAYLOAD_TOO_LARGE)))
        }
    }
}

/// An edge failure that PRECEDES dispatch (it bounds an allocation the dispatcher would
/// otherwise be forced to make). The only variant today is the body-size overflow that
/// maps to `413`.
enum BodyEdgeError {
    /// The request body exceeded the configured `max_request_body_bytes` cap.
    TooLarge,
}

/// Convert a hyper `Request<Incoming>` into a leaf [`Request`], collecting the streamed
/// body into [`Bytes`] at the edge (the leaf abstraction is body-eager: the whole body
/// is materialized before dispatch).
///
/// The body is wrapped in [`http_body_util::Limited`] so collection ABORTS once more than
/// `max_body` bytes have arrived — the cap is enforced as the body streams in, before the
/// whole thing is buffered. A `Limited` overflow yields [`BodyEdgeError::TooLarge`] (→ a
/// 413 at the caller); any OTHER body-read failure (a truncated/aborted client body)
/// degrades to an empty body rather than poisoning the request — the handler/extractor
/// then sees no bytes and can map that to a 4xx via the advice chain.
async fn to_leaf_request(
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<Request, BodyEdgeError> {
    let (parts, body) = hyper_req.into_parts();
    let bytes = match Limited::new(body, max_body).collect().await {
        Ok(collected) => collected.to_bytes(),
        // `Limited` boxes a `LengthLimitError` on overflow; distinguish it from a generic
        // read failure so an oversize body becomes a 413 (not a silently empty body).
        Err(err) if err.downcast_ref::<http_body_util::LengthLimitError>().is_some() => {
            return Err(BodyEdgeError::TooLarge);
        }
        Err(_) => Bytes::new(),
    };
    Ok(Request::new(parts.method, parts.uri, parts.headers, bytes))
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
