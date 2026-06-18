//! An in-memory [`MockServer`] backend — the STAGE-1 PLUGGABILITY PROOF.
//!
//! [`MockServer`] implements the leaf [`WebServer`] trait WITHOUT any HTTP transport:
//! it holds nothing but the shared [`Dispatcher`] and exposes [`handle`](MockServer::handle)
//! to drive a leaf [`Request`] straight through the request engine, returning the
//! [`Response`]. It proves the leaf-web abstraction is genuinely backend-free — the
//! SAME [`Dispatcher`] the hyper backend (Stage 3) feeds runs here with no socket, no
//! runtime, no hyper. A test assembles the dispatcher from the container (collection +
//! by-trait injection) and serves through this mock; the only difference from
//! production is the transport edge.
//!
//! This module is gated behind `#[cfg(any(test, feature = "testing"))]`: it is a TEST
//! harness, not production surface. leaf-web's own integration tests enable the
//! `testing` feature (via the package's self dev-dependency); external consumers opt
//! in with `features = ["testing"]`.

use std::sync::Arc;

use leaf_core::{BoxFuture, LeafError};

use crate::server::{Dispatcher, ServerProperties, WebServer};
use crate::{Request, Response};

/// An in-memory [`WebServer`] that dispatches requests directly through a shared
/// [`Dispatcher`] — no transport.
///
/// It is the mock backend the Stage-1 pluggability proof drives: construct it from a
/// container-assembled [`Dispatcher`], then call [`handle`](MockServer::handle) to run
/// a [`Request`] through the full filter → route → advice engine and inspect the
/// [`Response`]. As a real [`WebServer`] impl it also proves the trait is
/// transport-agnostic — a `dyn WebServer` can be this mock or the hyper backend.
pub struct MockServer {
    dispatcher: Arc<Dispatcher>,
}

impl MockServer {
    /// Wrap a shared [`Dispatcher`] as an in-memory backend.
    #[must_use]
    pub fn new(dispatcher: Arc<Dispatcher>) -> Self {
        MockServer { dispatcher }
    }

    /// Drive `req` straight through the dispatcher (filters → route → handler →
    /// advice), returning the resulting [`Response`]. Like [`Dispatcher::dispatch`],
    /// this never errors out — every failure becomes a response. This is the
    /// in-memory analogue of a request arriving on a socket.
    pub async fn handle(&self, req: Request) -> Response {
        self.dispatcher.dispatch(req).await
    }
}

impl WebServer for MockServer {
    /// "Serve" is a no-op for the in-memory backend: there is no socket to bind. It
    /// captures (ignores) the dispatcher/props and returns immediately, so a test can
    /// exercise the [`WebServer`] contract; real request driving goes through
    /// [`handle`](MockServer::handle).
    fn serve<'a>(
        &'a self,
        _dispatcher: Arc<Dispatcher>,
        _props: &'a ServerProperties,
    ) -> BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::{Handler, Route};
    use bytes::Bytes;
    use http::{Method, StatusCode};

    /// A `#[cfg(test)]` fake route (the lone Stage-1 hand-written impl kind) so the
    /// mock-server unit test needs no container.
    struct PingRoute;
    struct PingHandler;
    impl Handler for PingHandler {
        fn handle<'a>(
            &'a self,
            _req: &'a Request,
        ) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async { Ok(Response::ok().with_body(Bytes::from_static(b"pong"))) })
        }
    }
    impl Route for PingRoute {
        fn method(&self) -> Method {
            Method::GET
        }
        fn path(&self) -> &str {
            "/ping"
        }
        fn handler(&self) -> &dyn Handler {
            &PingHandler
        }
    }

    fn get(path: &str) -> Request {
        Request::new(Method::GET, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
    }

    #[test]
    fn mock_server_handle_drives_a_request_through_the_dispatcher() {
        let route: Arc<dyn Route> = Arc::new(PingRoute);
        let dispatcher = Arc::new(Dispatcher::new(vec![route], vec![], vec![]));
        let server = MockServer::new(dispatcher);

        let resp = futures::executor::block_on(server.handle(get("/ping")));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"pong".as_slice());

        // An unmatched route is the dispatcher's default 404 (it never errors out).
        let miss = futures::executor::block_on(server.handle(get("/nope")));
        assert_eq!(miss.status(), StatusCode::NOT_FOUND);
    }
}
