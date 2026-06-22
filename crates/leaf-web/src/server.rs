//! The server seam: [`ServerProperties`] (`@ConfigurationProperties`), the
//! pluggable [`WebServer`] bean, and the protocol-agnostic [`Dispatcher`] — the
//! request engine the backend feeds.
//!
//! The [`Dispatcher`] is the heart of the leaf web layer and is BACKEND-FREE: a
//! backend (`leaf-web-hyper` / the mock) converts its native request to a leaf
//! [`Request`], calls [`Dispatcher::dispatch`], and writes the returned
//! [`Response`] — nothing about HTTP transport leaks in. The dispatcher runs the
//! ordered [`WebFilter`] chain, matches a [`Route`], invokes its
//! [`Handler`](crate::Handler), and — on any `Err(LeafError)` from anywhere in
//! that path — maps it to a
//! [`Response`] via the ordered [`ControlAdvice`] chain (falling back to a default
//! mapping). `dispatch` therefore NEVER errors out: every failure becomes a
//! response.
//!
//! The dispatcher is assembled from the container: the routes, filters, and advice
//! are `Vec<Ref<dyn _>>` resolved by collection + by-trait injection (the
//! [`EmbeddedWebServer`](crate::EmbeddedWebServer) keep-alive bean) — no central registry.

use std::sync::Arc;

use http::StatusCode;
use leaf_core::{BoxFuture, LeafError};

use crate::advice::ControlAdvice;
use crate::filter::{FilterChain, Terminal, WebFilter};
use crate::handler::{Route, RouteOutcome, RouteTable};
use crate::{Request, Response};

/// The embedded web server's bound address — `@ConfigurationProperties` keyed
/// `leaf.web.server` (Spring's `server.address` / `server.port`).
///
/// A `#[config_properties(prefix = "leaf.web.server")]` bean: the run pipeline binds
/// `leaf.web.server.host` / `leaf.web.server.port` from the environment (CLI args / env /
/// config files) purely from the macro-emitted bind thunk, auto-registers it (so it is
/// resolvable as `Ref<ServerProperties>`), and the [`EmbeddedWebServer`](crate::EmbeddedWebServer)
/// injects it and hands it to [`WebServer::serve`]. The abstraction crate owns its shape;
/// the unset default is `127.0.0.1:8080`.
#[leaf_macros::config_properties(prefix = "leaf.web.server")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerProperties {
    /// The bind host (`leaf.web.server.host`, e.g. `"127.0.0.1"` or `"0.0.0.0"`).
    pub host: String,
    /// The bind port (`leaf.web.server.port`; `0` = an ephemeral OS-assigned port, used
    /// by tests).
    pub port: u16,
    /// The maximum request body size, in bytes, the server will buffer
    /// (`leaf.web.server.max-request-body-bytes`). A request whose body exceeds this cap
    /// is rejected with `413 Payload Too Large` at the transport edge BEFORE the whole
    /// body is materialized, so an oversize (or unbounded) body can never exhaust memory.
    /// Defaults to 2 MiB. This is a transport-neutral policy knob: it bounds an
    /// allocation the abstraction layer mandates, naming no backend.
    pub max_request_body_bytes: usize,
}

impl Default for ServerProperties {
    /// `127.0.0.1:8080` with a 2 MiB request-body cap — safe local defaults.
    fn default() -> Self {
        ServerProperties {
            host: "127.0.0.1".to_string(),
            port: 8080,
            max_request_body_bytes: 2 * 1024 * 1024,
        }
    }
}

