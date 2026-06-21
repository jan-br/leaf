//! The four CALL-SHAPE framing/codec wrappers the `#[grpc_controller]` macro (Stage 4)
//! references — the runtime adapters that sit between the wire [`Request`]/[`Response`]
//! and the typed user RPC method. Each wrapper has the SAME shape `(req, codec, body)`:
//! it de-frames + decodes the inbound side into the typed argument the closure wants
//! (`T` or [`Streaming<T>`]), runs the user method, then encodes + frames the typed
//! result (`U` or `Streaming<U>`) and appends the `grpc-status` trailer.
//!
//! The shape (which wrapper) is chosen by the macro from the Stage-3
//! [`MethodDescriptor`](crate::MethodDescriptor) seam — NEVER from the textual type of
//! the request/response. These fns only DO the framing once the shape is known.
//!
//! A handler NEVER returns `Err`: a [`Status`] from the user method (or a malformed
//! request) is RENDERED as `grpc-status`/`grpc-message` trailers (a valid gRPC response),
//! exactly like [`status_response`](crate::dispatch::status_response).

use std::future::Future;

use futures::StreamExt;
use http::{HeaderMap, HeaderValue};
use leaf_core::LeafError;
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
/// `Ok(None)` means the client sent no message — surfaced as an `InvalidArgument` status by
/// the callers.
async fn decode_first<T: Message + Default>(codec: &ProstCodec, body: Body) -> Result<T, Status> {
    let mut msgs = decode_frames(body);
    match msgs.next().await {
        Some(Ok(raw)) => codec.decode::<T>(&raw),
        Some(Err(status)) => Err(status),
        None => Err(Status::invalid_argument("expected one request message, got none")),
    }
}

/// **Unary** (`T -> Result<U, Status>`): decode the single request `T`, run the method,
/// encode the single `U`, frame it + Ok trailers. Any [`Status`] renders as trailers.
pub async fn call_unary<T, U, F, Fut>(req: Request, codec: &ProstCodec, body: F) -> Response
where
    T: Message + Default,
    U: Message,
    F: FnOnce(T) -> Fut,
    Fut: Future<Output = Result<U, Status>>,
{
    let codec_owned = *codec;
    let msg: T = match decode_first(codec, req.into_body()).await {
        Ok(m) => m,
        Err(status) => return status_response(&status),
    };
    match body(msg).await {
        Ok(out) => {
            let data = Frame::Data(encode_frame(&codec_owned.encode(&out)));
            grpc_response(futures::stream::iter(vec![Ok(data), Ok(ok_trailers())]))
        }
        Err(status) => status_response(&status),
    }
}

/// **Server-stream** (`T -> Result<Streaming<U>, Status>`): decode the single request `T`,
/// run the method, then frame EACH yielded `U` as a data frame, terminating with Ok trailers
/// (or a mid-stream `Status` as trailers).
pub async fn call_server_stream<T, U, F, Fut>(req: Request, codec: &ProstCodec, body: F) -> Response
where
    T: Message + Default,
    U: Message + Send + Sync + 'static,
    F: FnOnce(T) -> Fut,
    Fut: Future<Output = Result<Streaming<U>, Status>>,
{
    let codec_owned = *codec;
    let msg: T = match decode_first(codec, req.into_body()).await {
        Ok(m) => m,
        Err(status) => return status_response(&status),
    };
    let stream = match body(msg).await {
        Ok(s) => s,
        Err(status) => return status_response(&status),
    };
    grpc_response(frame_out_stream(codec_owned, stream))
}

