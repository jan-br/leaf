//! The hyper [`WebServer`] backend — bind, accept, and drive each request through the
//! shared [`Dispatcher`].
//!
//! [`HyperServer`] is the swappable default HTTP backend. Its [`serve`](HyperServer::serve)
//! binds a [`tokio::net::TcpListener`] per the [`ServerProperties`], accepts
//! connections, and for each request:
//!
//! 1. converts hyper's native `Request<Incoming>` → a leaf [`Request`] whose body is a
//!    STREAMING [`Body`](leaf_web::Body) of the hyper frames (data + trailers); a buffered
//!    HTTP body is collected to Full + capped at the edge (the 413 guarantee),
//! 2. calls [`Dispatcher::dispatch`] — the same backend-free request engine the mock
//!    backend feeds — which NEVER errors out,
//! 3. converts the returned leaf [`Response`] → a hyper response whose body is written as
//!    a buffered `Full<Bytes>` OR as a streaming frame body (data + trailers) and writes it.
//!
//! NOTHING the leaf-web abstraction exposes ever names hyper: the conversion lives
//! entirely inside this boundary. The serve body is written as a native `async fn` via
//! [`#[async_impl]`](leaf_macros::async_impl) — no hand-rolled `BoxFuture`/`Box::pin`.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body::Body as HttpBody;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use hyper_util::server::graceful::GracefulShutdown;
use leaf_core::{Cause, ErrorKind, LeafError, LifecycleCtx};
use leaf_web::body::{Body as LeafBody, Frame as LeafFrame};
use leaf_web::server::{Dispatcher, ServerProperties, WebServer};
use leaf_web::{Request, Response};
use tokio::net::TcpListener;

/// The default hyper-backed [`WebServer`].
///
/// Stateless (a unit type): the routing table, filters, and advice live in the
/// [`Dispatcher`] handed to [`serve`](HyperServer::serve), which the container
/// assembles by collection + by-trait injection (the `EmbeddedWebServer` keep-alive bean).
/// Construct with [`HyperServer::new`]; resolve it as the FALLBACK `dyn WebServer`
/// auto-config bean in production.
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

