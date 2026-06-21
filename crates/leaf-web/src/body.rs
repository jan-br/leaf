//! The streaming [`Body`] — a request/response body that is EITHER a fully-buffered
//! [`Bytes`] (the HTTP-ergonomic default) OR a stream of [`Frame`]s (the gRPC / SSE
//! streaming case). Backend-free: a stream is a [`leaf_core::BoxStream`] (the `futures`
//! standard), never a hyper type — the backend maps its native body to/from this at the
//! edge.
//!
//! Trailers are FIRST-CLASS ([`Frame::Trailers`]): gRPC carries its `grpc-status` /
//! `grpc-message` as HTTP/2 trailers after the data, so the frame stream must be able to
//! express "data, then trailers".

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::HeaderMap;
use leaf_core::{BoxStream, Cause, ErrorKind, LeafError};

/// A request/response body: a buffered blob or a stream of [`Frame`]s.
///
/// HTTP stays ergonomic — the [`Dispatcher`](crate::Dispatcher) COLLECTS a streamed body
/// to [`Body::Full`] before invoking an HTTP route handler, so every existing extractor /
/// [`Request::body_bytes`](crate::Request::body_bytes) call sees the Full variant. gRPC
/// handlers consume/produce the [`Body::Stream`] frames directly, never buffering.
pub enum Body {
    /// A fully-buffered body (the HTTP default; what `with_body(bytes)` produces).
    Full(Bytes),
    /// A stream of HTTP/2 frames (data + a terminating trailers frame). Backend-free:
    /// `BoxStream` is `futures`, not hyper.
    Stream(BoxStream<'static, Result<Frame, LeafError>>),
}

/// One frame of a streamed [`Body`]: a chunk of data, or the terminating trailers.
pub enum Frame {
    /// A chunk of body bytes.
    Data(Bytes),
    /// The terminating trailers (gRPC's `grpc-status`/`grpc-message` ride here).
    Trailers(HeaderMap),
}

impl Body {
    /// A fully-buffered body from anything that is `Into<Bytes>` (the ergonomic ctor the
    /// `Response`/`Request` builders delegate to).
    #[must_use]
    pub fn full(b: impl Into<Bytes>) -> Body {
        Body::Full(b.into())
    }

    /// Whether this body is the streaming variant (the dispatcher checks this to decide
    /// whether to collect before an HTTP handler).
    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self, Body::Stream(_))
    }

    /// Collect the WHOLE body into [`Bytes`], bounded by `limit` bytes.
    ///
    /// A [`Body::Full`] returns its bytes directly (still checked against `limit`). A
    /// [`Body::Stream`] drains its [`Frame::Data`] frames (ignoring trailers — the
    /// collected form has no trailer channel) into one buffer, aborting with a
    /// `ConvertError` `LeafError` once more than `limit` bytes have accumulated, so an
    /// unbounded streamed body can never exhaust memory on the collect path.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] (`ConvertError`) if the body exceeds `limit`, or propagates
    /// a frame's own stream error.
    pub async fn collect(self, limit: usize) -> Result<Bytes, LeafError> {
        match self {
            Body::Full(bytes) => {
                if bytes.len() > limit {
                    return Err(too_large(bytes.len(), limit));
                }
                Ok(bytes)
            }
            Body::Stream(mut stream) => {
                let mut buf = BytesMut::new();
                while let Some(frame) = stream.next().await {
                    match frame? {
                        Frame::Data(chunk) => {
                            if buf.len() + chunk.len() > limit {
                                return Err(too_large(buf.len() + chunk.len(), limit));
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        // Trailers carry no body bytes; the collected form drops them.
                        Frame::Trailers(_) => {}
                    }
                }
                Ok(buf.freeze())
            }
        }
    }
}

/// The over-cap `LeafError` (a client-fault `ConvertError`, mapped to 4xx by the default
/// advice floor — the same status an oversize body gets at the transport edge).
fn too_large(got: usize, limit: usize) -> LeafError {
    LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
        "collecting the request body",
        format!("body of {got} bytes exceeds the {limit}-byte limit"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_body_is_not_a_stream_and_collects_to_itself() {
        let body = Body::full(Bytes::from_static(b"hello"));
        assert!(!body.is_stream());
        let out = futures::executor::block_on(body.collect(1024)).expect("collects");
        assert_eq!(out, Bytes::from_static(b"hello"));
    }

    #[test]
    fn stream_body_collects_data_frames_ignoring_trailers() {
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"ab"))),
            Ok(Frame::Data(Bytes::from_static(b"cd"))),
            Ok(Frame::Trailers(trailers)),
        ]);
        let body = Body::Stream(Box::pin(frames));
        assert!(body.is_stream());
        let out = futures::executor::block_on(body.collect(1024)).expect("collects");
        assert_eq!(out, Bytes::from_static(b"abcd"));
    }

    #[test]
    fn collect_aborts_a_full_body_over_the_limit() {
        let body = Body::full(Bytes::from_static(b"0123456789"));
        let err = futures::executor::block_on(body.collect(4)).expect_err("over cap");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn collect_aborts_a_stream_body_over_the_limit_mid_stream() {
        // The cap is enforced as frames accumulate — the second frame blows the 3-byte cap.
        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"ab"))),
            Ok(Frame::Data(Bytes::from_static(b"cd"))),
        ]);
        let body = Body::Stream(Box::pin(frames));
        let err = futures::executor::block_on(body.collect(3)).expect_err("over cap");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn collect_propagates_a_frame_stream_error() {
        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"ab"))),
            Err(LeafError::new(ErrorKind::ConstructionFailed)),
        ]);
        let body = Body::Stream(Box::pin(frames));
        let err = futures::executor::block_on(body.collect(1024)).expect_err("stream errored");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }
}
