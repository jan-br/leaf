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
//! are `Vec<Ref<dyn _>>` resolved by collection + by-trait injection (Task 7 / the
//! `WebServerRunner` in Stage 3) — no central registry.

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
/// resolvable as `Ref<ServerProperties>`), and the [`WebServerRunner`](crate::WebServerRunner)
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
}

impl Default for ServerProperties {
    /// `127.0.0.1:8080` — a safe local default.
    fn default() -> Self {
        ServerProperties { host: "127.0.0.1".to_string(), port: 8080 }
    }
}

/// The pluggable embedded HTTP server (Spring's `WebServer` over a pluggable
/// Tomcat/Netty). The backend (`leaf-web-hyper`) implements it as a `FALLBACK`
/// auto-config bean; a mock backend implements it for tests — proving the
/// abstraction is transport-agnostic.
///
/// `serve` is dyn-dispatched and async → a [`BoxFuture`] at the `dyn` seam. It
/// takes the shared [`Dispatcher`] (the request engine) plus the bound
/// [`ServerProperties`]; it binds, accepts connections, and drives each request
/// through `dispatcher.dispatch(..)`.
pub trait WebServer: Send + Sync {
    /// Bind per `props` and serve requests through `dispatcher` until shutdown.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] if binding/serving fails at the transport edge
    /// (e.g. the port is in use).
    fn serve<'a>(
        &'a self,
        dispatcher: Arc<Dispatcher>,
        props: &'a ServerProperties,
    ) -> BoxFuture<'a, Result<(), LeafError>>;
}

// The by-trait-injection seam for the pluggable server (emitted once, beside the trait —
// orphan-rule-OK, `dyn WebServer` is local). It makes `Ref<dyn WebServer>` injectable, so
// the `WebServerRunner` resolves whichever `dyn WebServer` bean won (the FALLBACK hyper
// auto-config, or a user-provided backend that supersedes it via OnMissingBean) — the
// same path `Ref<dyn CacheManager>` / `Ref<dyn TransactionManager>` use.
leaf_core::impl_resolve_view!(dyn WebServer);

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
    ) -> Self {
        let mut filters = filters;
        filters.sort_by_key(|f| f.order());
        let mut advice = advice;
        advice.sort_by_key(|a| a.order());
        Dispatcher { routes, filters, advice }
    }

    /// Handle one request, ALWAYS yielding a [`Response`].
    ///
    /// Runs the ordered filter chain (whose terminal matches a route + invokes its
    /// handler); on any `Err(LeafError)` from the chain (filter, extractor, or
    /// handler), maps it via the ordered advice chain, falling back to the default
    /// mapping when no advice claims it. An unmatched route is itself a
    /// [`LeafError`] (mapped to `404` by the default mapping).
    pub async fn dispatch(&self, req: Request) -> Response {
        // Build the routing table over the owned routes (borrow for this call) and
        // the terminal that matches + invokes. Both are locals living across the
        // single `.await` below — no borrow escapes `dispatch`.
        let route_refs: Vec<&dyn Route> = self.routes.iter().map(AsRef::as_ref).collect();
        let table = RouteTable::build(&route_refs);
        let terminal = RouteTerminal { table: &table };

        // The filter chain ends in the route-dispatch terminal.
        let filter_refs: Vec<&dyn WebFilter> = self.filters.iter().map(AsRef::as_ref).collect();
        let chain = FilterChain::new(&filter_refs, &terminal);

        // We need the original request to feed the advice chain on error, but the
        // chain consumes `req`. Clone the lightweight request for the error path.
        let req_for_advice = req.clone();
        match chain.run(req).await {
            Ok(resp) => resp,
            Err(err) => self.map_error(&err, &req_for_advice),
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
/// Convention: a `NoSuchBean` (an unmatched route / missing-resource shape) → `404`;
/// everything else (construction/internal failures, bad extraction) → `500`. Richer,
/// app-specific mappings are contributed as advice beans, never patched here.
fn default_error_response(err: &LeafError) -> Response {
    let status = match err.kind {
        leaf_core::ErrorKind::NoSuchBean => StatusCode::NOT_FOUND,
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

    /// A handler that either succeeds with a fixed body or fails with a fixed kind.
    enum FakeHandler {
        Ok(&'static str),
        Err(ErrorKind),
    }

    impl Handler for FakeHandler {
        fn handle<'a>(
            &'a self,
            _req: &'a Request,
        ) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async move {
                match self {
                    FakeHandler::Ok(body) => {
                        Ok(Response::ok().with_body(Bytes::from_static(body.as_bytes())))
                    }
                    FakeHandler::Err(kind) => Err(LeafError::new(*kind)),
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

        let dispatcher = Dispatcher::new(vec![route], vec![filter], vec![]);
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

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![advice]);
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

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![advice]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/boom")));

        // No advice claimed it → the built-in default maps non-NoSuchBean → 500.
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn unmatched_route_is_the_default_404() {
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/ok", handler: FakeHandler::Ok("hi") });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![]);

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
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![late, early]);

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
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![]);

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
    }
}