/// The pluggable embedded HTTP server (Spring's `WebServer` over a pluggable
/// Tomcat/Netty). The backend (`leaf-web-hyper`) implements it as a `FALLBACK`
/// auto-config bean; a mock backend implements it for tests — proving the
/// abstraction is transport-agnostic.
///
/// `serve` is dyn-dispatched and async → a [`BoxFuture`] at the `dyn` seam. It
/// takes the shared [`Dispatcher`] (the request engine), the OWNED
/// [`ServerProperties`], and the [`LifecycleCtx`](leaf_core::LifecycleCtx) the
/// embedded-server [`KeepAlive`](leaf_core::KeepAlive) hands it: it binds, calls
/// [`ctx.on_ready`](leaf_core::LifecycleCtx::on_ready) once it is serving, accepts
/// connections through `dispatcher.dispatch(..)`, parks on
/// [`ctx.shutdown`](leaf_core::LifecycleCtx), then DRAINS (bounded by
/// [`ctx.grace`](leaf_core::LifecycleCtx)) and resolves.
///
/// The signature is `'static` (owned `Arc<ServerProperties>` + a `'static` future,
/// borrowing NOTHING of `&self` across an await) so the embedded-server
/// [`KeepAlive::start`](leaf_core::KeepAlive::start) can spawn it. A backend clones
/// what it needs out of `&self` before its `async move`. NOTHING here names a backend
/// library — `serve` is the backend-free seam; the [`LifecycleCtx`](leaf_core::LifecycleCtx)
/// it takes is a leaf-CORE type (the runtime-neutral shutdown signal), not a runtime.
pub trait WebServer: Send + Sync {
    /// Bind per `props`, serve requests through `dispatcher`, latch readiness via
    /// `ctx.on_ready`, then drain on `ctx.shutdown` (bounded by `ctx.grace`).
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] if binding/serving fails at the transport edge
    /// (e.g. the port is in use).
    fn serve(
        &self,
        dispatcher: Arc<Dispatcher>,
        props: Arc<ServerProperties>,
        ctx: leaf_core::LifecycleCtx,
    ) -> BoxFuture<'static, Result<(), LeafError>>;
}

// The by-trait-injection seam for the pluggable server (emitted once, beside the trait —
// orphan-rule-OK, `dyn WebServer` is local). It makes `Ref<dyn WebServer>` injectable, so
// the `EmbeddedWebServer` resolves whichever `dyn WebServer` bean won (the FALLBACK hyper
// auto-config, or a user-provided backend that supersedes it via OnMissingBean) — the
// same path `Ref<dyn CacheManager>` / `Ref<dyn TransactionManager>` use.
leaf_core::impl_resolve_view!(dyn WebServer);

/// The ABSTRACT protocol-dispatch seam (the design's §1): a second `Handler` family that
/// runs on the SHARED [`WebServer`]/[`Dispatcher`], selected by `content-type`. A request
/// whose `content-type` no HTTP [`Route`] claims is delegated to the first
/// `ProtocolDispatch` whose [`handles`](ProtocolDispatch::handles) returns `true`.
///
/// This is how leaf-web routes to gRPC WITHOUT naming `leaf-grpc`: the gRPC family is ONE
/// `dyn ProtocolDispatch` impl contributed BY `leaf-grpc` (matching `application/grpc*`),
/// so the dep arrow stays `leaf-grpc → leaf-web`, never the reverse. WebSocket etc. plug in
/// the same way. Selection is by the runtime `content-type` HEADER VALUE — never a Rust
/// type's spelled name.
pub trait ProtocolDispatch: Send + Sync {
    /// Whether this protocol claims a request with the given `content-type` (e.g. gRPC
    /// claims any value starting `application/grpc`). `None` = no `content-type` header.
    fn handles(&self, content_type: Option<&str>) -> bool;

    /// Dispatch a claimed request to a [`Response`] (whose [`Body`](crate::Body) may be a
    /// frame stream with trailers — gRPC renders `grpc-status` as trailers, never an `Err`).
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] only for a protocol-level failure the [`ControlAdvice`] chain
    /// should map; a gRPC protocol renders application status as trailers, not `Err`.
    fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>>;

    /// Render a chain [`LeafError`] (from a [`WebFilter`] short-circuit, an extractor, or
    /// this protocol's own `dispatch`) into a PROTOCOL-shaped [`Response`], or decline
    /// (`None`) and let the HTTP [`ControlAdvice`] chain map it.
    ///
    /// This is the protocol analogue of the HTTP advice path: when a `ProtocolDispatch`
    /// CLAIMED a request (its `content-type` matched) but the wrapping filter chain returns
    /// `Err` BEFORE/around the terminal, a generic filter cannot know to emit a protocol
    /// error frame — it just raises a domain `LeafError`. The claiming protocol gets first
    /// refusal to render that error in its own shape (gRPC maps it through its
    /// `GrpcStatusMapper` chain to a `grpc-status` trailer response, so a rejected gRPC call
    /// still reads as a valid `Status`, never a raw HTTP body). The default declines, so a
    /// protocol that has no error shape keeps the HTTP advice behaviour.
    fn render_error(&self, _err: &LeafError) -> Option<Response> {
        None
    }
}

