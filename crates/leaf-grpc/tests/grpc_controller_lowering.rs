//! The REAL Stage-4 `#[grpc_controller]` end-to-end proof (the successor to
//! `grpc_di_assembly.rs`'s hand-written Stage-2 route-bean stand-in): a `#[grpc_controller]`
//! struct + `#[grpc_controller] impl ServiceTrait` lowers each RPC to a `#[doc(hidden)]`
//! `GrpcRoute` bean that leaf-boot collects as `Vec<Ref<dyn GrpcRoute>>` by collection +
//! by-trait injection, and whose `GrpcHandler::call` decodes + runs + frames the RPC end to
//! end — exercised here for ALL FOUR call shapes (unary / server-stream / client-stream /
//! bidi).
//!
//! This exercises EVERY load-bearing piece of the stereotype against the SAME seam Stage-3
//! emits (a server trait + a `<service_snake>` module of `<METHOD>_DESCRIPTOR` consts): the
//! descriptor-seam PATH read (`echo::GET_DESCRIPTOR.path` — never a spelled literal or a type
//! check), the field injection of `Ref<Controller>` + `Ref<ProstCodec>`, the UNIFORM
//! `::leaf_grpc::GrpcRecv`/`GrpcSend` framing seams (the call shape resolved by trait dispatch
//! on the REAL arg/return types — never their textual spelling), and the `dyn GrpcRoute`
//! provides[] view. The dual-form `GrpcControllerKind` guard compiling here proves the
//! struct's marker + the impl's assertion agree (a mismatch would be a hard compile error).

use futures::executor::block_on;
use http::{HeaderMap, Method};
use leaf_boot::App;
use leaf_core::{Injectable, Ref, ResolveCtx};
use futures::StreamExt;
use leaf_grpc::framing::{decode_frames, encode_frame};
use leaf_grpc::{GrpcCodec, GrpcRoute, ProstCodec, Status, Streaming};
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
    async fn list(&self, req: Ping) -> Result<Streaming<Pong>, Status>;
    async fn collect(&self, reqs: Streaming<Ping>) -> Result<Pong, Status>;
    async fn chat(&self, reqs: Streaming<Ping>) -> Result<Streaming<Pong>, Status>;
}

