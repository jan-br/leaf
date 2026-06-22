//! The gRPC framing/codec dispatch the `#[grpc_controller]` macro (Stage 4) references —
//! the runtime adapters that sit between the wire [`Request`]/[`Response`] and the typed
//! user RPC method, expressed as TWO DISJOINT-TRAIT seams resolved on the REAL message
//! types (NEVER a textual type name):
//!
//! - [`GrpcRecv`] de-frames + decodes the inbound [`Request`] into the typed handler
//!   argument. The blanket `impl for M: prost::Message` decodes exactly ONE message (the
//!   unary / server-stream input `T`); the `impl for Streaming<M>` decodes the inbound
//!   message STREAM (the client-stream / bidi input). These are DISJOINT — `Streaming<M>`
//!   is not a `prost::Message`, so no autoref/specialization is needed.
//! - [`GrpcSend`] encodes + frames the handler's `Ok` output into the response [`Body`] +
//!   the `grpc-status` trailers. The blanket `impl for M: prost::Message` encodes ONE
//!   message + Ok trailers (the unary / client-stream output `U`); the `impl for
//!   Streaming<M>` frames the outbound message STREAM (the server-stream / bidi output).
//!   Also disjoint.
//!
//! The call SHAPE (unary vs server/client/bidi) thus falls out of TRAIT RESOLUTION on the
//! method's real argument/return types — the macro emits ONE uniform wrapper and never
//! inspects the signature for a `Streaming` name. A handler NEVER returns `Err`: a
//! [`Status`] from the user method (or a malformed request) is RENDERED as
//! `grpc-status`/`grpc-message` trailers (a valid gRPC response), exactly like
//! [`crate::dispatch::status_response`].

use futures::StreamExt;
use http::{HeaderMap, HeaderValue};
use leaf_core::{BoxFuture, LeafError};
use leaf_web::{Body, Frame, Request, Response};
use prost::Message;

use crate::codec::{GrpcCodec, ProstCodec};
use crate::dispatch::status_response;
use crate::framing::{decode_frames, encode_frame};
use crate::status::Status;
use crate::streaming::Streaming;

/// The `grpc-status: 0` trailer frame that terminates a successful response stream.
fn ok_trailers() -> Frame {
    let mut t = HeaderMap::new();
    t.insert("grpc-status", HeaderValue::from_static("0"));
    Frame::Trailers(t)
}

/// The trailer frame for a carried [`Status`] (`grpc-status` + optional `grpc-message`).
fn status_trailers(status: &Status) -> Frame {
    let mut t = HeaderMap::new();
    let code = (status.code as i32).to_string();
    t.insert("grpc-status", HeaderValue::from_str(&code).unwrap_or(HeaderValue::from_static("2")));
    if !status.message.is_empty()
        && let Ok(v) = HeaderValue::from_str(&status.message)
    {
        t.insert("grpc-message", v);
    }
    Frame::Trailers(t)
}

/// Build a framed gRPC [`Response`] from a stream of result frames.
fn grpc_response(
    frames: impl futures::Stream<Item = Result<Frame, LeafError>> + Send + Sync + 'static,
) -> Response {
    Response::ok()
        .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
        .with_body_stream(Box::pin(frames))
}

/// Decode the FIRST inbound message as `T` (the single-request side of unary/server-stream).
/// `Ok(None)` means the client sent no message — surfaced as an `InvalidArgument` status.
async fn decode_first<T: Message + Default>(codec: ProstCodec, body: Body) -> Result<T, Status> {
    let mut msgs = decode_frames(body);
    match msgs.next().await {
        Some(Ok(raw)) => codec.decode::<T>(&raw),
        Some(Err(status)) => Err(status),
        None => Err(Status::invalid_argument("expected one request message, got none")),
    }
}

/// De-frame + decode the inbound body into a typed [`Streaming<T>`] (each wire frame decoded
/// via the codec; a malformed frame becomes a `Status` item in the stream).
fn decode_in_stream<T: Message + Default + Send + Sync + 'static>(
    codec: ProstCodec,
    body: Body,
) -> Streaming<T> {
    let inner = decode_frames(body).map(move |r| r.and_then(|raw| codec.decode::<T>(&raw)));
    Streaming::new(Box::pin(inner))
}