impl WebServer for HyperServer {
    /// Bind per `props`, latch readiness via [`ctx.on_ready`](LifecycleCtx) once the socket
    /// is accepting, then run a SELECT-driven accept loop that breaks on
    /// [`ctx.shutdown`](LifecycleCtx) and DRAINS the in-flight connections (bounded by
    /// [`ctx.grace`](LifecycleCtx)) via [`GracefulShutdown`].
    ///
    /// The returned future is `'static`: it owns the dispatcher, the `Arc<ServerProperties>`,
    /// and the `ctx`, borrowing nothing of `&self` (a `HyperServer` is a stateless `Copy`
    /// unit). `tokio::select!`/`tokio::time` are fine HERE — this IS the backend.
    fn serve(
        &self,
        dispatcher: Arc<Dispatcher>,
        props: Arc<ServerProperties>,
        ctx: LifecycleCtx,
    ) -> leaf_core::BoxFuture<'static, Result<(), LeafError>> {
        Box::pin(async move {
            let addr = format!("{}:{}", props.host, props.port);
            let listener = TcpListener::bind(&addr).await.map_err(|e| bind_error(&addr, &e))?;

            // The socket is bound and about to accept: LATCH READINESS now (this is what
            // flips availability to AcceptingTraffic WHILE we serve, not merely when spawned).
            (ctx.on_ready)();

            // The request-body cap (bytes) copied out of `props` so each `'static` connection
            // task owns it — the edge enforces it BEFORE the whole body is buffered.
            let max_body = props.max_request_body_bytes;

            // Track every in-flight connection so the drain can wait them out gracefully.
            let graceful = GracefulShutdown::new();
            // The spawned per-connection task handles, retained so the grace timeout can
            // ABORT (not merely abandon) a straggler — dropping `graceful.shutdown()` on a
            // timeout does NOT cancel the detached tasks, so a slow connection would leak
            // until the runtime drops. We abort these explicitly past the budget.
            let mut conn_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

            loop {
                tokio::select! {
                    // A new connection arrived: serve it, tracked by the graceful watcher.
                    accepted = listener.accept() => {
                        let (stream, _peer) = match accepted {
                            Ok(conn) => conn,
                            // A transient per-connection accept error (e.g. an fd limit):
                            // skip it and keep serving rather than tearing the server down.
                            Err(_e) => continue,
                        };
                        let io = TokioIo::new(stream);

                        // Each connection gets its own clone of the shared dispatcher (an Arc
                        // bump) so the spawned service closure is `'static`.
                        let conn_dispatcher = Arc::clone(&dispatcher);
                        let service = service_fn(move |hyper_req: hyper::Request<Incoming>| {
                            // Clone again per request so each request future owns its handle.
                            let req_dispatcher = Arc::clone(&conn_dispatcher);
                            async move { serve_one(req_dispatcher, hyper_req, max_body).await }
                        });

                        // hyper-util's auto builder negotiates HTTP/1 (and HTTP/2 if enabled)
                        // on the leaf-tokio executor; WATCH the connection so the graceful
                        // drain can signal + await it. `serve_connection` borrows the builder,
                        // so bind it to a local; `into_owned()` then detaches the connection
                        // from that borrow so the spawned task is `'static`.
                        let builder = ConnBuilder::new(TokioExecutor::new());
                        let conn = builder.serve_connection(io, service);
                        let watched = graceful.watch(conn.into_owned());
                        // RETAIN the JoinHandle (not a detached spawn) so the grace timeout
                        // can abort the connection's task if it overruns the budget.
                        conn_tasks.push(tokio::spawn(async move {
                            // A connection error is per-connection and does not bring the
                            // server down.
                            let _ = watched.await;
                        }));
                    }
                    // Shutdown requested: stop accepting and break to the drain. Dropping the
                    // listener here refuses any NEW connection that arrives during the drain.
                    () = ctx.shutdown.quiesce() => break,
                }
            }

            // Stop accepting (refuse new connections), then DRAIN the in-flight ones:
            // `graceful.shutdown()` signals every watched connection to finish and awaits
            // them, BOUNDED by `ctx.grace`. On a clean drain (or an unbounded grace) every
            // connection task finishes on its own. On a grace TIMEOUT we ABORT every
            // still-running connection task — so a too-slow straggler is genuinely torn
            // down (its socket closed), never leaked to run until the runtime drops.
            drop(listener);
            match ctx.grace {
                Some(d) => {
                    if tokio::time::timeout(d, graceful.shutdown()).await.is_err() {
                        // The budget elapsed with connections still in flight: abort them.
                        for task in &conn_tasks {
                            task.abort();
                        }
                    }
                }
                None => graceful.shutdown().await,
            }
            Ok(())
        })
    }
}

