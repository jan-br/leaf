//! Cross-cutting integration: the existing `WebFilter` chain + the `GrpcStatusMapper`
//! error model, exercised at the `leaf-web` `Dispatcher` boundary (no hyper/tonic — that
//! is Stage 6). Proves a filter authenticating via gRPC metadata short-circuits to an
//! `Unauthenticated` Status, and a domain `LeafError` maps to a grpc-status — both
//! through the SAME ordered filter chain that wraps HTTP, never a forked mechanism.

use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use leaf_core::{BoxFuture, ContractId, ErrorKind, LeafError};
use leaf_grpc::{Code, DefaultGrpcStatusMapper, GrpcStatusMapper, Status};
use leaf_web::{Dispatcher, Next, ProtocolDispatch, Request, Response, WebFilter};

/// A grpc request carrying (or omitting) an `authorization` metadata header.
fn grpc_call(authed: bool) -> Request {
    let mut h = HeaderMap::new();
    h.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    if authed {
        h.insert(http::header::AUTHORIZATION, HeaderValue::from_static("Bearer ok"));
    }
    Request::new(Method::POST, "/pkg.Svc/M".parse().unwrap(), h, Bytes::new())
}

/// An auth filter: gRPC metadata ARE HTTP/2 headers, so the SAME `WebFilter` inspects
/// `authorization`. On a missing token it short-circuits with an `Unauthenticated`
/// Status rendered as trailers (HTTP 200 + grpc-status=16) — a rejected gRPC call still
/// produces a valid grpc-status trailer, never a raw HTTP body (§6).
struct GrpcAuthFilter;

#[leaf_macros::async_impl]
impl WebFilter for GrpcAuthFilter {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        let is_grpc = req
            .header(http::header::CONTENT_TYPE.as_str())
            .is_some_and(|ct| ct.starts_with("application/grpc"));
        if is_grpc && req.header(http::header::AUTHORIZATION.as_str()).is_none() {
            // Short-circuit: render the Unauthenticated Status as grpc trailers.
            let status = Status::new(Code::Unauthenticated, "missing token");
            return Ok(Response::ok().with_body_stream(leaf_grpc::status_trailers_stream(status)));
        }
        next.run(req).await
    }
}

/// A `ProtocolDispatch` that proves it ran (the authed path reaches it): it renders a
/// `Code::Ok` trailer (the success shape).
struct OkProto;
impl ProtocolDispatch for OkProto {
    fn handles(&self, ct: Option<&str>) -> bool {
        ct.is_some_and(|c| c.starts_with("application/grpc"))
    }
    fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            Ok(Response::ok()
                .with_body_stream(leaf_grpc::status_trailers_stream(Status::new(Code::Ok, ""))))
        })
    }
}

#[tokio::test]
async fn an_unauthenticated_grpc_call_is_short_circuited_to_unauthenticated_trailers() {
    let filter: Arc<dyn WebFilter> = Arc::new(GrpcAuthFilter);
    let proto: Arc<dyn ProtocolDispatch> = Arc::new(OkProto);
    let dispatcher = Dispatcher::new(vec![], vec![filter], vec![], vec![proto]);

    let resp = dispatcher.dispatch(grpc_call(false)).await;
    // gRPC ALWAYS returns HTTP 200; the rejection is in the trailers.
    assert_eq!(resp.status(), StatusCode::OK);
    let trailers = leaf_grpc::collect_trailers(resp.into_body()).await;
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("16"),
        "an unauthenticated gRPC call => grpc-status=16 (Unauthenticated)"
    );
}

#[tokio::test]
async fn an_authenticated_grpc_call_reaches_the_protocol_terminal() {
    let filter: Arc<dyn WebFilter> = Arc::new(GrpcAuthFilter);
    let proto: Arc<dyn ProtocolDispatch> = Arc::new(OkProto);
    let dispatcher = Dispatcher::new(vec![], vec![filter], vec![], vec![proto]);

    let resp = dispatcher.dispatch(grpc_call(true)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let trailers = leaf_grpc::collect_trailers(resp.into_body()).await;
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("0"),
        "the authed call reached OkProto => grpc-status=0 (Ok)"
    );
}

// ── Part 2: a domain LeafError -> grpc-status via the user mapper chain. ──

fn unknown_sku() -> ContractId {
    ContractId::of("storefront::catalog::UnknownSku")
}

/// A user `GrpcStatusMapper`: the unknown-SKU domain `Integration` error -> `NotFound`
/// (the gRPC analogue of the storefront's 404 `ControlAdvice`). Declines everything else.
struct UnknownSkuMapper;
impl GrpcStatusMapper for UnknownSkuMapper {
    fn map(&self, err: &LeafError) -> Option<Status> {
        match err.kind {
            ErrorKind::Integration { kind_id } if kind_id == unknown_sku() => {
                Some(Status::not_found("unknown sku"))
            }
            _ => None,
        }
    }
}

/// A `ProtocolDispatch` that maps a raised domain `LeafError` through the mapper chain
/// (user-first, then the FALLBACK floor) and renders the resulting Status trailers — the
/// in-test stand-in for `GrpcDispatch::status_for` + the shared trailer renderer.
struct DomainErrProto {
    mappers: Vec<Arc<dyn GrpcStatusMapper>>,
}
impl ProtocolDispatch for DomainErrProto {
    fn handles(&self, ct: Option<&str>) -> bool {
        ct.is_some_and(|c| c.starts_with("application/grpc"))
    }
    fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        let mappers = self.mappers.clone();
        Box::pin(async move {
            // The collaborator raised an unknown-SKU domain error.
            let err = LeafError::new(ErrorKind::Integration { kind_id: unknown_sku() });
            let status = leaf_grpc::GrpcDispatch::status_for(&mappers, &err);
            Ok(Response::ok().with_body_stream(leaf_grpc::status_trailers_stream(status)))
        })
    }
}

#[tokio::test]
async fn a_domain_leaf_error_maps_to_a_grpc_status_via_the_user_mapper() {
    // User mapper first, the FALLBACK floor last (the chain GrpcDispatch builds).
    let mappers: Vec<Arc<dyn GrpcStatusMapper>> =
        vec![Arc::new(UnknownSkuMapper), Arc::new(DefaultGrpcStatusMapper::new())];
    let proto: Arc<dyn ProtocolDispatch> = Arc::new(DomainErrProto { mappers });
    let dispatcher = Dispatcher::new(vec![], vec![], vec![], vec![proto]);

    let resp = dispatcher
        .dispatch({
            let mut h = HeaderMap::new();
            h.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
            Request::new(Method::POST, "/pkg.Svc/Get".parse().unwrap(), h, Bytes::new())
        })
        .await;

    assert_eq!(resp.status(), StatusCode::OK, "gRPC always returns HTTP 200");
    let trailers = leaf_grpc::collect_trailers(resp.into_body()).await;
    assert_eq!(
        trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("5"),
        "the unknown-SKU domain LeafError mapped to NotFound (5) via the user mapper"
    );
}
