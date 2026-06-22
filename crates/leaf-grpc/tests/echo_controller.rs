//! The `#[cfg(test)]` gRPC beans the integration proof boots: a #[grpc_controller]
//! implementing the generated `Echo` server trait across all four call shapes (plus the
//! explicit-`Status` `Boom` and the domain-error `Domain`), an auth WebFilter (gRPC
//! metadata = H2 headers, so the SAME filter chain wraps HTTP + gRPC), and a domain
//! GrpcStatusMapper. EVERY one is a stereotype bean — the ONLY hand-written impl is the
//! controller's TYPED method bodies (which the macro lowers to GrpcRoute beans) + the
//! filter/mapper trait bodies (which the macros register as the dyn views). No hand-rolled
//! GrpcRoute/GrpcHandler/ProtocolDispatch/Provider.
//!
//! Drift from the plan: the Stage-3 generator emits the server trait as a TOP-LEVEL `Echo`
//! (not `echo::echo_server::Echo`), the prost messages as TOP-LEVEL structs (`EchoRequest`,
//! …), and the path/descriptor consts in a `pub mod echo`. So the controller `impl Echo for
//! EchoController` uses the bare message names. And there is no `Status::from(LeafError)` —
//! the domain channel is the real `leaf_grpc::map_first` over the COLLECTION-INJECTED mapper
//! chain (user mappers + the FALLBACK floor), so the `Domain` RPC genuinely rides the
//! GrpcStatusMapper SPI to a Status.

use std::sync::atomic::{AtomicU32, Ordering};

use leaf_core::{BoxStream, ContractId, ErrorKind, LeafError, Ref};
use leaf_grpc::{Code, GrpcStatusMapper, Status, Streaming};
use leaf_web::filter::Next;
use leaf_web::{Request, Response, WebFilter};
use futures::StreamExt;

// The generated server trait + message structs + the `echo::*_DESCRIPTOR` path module.
leaf_grpc::include_proto!("echo");

/// The domain ContractId the `Domain` RPC raises (the sanctioned Integration error channel),
/// mirroring the storefront's `unknown_sku_kind` — the GrpcStatusMapper claims it as NotFound.
pub fn missing_kind() -> ContractId {
    ContractId::of("leaf_grpc::tests::Missing")
}

/// The ContractId the missing-key rejection raises; the GrpcStatusMapper maps it to Unauthenticated.
pub fn unauthorized_kind() -> ContractId {
    ContractId::of("leaf_grpc::tests::Unauthorized")
}

/// Counts WebFilter invocations so the proof can assert the filter ran around gRPC too.
pub static FILTER_CALLS: AtomicU32 = AtomicU32::new(0);

// ── the #[grpc_controller] bean + its four-shape RPC impl ────────────────────────

/// The controller BEAN (a #[component]-family bean). It field-injects the SAME ordered
/// `dyn GrpcStatusMapper` chain `GrpcDispatch` collects (user mappers + the FALLBACK floor)
/// so the `Domain` RPC can run a raised domain `LeafError` through it — genuinely the
/// GrpcStatusMapper domain channel (collection + by-trait injection), not a spelled status.
#[leaf_macros::grpc_controller]
pub struct EchoController {
    mappers: Vec<Ref<dyn GrpcStatusMapper>>,
}

#[leaf_macros::grpc_controller]
impl Echo for EchoController {
    // unary:  async fn m(&self, req: T) -> Result<U, Status>
    async fn unary(&self, req: EchoRequest) -> Result<EchoReply, Status> {
        Ok(EchoReply { text: req.text })
    }

    // server: async fn m(&self, req: T) -> Result<Streaming<U>, Status>
    async fn server_stream(&self, req: EchoRequest) -> Result<Streaming<EchoReply>, Status> {
        let words: Vec<EchoReply> = req
            .text
            .split_whitespace()
            .map(|w| EchoReply { text: w.to_string() })
            .collect();
        let stream: BoxStream<'static, Result<EchoReply, Status>> =
            Box::pin(futures::stream::iter(words.into_iter().map(Ok)));
        Ok(Streaming::new(stream))
    }

    // client: async fn m(&self, req: Streaming<T>) -> Result<U, Status>
    async fn client_stream(&self, mut req: Streaming<EchoRequest>) -> Result<CountReply, Status> {
        let mut n = 0u32;
        while let Some(item) = req.next().await {
            item?; // a malformed inbound frame surfaces as a Status
            n += 1;
        }
        Ok(CountReply { n })
    }