pub mod echo {
    #[doc(hidden)]
    pub const GET_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = ::leaf_grpc::MethodDescriptor {
        path: "/echo.v1.Echo/Get",
        shape: ::leaf_grpc::CallShape::Unary,
    };
    #[doc(hidden)]
    pub const LIST_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = ::leaf_grpc::MethodDescriptor {
        path: "/echo.v1.Echo/List",
        shape: ::leaf_grpc::CallShape::ServerStream,
    };
    #[doc(hidden)]
    pub const COLLECT_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = ::leaf_grpc::MethodDescriptor {
        path: "/echo.v1.Echo/Collect",
        shape: ::leaf_grpc::CallShape::ClientStream,
    };
    #[doc(hidden)]
    pub const CHAT_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = ::leaf_grpc::MethodDescriptor {
        path: "/echo.v1.Echo/Chat",
        shape: ::leaf_grpc::CallShape::Bidi,
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

/// The RPC ITERATOR: `#[grpc_controller]` desugars the native `async fn` and lowers each RPC
/// to a `#[doc(hidden)]` `GrpcRoute` bean (path from `echo::<METHOD>_DESCRIPTOR`, controller +
/// codec field-injected, the typed method wrapped through the UNIFORM `GrpcRecv`/`GrpcSend`
/// seams — the shape resolved by trait dispatch on the real arg/return types, all four shapes
/// here proving the disjoint impls resolve correctly end to end).
#[leaf_macros::grpc_controller]
impl Echo for EchoController {
    async fn get(&self, req: Ping) -> Result<Pong, Status> {
        Ok(format!("{req}-pong"))
    }

    async fn list(&self, req: Ping) -> Result<Streaming<Pong>, Status> {
        let items = vec![Ok(format!("{req}-1")), Ok(format!("{req}-2"))];
        Ok(Streaming::new(Box::pin(futures::stream::iter(items))))
    }

    async fn collect(&self, mut reqs: Streaming<Ping>) -> Result<Pong, Status> {
        let mut count = 0;
        while let Some(item) = reqs.next().await {
            item?;
            count += 1;
        }
        Ok(format!("collected {count}"))
    }

    async fn chat(&self, mut reqs: Streaming<Ping>) -> Result<Streaming<Pong>, Status> {
        let mut out = Vec::new();
        while let Some(item) = reqs.next().await {
            out.push(Ok(format!("{}-echo", item?)));
        }
        Ok(Streaming::new(Box::pin(futures::stream::iter(out))))
    }
}

/// A framed gRPC request for `path` carrying the given already-framed wire `body` bytes.
fn grpc_req(path: &str, body: bytes::Bytes) -> Request {
    let mut h = HeaderMap::new();
    h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
    Request::new(Method::POST, path.parse().expect("uri"), h, body)
}

/// One framed `Ping` message (the unary/server-stream inbound shape).
fn one_ping(codec: &ProstCodec, s: &str) -> bytes::Bytes {
    encode_frame(&codec.encode(&s.to_string()))
}

/// Several framed `Ping` messages concatenated (the client-stream/bidi inbound shape).
fn many_pings(codec: &ProstCodec, items: &[&str]) -> bytes::Bytes {
    let mut wire = Vec::new();
    for s in items {
        wire.extend_from_slice(&encode_frame(&codec.encode(&s.to_string())));
    }
    bytes::Bytes::from(wire)
}

/// Drain a gRPC response into its frames.
fn frames_of(resp: leaf_web::Response) -> Vec<Frame> {
    match resp.into_body() {
        Body::Stream(s) => block_on(s.collect::<Vec<_>>()).into_iter().map(|r| r.expect("frame")).collect(),
        Body::Full(_) => panic!("a gRPC response is a streaming body"),
    }
}

/// Decode the i-th data frame's single message as a `Pong`.
fn data_msg(codec: &ProstCodec, frames: &[Frame], i: usize) -> Pong {
    match &frames[i] {
        Frame::Data(b) => {
            let raw: Vec<_> = block_on(decode_frames(Body::full(b.clone())).collect());
            codec.decode(&raw.into_iter().next().expect("a message").expect("ok")).expect("decode Pong")
        }
        Frame::Trailers(_) => panic!("expected a data frame at {i}"),
    }
}

/// The `grpc-status` of the final trailer frame.
fn trailer_status(frames: &[Frame]) -> String {
    match frames.last().expect("a trailer") {
        Frame::Trailers(t) => t.get("grpc-status").expect("grpc-status").to_str().expect("ascii").to_string(),
        Frame::Data(_) => panic!("expected a trailers frame last"),
    }
}

#[test]
fn grpc_controller_route_beans_collect_and_dispatch_all_four_call_shapes() {
    // (1) Assemble: leaf-boot lifts every macro-emitted #[component]/bean row (the
    //     EchoController bean, its four generated __LeafGrpcRoute_EchoController_<method> beans,
    //     and the ProstCodec bean the routes field-inject) into a builder via the auto-collected
    //     SEED_PAIRINGS base, then freezes to an Engine (the LAZY resolve path).
    let registry = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[grpc_controller] row")
        .into_builder()
        .freeze()
        .expect("the grpc-controller beans registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    // (2) The generated GrpcRoute beans are collected by the `dyn GrpcRoute` collection +
    //     by-trait injection — exactly how GrpcDispatch finds every #[grpc_controller] route.
    let routes: Vec<Ref<dyn GrpcRoute>> =
        block_on(<Vec<Ref<dyn GrpcRoute>> as Injectable>::inject(&cx)).expect("routes resolve");
    // The EchoController's four RPC methods each lowered to one GrpcRoute bean. (leaf-grpc
    // also ships the two reflection controllers' route beans, which the LAZY `from_slices`
    // path — no condition pruning — admits unconditionally; count only the echo routes this
    // test owns.)
    let echo_routes = routes.iter().filter(|r| r.path().starts_with("/echo.v1.Echo/")).count();
    assert_eq!(echo_routes, 4, "the four RPC methods lowered to four GrpcRoute beans");

    // (3) `path()` reads the Stage-3 descriptor seam by method name (never a spelled literal).
    let by_path: std::collections::HashMap<&str, &Ref<dyn GrpcRoute>> =
        routes.iter().map(|r| (r.path(), r)).collect();
    let codec = ProstCodec::new();

    // (4a) UNARY: one in, one out. `GrpcRecv for Ping` decodes one message; `GrpcSend for Pong`
    //      encodes one + Ok trailers. The shape is resolved by trait dispatch on the REAL types.
    let route = by_path.get("/echo.v1.Echo/Get").expect("the unary route");
    let frames = frames_of(block_on(route.handler().call(grpc_req("/echo.v1.Echo/Get", one_ping(&codec, "ping")))));
    assert_eq!(data_msg(&codec, &frames, 0), "ping-pong", "the unary method ran via GrpcRecv/GrpcSend");
    assert_eq!(trailer_status(&frames), "0");

    // (4b) SERVER-STREAM: one in, many out. `GrpcSend for Streaming<Pong>` frames each message.
    let route = by_path.get("/echo.v1.Echo/List").expect("the server-stream route");
    let frames = frames_of(block_on(route.handler().call(grpc_req("/echo.v1.Echo/List", one_ping(&codec, "x")))));
    assert_eq!(frames.len(), 3, "two data frames + the Ok trailer");
    assert_eq!(data_msg(&codec, &frames, 0), "x-1");
    assert_eq!(data_msg(&codec, &frames, 1), "x-2");
    assert_eq!(trailer_status(&frames), "0");

    // (4c) CLIENT-STREAM: many in, one out. `GrpcRecv for Streaming<Ping>` decodes the stream.
    let route = by_path.get("/echo.v1.Echo/Collect").expect("the client-stream route");
    let frames = frames_of(block_on(
        route.handler().call(grpc_req("/echo.v1.Echo/Collect", many_pings(&codec, &["a", "b", "c"]))),
    ));
    assert_eq!(data_msg(&codec, &frames, 0), "collected 3", "the client-stream method drained the inbound stream");
    assert_eq!(trailer_status(&frames), "0");

    // (4d) BIDI: many in, many out. `Streaming` on BOTH the GrpcRecv arg and the GrpcSend Ok type.
    let route = by_path.get("/echo.v1.Echo/Chat").expect("the bidi route");
    let frames = frames_of(block_on(
        route.handler().call(grpc_req("/echo.v1.Echo/Chat", many_pings(&codec, &["one", "two"]))),
    ));
    assert_eq!(frames.len(), 3, "two echoed data frames + the Ok trailer");
    assert_eq!(data_msg(&codec, &frames, 0), "one-echo");
    assert_eq!(data_msg(&codec, &frames, 1), "two-echo");
    assert_eq!(trailer_status(&frames), "0");
}