// The by-trait-injection seam (emitted once, beside the trait — orphan-rule-OK,
// `dyn ProtocolDispatch` is local). It makes `Ref<dyn ProtocolDispatch>` injectable and
// `Vec<Ref<dyn ProtocolDispatch>>` collectible through the SAME path `dyn Route`/`dyn
// WebFilter` use — the gRPC family is collected exactly like the HTTP route/filter families.
leaf_core::impl_resolve_view!(dyn ProtocolDispatch);

/// The protocol-agnostic request engine: it owns the container-collected routes,
/// filters, and advice, and turns a leaf [`Request`] into a [`Response`].
///
/// Built once at startup (`Dispatcher::new`, ordering applied there) and shared
/// (`Arc<Dispatcher>`) across every connection. [`dispatch`](Dispatcher::dispatch)
/// runs the ordered [`WebFilter`] chain whose terminal matches a [`Route`] and
/// invokes its [`Handler`](crate::Handler); any `Err` is mapped by the ordered
/// [`ControlAdvice`] chain (or the default mapping). It never errors out.
pub struct Dispatcher {
    /// The container-collected routes (owned as `Arc` so the table can borrow them
    /// per-dispatch). PRODUCTION routes come from the controller macro.
    routes: Vec<Arc<dyn Route>>,
    /// The container-collected filters, ALREADY sorted by [`WebFilter::order`]
    /// (stable). The chain runs them in this order.
    filters: Vec<Arc<dyn WebFilter>>,
    /// The container-collected advice, ALREADY sorted by [`ControlAdvice::order`]
    /// (stable). Consulted first-match-wins on a handler/filter `Err`.
    advice: Vec<Arc<dyn ControlAdvice>>,
    /// The container-collected protocol families (gRPC etc.), checked by `content-type`
    /// BEFORE the HTTP route family. Empty in a pure-HTTP app.
    protocols: Vec<Arc<dyn ProtocolDispatch>>,
}

impl Dispatcher {
    /// Build a dispatcher from the container-collected routes, filters, and advice.
    ///
    /// Ordering is applied HERE (once): filters and advice are stably sorted by
    /// their `order()` so each per-request `dispatch` is a cheap walk. Routes keep
    /// their contributed order (the matcher is first-match-wins).
    #[must_use]
    pub fn new(
        routes: Vec<Arc<dyn Route>>,
        filters: Vec<Arc<dyn WebFilter>>,
        advice: Vec<Arc<dyn ControlAdvice>>,
        protocols: Vec<Arc<dyn ProtocolDispatch>>,
    ) -> Self {
        let mut filters = filters;
        filters.sort_by_key(|f| f.order());
        let mut advice = advice;
        advice.sort_by_key(|a| a.order());
        Dispatcher { routes, filters, advice, protocols }
    }