/// Handle one hyper request end to end at the boundary: convert in → dispatch → convert
/// out. `to_leaf_request` builds a STREAMING leaf body (the dispatcher collects an HTTP
/// body to Full before a route handler; a gRPC protocol reads the stream). The dispatcher
/// never errors out, so this is `Infallible`.
///
/// `max_body` caps a buffered HTTP body at the edge: a NON-streaming-protocol request whose
/// body exceeds the cap is rejected with a `413 Payload Too Large` BEFORE dispatch (the
/// oversize body is never materialized wholesale, so it cannot exhaust memory). A
/// streaming-protocol request (`application/grpc*`) keeps its stream — its own framing
/// bounds it. Selection is by the runtime content-type value, never a Rust type name.
async fn serve_one(
    dispatcher: Arc<Dispatcher>,
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<hyper::Response<HyperOutBody>, Infallible> {
    let mut leaf_req = to_leaf_request(hyper_req, max_body);

    // The 413 contract is an EDGE guarantee for buffered HTTP bodies: if this is not a
    // streaming-protocol request, collect the body here bounded by `max_body` and emit 413
    // BEFORE dispatch when it overflows (mirrors the pre-streaming behaviour exactly). A
    // streaming-protocol request (application/grpc*) keeps its stream — its own framing
    // bounds it. The "application/grpc" literal is a TRANSPORT-EDGE content-type value (a
    // runtime header string), not a Rust type name.
    let is_streaming_protocol = leaf_req
        .header(http::header::CONTENT_TYPE.as_str())
        .is_some_and(|ct| ct.starts_with("application/grpc"));
    if !is_streaming_protocol && leaf_req.body_is_stream() {
        let body = leaf_req.take_body();
        match body.collect(max_body).await {
            Ok(collected) => leaf_req.set_body(LeafBody::Full(collected)),
            Err(_over_cap) => {
                return Ok(to_hyper_response(Response::new(http::StatusCode::PAYLOAD_TOO_LARGE)));
            }
        }
    }
    Ok(to_hyper_response(dispatcher.dispatch(leaf_req).await))
}

/// Convert a hyper `Request<Incoming>` into a leaf [`Request`] whose body is a
/// [`LeafBody::Stream`] of the hyper [`Incoming`] frames (data + trailers).
///
/// Nothing is collected HERE: the leaf [`Dispatcher`] collects an HTTP body to Full
/// (bounded by `max_body`, via [`serve_one`]) before a route handler, while a gRPC protocol
/// reads the stream directly. The per-frame `budget` enforces `max_body` as the data drains
/// so an oversize HTTP body is rejected before being buffered wholesale; a frame-read
/// failure becomes a stream `Err` the leaf advice chain maps.
fn to_leaf_request(hyper_req: hyper::Request<Incoming>, max_body: usize) -> Request {
    let (parts, incoming) = hyper_req.into_parts();
    let stream = incoming_to_leaf_frames(incoming, max_body);
    let mut req = Request::new(parts.method, parts.uri, parts.headers, Bytes::new());
    req.set_body(LeafBody::Stream(Box::pin(stream)));
    req
}

/// Drain a hyper [`Incoming`] into a `Stream` of leaf [`LeafFrame`]s (data → `Data`,
/// trailers → `Trailers`), enforcing `max_body` across the cumulative data bytes (an
/// overflow yields a `ConvertError` stream `Err` → the leaf side maps it to 413/4xx).
fn incoming_to_leaf_frames(
    incoming: Incoming,
    max_body: usize,
) -> impl futures::Stream<Item = Result<LeafFrame, LeafError>> + Send + Sync + 'static {
    futures::stream::unfold((incoming, 0usize), move |(mut incoming, mut seen)| async move {
        let polled =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut incoming).poll_frame(cx)).await;
        match polled {
            None => None,
            Some(Err(_e)) => {
                Some((Err(edge_stream_error("reading the request body")), (incoming, seen)))
            }
            Some(Ok(frame)) => {
                // A hyper frame is data OR trailers; `into_data` hands the frame back on Err.
                match frame.into_data() {
                    Ok(data) => {
                        seen += data.len();
                        if seen > max_body {
                            Some((
                                Err(edge_stream_error("request body over the limit")),
                                (incoming, seen),
                            ))
                        } else {
                            Some((Ok(LeafFrame::Data(data)), (incoming, seen)))
                        }
                    }
                    Err(frame) => match frame.into_trailers() {
                        Ok(trailers) => Some((Ok(LeafFrame::Trailers(trailers)), (incoming, seen))),
                        // Neither data nor trailers (no other hyper frame kind today): skip it
                        // by ending the stream — there is nothing meaningful to yield.
                        Err(_other) => None,
                    },
                }
            }
        }
    })
}

/// A streamed-body read failure as a `ConvertError` (a client-fault → 4xx via the default
/// advice floor; an oversize body likewise maps to a client-fault status).
fn edge_stream_error(what: &'static str) -> LeafError {
    LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain("the request body stream", what))
}