/// Frame an outbound typed [`Streaming<U>`] into the response frame stream: each `Ok(U)` is
/// encoded + framed as a data frame; the stream terminates with Ok trailers, or — on the
/// first `Err(Status)` — that status's trailers (and stops).
fn frame_out_stream<U: Message + Send + Sync + 'static>(
    codec: ProstCodec,
    stream: Streaming<U>,
) -> impl futures::Stream<Item = Result<Frame, LeafError>> + Send + Sync + 'static {
    // Thread (stream, done) so the trailers frame is emitted exactly once at the end.
    enum St<U> {
        Running(Streaming<U>),
        Done,
    }
    futures::stream::unfold(St::Running(stream), move |state| async move {
        match state {
            St::Running(mut s) => match s.next().await {
                Some(Ok(msg)) => {
                    let frame = Frame::Data(encode_frame(&codec.encode(&msg)));
                    Some((Ok(frame), St::Running(s)))
                }
                Some(Err(status)) => Some((Ok(status_trailers(&status)), St::Done)),
                None => Some((Ok(ok_trailers()), St::Done)),
            },
            St::Done => None,
        }
    })
}

/// Encode a single message `U` + Ok trailers as a complete two-frame gRPC response.
fn unary_response<U: Message>(codec: ProstCodec, out: &U) -> Response {
    let data = Frame::Data(encode_frame(&codec.encode(out)));
    grpc_response(futures::stream::iter(vec![Ok(data), Ok(ok_trailers())]))
}

/// The INBOUND side of a gRPC method: produce the typed handler argument from the framed
/// [`Request`]. Resolved on the REAL argument type — the blanket `impl for M: Message`
/// decodes ONE message (unary / server-stream input), the `impl for Streaming<M>` decodes
/// the inbound message STREAM (client-stream / bidi input). DISJOINT (a `Streaming<M>` is
/// not a `prost::Message`), so the `#[grpc_controller]` macro picks the shape by TRAIT
/// resolution on the type the user wrote — never by spelling its name.
pub trait GrpcRecv: Sized {
    /// De-frame + decode `req` into `Self` (the typed handler argument). The future is
    /// `'static` (the codec is copied in), so it composes inside the boxed `GrpcHandler::call`
    /// future. A malformed request / decode failure surfaces as an `Err(Status)` the caller
    /// renders as trailers.
    fn recv(req: Request, codec: &ProstCodec) -> BoxFuture<'static, Result<Self, Status>>;
}

/// **Unary / server-stream input** (`T`): decode exactly ONE inbound message.
impl<M: Message + Default + 'static> GrpcRecv for M {
    fn recv(req: Request, codec: &ProstCodec) -> BoxFuture<'static, Result<Self, Status>> {
        let codec = *codec;
        Box::pin(async move { decode_first::<M>(codec, req.into_body()).await })
    }
}

/// **Client-stream / bidi input** (`Streaming<T>`): de-frame the inbound body into the typed
/// message STREAM (decode is lazy per item, so this never fails at recv time).
impl<M: Message + Default + Send + Sync + 'static> GrpcRecv for Streaming<M> {
    fn recv(req: Request, codec: &ProstCodec) -> BoxFuture<'static, Result<Self, Status>> {
        let codec = *codec;
        Box::pin(async move { Ok(decode_in_stream::<M>(codec, req.into_body())) })
    }
}

/// The OUTBOUND side of a gRPC method: render the handler's `Result<Self, Status>` into the
/// framed [`Response`]. Resolved on the REAL `Ok` type — the blanket `impl for M: Message`
/// encodes ONE message + Ok trailers (unary / client-stream output), the `impl for
/// Streaming<M>` frames the outbound message STREAM (server-stream / bidi output). DISJOINT,
/// so the macro picks the shape by TRAIT resolution. A handler `Err(Status)` (or a recv
/// failure threaded in by the caller) renders as `grpc-status` trailers, NEVER an `Err`.
pub trait GrpcSend: Sized {
    /// Render `out` into the framed [`Response`]: encode + frame the `Ok` value, or render a
    /// carried [`Status`] as `grpc-status`/`grpc-message` trailers.
    fn send(out: Result<Self, Status>, codec: &ProstCodec) -> Response;
}

