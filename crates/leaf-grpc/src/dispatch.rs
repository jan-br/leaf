//! [`GrpcDispatch`] — the `leaf_web::ProtocolDispatch` impl (O(1) routing).

use std::collections::HashMap;
use std::sync::Arc;

use http::{HeaderMap, HeaderValue};
use leaf_core::{BoxFuture, LeafError, Ref};
use leaf_web::{Frame, ProtocolDispatch, Request, Response};

use crate::handler::GrpcRoute;
use crate::status::Status;

/// Render a [`Status`] as a STATUS-ONLY gRPC response (no data frames, just the
/// `grpc-status`/`grpc-message` trailer frame). The shape an unknown method or a
/// short-circuited request takes. Backend-free: a leaf [`Response`] with a
/// `Body::Stream` of one [`Frame::Trailers`].
#[must_use]
pub fn status_response(status: &Status) -> Response {
    let mut trailers = HeaderMap::new();
    // The discriminant IS the wire number (Code is repr(i32)), so no lookup table.
    let code_str = (status.code as i32).to_string();
    trailers.insert(
        "grpc-status",
        HeaderValue::from_str(&code_str).unwrap_or(HeaderValue::from_static("2")),
    );
    if !status.message.is_empty()
        && let Ok(v) = HeaderValue::from_str(&status.message)
    {
        trailers.insert("grpc-message", v);
    }
    let out = futures::stream::once(async move { Ok::<_, LeafError>(Frame::Trailers(trailers)) });
    Response::ok()
        .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
        .with_body_stream(Box::pin(out))
}

/// The gRPC [`ProtocolDispatch`] impl (the design's §4 protocol branch): leaf-web's
/// `Dispatcher` collection-injects `Vec<Arc<dyn ProtocolDispatch>>` and delegates any
/// request whose content-type no HTTP route claims to the first dispatch whose
/// [`handles`](ProtocolDispatch::handles) is true. This claims `application/grpc*`.
///
/// gRPC method paths are full literals, so routing is an O(1) `HashMap` lookup built
/// once from the collection-injected routes — no pattern matching. An unknown method
/// renders `Code::Unimplemented` trailers (never an `Err`). It is PROVIDED as
/// `dyn ProtocolDispatch` by the [`GrpcDispatchConfig`] `#[bean]` factory below,
/// field-injecting `Vec<Ref<dyn GrpcRoute>>`.
pub struct GrpcDispatch {
    /// Every `#[grpc_controller]`-contributed route (collection + by-trait injection).
    /// Field-injected; the O(1) map is built per dispatch off it (cheap — a
    /// borrow-and-find — see `routes_map`).
    routes: Vec<Ref<dyn GrpcRoute>>,
}

impl GrpcDispatch {
    /// The constructor the `#[bean]` provider calls: it injects the route collection
    /// (the `#[bean]` factory wires this from `Vec<Ref<dyn GrpcRoute>>`).
    #[must_use]
    pub fn new(routes: Vec<Ref<dyn GrpcRoute>>) -> Self {
        GrpcDispatch { routes }
    }

    /// A test/explicit constructor over already-resolved `Arc<dyn GrpcRoute>`s (the
    /// `Ref` newtype wraps an `Arc`, so this is the same shape DI produces).
    #[must_use]
    pub fn from_routes(routes: Vec<Arc<dyn GrpcRoute>>) -> Self {
        GrpcDispatch { routes: routes.into_iter().map(Ref::from_arc).collect() }
    }

    /// Build the O(1) `path -> route` map once (a borrow of the injected collection).
    fn routes_map(&self) -> HashMap<&str, &dyn GrpcRoute> {
        self.routes.iter().map(|r| (r.path(), &**r)).collect()
    }
}

impl ProtocolDispatch for GrpcDispatch {
    fn handles(&self, content_type: Option<&str>) -> bool {
        // Claim every gRPC content-type (`application/grpc`, `application/grpc+proto`,
        // `application/grpc+<codec>`): a prefix test, never an exact-name match.
        content_type.is_some_and(|ct| ct.starts_with("application/grpc"))
    }

    fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            let map = self.routes_map();
            match map.get(req.path()) {
                // Known method: hand the framed request to its handler (which NEVER
                // errors — it renders any Status as trailers), wrapped as Ok.
                Some(route) => Ok(route.handler().call(req).await),
                // Unknown method: Unimplemented, rendered as trailers (never an Err —
                // a gRPC client must still read a valid grpc-status, not an HTTP body).
                None => Ok(status_response(&Status::unimplemented(format!(
                    "no gRPC method registered for {}",
                    req.path()
                )))),
            }
        })
    }
}