/// The outbound body the hyper edge writes: a buffered [`Full`] (the HTTP default) or a
/// STREAM of leaf frames (data + trailers) rendered as an `http_body::Body`. This is the
/// out-half of the streaming `Body`, confined to the backend edge.
enum HyperOutBody {
    Full(Full<Bytes>),
    Stream(std::pin::Pin<Box<dyn futures::Stream<Item = Result<LeafFrame, LeafError>> + Send + Sync>>),
}

impl HttpBody for HyperOutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        // Project the pin to the active variant.
        match self.get_mut() {
            HyperOutBody::Full(full) => std::pin::Pin::new(full)
                .poll_frame(cx)
                // `Full<Bytes>` is itself Infallible.
                .map(|opt| opt.map(|res| res.map_err(|never| match never {}))),
            HyperOutBody::Stream(stream) => match stream.as_mut().poll_next(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                std::task::Poll::Ready(Some(Ok(LeafFrame::Data(data)))) => {
                    std::task::Poll::Ready(Some(Ok(http_body::Frame::data(data))))
                }
                std::task::Poll::Ready(Some(Ok(LeafFrame::Trailers(map)))) => {
                    std::task::Poll::Ready(Some(Ok(http_body::Frame::trailers(map))))
                }
                // A leaf stream error has no HTTP-status channel mid-body; end the body
                // (the peer observes a truncated/incomplete response). gRPC renders status
                // as trailers, so it never reaches here as an Err.
                std::task::Poll::Ready(Some(Err(_e))) => std::task::Poll::Ready(None),
            },
        }
    }

    /// Delegate to the inner `Full` for the buffered variant so hyper reports its EXACT
    /// length (a `Content-Length` header, un-chunked — the pre-streaming HTTP-1 behaviour);
    /// the stream variant keeps the default unknown size (chunked / h2 framing), which is
    /// correct for a frame stream whose length is not known up front.
    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            HyperOutBody::Full(full) => full.size_hint(),
            HyperOutBody::Stream(_) => http_body::SizeHint::default(),
        }
    }

    /// The buffered variant is end-of-stream exactly when its inner `Full` is (so a
    /// zero-length body needs no body frame at all); a frame stream is never statically known
    /// to be at its end here.
    fn is_end_stream(&self) -> bool {
        match self {
            HyperOutBody::Full(full) => full.is_end_stream(),
            HyperOutBody::Stream(_) => false,
        }
    }
}

/// Convert a leaf [`Response`] into a hyper `Response<HyperOutBody>`, copying status +
/// headers and writing the body as [`Full`] (the HTTP default) or as a streaming frame
/// body (data + trailers) — the out-half of the streaming `Body`.
fn to_hyper_response(leaf_resp: Response) -> hyper::Response<HyperOutBody> {
    let status = leaf_resp.status();
    let headers = leaf_resp.headers().clone();
    let out = match leaf_resp.into_body() {
        LeafBody::Full(bytes) => HyperOutBody::Full(Full::new(bytes)),
        LeafBody::Stream(stream) => HyperOutBody::Stream(stream),
    };
    let mut builder = hyper::Response::builder().status(status);
    if let Some(h) = builder.headers_mut() {
        h.clone_from(&headers);
    }
    builder.body(out).unwrap_or_else(|_| {
        hyper::Response::builder()
            .status(http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(HyperOutBody::Full(Full::new(Bytes::new())))
            .expect("a bare 500 always builds")
    })
}

/// The transport-edge bind failure (e.g. the port is in use) as a [`LeafError`]. A bind
/// failure is the ONE fatal serve-level fault (the address is unusable); a per-connection
/// accept error, by contrast, is transient and skipped in the loop (we keep serving).
fn bind_error(addr: &str, err: &std::io::Error) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed)
        .caused_by(Cause::plain("binding the web server", format!("{addr}: {err}")))
}