    /// Handle one request, ALWAYS yielding a [`Response`].
    ///
    /// Runs the ordered filter chain (whose terminal matches a route + invokes its
    /// handler); on any `Err(LeafError)` from the chain (filter, extractor, or
    /// handler), maps it via the ordered advice chain, falling back to the default
    /// mapping when no advice claims it. An unmatched route is itself a
    /// [`LeafError`] (mapped to `404` by the default mapping).
    pub async fn dispatch(&self, mut req: Request) -> Response {
        // Choose the chain's terminal up-front by content-type: if a ProtocolDispatch
        // claims this request (a non-HTTP content-type, e.g. gRPC) the protocol branch
        // IS the terminal; otherwise the HTTP route table is. EITHER WAY the SAME ordered
        // WebFilter chain wraps it — auth/log/trace run uniformly across HTTP and gRPC
        // (§6). leaf-web names no gRPC type: it only sees `dyn ProtocolDispatch`, selected
        // by the runtime content-type HEADER VALUE, never a Rust type's spelled name.
        let content_type = req.header(http::header::CONTENT_TYPE.as_str()).map(str::to_owned);
        let proto = self.protocols.iter().find(|p| p.handles(content_type.as_deref()));

        // The PROTOCOL path reads the frame stream INTACT (gRPC de-frames the body
        // directly); the HTTP path COLLECTS a streamed body to Full BEFORE the route
        // family runs, so every extractor / body_bytes() call sees buffered bytes (REST
        // stays ergonomic). Collection happens ONLY when no protocol claims the request.
        if proto.is_none() && req.body_is_stream() {
            let body = req.take_body();
            match body.collect(usize::MAX).await {
                Ok(collected) => req.set_body(crate::body::Body::Full(collected)),
                Err(err) => {
                    let parts = req.parts_clone();
                    return self.map_error(&err, &parts);
                }
            }
        }

        // Build BOTH possible terminals' backing locals (they must live across the single
        // `.await` below — no borrow escapes `dispatch`). The HTTP route table is built
        // regardless; the protocol terminal borrows the claiming `dyn ProtocolDispatch`.
        let route_refs: Vec<&dyn Route> = self.routes.iter().map(AsRef::as_ref).collect();
        let table = RouteTable::build(&route_refs);
        let route_terminal = RouteTerminal { table: &table };

        // Select the terminal: a claiming ProtocolDispatch wins for its content-type;
        // else the HTTP route table. The protocol-terminal local lives only when used.
        let proto_terminal;
        let terminal: &dyn Terminal = match proto {
            Some(p) => {
                proto_terminal = ProtocolTerminal { proto: p.as_ref() };
                &proto_terminal
            }
            None => &route_terminal,
        };

        // ONE ordered filter chain wraps whichever terminal was chosen.
        let filter_refs: Vec<&dyn WebFilter> = self.filters.iter().map(AsRef::as_ref).collect();
        let chain = FilterChain::new(&filter_refs, terminal);

        // The advice path needs the request PARTS (Request is not Clone); snapshot them
        // before the chain consumes `req`.
        let parts = req.parts_clone();
        match chain.run(req).await {
            Ok(resp) => resp,
            // A chain `Err` (filter short-circuit / extractor / protocol dispatch): if a
            // PROTOCOL claimed this request, it gets first refusal to render the error in
            // its own shape (gRPC → a `grpc-status` trailer response via its mapper chain),
            // so a rejected gRPC call reads as a valid `Status`, never a raw HTTP body.
            // Only when no protocol claimed it (or its `render_error` declines) does the
            // HTTP `ControlAdvice` chain map it — the unchanged HTTP behaviour.
            Err(err) => proto
                .and_then(|p| p.render_error(&err))
                .unwrap_or_else(|| self.map_error(&err, &parts)),
        }
    }

    /// Map a [`LeafError`] to a [`Response`]: the first [`ControlAdvice`] (in order)
    /// that returns `Some` wins; otherwise the built-in default mapping.
    fn map_error(&self, err: &LeafError, req: &Request) -> Response {
        for advice in &self.advice {
            if let Some(resp) = advice.handle(err, req) {
                return resp;
            }
        }
        default_error_response(err)
    }
}

/// The bottom of the filter chain: match the (filtered) request against the
/// [`RouteTable`] and invoke the matched [`Route`]'s [`Handler`](crate::Handler).
/// An unmatched
/// route is a `NoSuchBean` [`LeafError`] (the default mapping turns it into `404`).
struct RouteTerminal<'r> {
    table: &'r RouteTable<'r>,
}

impl Terminal for RouteTerminal<'_> {
    fn dispatch<'a>(&'a self, mut req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            match self.table.match_route(req.method(), req.path()) {
                RouteOutcome::Matched((route, params)) => {
                    // Install the captured path params for the extractors, then run
                    // the route's handler.
                    req.set_path_params(params);
                    route.handler().handle(&req).await
                }
                // A pattern matched the PATH but no route answers this method: a
                // definitive `405` (NOT an error to be mapped by advice / NoSuchBean's
                // `404`). The route table decided; we emit the response directly with
                // the `Allow` header listing the methods that DO match the path.
                RouteOutcome::MethodNotAllowed(allowed) => Ok(method_not_allowed(&allowed)),
                // No pattern matched the path at all → a NoSuchBean (default → 404).
                RouteOutcome::NotFound => Err(route_not_found(&req)),
            }
        })
    }
}

