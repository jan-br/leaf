//! [`GrpcDispatch`] — the `leaf_web::ProtocolDispatch` impl (O(1) routing).

use std::collections::HashMap;
use std::sync::Arc;

use leaf_core::{BoxFuture, BoxStream, LeafError, Ref};
use leaf_web::{Body, Frame, ProtocolDispatch, Request, Response};

use crate::handler::GrpcRoute;
use crate::mapper::GrpcStatusMapper;
use crate::status::{Code, Status};

/// A one-element [`Body::Stream`] payload carrying ONLY the gRPC status trailers —
/// the wire shape for an empty (or short-circuited) gRPC response: HTTP 200, no data
/// frames, a single `Frame::Trailers(grpc-status/grpc-message)`. Backend-free
/// (`BoxStream` is `futures`, not hyper). Used by the GrpcHandler success/error edges
/// and by a filter short-circuiting a gRPC call. The single trailer-stream builder so
/// explicit-Status and mapped-LeafError trailers are byte-identical.
#[must_use]
pub fn status_trailers_stream(status: Status) -> BoxStream<'static, Result<Frame, LeafError>> {
    let trailers = status.to_trailers();
    Box::pin(futures::stream::once(async move { Ok(Frame::Trailers(trailers)) }))
}

/// Drain a [`Body`] and return its trailers (the last `Frame::Trailers` map), ignoring
/// any data frames. A test/utility helper for asserting a rendered gRPC response's
/// grpc-status; it walks the abstract `leaf_web::Body`, naming no backend.
pub async fn collect_trailers(body: Body) -> http::HeaderMap {
    use futures::StreamExt;
    match body {
        Body::Full(_) => http::HeaderMap::new(),
        Body::Stream(mut s) => {
            let mut trailers = http::HeaderMap::new();
            while let Some(frame) = s.next().await {
                if let Ok(Frame::Trailers(t)) = frame {
                    trailers = t;
                }
            }
            trailers
        }
    }
}

/// Render a [`Status`] as a STATUS-ONLY gRPC response (no data frames, just the
/// `grpc-status`/`grpc-message` trailer frame). The shape an unknown method, a
/// mapped handler `LeafError`, or a short-circuited request takes. Backend-free: a leaf
/// [`Response`] with a `Body::Stream` of one [`Frame::Trailers`] over HTTP 200 — gRPC's
/// failure lives in the trailers, never the HTTP status.
#[must_use]
pub fn status_response(status: &Status) -> Response {
    Response::ok()
        .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
        .with_body_stream(status_trailers_stream(status.clone()))
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
    /// The ordered `LeafError -> Status` mappers (user mappers + the FALLBACK floor),
    /// field-injected as the `dyn GrpcStatusMapper` collection. Consulted first-`Some`-
    /// wins to render a domain/framework error (or a filter short-circuit) as grpc-status
    /// trailers — the gRPC analogue of leaf-web's `ControlAdvice` chain.
    mappers: Vec<Ref<dyn GrpcStatusMapper>>,
}

impl GrpcDispatch {
    /// The constructor the `#[bean]` provider calls: it injects the route collection AND
    /// the mapper collection (the `#[bean]` factory wires both from their `Vec<Ref<dyn _>>`).
    #[must_use]
    pub fn new(routes: Vec<Ref<dyn GrpcRoute>>, mappers: Vec<Ref<dyn GrpcStatusMapper>>) -> Self {
        GrpcDispatch { routes, mappers }
    }

    /// A test/explicit constructor over already-resolved `Arc<dyn GrpcRoute>`s (the
    /// `Ref` newtype wraps an `Arc`, so this is the same shape DI produces). The mapper
    /// chain is seeded with the [`DefaultGrpcStatusMapper`](crate::DefaultGrpcStatusMapper)
    /// FALLBACK floor alone — the SAME floor production always has present — so the
    /// unknown-method path maps `NoSuchBean -> Unimplemented` exactly as DI does.
    #[must_use]
    pub fn from_routes(routes: Vec<Arc<dyn GrpcRoute>>) -> Self {
        let floor: Arc<dyn GrpcStatusMapper> = Arc::new(crate::mapper::DefaultGrpcStatusMapper::new());
        GrpcDispatch {
            routes: routes.into_iter().map(Ref::from_arc).collect(),
            mappers: vec![Ref::from_arc(floor)],
        }
    }

    /// Build the O(1) `path -> route` map once (a borrow of the injected collection).
    fn routes_map(&self) -> HashMap<&str, &dyn GrpcRoute> {
        self.routes.iter().map(|r| (r.path(), &**r)).collect()
    }