/// **Client-stream** (`Streaming<T> -> Result<U, Status>`): de-frame the inbound body into a
/// typed `Streaming<T>` (decode each message via the codec), hand it to the method, encode the
/// single `U`, frame it + Ok trailers.
pub async fn call_client_stream<T, U, F, Fut>(req: Request, codec: &ProstCodec, body: F) -> Response
where
    T: Message + Default + Send + Sync + 'static,
    U: Message,
    F: FnOnce(Streaming<T>) -> Fut,
    Fut: Future<Output = Result<U, Status>>,
{
    let codec_owned = *codec;
    let inbound = decode_in_stream::<T>(codec_owned, req.into_body());
    match body(inbound).await {
        Ok(out) => {
            let data = Frame::Data(encode_frame(&codec_owned.encode(&out)));
            grpc_response(futures::stream::iter(vec![Ok(data), Ok(ok_trailers())]))
        }
        Err(status) => status_response(&status),
    }
}

/// **Bidi** (`Streaming<T> -> Result<Streaming<U>, Status>`): de-frame the inbound body into a
/// typed `Streaming<T>`, hand it to the method, then frame EACH yielded `U`, terminating with
/// trailers.
pub async fn call_bidi<T, U, F, Fut>(req: Request, codec: &ProstCodec, body: F) -> Response
where
    T: Message + Default + Send + Sync + 'static,
    U: Message + Send + Sync + 'static,
    F: FnOnce(Streaming<T>) -> Fut,
    Fut: Future<Output = Result<Streaming<U>, Status>>,
{
    let codec_owned = *codec;
    let inbound = decode_in_stream::<T>(codec_owned, req.into_body());
    let stream = match body(inbound).await {
        Ok(s) => s,
        Err(status) => return status_response(&status),
    };
    grpc_response(frame_out_stream(codec_owned, stream))
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

    // `String` is a complete prost Message (prost impls Message for the scalar wrappers), so
    // the unary round-trip codec test uses it as both T and U.

    #[test]
    fn call_unary_decodes_runs_encodes_and_appends_ok_trailers() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"ping".to_string())));
        let resp = block_on(call_unary::<String, String, _, _>(req, &codec, |msg: String| async move {
            Ok(format!("{msg}-pong"))
        }));
        let frames = frames_of(resp);
        let out: String = codec.decode(&first_message(&frames)).expect("decode U");
        assert_eq!(out, "ping-pong");
        assert_eq!(trailer_status(&frames), "0", "Ok trailers");
    }

    #[test]
    fn call_unary_renders_a_status_as_trailers_never_an_err() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"x".to_string())));
        let resp = block_on(call_unary::<String, String, _, _>(req, &codec, |_msg: String| async move {
            Err(Status::not_found("nope"))
        }));
        let frames = frames_of(resp);
        assert_eq!(trailer_status(&frames), "5", "NotFound → grpc-status 5 as trailers");
    }

    #[test]
    fn call_server_stream_frames_each_message_then_ok_trailers() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"seed".to_string())));
        let resp = block_on(call_server_stream::<String, String, _, _>(req, &codec, |seed: String| async move {
            let items = vec![Ok(format!("{seed}-1")), Ok(format!("{seed}-2"))];
            Ok(Streaming::new(Box::pin(futures::stream::iter(items))))
        }));
        let frames = frames_of(resp);
        // Two data frames + the Ok trailer.
        assert_eq!(frames.len(), 3, "two messages + a trailer");
        assert_eq!(trailer_status(&frames), "0");
    }

    #[test]
    fn call_client_stream_consumes_the_inbound_stream_and_replies_once() {
        let codec = ProstCodec::new();
        // Two framed inbound messages in one Full body.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(&codec.encode(&"a".to_string())));
        wire.extend_from_slice(&encode_frame(&codec.encode(&"b".to_string())));
        let req = grpc_req(Bytes::from(wire));
        let resp = block_on(call_client_stream::<String, String, _, _>(req, &codec, |mut s| async move {
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
    fn call_bidi_streams_both_ways() {
        let codec = ProstCodec::new();
        let req = grpc_req(encode_frame(&codec.encode(&"hi".to_string())));
        let resp = block_on(call_bidi::<String, String, _, _>(req, &codec, |mut s| async move {
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