/// **Unary / client-stream output** (`U`): encode the single message + Ok trailers.
impl<M: Message + 'static> GrpcSend for M {
    fn send(out: Result<Self, Status>, codec: &ProstCodec) -> Response {
        match out {
            Ok(msg) => unary_response(*codec, &msg),
            Err(status) => status_response(&status),
        }
    }
}

/// **Server-stream / bidi output** (`Streaming<U>`): frame each yielded message, terminating
/// with Ok trailers (or a mid-stream `Status` as trailers).
impl<M: Message + Send + Sync + 'static> GrpcSend for Streaming<M> {
    fn send(out: Result<Self, Status>, codec: &ProstCodec) -> Response {
        match out {
            Ok(stream) => grpc_response(frame_out_stream(*codec, stream)),
            Err(status) => status_response(&status),
        }
    }
}

/// The `#[grpc_controller]` dual-form CONSISTENCY marker — the gRPC twin of
/// [`leaf_web::ControllerKind`]. The `#[grpc_controller]` STRUCT form emits `impl
/// GrpcControllerKind for Bean { const IS_GRPC_CONTROLLER = true; }`; the `#[grpc_controller]`
/// IMPL form appends a `const _` guard asserting that const, so a `#[grpc_controller] impl`
/// on a struct never annotated `#[grpc_controller]` (which lacks the marker) is a hard
/// compile error. `#[doc(hidden)]` — written ONLY by the macro, never by hand.
#[doc(hidden)]
pub trait GrpcControllerKind {
    /// `true` for a `#[grpc_controller]` struct (the marker is the bean's anchor).
    const IS_GRPC_CONTROLLER: bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::executor::block_on;
    use http::Method;

    fn grpc_req(framed: Bytes) -> Request {
        Request::new(Method::POST, "/pkg.Svc/M".parse().expect("uri"), HeaderMap::new(), framed)
    }

    fn frames_of(resp: Response) -> Vec<Frame> {
        match resp.into_body() {
            Body::Stream(s) => {
                block_on(s.collect::<Vec<_>>()).into_iter().map(|r| r.expect("frame")).collect()
            }
            Body::Full(_) => panic!("a gRPC response is a streaming body"),
        }
    }

    fn first_message(frames: &[Frame]) -> Bytes {
        match &frames[0] {
            Frame::Data(b) => {
                let raw: Vec<Result<Bytes, _>> =
                    block_on(decode_frames(Body::full(b.clone())).collect());
                raw.into_iter().next().expect("a message").expect("ok")
            }
            Frame::Trailers(_) => panic!("expected a data frame first"),
        }
    }

    fn trailer_status(frames: &[Frame]) -> String {
        match frames.last().expect("a trailer") {
            Frame::Trailers(t) => {
                t.get("grpc-status").expect("grpc-status").to_str().expect("ascii").to_string()
            }
            Frame::Data(_) => panic!("expected a trailers frame last"),
        }
    }

    /// The uniform wrapper the `#[grpc_controller]` macro emits, expressed once over the
    /// disjoint `GrpcRecv`/`GrpcSend` seams for the test (trait resolution picks the shape
    /// from `Arg`/`Out`, never a name): recv the arg, run the handler, send the result.
    async fn run<Arg, Out, F, Fut>(req: Request, codec: &ProstCodec, handler: F) -> Response
    where
        Arg: GrpcRecv,
        Out: GrpcSend,
        F: FnOnce(Arg) -> Fut,
        Fut: std::future::Future<Output = Result<Out, Status>>,
    {
        let arg = match <Arg as GrpcRecv>::recv(req, codec).await {
            Ok(a) => a,
            Err(status) => return status_response(&status),
        };
        <Out as GrpcSend>::send(handler(arg).await, codec)
    }