/// The bottom of the filter chain when a non-HTTP protocol (e.g. gRPC) claims the
/// request's content-type: delegate to the claiming [`ProtocolDispatch`]. The SAME
/// ordered [`WebFilter`] chain wraps it, so a filter can authenticate / short-circuit
/// a gRPC call exactly as it does an HTTP one (§6). leaf-web names no gRPC type — only
/// the abstract `dyn ProtocolDispatch` seam.
struct ProtocolTerminal<'p> {
    proto: &'p dyn ProtocolDispatch,
}

impl Terminal for ProtocolTerminal<'_> {
    fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        self.proto.dispatch(req)
    }
}

/// Build the `405 Method Not Allowed` response, carrying the `Allow` header that
/// lists the methods whose patterns matched the requested path (comma-joined, per
/// RFC 9110). An empty `allowed` would never reach here (the terminal only calls
/// this for a non-empty set), but it stays well-formed regardless.
fn method_not_allowed(allowed: &[http::Method]) -> Response {
    let allow = allowed.iter().map(http::Method::as_str).collect::<Vec<_>>().join(", ");
    Response::new(StatusCode::METHOD_NOT_ALLOWED).with_header(http::header::ALLOW, allow)
}

/// The `LeafError` an unmatched route raises (the default mapping → `404`).
fn route_not_found(req: &Request) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::NoSuchBean).caused_by(leaf_core::Cause::plain(
        "no route matched",
        format!("{} {}", req.method(), req.path()),
    ))
}

