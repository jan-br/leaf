//! The gRPC `Handler` family: [`GrpcHandler`] + [`GrpcRoute`] + the injectable view.

use leaf_core::BoxFuture;
use leaf_web::{Request, Response};

/// The gRPC dispatch unit (the [`leaf_web::Handler`] analogue): consume the inbound
/// [`Request`] — whose [`Body::Stream`](leaf_web::Body) is the H2 frame stream — and
/// produce a [`Response`] whose body is the outbound data frames + a final
/// `Frame::Trailers` carrying `grpc-status`/`grpc-message`.
///
/// Unlike the HTTP [`Handler`](leaf_web::Handler), a `GrpcHandler` NEVER returns
/// `Err`: a [`Status`](crate::Status) is RENDERED as trailers (a rejected gRPC call
/// still yields a valid grpc-status trailer, not a transport error). It is
/// dyn-dispatched + async → a [`BoxFuture`] at the `dyn` seam. WRITTEN BY the
/// Stage-4 `#[grpc_controller]` macro (the `#[cfg(test)]` fakes are the Stage-2
/// exception).
pub trait GrpcHandler: Send + Sync {
    /// Handle `req`, yielding the framed [`Response`] (data frames + status trailers).
    fn call<'a>(&'a self, req: Request) -> BoxFuture<'a, Response>;
}

/// A gRPC routing registration (the [`leaf_web::Route`] analogue): a full
/// `/package.Service/Method` path bound to a [`GrpcHandler`]. The container collects
/// every provider as `Vec<Ref<dyn GrpcRoute>>` (collection + by-trait injection), the
/// same way HTTP routes are collected; the Stage-4 `#[grpc_controller]` macro emits
/// one `#[doc(hidden)]` `GrpcRoute` bean per RPC method.
pub trait GrpcRoute: Send + Sync {
    /// The full gRPC method path, e.g. `/catalog.Catalog/GetProduct` (a literal, not
    /// a pattern — gRPC method paths are exact, enabling O(1) dispatch).
    fn path(&self) -> &str;
    /// The [`GrpcHandler`] that runs when this route's path is requested.
    fn handler(&self) -> &dyn GrpcHandler;
}

// Make `dyn GrpcRoute` an injectable VIEW (the by-trait-injection seam, emitted ONCE
// — orphan-rule-OK since `dyn GrpcRoute` is local to this crate). A `#[grpc_controller]`
// bean (Stage 4) publishes the `dyn GrpcRoute` view; `GrpcDispatch` collects EVERY
// provider as `Vec<Ref<dyn GrpcRoute>>`, exactly as the web server collects `dyn Route`.
leaf_core::impl_resolve_view!(dyn GrpcRoute);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::encode_frame;
    use crate::status::{Code, Status};
    use bytes::Bytes;
    use futures::executor::block_on;
    use futures::StreamExt;
    use http::{HeaderMap, Method};
    use leaf_core::BoxFuture;
    use leaf_web::request::Request;
    use leaf_web::{Body, Frame};

    /// A `#[cfg(test)]` fake handler (PRODUCTION handlers come from the Stage-4
    /// `#[grpc_controller]` macro). It echoes the first inbound message back as one
    /// data frame, then a `grpc-status: 0` (Ok) trailer — a unary identity.
    struct EchoHandler;

    impl GrpcHandler for EchoHandler {
        fn call<'a>(&'a self, req: Request) -> BoxFuture<'a, leaf_web::Response> {
            Box::pin(async move {
                // De-frame the request body, echo the first message, render Ok trailers.
                let mut msgs = crate::framing::decode_frames(req.into_body());
                let first = msgs.next().await;
                let data = match first {
                    Some(Ok(m)) => m,
                    _ => Bytes::new(),
                };
                let mut trailers = HeaderMap::new();
                trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                let out = futures::stream::iter(vec![
                    Ok::<_, leaf_core::LeafError>(Frame::Data(encode_frame(&data))),
                    Ok(Frame::Trailers(trailers)),
                ]);
                leaf_web::Response::ok()
                    .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
                    .with_body_stream(Box::pin(out))
            })
        }
    }

    /// A `#[cfg(test)]` fake route binding a path to the echo handler.
    struct EchoRoute {
        path: &'static str,
        handler: EchoHandler,
    }

    impl GrpcRoute for EchoRoute {
        fn path(&self) -> &str {
            self.path
        }
        fn handler(&self) -> &dyn GrpcHandler {
            &self.handler
        }
    }

    #[test]
    fn grpc_handler_echoes_a_unary_message_with_ok_trailers() {
        let route = EchoRoute { path: "/pkg.Svc/Echo", handler: EchoHandler };
        assert_eq!(route.path(), "/pkg.Svc/Echo");

        // An inbound request whose body is ONE framed message. `Request::new` wraps the
        // given `Bytes` in a `Body::Full`, so the framed bytes are the request body.
        let req = Request::new(
            Method::POST,
            "/pkg.Svc/Echo".parse().expect("uri"),
            HeaderMap::new(),
            encode_frame(b"ping"),
        );

        let resp = block_on(route.handler().call(req));
        // The response body is a stream: one data frame ("ping") + an Ok trailer.
        let frames: Vec<Frame> = match resp.into_body() {
            Body::Stream(s) => block_on(s.collect::<Vec<_>>())
                .into_iter()
                .map(|r| r.expect("frame ok"))
                .collect(),
            Body::Full(_) => panic!("a gRPC handler yields a streaming body"),
        };
        // First frame: the echoed message, re-framed.
        match &frames[0] {
            Frame::Data(b) => {
                let raw: Vec<Result<Bytes, _>> = block_on(
                    crate::framing::decode_frames(Body::full(b.clone())).collect::<Vec<_>>(),
                );
                let msgs: Vec<Bytes> = raw.into_iter().map(|r| r.expect("msg")).collect();
                assert_eq!(msgs, vec![Bytes::from_static(b"ping")]);
            }
            Frame::Trailers(_) => panic!("expected a data frame, got a trailers frame"),
        }
        // Last frame: the grpc-status trailers (Ok).
        match frames.last().expect("a trailer frame") {
            Frame::Trailers(t) => {
                assert_eq!(t.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));
            }
            Frame::Data(_) => panic!("expected a trailers frame, got a data frame"),
        }
        // `Status`/`Code` are reachable here (proving the handler module sees them).
        let _ = Status::new(Code::Ok, "");
    }
}