    // `String` is a complete prost Message (prost impls Message for the scalar wrappers), so
    // the round-trip codec tests use it as both T and U.

    #[test]
    fn unary_recv_decodes_one_then_send_encodes_with_ok_trailers() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"ping".to_string())));
        let resp = block_on(run(req, &codec, |msg: String| async move { Ok(format!("{msg}-pong")) }));
        let frames = frames_of(resp);
        let out: String = codec.decode(&first_message(&frames)).expect("decode U");
        assert_eq!(out, "ping-pong");
        assert_eq!(trailer_status(&frames), "0", "Ok trailers");
    }

    #[test]
    fn a_status_renders_as_trailers_never_an_err() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"x".to_string())));
        let resp = block_on(run::<String, String, _, _>(req, &codec, |_msg| async move {
            Err(Status::not_found("nope"))
        }));
        let frames = frames_of(resp);
        assert_eq!(trailer_status(&frames), "5", "NotFound → grpc-status 5 as trailers");
    }

    #[test]
    fn server_stream_send_frames_each_message_then_ok_trailers() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"seed".to_string())));
        let resp = block_on(run(req, &codec, |seed: String| async move {
            let items = vec![Ok(format!("{seed}-1")), Ok(format!("{seed}-2"))];
            Ok(Streaming::new(Box::pin(futures::stream::iter(items))))
        }));
        let frames = frames_of(resp);
        // Two data frames + the Ok trailer.
        assert_eq!(frames.len(), 3, "two messages + a trailer");
        assert_eq!(trailer_status(&frames), "0");
    }

    #[test]
    fn client_stream_recv_consumes_the_inbound_stream_then_sends_once() {
        let codec = ProstCodec::new();
        // Two framed inbound messages in one Full body.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(&codec.encode(&"a".to_string())));
        wire.extend_from_slice(&encode_frame(&codec.encode(&"b".to_string())));
        let req = grpc_req(Bytes::from(wire));
        let resp = block_on(run(req, &codec, |mut s: Streaming<String>| async move {
            let mut count = 0;
            while let Some(item) = s.next().await {
                item.expect("decoded inbound");
                count += 1;
            }
            Ok(format!("got {count}"))
        }));
        let frames = frames_of(resp);
        let out: String = codec.decode(&first_message(&frames)).expect("decode U");
        assert_eq!(out, "got 2");
        assert_eq!(trailer_status(&frames), "0");
    }

    #[test]
    fn bidi_streams_both_ways() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"hi".to_string())));
        let resp = block_on(run(req, &codec, |mut s: Streaming<String>| async move {
            // Echo each inbound message back.
            let mut out = Vec::new();
            while let Some(item) = s.next().await {
                out.push(Ok(item.expect("decoded")));
            }
            Ok(Streaming::new(Box::pin(futures::stream::iter(out))))
        }));
        let frames = frames_of(resp);
        // One echoed message + the Ok trailer.
        assert_eq!(frames.len(), 2);
        assert_eq!(trailer_status(&frames), "0");
    }

    #[test]
    fn a_recv_decode_failure_renders_invalid_argument_trailers() {
        let codec = ProstCodec::new();
        // An empty body (no framed message) → recv yields InvalidArgument, rendered as trailers.
        let req = grpc_req(Bytes::new());
        let resp = block_on(run::<String, String, _, _>(req, &codec, |msg| async move { Ok(msg) }));
        let frames = frames_of(resp);
        assert_eq!(trailer_status(&frames), "3", "no inbound message → InvalidArgument (3)");
    }

    #[test]
    fn grpc_controller_kind_is_a_marker_a_struct_can_carry() {
        struct Fake;
        impl GrpcControllerKind for Fake {
            const IS_GRPC_CONTROLLER: bool = true;
        }
        // Read the const through a runtime binding (a direct `assert!` on a const value trips
        // clippy::assertions-on-constants — the marker is exercised structurally here).
        let is_controller = <Fake as GrpcControllerKind>::IS_GRPC_CONTROLLER;
        assert!(is_controller, "a struct can carry the GrpcControllerKind marker");
    }
}