    // bidi:   async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>
    async fn bidi(&self, req: Streaming<EchoRequest>) -> Result<Streaming<EchoReply>, Status> {
        let out: BoxStream<'static, Result<EchoReply, Status>> =
            Box::pin(req.map(|item| item.map(|r| EchoReply { text: r.text.to_uppercase() })));
        Ok(Streaming::new(out))
    }

    // explicit Status: the handler returns Err(Status) directly (rendered as trailers).
    async fn boom(&self, _req: EchoRequest) -> Result<EchoReply, Status> {
        Err(Status::invalid_argument("boom: explicit status from the handler"))
    }

    // domain error: raise a LeafError on the Integration channel; map it through the
    // collection-injected GrpcStatusMapper chain (user-first, the FALLBACK floor last) —
    // genuinely the gRPC ControlAdvice analogue. `map_first` always yields Some for the
    // production chain (the floor never declines).
    async fn domain(&self, _req: EchoRequest) -> Result<EchoReply, Status> {
        let err = LeafError::new(ErrorKind::Integration { kind_id: missing_kind() });
        let refs: Vec<&dyn GrpcStatusMapper> = self.mappers.iter().map(|m| &**m).collect();
        Err(leaf_grpc::map_first(&refs, &err)
            .unwrap_or_else(|| Status::new(Code::Unknown, err.to_string())))
    }
}

// ── the auth WebFilter (metadata = H2 headers) ───────────────────────────────────

/// An auth WebFilter: requires an `x-api-key: secret` metadata header (gRPC metadata ARE
/// H2 headers, so the SAME filter chain wraps HTTP + gRPC). Missing/wrong key → an Err the
/// gRPC edge renders as an Unauthenticated Status trailer (via the GrpcStatusMapper chain);
/// a present key continues the chain.
#[leaf_macros::web_filter]
#[derive(Default)]
pub struct ApiKeyFilter;

#[leaf_macros::async_impl]
impl WebFilter for ApiKeyFilter {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        FILTER_CALLS.fetch_add(1, Ordering::SeqCst);
        let ok = req.header("x-api-key").is_some_and(|k| k == "secret");
        if ok {
            next.run(req).await
        } else {
            // The sanctioned domain-error channel: the gRPC edge maps it via the
            // GrpcStatusMapper below; for HTTP the ControlAdvice chain would map it.
            Err(LeafError::new(ErrorKind::Integration { kind_id: unauthorized_kind() }))
        }
    }
}

// ── the domain GrpcStatusMapper (the ControlAdvice analogue for gRPC) ─────────────

/// A domain GrpcStatusMapper: maps the test's two Integration kinds to gRPC Codes. It is a
/// #[component] holder publishing the `dyn GrpcStatusMapper` view via the dogfooded
/// #[configuration] + #[bean(provides = "dyn …")] idiom (a struct stereotype takes no
/// `provides`) — the SAME collection-injection DI the default FALLBACK mapper rides;
/// first-Some wins, so this user mapper supersedes the FALLBACK for its kinds.
#[leaf_macros::component]
pub struct EchoStatusMapperConfig;

impl EchoStatusMapperConfig {
    fn new() -> Self {
        EchoStatusMapperConfig
    }
}

impl Default for EchoStatusMapperConfig {
    fn default() -> Self {
        EchoStatusMapperConfig::new()
    }
}

#[leaf_macros::configuration]
impl EchoStatusMapperConfig {
    #[bean(name = "echoStatusMapper", provides = "dyn ::leaf_grpc::GrpcStatusMapper")]
    fn echo_status_mapper(&self) -> EchoStatusMapper {
        EchoStatusMapper
    }
}

/// The mapper value the bean publishes: the two Integration kinds → gRPC Codes.
pub struct EchoStatusMapper;

impl GrpcStatusMapper for EchoStatusMapper {
    fn map(&self, err: &LeafError) -> Option<Status> {
        match err.kind {
            ErrorKind::Integration { kind_id } if kind_id == missing_kind() => {
                Some(Status::not_found("no such echo resource"))
            }
            ErrorKind::Integration { kind_id } if kind_id == unauthorized_kind() => {
                Some(Status::new(Code::Unauthenticated, "missing or invalid api key"))
            }
            _ => None,
        }
    }
}