/// The built-in default error → [`Response`] mapping, applied when no
/// [`ControlAdvice`] claims an error. It is the floor of the advice chain
/// (Spring's `DefaultHandlerExceptionResolver`): a user `ControlAdvice` overrides
/// it by returning `Some` first.
///
/// Convention: a `NoSuchBean` (an unmatched route / missing-resource shape) → `404`; a
/// client-fault (`ConvertError` from a malformed body / missing param, or a
/// `ValidationError` bean-constraint violation) → `400`; everything else
/// (construction/internal failures) → `500`. Richer, app-specific mappings are
/// contributed as advice beans, never patched here.
fn default_error_response(err: &LeafError) -> Response {
    let status = match err.kind {
        leaf_core::ErrorKind::NoSuchBean => StatusCode::NOT_FOUND,
        // A malformed body / missing param / failed bean-validation is a client fault,
        // not a server fault (the design spec promises bad request → 4xx).
        leaf_core::ErrorKind::ConvertError | leaf_core::ErrorKind::ValidationError => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    Response::new(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::Handler;
    use bytes::Bytes;
    use http::Method;
    use leaf_core::ErrorKind;

    // ── `#[cfg(test)]` fakes: the lone hand-written Stage-1 impls. PRODUCTION
    // routes/advice come from the controller / control-advice macros (Stage 2). ──

    /// A fake route whose handler returns a fixed result (a `Response` or an
    /// `Err`), so a test can prove the dispatch/advice path without a real macro.
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

    /// A handler that either succeeds with a fixed body, echoes the (collected) request
    /// body, or fails with a fixed kind.
    enum FakeHandler {
        Ok(&'static str),
        Err(ErrorKind),
        Echo,
    }

    impl Handler for FakeHandler {
        fn handle<'a>(
            &'a self,
            req: &'a Request,
        ) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async move {
                match self {
                    FakeHandler::Ok(body) => {
                        Ok(Response::ok().with_body(Bytes::from_static(body.as_bytes())))
                    }
                    FakeHandler::Err(kind) => Err(LeafError::new(*kind)),
                    FakeHandler::Echo => {
                        Ok(Response::ok().with_body(Bytes::copy_from_slice(req.body_bytes())))
                    }
                }
            })
        }
    }

    /// A control advice that maps a SINGLE target `ErrorKind` to a fixed status.
    struct MapKindAdvice {
        target: ErrorKind,
        status: StatusCode,
        order: i32,
    }

    impl ControlAdvice for MapKindAdvice {
        fn handle(&self, err: &LeafError, _req: &Request) -> Option<Response> {
            (err.kind == self.target).then(|| Response::new(self.status))
        }
        fn order(&self) -> i32 {
            self.order
        }
    }

    /// A filter that records its tag in a shared log then continues (proves the
    /// dispatcher runs the chain around the terminal).
    struct LogFilter {
        tag: &'static str,
        log: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    #[leaf_macros::async_impl]
    impl WebFilter for LogFilter {
        async fn filter(
            &self,
            req: Request,
            next: crate::filter::Next<'_>,
        ) -> Result<Response, LeafError> {
            self.log.lock().expect("log lock").push(self.tag);
            next.run(req).await
        }
    }

    fn get(path: &str) -> Request {
        Request::new(Method::GET, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
    }

    #[test]
    fn successful_route_response_flows_back_through_filters() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/ok", handler: FakeHandler::Ok("hi") });
        let filter: Arc<dyn WebFilter> =
            Arc::new(LogFilter { tag: "log", log: log.clone() });

        let dispatcher = Dispatcher::new(vec![route], vec![filter], vec![], vec![]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/ok")));

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"hi".as_slice());
        // The filter ran (the chain wrapped the terminal).
        assert_eq!(*log.lock().expect("log"), vec!["log"]);
    }

    #[test]
    fn handler_error_is_mapped_by_a_matching_control_advice() {
        let route: Arc<dyn Route> = Arc::new(FakeRoute {
            method: Method::GET,
            path: "/boom",
            handler: FakeHandler::Err(ErrorKind::ConstructionFailed),
        });
        // Advice maps the handler's ConstructionFailed → 404 (proves advice, not the
        // default 500, claims it).
        let advice: Arc<dyn ControlAdvice> = Arc::new(MapKindAdvice {
            target: ErrorKind::ConstructionFailed,
            status: StatusCode::NOT_FOUND,
            order: 0,
        });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![advice], vec![]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/boom")));

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn handler_error_with_no_matching_advice_is_the_default_500() {
        let route: Arc<dyn Route> = Arc::new(FakeRoute {
            method: Method::GET,
            path: "/boom",
            handler: FakeHandler::Err(ErrorKind::ConstructionFailed),
        });
        // Advice that does NOT match (targets a different kind) → declines → default.
        let advice: Arc<dyn ControlAdvice> = Arc::new(MapKindAdvice {
            target: ErrorKind::ValidationError,
            status: StatusCode::BAD_REQUEST,
            order: 0,
        });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![advice], vec![]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/boom")));

        // No advice claimed it → the built-in default maps non-NoSuchBean → 500.
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn unmatched_route_is_the_default_404() {
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/ok", handler: FakeHandler::Ok("hi") });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);

        let resp = futures::executor::block_on(dispatcher.dispatch(get("/nope")));
        // An unmatched route is a NoSuchBean LeafError → default mapping → 404.
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn first_matching_advice_in_order_wins() {
        let route: Arc<dyn Route> = Arc::new(FakeRoute {
            method: Method::GET,
            path: "/boom",
            handler: FakeHandler::Err(ErrorKind::ConstructionFailed),
        });
        // Two advices both claim ConstructionFailed; the lower-order one (consulted
        // first) must win. Declared out of order to prove the sort, not declaration.
        let late: Arc<dyn ControlAdvice> = Arc::new(MapKindAdvice {
            target: ErrorKind::ConstructionFailed,
            status: StatusCode::BAD_GATEWAY,
            order: 10,
        });
        let early: Arc<dyn ControlAdvice> = Arc::new(MapKindAdvice {
            target: ErrorKind::ConstructionFailed,
            status: StatusCode::NOT_FOUND,
            order: 1,
        });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![late, early], vec![]);

        let resp = futures::executor::block_on(dispatcher.dispatch(get("/boom")));
        // early (order 1) wins → its 404, not late's 502.
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    fn req(method: Method, path: &str) -> Request {
        Request::new(method, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
    }

    #[test]
    fn wrong_method_on_a_matched_path_is_405_with_allow() {
        // GET /x is registered; a POST /x must resolve to 405 (path matched, method
        // mismatched) with an `Allow` header listing the matched methods — NOT a 404
        // (which stays reserved for a genuinely unmatched path / NoSuchBean).
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/x", handler: FakeHandler::Ok("ok") });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);

        let resp = futures::executor::block_on(dispatcher.dispatch(req(Method::POST, "/x")));

        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            resp.headers().get(http::header::ALLOW).and_then(|v| v.to_str().ok()),
            Some("GET"),
            "405 must carry an Allow header listing the matched methods"
        );
    }

    #[test]
    fn server_properties_default_is_local_8080() {
        let props = ServerProperties::default();
        assert_eq!(props.host, "127.0.0.1");
        assert_eq!(props.port, 8080);
        // The body-size cap defaults to 2 MiB — a sane ceiling that protects against an
        // unbounded body allocation (OOM/DoS) while comfortably fitting ordinary JSON.
        assert_eq!(props.max_request_body_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn max_request_body_bytes_binds_from_config_like_host_and_port() {
        // The cap is a plain `#[config_properties]` field, so it binds from the
        // `leaf.web.server.*` namespace exactly like host/port (kebab key →
        // snake_case field), proving it is operator-configurable.
        use leaf_core::{
            Binder, CanonicalName, ConversionService, EnvBuilder, MapPropertySource,
            NoopBindHandler, StackCps,
        };
        use std::sync::Arc;

        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs(
            "test",
            [
                ("leaf.web.server.host".to_string(), "0.0.0.0".to_string()),
                ("leaf.web.server.port".to_string(), "9090".to_string()),
                ("leaf.web.server.max-request-body-bytes".to_string(), "4096".to_string()),
            ],
        )));
        let cps = StackCps::new(b.seal_env());
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("leaf.web.server").unwrap();

        let bound = binder
            .bind::<ServerProperties>(&prefix)
            .bound()
            .expect("ServerProperties binds from the env");
        assert_eq!(bound.host, "0.0.0.0");
        assert_eq!(bound.port, 9090);
        assert_eq!(bound.max_request_body_bytes, 4096);
    }

    #[test]
    fn a_convert_error_maps_to_the_default_400_not_500() {
        // A malformed body / missing param is `ErrorKind::ConvertError`; the design spec
        // promises bad-request → 4xx, so the default floor must map it to 400 (NOT the
        // generic 500). A handler that fails with ConvertError (what the Json/Header
        // extractors raise) flows through the default mapping with no advice.
        let route: Arc<dyn Route> = Arc::new(FakeRoute {
            method: Method::GET,
            path: "/bad",
            handler: FakeHandler::Err(ErrorKind::ConvertError),
        });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/bad")));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_validation_error_maps_to_the_default_400() {
        // A bean-validation violation is likewise a client-fault → 400 at the default floor.
        let route: Arc<dyn Route> = Arc::new(FakeRoute {
            method: Method::GET,
            path: "/invalid",
            handler: FakeHandler::Err(ErrorKind::ValidationError),
        });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/invalid")));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── ProtocolDispatch: the abstract protocol-routing seam (gRPC plugs in here) ──

    /// A fake protocol-dispatch that CLAIMS one content-type and answers a fixed status,
    /// so the dispatcher test can prove the branch without naming leaf-grpc.
    struct FakeProtocol {
        claims: &'static str,
        status: StatusCode,
    }

    impl crate::server::ProtocolDispatch for FakeProtocol {
        fn handles(&self, content_type: Option<&str>) -> bool {
            content_type.is_some_and(|ct| ct.starts_with(self.claims))
        }
        fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async move { Ok(Response::new(self.status)) })
        }
    }

    #[test]
    fn protocol_dispatch_handles_matches_by_content_type_prefix() {
        let p = FakeProtocol { claims: "application/grpc", status: StatusCode::OK };
        assert!(p.handles(Some("application/grpc")));
        assert!(p.handles(Some("application/grpc+proto")));
        assert!(!p.handles(Some("application/json")));
        assert!(!p.handles(None));
    }

    fn grpc_req(path: &str) -> Request {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
        Request::new(Method::POST, path.parse().expect("uri"), headers, Bytes::new())
    }

    #[test]
    fn a_grpc_content_type_is_delegated_to_the_protocol_dispatch() {
        // GET /ok is a registered HTTP route, but THIS request is application/grpc, so the
        // protocol family must claim it (proving the content-type branch, not the route).
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::POST, path: "/ok", handler: FakeHandler::Ok("http") });
        let proto: Arc<dyn ProtocolDispatch> =
            Arc::new(FakeProtocol { claims: "application/grpc", status: StatusCode::ACCEPTED });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(grpc_req("/ok")));

        // 202 ACCEPTED comes from the protocol family, NOT the route's 200 "http".
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    /// A fake protocol dispatch that claims `application/grpc*` and echoes a fixed
    /// body so a test can prove the dispatcher reached the protocol branch.
    struct FakeProto {
        claim: &'static str,
        body: &'static str,
    }

    impl crate::server::ProtocolDispatch for FakeProto {
        fn handles(&self, content_type: Option<&str>) -> bool {
            content_type.is_some_and(|ct| ct.starts_with(self.claim))
        }
        fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async move {
                Ok(Response::ok().with_body(Bytes::from_static(self.body.as_bytes())))
            })
        }
    }

    #[test]
    fn filter_chain_wraps_the_protocol_dispatch_terminal() {
        // A grpc-content-type request: NO HTTP route claims it, so the dispatcher
        // must run the SAME ordered filter chain around the ProtocolDispatch branch
        // (the auth/log/trace filters are uniform across HTTP and gRPC, §6).
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let filter: Arc<dyn WebFilter> = Arc::new(LogFilter { tag: "log", log: log.clone() });
        let proto: Arc<dyn crate::server::ProtocolDispatch> =
            Arc::new(FakeProto { claim: "application/grpc", body: "grpc-ok" });

        let dispatcher = Dispatcher::new(vec![], vec![filter], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(grpc_req("/pkg.Svc/M")));

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"grpc-ok".as_slice());
        // The filter ran AROUND the protocol terminal — gRPC is filtered too.
        assert_eq!(*log.lock().expect("log"), vec!["log"]);
    }

    #[test]
    fn a_filter_can_short_circuit_a_grpc_request_before_the_protocol_terminal() {
        // The auth analogue: a filter short-circuits a grpc request (returns its own
        // Response without calling next), so the ProtocolDispatch terminal NEVER runs.
        let proto: Arc<dyn crate::server::ProtocolDispatch> =
            Arc::new(FakeProto { claim: "application/grpc", body: "grpc-ok" });
        struct Block;
        #[leaf_macros::async_impl]
        impl WebFilter for Block {
            async fn filter(
                &self,
                _req: Request,
                _next: crate::filter::Next<'_>,
            ) -> Result<Response, LeafError> {
                Ok(Response::new(StatusCode::FORBIDDEN))
            }
        }
        let blocker: Arc<dyn WebFilter> = Arc::new(Block);

        let dispatcher = Dispatcher::new(vec![], vec![blocker], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(grpc_req("/pkg.Svc/M")));

        // The filter short-circuited: 403, and the protocol body never appeared.
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(resp.body_bytes().is_empty());
    }

    #[test]
    fn a_non_grpc_content_type_stays_on_the_http_route_family() {
        // A plain request (no application/grpc content-type) runs the HTTP route family even
        // when a ProtocolDispatch is present — the protocol must DECLINE it.
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/ok", handler: FakeHandler::Ok("http") });
        let proto: Arc<dyn ProtocolDispatch> =
            Arc::new(FakeProtocol { claims: "application/grpc", status: StatusCode::ACCEPTED });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/ok")));

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"http".as_slice());
    }

    #[test]
    fn a_streamed_http_body_is_collected_before_the_route_handler() {
        use crate::body::{Body, Frame};
        // An HTTP request arriving with a STREAM body (no grpc content-type) must be
        // collected to Full before the route handler runs, so the Echo handler sees the
        // buffered bytes via body_bytes() — proving collect-before-handler keeps REST
        // ergonomic.
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::POST, path: "/echo", handler: FakeHandler::Echo });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);

        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"strea"))),
            Ok(Frame::Data(Bytes::from_static(b"med"))),
        ]);
        let mut req = Request::new(Method::POST, "/echo".parse().expect("uri"), http::HeaderMap::new(), Bytes::new());
        req.set_body(Body::Stream(Box::pin(frames)));

        let resp = futures::executor::block_on(dispatcher.dispatch(req));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"streamed".as_slice());
    }
}
