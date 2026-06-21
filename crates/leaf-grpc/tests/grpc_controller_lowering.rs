//! The REAL Stage-4 `#[grpc_controller]` end-to-end proof (the successor to
//! `grpc_di_assembly.rs`'s hand-written Stage-2 route-bean stand-in): a `#[grpc_controller]`
//! struct + `#[grpc_controller] impl ServiceTrait` lowers to a `#[doc(hidden)]` `GrpcRoute`
//! bean that leaf-boot collects as `Vec<Ref<dyn GrpcRoute>>` by collection + by-trait
//! injection, and whose `GrpcHandler::call` decodes + runs + frames a unary RPC end to end.
//!
//! This exercises EVERY load-bearing piece of the stereotype against the SAME seam Stage-3
//! emits (a server trait + a `<service_snake>` module of `<METHOD>_DESCRIPTOR` consts): the
//! descriptor-seam path/shape read (`echo::GET_DESCRIPTOR.path`/`.shape` — never a spelled
//! literal or a type check), the field injection of `Ref<Controller>` + `Ref<ProstCodec>`,
//! the `::leaf_grpc::call_unary` framing wrapper, and the `dyn GrpcRoute` provides[] view.
//! The dual-form `GrpcControllerKind` guard compiling here proves the struct's marker + the
//! impl's assertion agree (a mismatch would be a hard compile error).

use futures::executor::block_on;
use http::{HeaderMap, Method};
use leaf_boot::App;
use leaf_core::{Injectable, Ref, ResolveCtx};
use leaf_grpc::framing::{decode_frames, encode_frame};
use leaf_grpc::{GrpcCodec, GrpcRoute, ProstCodec, Status};
use leaf_web::request::Request;
use leaf_web::{Body, Frame};

// ── a hand-written stand-in for the Stage-3 generated seam (trait + descriptor module) ──
// This is EXACTLY the shape `leaf_grpc_build::service_gen::render_service` emits: a
// `Send + Sync` server trait + a `pub mod <service_snake>` of `<METHOD>_DESCRIPTOR` consts.
// `#[grpc_controller]` reads the path/shape from these consts, never from the message types.

/// One scalar prost message (prost implements `Message` for the well-known scalar wrappers,
/// so a `String` is a complete message we round-trip through the codec — no `#[derive]`/build
/// step needed in this unit-level proof).
type Ping = String;
type Pong = String;

#[allow(async_fn_in_trait)]
pub trait Echo: Send + Sync {
    async fn get(&self, req: Ping) -> Result<Pong, Status>;
}

pub mod echo {
    #[doc(hidden)]
    pub const GET_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = ::leaf_grpc::MethodDescriptor {
        path: "/echo.v1.Echo/Get",
        shape: ::leaf_grpc::CallShape::Unary,
    };
}

// ── the dogfooded #[grpc_controller] controller (the REAL Stage-4 macro) ──

/// The controller BEAN: `#[grpc_controller]` makes it a `#[component]`-equivalent (so it is
/// registered + resolvable) AND emits the `GrpcControllerKind` marker the impl-form guard
/// asserts. No collaborators here (a unit controller).
#[leaf_macros::grpc_controller]
struct EchoController;

impl EchoController {
    fn new() -> Self {
        EchoController
    }
}

impl Default for EchoController {
    fn default() -> Self {
        EchoController::new()
    }
}

/// The RPC ITERATOR: `#[grpc_controller]` desugars the native `async fn` and lowers `get` to
/// a `#[doc(hidden)]` `GrpcRoute` bean (path/shape from `echo::GET_DESCRIPTOR`, controller +
/// codec field-injected, the typed method wrapped through `::leaf_grpc::call_unary`).
#[leaf_macros::grpc_controller]
impl Echo for EchoController {
    async fn get(&self, req: Ping) -> Result<Pong, Status> {
        Ok(format!("{req}-pong"))
    }
}

fn unary_req(path: &str, body: &[u8]) -> Request {
    let mut h = HeaderMap::new();
    h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
    Request::new(Method::POST, path.parse().expect("uri"), h, encode_frame(body))
}

#[test]
fn grpc_controller_route_bean_collects_and_dispatches_a_unary_rpc() {
    // (1) Assemble: leaf-boot lifts every macro-emitted #[component]/bean row (the
    //     EchoController bean, its generated __LeafGrpcRoute_EchoController_get bean, and the
    //     ProstCodec bean the route field-injects) into a builder via the auto-collected
    //     SEED_PAIRINGS base, then freezes to an Engine (the LAZY resolve path).
    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[grpc_controller] row")
        .into_builder()
        .freeze()
        .expect("the grpc-controller beans registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    // (2) The generated GrpcRoute bean is collected by the `dyn GrpcRoute` collection +
    //     by-trait injection — exactly how GrpcDispatch finds every #[grpc_controller] route.
    let routes: Vec<Ref<dyn GrpcRoute>> =
        block_on(<Vec<Ref<dyn GrpcRoute>> as Injectable>::inject(&cx)).expect("routes resolve");
    assert_eq!(routes.len(), 1, "the one RPC method lowered to one GrpcRoute bean");

    // (3) `path()` reads the Stage-3 descriptor seam by method name (never a spelled literal).
    assert_eq!(routes[0].path(), "/echo.v1.Echo/Get");

    // (4) The handler decodes the request, runs the typed method, encodes + frames the
    //     response, and appends Ok trailers — the full unary wrapper path end to end.
    let codec = ProstCodec::new();
    let req = unary_req("/echo.v1.Echo/Get", &codec.encode(&"ping".to_string()));
    let resp = block_on(routes[0].handler().call(req));

    let frames: Vec<Frame> = match resp.into_body() {
        Body::Stream(s) => {
            use futures::StreamExt;
            block_on(s.collect::<Vec<_>>()).into_iter().map(|r| r.expect("frame")).collect()
        }
        Body::Full(_) => panic!("a gRPC response is a streaming body"),
    };
    // First frame: the encoded "ping-pong" message; last frame: the Ok (grpc-status 0) trailer.
    let out: Pong = match &frames[0] {
        Frame::Data(b) => {
            use futures::StreamExt;
            let raw: Vec<_> = block_on(decode_frames(Body::full(b.clone())).collect());
            let msg = raw.into_iter().next().expect("a message").expect("ok");
            codec.decode(&msg).expect("decode Pong")
        }
        Frame::Trailers(_) => panic!("expected a data frame first"),
    };
    assert_eq!(out, "ping-pong", "the typed method ran inside the call_unary wrapper");
    match frames.last().expect("a trailer") {
        Frame::Trailers(t) => {
            assert_eq!(t.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));
        }
        Frame::Data(_) => panic!("expected a trailers frame last"),
    }
}