    /// A shareable `Arc` view of the injected mapper chain (the `Ref` newtype wraps an
    /// `Arc`), the slice [`status_for`](Self::status_for) consumes.
    fn mappers_as_arcs(&self) -> Vec<Arc<dyn GrpcStatusMapper>> {
        self.mappers.iter().cloned().map(Ref::into_arc).collect()
    }

    /// Map a handler/filter [`LeafError`] to a [`Status`] via the ordered mapper chain
    /// (user mappers consulted first, the `DefaultGrpcStatusMapper` FALLBACK last). The
    /// floor never declines, so the production chain always yields a `Status`; if NO
    /// mapper is wired at all (degenerate), it defaults to [`Code::Unknown`] so a
    /// well-formed trailer is still produced. Pure + backend-free.
    #[must_use]
    pub fn status_for(mappers: &[Arc<dyn GrpcStatusMapper>], err: &LeafError) -> Status {
        for m in mappers {
            if let Some(s) = m.map(err) {
                return s;
            }
        }
        Status::new(Code::Unknown, err.to_string())
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
                // Unknown method: a `NoSuchBean` LeafError run through the ordered mapper
                // chain (user mappers first, the FALLBACK floor last — which maps
                // NoSuchBean -> Unimplemented). Rendered as trailers (never an Err — a
                // gRPC client must still read a valid grpc-status, not an HTTP body). This
                // is the SAME mapper channel a handler-surfaced domain error rides, so a
                // user mapper can even reshape "unknown method".
                None => {
                    let err = LeafError::new(leaf_core::ErrorKind::NoSuchBean).caused_by(
                        leaf_core::Cause::plain(
                            "no gRPC method registered",
                            req.path().to_owned(),
                        ),
                    );
                    let status = Self::status_for(&self.mappers_as_arcs(), &err);
                    Ok(status_response(&status))
                }
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
    /// BOTH the collection of every contributed `dyn GrpcRoute` AND the ordered
    /// `dyn GrpcStatusMapper` chain (user mappers + the FALLBACK floor) — the gRPC
    /// analogue of leaf-web's `ControlAdvice` chain, collected the SAME DI way.
    #[bean(name = "grpcDispatch", provides = "dyn ::leaf_web::ProtocolDispatch")]
    fn grpc_dispatch(
        &self,
        routes: Vec<Ref<dyn GrpcRoute>>,
        mappers: Vec<Ref<dyn GrpcStatusMapper>>,
    ) -> GrpcDispatch {
        GrpcDispatch::new(routes, mappers)
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
    fn a_handler_leaf_error_is_rendered_as_status_trailers_via_the_mapper_chain() {
        use crate::mapper::{DefaultGrpcStatusMapper, GrpcStatusMapper};
        use crate::status::{Code, Status};
        use leaf_core::{ContractId, ErrorKind, LeafError};
        use std::sync::Arc;

        // A user mapper claiming a domain Integration{kind_id} -> NotFound (the gRPC
        // analogue of the storefront's unknown-SKU -> 404 advice).
        fn unknown_sku() -> ContractId {
            ContractId::of("storefront::catalog::UnknownSku")
        }
        struct DomainMapper;
        impl GrpcStatusMapper for DomainMapper {
            fn map(&self, err: &LeafError) -> Option<Status> {
                match err.kind {
                    ErrorKind::Integration { kind_id } if kind_id == unknown_sku() => {
                        Some(Status::not_found("unknown sku"))
                    }
                    _ => None,
                }
            }
        }

        // The mapper chain: the user mapper FIRST, the FALLBACK floor LAST.
        let mappers: Vec<Arc<dyn GrpcStatusMapper>> =
            vec![Arc::new(DomainMapper), Arc::new(DefaultGrpcStatusMapper::new())];

        // GrpcDispatch's pure error->trailers entry point (no transport): given a
        // handler LeafError, it consults the chain (user-first) and renders trailers.
        let err = LeafError::new(ErrorKind::Integration { kind_id: unknown_sku() });
        let trailers = GrpcDispatch::status_for(&mappers, &err).to_trailers();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("5"),
            "the domain Integration error mapped to NotFound (5) via the user mapper"
        );

        // An UNCLAIMED error falls through to the FALLBACK floor -> Unknown (2).
        let other = LeafError::new(ErrorKind::ConstructionFailed);
        let s = GrpcDispatch::status_for(&mappers, &other);
        assert_eq!(s.code, Code::Unknown);
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