/// The `#[component]` holder that PROVIDES [`GrpcDispatch`] as the `dyn ProtocolDispatch`
/// view. A struct stereotype takes no `provides`; the `#[configuration]` +
/// `#[bean(provides = "dyn …")]` factory is leaf's dogfooded idiom for a concrete bean
/// that publishes a `dyn` view AND injects collaborators (the same shape leaf-serde's
/// `JsonConverterConfig` uses). The factory field-injects `Vec<Ref<dyn GrpcRoute>>`, so
/// `GrpcDispatch` collects every `#[grpc_controller]` route by collection + by-trait
/// injection — no hand-rolled `Provider`.
#[leaf_macros::component]
pub struct GrpcDispatchConfig;

impl GrpcDispatchConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        GrpcDispatchConfig
    }
}

impl Default for GrpcDispatchConfig {
    fn default() -> Self {
        GrpcDispatchConfig::new()
    }
}

#[leaf_macros::configuration]
impl GrpcDispatchConfig {
    /// Contribute [`GrpcDispatch`] as the `dyn ProtocolDispatch` bean, field-injecting
    /// the collection of every contributed `dyn GrpcRoute`.
    #[bean(name = "grpcDispatch", provides = "dyn ::leaf_web::ProtocolDispatch")]
    fn grpc_dispatch(&self, routes: Vec<Ref<dyn GrpcRoute>>) -> GrpcDispatch {
        GrpcDispatch::new(routes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::encode_frame;
    use crate::handler::{GrpcHandler, GrpcRoute};
    use bytes::Bytes;
    use futures::executor::block_on;
    use futures::StreamExt;
    use http::{HeaderMap, Method};
    use leaf_core::BoxFuture;
    use leaf_web::request::Request;
    use leaf_web::{Body, Frame};
    use leaf_web::ProtocolDispatch;
    use std::sync::Arc;

    struct OkHandler;
    impl GrpcHandler for OkHandler {
        fn call<'a>(&'a self, _req: Request) -> BoxFuture<'a, leaf_web::Response> {
            Box::pin(async move {
                let mut trailers = HeaderMap::new();
                trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                let out =
                    futures::stream::iter(vec![Ok::<_, leaf_core::LeafError>(Frame::Trailers(trailers))]);
                leaf_web::Response::ok()
                    .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
                    .with_body_stream(Box::pin(out))
            })
        }
    }
    struct OkRoute;
    impl GrpcRoute for OkRoute {
        fn path(&self) -> &str {
            "/pkg.Svc/Known"
        }
        fn handler(&self) -> &dyn GrpcHandler {
            &OkHandler
        }
    }

    fn grpc_req(path: &str) -> Request {
        let mut h = HeaderMap::new();
        h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
        // `Request::new` wraps the given `Bytes` in a `Body::Full` — pass the framed bytes.
        Request::new(Method::POST, path.parse().expect("uri"), h, encode_frame(b""))
    }

    #[test]
    fn handles_only_grpc_content_types() {
        let d = GrpcDispatch::from_routes(vec![]);
        assert!(d.handles(Some("application/grpc")));
        assert!(d.handles(Some("application/grpc+proto")));
        assert!(!d.handles(Some("application/json")));
        assert!(!d.handles(None));
    }

    #[test]
    fn known_method_dispatches_to_its_handler_with_ok_trailers() {
        let route: Arc<dyn GrpcRoute> = Arc::new(OkRoute);
        let d = GrpcDispatch::from_routes(vec![route]);
        let resp = block_on(d.dispatch(grpc_req("/pkg.Svc/Known"))).expect("dispatch never errs");
        let last = match resp.into_body() {
            Body::Stream(s) => block_on(s.collect::<Vec<_>>())
                .into_iter()
                .map(|r| r.expect("frame"))
                .last()
                .expect("a trailer"),
            Body::Full(_) => panic!("grpc body is a stream"),
        };
        match last {
            Frame::Trailers(t) => {
                assert_eq!(t.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));
            }
            Frame::Data(_) => panic!("expected trailers, got a data frame"),
        }
    }

    #[test]
    fn unknown_method_renders_unimplemented_trailers_never_an_err() {
        let d = GrpcDispatch::from_routes(vec![]);
        // No route claims this path → Unimplemented, rendered as trailers, Ok(resp).
        let resp = block_on(d.dispatch(grpc_req("/pkg.Svc/Nope"))).expect("dispatch never errs");
        let frames: Vec<Frame> = match resp.into_body() {
            Body::Stream(s) => block_on(s.collect::<Vec<_>>())
                .into_iter()
                .map(|r| r.expect("frame"))
                .collect(),
            Body::Full(_) => panic!("grpc body is a stream"),
        };
        let trailers = frames.iter().rev().find_map(|f| match f {
            Frame::Trailers(t) => Some(t),
            Frame::Data(_) => None,
        }).expect("a trailers frame");
        assert_eq!(
            trailers.get("grpc-status").unwrap(),
            &http::HeaderValue::from_static("12"),
            "unknown method → grpc-status 12 (Unimplemented)"
        );
        let _ = Bytes::new();
    }
}
