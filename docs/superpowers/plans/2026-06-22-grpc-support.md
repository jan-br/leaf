# gRPC Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add full gRPC (unary + server/client/bidi streaming) to leaf as a second `Handler` family on the shared `WebServer`, proto-first, via a `#[grpc_controller]` stereotype.

**Architecture:** leaf owns the gRPC layer (`prost` = message codec only) running on the existing hyper `WebServer` with HTTP/2 enabled. A streaming `Body` (buffered-or-stream + trailers) plus an abstract `ProtocolDispatch` seam let `leaf-web` route `application/grpc` to `leaf-grpc` without ever naming it. Proto-first codegen (`protox`+`prost-build`, no `protoc`) generates message structs + a leaf service trait; `#[grpc_controller]` lowers each RPC method to a `GrpcRoute` bean collected by DI, reusing the `WebServer`/`KeepAlive`/`Dispatcher`/`WebFilter` from sub-project A.

**Tech Stack:** Rust, leaf DI, `prost` (protobuf codec), `protox` + `prost-build` (proto-first codegen), `hyper`/`h2` (confined to `leaf-web-hyper`), `tonic` (dev-only interop test client).

**Spec:** `docs/superpowers/specs/2026-06-22-grpc-support-design.md`

---

## File structure (new + modified)

**New crates**
- `crates/leaf-grpc/` — backend-free gRPC abstractions: `Code`/`Status`, length-prefix framing, `Streaming<T>`, `GrpcCodec`/`ProstCodec`, `GrpcHandler`, `GrpcRoute`, `GrpcDispatch` (the `ProtocolDispatch` impl), `GrpcStatusMapper` + `DefaultGrpcStatusMapper`.
- `crates/leaf-grpc-build/` — `compile(protos, includes)`: `protox` parse -> `prost-build` messages + the leaf service-trait generator.
- `crates/leaf-starter-grpc/` — the `grpc` capability bundle.

**Modified**
- `crates/leaf-core/` — `BoxStream` alias (streaming analogue of `BoxFuture`).
- `crates/leaf-web/` — the streaming `Body`/`Frame`, the `ProtocolDispatch` seam + `Dispatcher` content-type branch, the embedded server injecting `Vec<Ref<dyn ProtocolDispatch>>`.
- `crates/leaf-web-hyper/` — enable `http2`; map hyper `Incoming` <-> leaf `Body` (stream + trailers).
- `crates/leaf-codegen/` + `crates/leaf-macros/` — `#[grpc_controller]` lowering (all 4 call shapes) + the dual-form guard.
- `crates/leaf/` — `grpc` umbrella feature + prelude exports.
- `examples/storefront/` — a dogfood `#[grpc_controller]` + its real-H2 integration test.

**Dependency direction (must stay acyclic):** `leaf-grpc -> leaf-web -> leaf-core`; `leaf-web-hyper -> leaf-web`; the umbrella sinks them. `leaf-web` never names `leaf-grpc`.

---

## Stage 1: Streaming Body + HTTP/2

This stage turns the leaf web `Request`/`Response` body from a buffered `Bytes` into a streaming `Body`/`Frame` model (with trailers as a first-class frame), adds the abstract `ProtocolDispatch` seam + the `Dispatcher` content-type branch, and maps hyper's `Incoming` ↔ the leaf streaming `Body` at the edge with `http2` enabled — all while keeping the entire existing HTTP suite green (REST collects-before-handler).

**HARD CONSTRAINTS for every task here:**
- `leaf-web` and `leaf-grpc` stay **backend-free** — only `leaf-web-hyper` names `hyper`/`h2`/`http-body`. `Body::Stream` is `leaf_core::BoxStream` (`futures`), never a hyper type.
- Dep arrow stays `leaf-web → leaf-core`; `leaf-web` never names `leaf-grpc`. The gRPC family plugs in later as ONE `dyn ProtocolDispatch` impl.
- No type-name detection: the dispatcher branches on `content-type` (a runtime header value) via `ProtocolDispatch::handles`, never on a Rust type's spelled name.
- Dogfood: `dyn ProtocolDispatch` is published via `leaf_core::impl_resolve_view!` (the same by-trait-injection seam `dyn Route`/`dyn WebFilter` use) — no hand-rolled `Resolve` impl.
- The existing ~1647-test HTTP suite must stay green: every `Request::new(.., Bytes)` / `Response::with_body(Bytes)` / `body_bytes()` call site keeps compiling and behaving identically (Full variant), and the Dispatcher COLLECTS the body before invoking an HTTP `Route`.

### Files
- **Create** `crates/leaf-core/src/stream.rs` — the `BoxStream` type alias (the streaming analogue of `BoxFuture`).
- **Modify** `crates/leaf-core/src/lib.rs` — `pub mod stream;` + `pub use stream::BoxStream;`.
- **Create** `crates/leaf-web/src/body.rs` — `enum Body`, `enum Frame`, `Body::full`/`is_stream`/`collect`.
- **Modify** `crates/leaf-web/src/request.rs` — `body: Body`; `Request::new(.., Bytes)` wraps `Body::Full`; keep `body_bytes()`; add `into_body()`.
- **Modify** `crates/leaf-web/src/response.rs` — `body: Body`; keep `with_body(impl Into<Bytes>)`; add `with_body_stream(..)`; keep `body_bytes()`.
- **Modify** `crates/leaf-web/src/server.rs` — the `ProtocolDispatch` trait + `impl_resolve_view!`, the `Dispatcher` `protocols` field + content-type branch + collect-before-HTTP.
- **Modify** `crates/leaf-web/src/embedded.rs` — collection-inject `Vec<Ref<dyn ProtocolDispatch>>` into the `Dispatcher`.
- **Modify** `crates/leaf-web/src/lib.rs` — `pub mod body;` + re-exports (`Body`, `Frame`, `ProtocolDispatch`).
- **Modify** `crates/leaf-web/src/testing.rs` — `MockServer` constructs no body (unaffected; only the `Dispatcher::new` arity changes, threaded through).
- **Modify** `crates/leaf-web-hyper/Cargo.toml` — enable `http2` on `hyper`/`hyper-util`, add `http-body` + `futures`.
- **Modify** `crates/leaf-web-hyper/src/server.rs` — `to_leaf_request` builds a `Body::Stream` from `Incoming` (bounded by `max_request_body_bytes` on the collect path); `to_hyper_response` writes `Body::Full` as today and `Body::Stream` as an H2 frame stream with trailers.

---

### Task 1.1: `BoxStream` type alias in leaf-core

**Files:** `crates/leaf-core/src/stream.rs` (create), `crates/leaf-core/src/lib.rs` (modify)

- [ ] **Step 1: Write the failing module.** Create `crates/leaf-core/src/stream.rs`:

```rust
//! The boxed-stream standard — the streaming analogue of [`BoxFuture`](crate::BoxFuture).
//!
//! A `dyn` seam that yields a SEQUENCE of values (a streaming request/response body,
//! a gRPC message stream) returns a [`BoxStream`] for the same reason `dyn`-async
//! returns a `BoxFuture`: `impl Stream` is not `dyn`-compatible. This mirrors
//! `futures::stream::BoxStream` exactly but is defined here so the kernel ABI does not
//! leak the `futures` crate at its surface (the leaf-web `Body` names `leaf_core::BoxStream`,
//! never `futures::...`).

use std::pin::Pin;

use futures::Stream;

/// The one boxed-stream shape returned at a streaming `dyn` seam in leaf.
///
/// `Send + 'a` mirrors [`BoxFuture`](crate::BoxFuture): a streaming body rides the
/// executor across threads, so the stream it wraps must be `Send`.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn box_stream_is_constructible_send_and_collectible() {
        let s: BoxStream<'static, i32> = Box::pin(futures::stream::iter([1, 2, 3]));
        assert_send(&s);
        let out: Vec<i32> = futures::executor::block_on(s.collect());
        assert_eq!(out, vec![1, 2, 3]);
    }
}
```

- [ ] **Step 2: Wire the module + export.** In `crates/leaf-core/src/lib.rs` add `pub mod stream;` beside `pub mod` blocks and `pub use stream::BoxStream;` beside `pub use future::BoxFuture;` (line 110):

```rust
pub use future::BoxFuture;
pub use stream::BoxStream;
```

- [ ] **Step 3: Run it — expect PASS.**

```
cargo test -p leaf-core stream:: -- --nocapture
```
Expected: `box_stream_is_constructible_send_and_collectible ... ok`.

- [ ] **Step 4: Commit.**

```
git add crates/leaf-core/src/stream.rs crates/leaf-core/src/lib.rs
git commit -m "leaf-core: BoxStream alias (the streaming analogue of BoxFuture)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.2: `Body` / `Frame` types + `Body::collect`

**Files:** `crates/leaf-web/src/body.rs` (create), `crates/leaf-web/src/lib.rs` (modify)

- [ ] **Step 1: Write the failing module with its tests.** Create `crates/leaf-web/src/body.rs`:

```rust
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
```

- [ ] **Step 2: Wire the module + re-exports.** In `crates/leaf-web/src/lib.rs` add `pub mod body;` (beside `pub mod advice;`, line 68) and the re-export beside `pub use request::Request;` (line 111):

```rust
pub use body::{Body, Frame};
```

- [ ] **Step 3: Run it — expect PASS.**

```
cargo test -p leaf-web body:: -- --nocapture
```
Expected: 5 tests `... ok` (full/stream collect, the two over-cap aborts, the stream-error propagation).

- [ ] **Step 4: Commit.**

```
git add crates/leaf-web/src/body.rs crates/leaf-web/src/lib.rs
git commit -m "leaf-web: streaming Body/Frame + Body::collect (bounded)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.3: Migrate `Request.body` from `Bytes` to `Body`

**Files:** `crates/leaf-web/src/request.rs` (modify)

The contract: `Request::new(..)` KEEPS a `Bytes` arg (wraps `Body::Full`) so every existing call site compiles; `body_bytes()` keeps working for the Full variant; add `into_body(self) -> Body`.

- [ ] **Step 1: Write the failing tests.** Append to the `tests` module in `crates/leaf-web/src/request.rs` (before its closing `}`):

```rust
    #[test]
    fn new_wraps_bytes_as_a_full_body_and_into_body_yields_it() {
        use crate::body::Body;
        let req = Request::new(
            Method::POST,
            "/x".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(b"payload"),
        );
        // body_bytes() still reads the Full variant, unchanged.
        assert_eq!(req.body_bytes(), b"payload".as_slice());
        // into_body() hands out the Body; it is the Full variant.
        match req.into_body() {
            Body::Full(b) => assert_eq!(b, Bytes::from_static(b"payload")),
            Body::Stream(_) => panic!("Request::new must wrap Bytes as Body::Full"),
        }
    }

    #[test]
    fn body_bytes_is_empty_for_a_stream_body() {
        use crate::body::{Body, Frame};
        let stream = futures::stream::iter(vec![Ok(Frame::Data(Bytes::from_static(b"abc")))]);
        let mut req = Request::new(Method::POST, "/x".parse().expect("uri"), HeaderMap::new(), Bytes::new());
        req.set_body(Body::Stream(Box::pin(stream)));
        // A streamed body has nothing buffered yet, so body_bytes() reports empty (the
        // dispatcher collects a streamed HTTP body BEFORE a handler sees it; gRPC reads the
        // stream directly). It must NOT panic.
        assert_eq!(req.body_bytes(), b"".as_slice());
    }
```

- [ ] **Step 2: Run it — expect FAIL (compile).** `into_body`/`set_body`/`Body` do not exist; `Request.body` is still `Bytes`.

```
cargo test -p leaf-web request:: 2>&1 | tail -15
```
Expected: `error[E0599]: no method named into_body` (and `set_body`).

- [ ] **Step 3: Migrate the field + ctor + accessors.** In `crates/leaf-web/src/request.rs`: replace the `use bytes::Bytes;` import-region top to also import `Body`, change the field, and rework `new`/`body_bytes`, then add `into_body`/`set_body`.

Change the imports (line 8) to add the body types:

```rust
use bytes::Bytes;
use http::{HeaderMap, Method, Uri};

use crate::body::Body;
```

Change the field (line 30) from `body: Bytes,` to:

```rust
    /// The request body — buffered ([`Body::Full`]) or streamed ([`Body::Stream`]). The
    /// [`Dispatcher`](crate::Dispatcher) collects a streamed HTTP body to Full BEFORE a
    /// route handler runs, so every extractor / [`body_bytes`](Request::body_bytes) call
    /// sees buffered bytes; a gRPC handler reads the stream directly.
    body: Body,
```

Change `new` (lines 42–44). It keeps the `Bytes` arg (source-compat) and wraps it:

```rust
    #[must_use]
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Request {
            method,
            uri,
            headers,
            path_params: Vec::new(),
            body: Body::Full(body),
            extensions: HashMap::new(),
        }
    }
```

Change `body_bytes` (lines 85–88) to read the Full variant (empty for a stream):

```rust
    /// The request body as a byte slice — the buffered bytes of a [`Body::Full`].
    ///
    /// A [`Body::Stream`] reports empty here (it is consumed frame-by-frame, not buffered):
    /// HTTP handlers only ever see a body the [`Dispatcher`](crate::Dispatcher) already
    /// collected to Full, and gRPC handlers read [`into_body`](Request::into_body) directly.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        match &self.body {
            Body::Full(bytes) => bytes,
            Body::Stream(_) => &[],
        }
    }
```

Add `into_body` + `set_body` right after `body_bytes` (after line ~96):

```rust
    /// Take the [`Body`] out of the request (a gRPC handler consumes the frame stream this
    /// way; the dispatcher takes it to collect a streamed HTTP body before a handler).
    #[must_use]
    pub fn into_body(self) -> Body {
        self.body
    }

    /// Replace the body (the backend edge installs a [`Body::Stream`]; the dispatcher swaps
    /// in the collected [`Body::Full`] before an HTTP handler runs).
    pub fn set_body(&mut self, body: Body) {
        self.body = body;
    }
```

The `Request` derives `#[derive(Clone, Debug)]` (line 18); `Body` is neither `Clone` nor `Debug`. Remove those derives and hand-roll nothing — but the dispatcher clones the request for the advice error path (server.rs line 170) and several tests `req.clone()`. To preserve `Clone` cheaply WITHOUT cloning a stream, do NOT keep `#[derive(Clone)]`. Instead, the advice path is reworked in Task 1.6 to clone only the request *parts* it needs (method/uri/headers/path_params), so `Request` no longer needs `Clone`/`Debug`. For THIS task, drop both derives:

Change line 18 from `#[derive(Clone, Debug)]` to (no derive — add the attribute removal):

```rust
// `Request` is intentionally NOT `Clone`/`Debug`: its `Body` may be a one-shot frame
// stream (a `BoxStream` is neither). The advice error path clones the request's PARTS
// (method/uri/headers/path_params) it needs instead of the whole request.
pub struct Request {
```

- [ ] **Step 4: Update the two `Request`-clone tests in request.rs.** The `typed_extension_round_trips_by_type` test is fine. The `request_stays_clone_with_extensions` test (lines 195–203) asserts `req.clone()`; replace its body to assert the extension survives a parts-style copy is out of scope — instead delete that test (Clone is gone) and replace it with a body-variant assertion already added in Step 1. Remove lines 195–203 (`request_stays_clone_with_extensions`).

- [ ] **Step 5: Run it — expect PASS for request, but the crate won't fully build yet** (server.rs/response.rs/embedded.rs still reference the old shapes). Run just the request module's tests after Task 1.6 makes the crate build. For now confirm the request module type-checks in isolation is not possible standalone; proceed to 1.4–1.6 and run the whole crate at 1.6 Step. Mark this step done once 1.4–1.6 land.

- [ ] **Step 6: Commit** (after 1.4–1.6 compile together — squash-friendly; commit at end of 1.6). Skip an isolated commit here.

---

### Task 1.4: Migrate `Response.body` to `Body` (+ `with_body_stream`)

**Files:** `crates/leaf-web/src/response.rs` (modify)

The contract: keep `Response::with_body(impl Into<Bytes>)` (→ `Body::Full`); add `with_body_stream(BoxStream<...>)`; keep `body_bytes()`.

- [ ] **Step 1: Write the failing tests.** Append to the `tests` module in `crates/leaf-web/src/response.rs`:

```rust
    #[test]
    fn with_body_accepts_anything_into_bytes_and_is_full() {
        use crate::body::Body;
        // A &'static [u8], a Vec<u8>, and Bytes all satisfy `impl Into<Bytes>`.
        let resp = Response::ok().with_body(b"hello".as_slice());
        assert_eq!(resp.body_bytes(), b"hello".as_slice());
        match resp.into_body() {
            Body::Full(b) => assert_eq!(b, Bytes::from_static(b"hello")),
            Body::Stream(_) => panic!("with_body must produce Body::Full"),
        }
    }

    #[test]
    fn with_body_stream_is_a_stream_with_empty_body_bytes() {
        use crate::body::{Body, Frame};
        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"chunk"))),
        ]);
        let resp = Response::ok().with_body_stream(Box::pin(frames));
        // A streamed response reports empty buffered bytes (it is written frame-by-frame).
        assert_eq!(resp.body_bytes(), b"".as_slice());
        assert!(matches!(resp.into_body(), Body::Stream(_)));
    }
```

- [ ] **Step 2: Run it — expect FAIL (compile):** `with_body_stream`/`into_body` missing, `with_body(&[u8])` not accepted (signature is `Bytes`).

```
cargo test -p leaf-web response:: 2>&1 | tail -15
```

- [ ] **Step 3: Migrate the field + builders.** In `crates/leaf-web/src/response.rs`:

Add the body import (after line 7 `use leaf_core::LeafError;`):

```rust
use leaf_core::BoxStream;

use crate::body::{Body, Frame};
```

Change the field (line 17) `body: Bytes,` to `body: Body,`, and drop `#[derive(Clone, Debug)]` on `Response` (line 13) — `Body` is neither (the advice path in Task 1.6 no longer clones a `Response`):

```rust
// `Response` is intentionally NOT `Clone`/`Debug`: its `Body` may be a one-shot frame
// stream. The error/advice paths build a fresh `Response`, never clone one.
pub struct Response {
```

Change `new` (lines 22–25) to start Full-empty:

```rust
    #[must_use]
    pub fn new(status: StatusCode) -> Self {
        Response { status, headers: HeaderMap::new(), body: Body::Full(Bytes::new()) }
    }
```

Change `body_bytes` (lines 46–49):

```rust
    /// The response body as a byte slice — the buffered bytes of a [`Body::Full`]; empty
    /// for a [`Body::Stream`] (written frame-by-frame at the edge).
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        match &self.body {
            Body::Full(bytes) => bytes,
            Body::Stream(_) => &[],
        }
    }
```

Change `with_body` (lines 59–63) to take `impl Into<Bytes>` (source-compatible — `Bytes`, `Vec<u8>`, `&[u8]`, `String` all qualify), wrapping Full:

```rust
    /// Replace the body with a buffered blob (builder style). Accepts anything that is
    /// `Into<Bytes>` (`Bytes`/`Vec<u8>`/`&[u8]`/`String`); produces a [`Body::Full`].
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Body::Full(body.into());
        self
    }
```

Add `with_body_stream` + `into_body` right after `with_body`:

```rust
    /// Replace the body with a STREAM of [`Frame`]s (builder style) — the streaming
    /// response path (gRPC data frames + a terminating trailers frame, SSE, etc.). The
    /// backend writes these frames at the edge; nothing is buffered.
    #[must_use]
    pub fn with_body_stream(
        mut self,
        stream: BoxStream<'static, Result<Frame, LeafError>>,
    ) -> Self {
        self.body = Body::Stream(stream);
        self
    }

    /// Take the [`Body`] out of the response (the backend edge consumes this to write the
    /// Full bytes or stream the frames).
    #[must_use]
    pub fn into_body(self) -> Body {
        self.body
    }
```

- [ ] **Step 4: Fix the `Bytes::from(self.into_bytes())` call sites.** The `IntoResponse for String` impl (line 107) calls `.with_body(Bytes::from(self.into_bytes()))` — still valid (`Bytes: Into<Bytes>`). The `IntoResponseWith` impls (lines 239, 251) call `.with_body(body)` where `body: Bytes` (the converter's `write` returns `Bytes`) — still valid. No change needed; `with_body` is strictly more permissive. The `into_response_for_response_is_identity` test (line 283) builds `Response::new(..).with_body(Bytes::from_static(b"x"))` — still valid.

- [ ] **Step 5: Run it — expect PASS for response (after the crate builds at 1.6).** Same as 1.3: the crate won't fully link until server.rs/embedded.rs are migrated. Defer the run to 1.6.

- [ ] **Step 6: Commit** — deferred to 1.6 (one squashed migration commit, since these four files only compile together).

---

### Task 1.5: The `ProtocolDispatch` trait + `impl_resolve_view!`

**Files:** `crates/leaf-web/src/server.rs` (modify), `crates/leaf-web/src/lib.rs` (modify)

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `crates/leaf-web/src/server.rs` (before its closing `}`):

```rust
    // ── ProtocolDispatch: the abstract protocol-routing seam (gRPC plugs in here) ──

    /// A fake protocol-dispatch that CLAIMS one content-type and answers a fixed status,
    /// so the dispatcher test can prove the branch without naming leaf-grpc.
    struct FakeProtocol {
        claims: &'static str,
        status: StatusCode,
    }

    impl crate::server::ProtocolDispatch for FakeProtocol {
        fn handles(&self, content_type: Option<&str>) -> bool {
            content_type.is_some_and(|ct| ct.starts_with(self.claims))
        }
        fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
            Box::pin(async move { Ok(Response::new(self.status)) })
        }
    }

    #[test]
    fn protocol_dispatch_handles_matches_by_content_type_prefix() {
        let p = FakeProtocol { claims: "application/grpc", status: StatusCode::OK };
        assert!(p.handles(Some("application/grpc")));
        assert!(p.handles(Some("application/grpc+proto")));
        assert!(!p.handles(Some("application/json")));
        assert!(!p.handles(None));
    }
```

- [ ] **Step 2: Run it — expect FAIL (compile):** `ProtocolDispatch` does not exist.

```
cargo test -p leaf-web protocol_dispatch 2>&1 | tail -10
```

- [ ] **Step 3: Define the trait + the view seam.** In `crates/leaf-web/src/server.rs`, after the `WebServer` `impl_resolve_view!` line (line 108), add:

```rust
/// The ABSTRACT protocol-dispatch seam (the design's §1): a second `Handler` family that
/// runs on the SHARED [`WebServer`]/[`Dispatcher`], selected by `content-type`. A request
/// whose `content-type` no HTTP [`Route`](crate::Route) claims is delegated to the first
/// `ProtocolDispatch` whose [`handles`](ProtocolDispatch::handles) returns `true`.
///
/// This is how leaf-web routes to gRPC WITHOUT naming `leaf-grpc`: the gRPC family is ONE
/// `dyn ProtocolDispatch` impl contributed BY `leaf-grpc` (matching `application/grpc*`),
/// so the dep arrow stays `leaf-grpc → leaf-web`, never the reverse. WebSocket etc. plug in
/// the same way. Selection is by the runtime `content-type` HEADER VALUE — never a Rust
/// type's spelled name.
pub trait ProtocolDispatch: Send + Sync {
    /// Whether this protocol claims a request with the given `content-type` (e.g. gRPC
    /// claims any value starting `application/grpc`). `None` = no `content-type` header.
    fn handles(&self, content_type: Option<&str>) -> bool;

    /// Dispatch a claimed request to a [`Response`] (whose [`Body`](crate::Body) may be a
    /// frame stream with trailers — gRPC renders `grpc-status` as trailers, never an `Err`).
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] only for a protocol-level failure the [`ControlAdvice`] chain
    /// should map; a gRPC protocol renders application status as trailers, not `Err`.
    fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>>;
}

// The by-trait-injection seam (emitted once, beside the trait — orphan-rule-OK,
// `dyn ProtocolDispatch` is local). It makes `Ref<dyn ProtocolDispatch>` injectable and
// `Vec<Ref<dyn ProtocolDispatch>>` collectible through the SAME path `dyn Route`/`dyn
// WebFilter` use — the gRPC family is collected exactly like the HTTP route/filter families.
leaf_core::impl_resolve_view!(dyn ProtocolDispatch);
```

- [ ] **Step 4: Re-export.** In `crates/leaf-web/src/lib.rs`, change the server re-export (line 113):

```rust
pub use server::{Dispatcher, ProtocolDispatch, ServerProperties, WebServer};
```

- [ ] **Step 5: Run it — expect PASS for the `handles` test** (after the crate builds at 1.6). The `protocol_dispatch_handles_matches_by_content_type_prefix` test does not touch the `Dispatcher` field, but the crate must build first. Defer the run to 1.6.

- [ ] **Step 6: Commit** — deferred to 1.6.

---

### Task 1.6: The `Dispatcher` content-type branch + `Vec<Arc<dyn ProtocolDispatch>>` collection

**Files:** `crates/leaf-web/src/server.rs` (modify), `crates/leaf-web/src/embedded.rs` (modify), `crates/leaf-web/src/testing.rs` (no change beyond the call already routed through `Dispatcher::new`)

This is the load-bearing change: `Dispatcher::new` gains a `protocols` arg; `dispatch` (a) reads the request's `content-type`, (b) if a `ProtocolDispatch` claims it, delegates the WHOLE `Request` (stream intact) to it, (c) otherwise COLLECTS the body to `Body::Full` and runs the HTTP route family (so REST is unchanged), and (d) reworks the advice error path to clone request PARTS (since `Request` is no longer `Clone`).

- [ ] **Step 1: Write the failing tests.** Append to the `tests` module in `crates/leaf-web/src/server.rs`:

```rust
    fn grpc_req(path: &str) -> Request {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
        Request::new(Method::POST, path.parse().expect("uri"), headers, Bytes::new())
    }

    #[test]
    fn a_grpc_content_type_is_delegated_to_the_protocol_dispatch() {
        // GET /ok is a registered HTTP route, but THIS request is application/grpc, so the
        // protocol family must claim it (proving the content-type branch, not the route).
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::POST, path: "/ok", handler: FakeHandler::Ok("http") });
        let proto: Arc<dyn ProtocolDispatch> =
            Arc::new(FakeProtocol { claims: "application/grpc", status: StatusCode::ACCEPTED });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(grpc_req("/ok")));

        // 202 ACCEPTED comes from the protocol family, NOT the route's 200 "http".
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[test]
    fn a_non_grpc_content_type_stays_on_the_http_route_family() {
        // A plain request (no application/grpc content-type) runs the HTTP route family even
        // when a ProtocolDispatch is present — the protocol must DECLINE it.
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::GET, path: "/ok", handler: FakeHandler::Ok("http") });
        let proto: Arc<dyn ProtocolDispatch> =
            Arc::new(FakeProtocol { claims: "application/grpc", status: StatusCode::ACCEPTED });

        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![proto]);
        let resp = futures::executor::block_on(dispatcher.dispatch(get("/ok")));

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"http".as_slice());
    }

    #[test]
    fn a_streamed_http_body_is_collected_before_the_route_handler() {
        use crate::body::{Body, Frame};
        // An HTTP request arriving with a STREAM body (no grpc content-type) must be
        // collected to Full before the route handler runs, so the Echo handler sees the
        // buffered bytes via body_bytes() — proving collect-before-handler keeps REST
        // ergonomic.
        let route: Arc<dyn Route> =
            Arc::new(FakeRoute { method: Method::POST, path: "/echo", handler: FakeHandler::Echo });
        let dispatcher = Dispatcher::new(vec![route], vec![], vec![], vec![]);

        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"strea"))),
            Ok(Frame::Data(Bytes::from_static(b"med"))),
        ]);
        let mut req = Request::new(Method::POST, "/echo".parse().expect("uri"), http::HeaderMap::new(), Bytes::new());
        req.set_body(Body::Stream(Box::pin(frames)));

        let resp = futures::executor::block_on(dispatcher.dispatch(req));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"streamed".as_slice());
    }
```

Add an `Echo` arm to the `FakeHandler` enum + impl in this module (it has only `Ok`/`Err` today). In `enum FakeHandler` add `Echo,`:

```rust
    enum FakeHandler {
        Ok(&'static str),
        Err(ErrorKind),
        Echo,
    }
```

and in its `Handler` impl add the arm (returns the collected body bytes):

```rust
                    FakeHandler::Echo => {
                        Ok(Response::ok().with_body(Bytes::copy_from_slice(_req.body_bytes())))
                    }
```

(rename the `_req` binding to `req` in that impl since `Echo` now reads it: change `_req: &'a Request` to `req: &'a Request`.)

- [ ] **Step 2: Run it — expect FAIL (compile):** `Dispatcher::new` takes 3 args, not 4.

```
cargo test -p leaf-web server:: 2>&1 | tail -15
```

- [ ] **Step 3: Add the `protocols` field + `new` arg.** In `crates/leaf-web/src/server.rs`, add to the `Dispatcher` struct (after the `advice` field, line 127):

```rust
    /// The container-collected protocol families (gRPC etc.), checked by `content-type`
    /// BEFORE the HTTP route family. Empty in a pure-HTTP app.
    protocols: Vec<Arc<dyn ProtocolDispatch>>,
```

Change `Dispatcher::new` (lines 137–147) to take + store `protocols`:

```rust
    #[must_use]
    pub fn new(
        routes: Vec<Arc<dyn Route>>,
        filters: Vec<Arc<dyn WebFilter>>,
        advice: Vec<Arc<dyn ControlAdvice>>,
        protocols: Vec<Arc<dyn ProtocolDispatch>>,
    ) -> Self {
        let mut filters = filters;
        filters.sort_by_key(|f| f.order());
        let mut advice = advice;
        advice.sort_by_key(|a| a.order());
        Dispatcher { routes, filters, advice, protocols }
    }
```

- [ ] **Step 4: Rework `dispatch` — protocol branch, collect-before-HTTP, parts-only advice.** Replace the whole `dispatch` method body (lines 156–175) with:

```rust
    pub async fn dispatch(&self, mut req: Request) -> Response {
        // 1. PROTOCOL BRANCH: if a ProtocolDispatch claims this request's content-type,
        //    delegate the WHOLE request (the frame stream INTACT — gRPC reads it directly).
        //    Selection is by the runtime content-type header value, never a Rust type name.
        let content_type = req.header(http::header::CONTENT_TYPE.as_str()).map(str::to_owned);
        if let Some(proto) = self
            .protocols
            .iter()
            .find(|p| p.handles(content_type.as_deref()))
        {
            // Capture the parts the advice path needs BEFORE the request moves into dispatch
            // (Request is no longer Clone — its Body may be a one-shot stream).
            let parts = RequestParts::snapshot(&req);
            return match proto.dispatch(req).await {
                Ok(resp) => resp,
                Err(err) => self.map_error(&err, &parts),
            };
        }

        // 2. HTTP PATH: COLLECT a streamed body to Full BEFORE the route family runs, so
        //    every extractor / body_bytes() call sees buffered bytes (REST stays ergonomic).
        //    The collect is bounded by the body limit (mirrors the transport edge cap).
        if req.body_is_stream() {
            let body = std::mem::replace(req.body_mut_placeholder(), crate::body::Body::Full(bytes::Bytes::new()));
            match body.collect(usize::MAX).await {
                Ok(collected) => req.set_body(crate::body::Body::Full(collected)),
                Err(err) => {
                    let parts = RequestParts::snapshot(&req);
                    return self.map_error(&err, &parts);
                }
            }
        }

        // 3. Build the routing table + the filter chain whose terminal matches + invokes.
        let route_refs: Vec<&dyn Route> = self.routes.iter().map(AsRef::as_ref).collect();
        let table = RouteTable::build(&route_refs);
        let terminal = RouteTerminal { table: &table };
        let filter_refs: Vec<&dyn WebFilter> = self.filters.iter().map(AsRef::as_ref).collect();
        let chain = FilterChain::new(&filter_refs, &terminal);

        // The advice path needs the request PARTS (Request is not Clone); snapshot them.
        let parts = RequestParts::snapshot(&req);
        match chain.run(req).await {
            Ok(resp) => resp,
            Err(err) => self.map_error(&err, &parts),
        }
    }
```

Note this introduces three helpers: a `RequestParts` snapshot the advice consumes (`ControlAdvice::handle` takes `&Request`, so we rebuild a body-less `Request` from the parts), plus `Request::body_is_stream()` and a body-take. To keep it clean, simplify by NOT inventing `body_mut_placeholder`: instead use a `Request::take_body` helper. Replace the step-2 block above with this cleaner form (use THIS version):

```rust
        // 2. HTTP PATH: COLLECT a streamed body to Full BEFORE the route family runs.
        if req.body_is_stream() {
            let body = req.take_body();
            match body.collect(usize::MAX).await {
                Ok(collected) => req.set_body(crate::body::Body::Full(collected)),
                Err(err) => {
                    let parts = RequestParts::snapshot(&req);
                    return self.map_error(&err, &parts);
                }
            }
        }
```

- [ ] **Step 5: Add `body_is_stream`/`take_body` to `Request` + the `RequestParts` snapshot, and retarget `map_error` to `&Request`.** The simplest design that keeps `ControlAdvice::handle(&self, &LeafError, &Request)` UNCHANGED: snapshot the request into a fresh body-less `Request` clone of its parts.

In `crates/leaf-web/src/request.rs` add (after `set_body`):

```rust
    /// Whether the body is the streaming variant (the dispatcher collects it before an
    /// HTTP handler runs).
    #[must_use]
    pub fn body_is_stream(&self) -> bool {
        self.body.is_stream()
    }

    /// Take the body OUT, leaving an empty Full body in its place (the dispatcher collects
    /// the taken stream, then installs the collected Full via [`set_body`](Request::set_body)).
    #[must_use]
    pub fn take_body(&mut self) -> Body {
        std::mem::replace(&mut self.body, Body::Full(Bytes::new()))
    }

    /// A body-less copy of the request's PARTS (method/uri/headers/path_params + extensions)
    /// — what the advice error path consumes (`Request` is not `Clone` because its body may
    /// be a one-shot stream, but the parts ARE cheaply cloneable).
    #[must_use]
    pub fn parts_clone(&self) -> Request {
        Request {
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            path_params: self.path_params.clone(),
            body: Body::Full(Bytes::new()),
            extensions: self.extensions.clone(),
        }
    }
```

In `crates/leaf-web/src/server.rs`, replace the `RequestParts::snapshot(&req)` calls with `req.parts_clone()` and drop the `RequestParts` type entirely (it was a placeholder). The final `dispatch` uses `let parts = req.parts_clone();` and `self.map_error(&err, &parts)`. Update `map_error` — it already takes `&Request` (line 179), no change.

So the THREE `RequestParts::snapshot(&req)` occurrences become `req.parts_clone()`, and the protocol branch must snapshot BEFORE moving `req`:

```rust
        if let Some(proto) = self
            .protocols
            .iter()
            .find(|p| p.handles(content_type.as_deref()))
        {
            let parts = req.parts_clone();
            return match proto.dispatch(req).await {
                Ok(resp) => resp,
                Err(err) => self.map_error(&err, &parts),
            };
        }
```

- [ ] **Step 6: Thread `protocols` through `embedded.rs`.** In `crates/leaf-web/src/embedded.rs`: import `ProtocolDispatch` (line 40 area) and add the injected collection field + thread it into `Dispatcher::new`.

Change the use (line 40):

```rust
use crate::server::{Dispatcher, ProtocolDispatch, ServerProperties, WebServer};
```

Add the field to `EmbeddedWebServer` (after `advice`, line 61):

```rust
    /// Every protocol family any crate contributed (gRPC etc.) — checked by content-type
    /// before the HTTP route family. Collection + by-trait injection, like routes/filters.
    protocols: Vec<Ref<dyn ProtocolDispatch>>,
```

Change the `Dispatcher::new` call in `start` (lines 78–82):

```rust
        let dispatcher = Arc::new(Dispatcher::new(
            self.routes.iter().map(|r| Ref::clone(r).into_arc()).collect(),
            self.filters.iter().map(|f| Ref::clone(f).into_arc()).collect(),
            self.advice.iter().map(|a| Ref::clone(a).into_arc()).collect(),
            self.protocols.iter().map(|p| Ref::clone(p).into_arc()).collect(),
        ));
```

- [ ] **Step 7: Fix the in-crate `Dispatcher::new` call sites in tests.** Every existing `Dispatcher::new(routes, filters, advice)` in `server.rs` tests, `testing.rs` tests, `dispatch_through_mock.rs`, `control_advice.rs`, `controller_routes.rs`, `web_filter.rs`, `web_extension.rs` gains a trailing `vec![]`. Find them:

```
grep -rn "Dispatcher::new(" crates/leaf-web/
```
For each, append `, vec![]` as the fourth arg. (The `server.rs` tests added in Step 1 already pass four args.)

- [ ] **Step 8: Build + run the whole leaf-web suite — expect PASS.** This is where Tasks 1.3–1.6 first compile together.

```
cargo test -p leaf-web 2>&1 | tail -20
```
Expected: ALL leaf-web tests pass — `body::`, `request::` (incl. the new `new_wraps_bytes_as_a_full_body...`/`body_bytes_is_empty_for_a_stream_body`), `response::` (incl. `with_body_stream_is_a_stream...`), `server::` (incl. `a_grpc_content_type_is_delegated...`, `a_non_grpc_content_type_stays...`, `a_streamed_http_body_is_collected...`, `protocol_dispatch_handles_...`), and every existing integration test (`controller_routes`, `control_advice`, `dispatch_through_mock`, `web_filter`, `web_extension`, `ui`) green.

- [ ] **Step 9: Commit the leaf-web migration (Tasks 1.3–1.6 squashed).**

```
git add crates/leaf-web/
git commit -m "leaf-web: migrate Request/Response to streaming Body + ProtocolDispatch branch

Request.body/Response.body are now Body (Full|Stream); Request::new keeps a Bytes
arg (=> Body::Full) and Response::with_body takes impl Into<Bytes>, so every existing
call site is unchanged. Add ProtocolDispatch + impl_resolve_view!; the Dispatcher
collection-injects Vec<Arc<dyn ProtocolDispatch>> and branches on content-type before
the HTTP route family, COLLECTING a streamed HTTP body to Full before any handler runs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.7: Enable `http2` + map the hyper `Incoming` ↔ streaming `Body`

**Files:** `crates/leaf-web-hyper/Cargo.toml` (modify), `crates/leaf-web-hyper/src/server.rs` (modify)

The contract: `to_leaf_request` builds a `Body::Stream` from hyper's `Incoming` (data + trailers frames), bounded by `max_request_body_bytes` on the collect path; the out-conversion writes `Body::Full` as today and `Body::Stream` as an H2 frame stream with trailers. Enable `http2`.

- [ ] **Step 1: Enable http2 + add the body crates in `Cargo.toml`.** In `crates/leaf-web-hyper/Cargo.toml`:

Change the `hyper` line (line 25) to add `http2`:

```toml
hyper = { workspace = true, features = ["server", "http1", "http2"] }
```

Change the `hyper-util` line (line 29) to add `http2`:

```toml
hyper-util = { workspace = true, features = ["server", "server-auto", "server-graceful", "tokio", "http1", "http2"] }
```

Add `http-body` (the `Body`/`Frame` trait for the out-stream + reading `Incoming` frames) and `futures` (to build a `BoxStream`) to `[dependencies]` (after the `http-body-util` line, line 30):

```toml
# The http-body 1.x trait — read hyper `Incoming` frames (data + trailers) for the
# inbound stream and IMPLEMENT a streaming body for the outbound frame stream. Pure HTTP
# value layer (not a server), the sanctioned place to name it: this IS the backend edge.
http-body.workspace = true
# `futures` builds the `leaf_core::BoxStream` the leaf `Body::Stream` carries at this edge.
futures.workspace = true
```

Add `http-body` to the workspace deps if not present. Check `Cargo.toml` (workspace root) line 64 area; it pins `http-body-util` but not `http-body` directly. Add beside it:

```toml
http-body = "1"
```

- [ ] **Step 2: Write the failing edge test.** Append to `crates/leaf-web-hyper/tests/serves_http.rs` a test that proves an HTTP/2 prior-knowledge request round-trips a body through the now-streaming edge (regression that http2 is on AND the Incoming→Body::Stream→collect path works for a body the route echoes):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_prior_knowledge_request_round_trips_through_the_streaming_edge() {
    // Enabling http2 + mapping Incoming to a streaming Body must not regress HTTP: an
    // HTTP/2 (prior-knowledge) POST /echo round-trips its body, proving the auto-builder
    // negotiates h2 AND the inbound stream collects to Full before the route handler.
    let routes: Vec<Arc<dyn Route>> =
        vec![Arc::new(FakeRoute { method: Method::POST, path: "/echo", handler: FakeHandler::Echo })];
    let dispatcher = Arc::new(Dispatcher::new(routes, vec![], vec![], vec![]));

    let port = free_port();
    let props = Arc::new(ServerProperties { host: "127.0.0.1".to_string(), port, ..Default::default() });
    let server = Arc::new(HyperServer::new());

    let (signal, trigger) = leaf_core::shutdown_channel();
    let ctx = leaf_core::LifecycleCtx { shutdown: signal, on_ready: Box::new(|| {}), grace: None };
    let serve_server = server.clone();
    let serve_dispatcher = dispatcher.clone();
    let serve_props = props.clone();
    let serving =
        tokio::spawn(async move { serve_server.serve(serve_dispatcher, serve_props, ctx).await });

    let base = format!("http://127.0.0.1:{port}");
    wait_until_up(port).await;

    // Force HTTP/2 with prior knowledge (cleartext h2c, no upgrade dance) — the auto
    // builder must accept it now that http2 is enabled.
    let client = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .expect("h2 client");
    let resp = client.post(format!("{base}/echo")).body("over-h2").send().await.expect("h2 POST");
    assert_eq!(resp.version(), reqwest::Version::HTTP_2);
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.expect("h2 echo body"), "over-h2");

    trigger.fire();
    assert!(serving.await.expect("serve joins").is_ok(), "clean drain after the h2 round-trip");
}
```

Update the existing `Dispatcher::new(routes, ...)` calls in this file to pass the trailing `vec![]` (the four other tests). The `Dispatcher::new` import is already there; the `Echo`/`FakeHandler` fakes already exist in this file.

- [ ] **Step 3: Run it — expect FAIL:** the http2 features aren't built yet (the client gets a connection error or the existing `Dispatcher::new` arity breaks compile).

```
cargo test -p leaf-web-hyper http2_prior_knowledge 2>&1 | tail -20
```

- [ ] **Step 4: Map the inbound `Incoming` to a `Body::Stream`.** In `crates/leaf-web-hyper/src/server.rs`, replace `to_leaf_request` (lines 193–208) so it builds a `Body::Stream` from the hyper `Incoming` frames (data + trailers), bounded by `max_body` on the COLLECT path (the dispatcher collects HTTP; gRPC reads the stream). Add the imports first.

Add to the imports near line 22:

```rust
use futures::StreamExt;
use http_body::Body as HttpBody;
use leaf_web::body::{Body as LeafBody, Frame as LeafFrame};
```

Replace `to_leaf_request`:

```rust
/// Convert a hyper `Request<Incoming>` into a leaf [`Request`] whose body is a
/// [`LeafBody::Stream`] of the hyper [`Incoming`] frames (data + trailers).
///
/// The body is NOT collected here: the leaf [`Dispatcher`] collects an HTTP body to Full
/// (bounded by `max_body`) before a route handler, while a gRPC protocol reads the stream
/// directly — so the edge stays uniform for both. The per-frame `byte_budget` enforces
/// `max_body` AS the stream is drained downstream (the leaf `Body::collect` re-checks too),
/// so an oversize HTTP body is rejected before being buffered wholesale; the gRPC path is
/// bounded by its own framing limits later. A frame-read failure becomes a stream `Err`
/// the leaf side maps via the advice chain.
async fn to_leaf_request(
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<Request, BodyEdgeError> {
    let (parts, incoming) = hyper_req.into_parts();

    // Turn the hyper body into a `futures::Stream` of leaf frames. `BodyStream`-style
    // polling: each hyper frame is either data or trailers; map both into `LeafFrame`.
    let mut budget = max_body;
    let frames = futures::stream::unfold(incoming, |mut body| async move {
        match std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await {
            Some(Ok(frame)) => {
                let leaf = if let Ok(data) = frame.into_data() {
                    Ok(LeafFrame::Data(data))
                } else {
                    // Not data → it is the trailers frame (the only other kind hyper emits).
                    // `into_data` returned the frame back on Err; re-poll is not needed —
                    // reconstruct by trying trailers on a fresh poll is avoided by handling
                    // both in one match below. (see note) — instead handle via a helper.
                    unreachable!("handled by frame_to_leaf")
                };
                Some((leaf, body))
            }
            Some(Err(_e)) => Some((Err(edge_stream_error()), body)),
            None => None,
        }
    });
    // The closure above cannot both consume `frame` for data AND fall back to trailers in
    // one arm, so use the dedicated `frame_to_leaf` mapper instead:
    let _ = (&frames, &mut budget); // placeholder to satisfy the compiler in the doc draft

    Ok(Request::new(parts.method, parts.uri, parts.headers, Bytes::new()))
}
```

The `unfold` body above is intentionally shown as a DRAFT to flag the `into_data`/`into_trailers` two-step; replace the WHOLE `to_leaf_request` with this CORRECT, compiling version (use THIS one):

```rust
/// Convert a hyper `Request<Incoming>` into a leaf [`Request`] whose body is a
/// [`LeafBody::Stream`] of the hyper [`Incoming`] frames (data + trailers).
///
/// Nothing is collected HERE: the leaf [`Dispatcher`] collects an HTTP body to Full
/// (bounded by `max_body`) before a route handler, while a gRPC protocol reads the stream
/// directly. The per-frame `budget` enforces `max_body` as the data drains so an oversize
/// HTTP body is rejected before being buffered wholesale; a frame-read failure becomes a
/// stream `Err` the leaf advice chain maps.
fn to_leaf_request(hyper_req: hyper::Request<Incoming>, max_body: usize) -> Request {
    let (parts, incoming) = hyper_req.into_parts();
    let stream = incoming_to_leaf_frames(incoming, max_body);
    let mut req = Request::new(parts.method, parts.uri, parts.headers, Bytes::new());
    req.set_body(LeafBody::Stream(Box::pin(stream)));
    req
}

/// Drain a hyper [`Incoming`] into a `Stream` of leaf [`LeafFrame`]s (data → `Data`,
/// trailers → `Trailers`), enforcing `max_body` across the cumulative data bytes (an
/// overflow yields a `ConvertError` stream `Err` → the leaf side maps it to 413/4xx).
fn incoming_to_leaf_frames(
    incoming: Incoming,
    max_body: usize,
) -> impl futures::Stream<Item = Result<LeafFrame, LeafError>> + Send + 'static {
    futures::stream::unfold((incoming, 0usize), move |(mut incoming, mut seen)| async move {
        let polled =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut incoming).poll_frame(cx)).await;
        match polled {
            None => None,
            Some(Err(_e)) => Some((Err(edge_stream_error("reading the request body")), (incoming, seen))),
            Some(Ok(frame)) => {
                // A hyper frame is data OR trailers; `into_data` hands the frame back on Err.
                match frame.into_data() {
                    Ok(data) => {
                        seen += data.len();
                        if seen > max_body {
                            Some((Err(edge_stream_error("request body over the limit")), (incoming, seen)))
                        } else {
                            Some((Ok(LeafFrame::Data(data)), (incoming, seen)))
                        }
                    }
                    Err(frame) => match frame.into_trailers() {
                        Ok(trailers) => Some((Ok(LeafFrame::Trailers(trailers)), (incoming, seen))),
                        // Neither data nor trailers (no other hyper frame kind today): skip it
                        // by ending the stream — there is nothing meaningful to yield.
                        Err(_other) => None,
                    },
                }
            }
        }
    })
}

/// A streamed-body read failure as a `ConvertError` (a client-fault → 4xx via the default
/// advice floor; an oversize body likewise maps to a client-fault status).
fn edge_stream_error(what: &'static str) -> LeafError {
    LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain("the request body stream", what))
}
```

- [ ] **Step 5: Update `serve_one` for the now-infallible `to_leaf_request` + drop the old `BodyEdgeError`/`Limited` path.** `to_leaf_request` no longer returns `Result` (the cap is enforced inside the stream → a `LeafError` the dispatcher maps), so the edge no longer pre-collects. Replace `serve_one` (lines 161–173) and delete the `BodyEdgeError` enum (lines 178–181):

```rust
/// Handle one hyper request end to end at the boundary: convert in → dispatch → convert
/// out. `to_leaf_request` builds a STREAMING leaf body (the dispatcher collects an HTTP
/// body to Full, bounded by `max_body`, before a route handler; a gRPC protocol reads the
/// stream). The dispatcher never errors out, so this is `Infallible`.
async fn serve_one(
    dispatcher: Arc<Dispatcher>,
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<hyper::Response<HyperOutBody>, Infallible> {
    let leaf_req = to_leaf_request(hyper_req, max_body);
    Ok(to_hyper_response(dispatcher.dispatch(leaf_req).await))
}
```

Remove the now-unused `Limited` import (line 23 `http_body_util::{BodyExt, Full, Limited}` → keep `Full`; `BodyExt`/`Limited` may be unused — drop them). The `LeafBody`-collect cap inside the dispatcher uses `usize::MAX` (Task 1.6 Step 4); to actually enforce `max_body` on the HTTP collect path the per-frame budget in `incoming_to_leaf_frames` is the real gate (it errors before buffering), so the 413 still happens — see Step 7.

- [ ] **Step 6: Map the outbound `Body` — Full as today, Stream as an H2 frame body.** Replace `to_hyper_response` (lines 212–230) so it writes a `Body::Full` exactly as before but as the unified out-body type, and a `Body::Stream` as a real hyper streaming body carrying data frames + trailers. Introduce a small `HyperOutBody` that is either a `Full<Bytes>` or a channel/stream body.

Add this out-body type + its `http_body::Body` impl near the bottom of the file:

```rust
/// The outbound body the hyper edge writes: a buffered [`Full`] (the HTTP default) or a
/// STREAM of leaf frames (data + trailers) rendered as an `http_body::Body`. This is the
/// out-half of the §2 streaming `Body`, confined to the backend edge.
enum HyperOutBody {
    Full(Full<Bytes>),
    Stream(std::pin::Pin<Box<dyn futures::Stream<Item = Result<LeafFrame, LeafError>> + Send>>),
}

impl HttpBody for HyperOutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        // Project the pin to the active variant.
        match self.get_mut() {
            HyperOutBody::Full(full) => std::pin::Pin::new(full)
                .poll_frame(cx)
                // `Full<Bytes>` is itself Infallible.
                .map(|opt| opt.map(|res| res.map_err(|never| match never {}))),
            HyperOutBody::Stream(stream) => match stream.as_mut().poll_next(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                std::task::Poll::Ready(Some(Ok(LeafFrame::Data(data)))) => {
                    std::task::Poll::Ready(Some(Ok(http_body::Frame::data(data))))
                }
                std::task::Poll::Ready(Some(Ok(LeafFrame::Trailers(map)))) => {
                    std::task::Poll::Ready(Some(Ok(http_body::Frame::trailers(map))))
                }
                // A leaf stream error has no HTTP-status channel mid-body; end the body
                // (the peer observes a truncated/incomplete response). gRPC renders status
                // as trailers, so it never reaches here as an Err.
                std::task::Poll::Ready(Some(Err(_e))) => std::task::Poll::Ready(None),
            },
        }
    }
}
```

Replace `to_hyper_response`:

```rust
/// Convert a leaf [`Response`] into a hyper `Response<HyperOutBody>`, copying status +
/// headers and writing the body as [`Full`] (the HTTP default) or as a streaming frame
/// body (data + trailers) — the out-half of the streaming `Body`.
fn to_hyper_response(leaf_resp: Response) -> hyper::Response<HyperOutBody> {
    let status = leaf_resp.status();
    let headers = leaf_resp.headers().clone();
    let out = match leaf_resp.into_body() {
        LeafBody::Full(bytes) => HyperOutBody::Full(Full::new(bytes)),
        LeafBody::Stream(stream) => HyperOutBody::Stream(stream),
    };
    let mut builder = hyper::Response::builder().status(status);
    if let Some(h) = builder.headers_mut() {
        h.clone_from(&headers);
    }
    builder.body(out).unwrap_or_else(|_| {
        hyper::Response::builder()
            .status(http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(HyperOutBody::Full(Full::new(Bytes::new())))
            .expect("a bare 500 always builds")
    })
}
```

Note `leaf_resp.into_body()` consumes the response, so read `status`/`headers` first (done above). `LeafBody::Stream(stream)` already IS the `Pin<Box<dyn Stream<...>>>` (`leaf_core::BoxStream`), assignable to `HyperOutBody::Stream` directly.

- [ ] **Step 7: Restore the `413` at the edge for an oversize HTTP body.** With `to_leaf_request` no longer pre-collecting, the `oversize_request_body_is_413_within_limit_succeeds` regression test expects a `413` (not a `ConvertError`-mapped 4xx). The clean, edge-faithful fix: keep the per-frame budget in `incoming_to_leaf_frames`, but have the DISPATCHER collect bound at the configured limit and map its over-cap `ConvertError` to `413` via the leaf default-advice floor — OR keep the 413 specifically at the edge. To preserve the EXACT existing 413 behavior with minimal blast radius, pass the cap into the dispatcher collect and emit 413 there is wrong layer; instead enforce it at the edge stream and map a `ConvertError` carrying the "over the limit" cause to 413.

Simplest faithful approach: keep the existing `413` semantics by having the dispatcher's HTTP collect use the server's configured limit and translate the over-cap error to `413`. Since the dispatcher does not know the limit, thread it: change the Task-1.6 collect to use a limit the Dispatcher stores. Add a `body_limit: usize` to `Dispatcher` (defaulted via a new-arg) — but that widens scope. Instead, KEEP it at the hyper edge: collect-and-cap the HTTP body at the edge ONLY when the content-type is not a streaming protocol. That re-introduces edge knowledge of protocols, which we do not want.

Resolution (chosen): the per-frame budget already errors with `ConvertError` + the cause string `"request body over the limit"`. Add ONE edge advice-free mapping: in `default_error_response` (leaf-web server.rs) a `ConvertError` already maps to `400`. The existing test asserts `413`. To keep `413` exactly, make `incoming_to_leaf_frames`'s overflow a DISTINCT signal the edge turns into 413 BEFORE dispatch by collecting eagerly for HTTP. Given the regression budget, implement it as: the hyper edge keeps a small `peek`-collect using the per-frame budget and emits `413` when the FIRST collect on a non-grpc request overflows.

Use this concrete, minimal implementation: in `serve_one`, branch on the request content-type the SAME way the dispatcher would (read the header), and for a NON-`application/grpc` request, eagerly collect via `LeafBody::collect(max_body)` at the edge, mapping the over-cap `ConvertError` to `413`; for a grpc request, hand the stream through untouched:

```rust
async fn serve_one(
    dispatcher: Arc<Dispatcher>,
    hyper_req: hyper::Request<Incoming>,
    max_body: usize,
) -> Result<hyper::Response<HyperOutBody>, Infallible> {
    let mut leaf_req = to_leaf_request(hyper_req, max_body);

    // The 413 contract is an EDGE guarantee for buffered HTTP bodies: if this is not a
    // streaming-protocol request, collect the body here bounded by `max_body` and emit 413
    // BEFORE dispatch when it overflows (mirrors the pre-streaming behaviour exactly). A
    // streaming-protocol request (application/grpc*) keeps its stream — its own framing
    // bounds it. Selection is by the runtime content-type value, never a Rust type name.
    let is_streaming_protocol = leaf_req
        .header(http::header::CONTENT_TYPE.as_str())
        .is_some_and(|ct| ct.starts_with("application/grpc"));
    if !is_streaming_protocol && leaf_req.body_is_stream() {
        let body = leaf_req.take_body();
        match body.collect(max_body).await {
            Ok(collected) => leaf_req.set_body(leaf_web::body::Body::Full(collected)),
            Err(_over_cap) => {
                return Ok(to_hyper_response(Response::new(http::StatusCode::PAYLOAD_TOO_LARGE)));
            }
        }
    }
    Ok(to_hyper_response(dispatcher.dispatch(leaf_req).await))
}
```

This keeps the `413` edge contract intact, keeps the dispatcher's collect (Task 1.6) as a safety net (`usize::MAX`, a no-op when already Full), and leaves the grpc path streaming. The `"application/grpc"` literal here is a TRANSPORT-EDGE content-type value (a runtime header string), not a Rust type name — allowed.

- [ ] **Step 8: Build + run the http2 + regression edge tests — expect PASS.**

```
cargo test -p leaf-web-hyper 2>&1 | tail -25
```
Expected: `http2_prior_knowledge_request_round_trips_through_the_streaming_edge ... ok`, `oversize_request_body_is_413_within_limit_succeeds ... ok`, `hyper_server_serves_real_http_through_the_dispatcher ... ok`, the two drain tests `... ok`, and `auto_config_server` green.

- [ ] **Step 9: Commit.**

```
git add crates/leaf-web-hyper/ Cargo.toml
git commit -m "leaf-web-hyper: http2 + map hyper Incoming<->streaming Body at the edge

Enable http2 on hyper/hyper-util (the auto builder already negotiates H1+H2). The edge
maps Incoming's frames (data + trailers) to a leaf Body::Stream and renders a leaf
Body::Stream back as an h2 frame body with trailers; a buffered HTTP body is still
collected + capped at the edge (413 unchanged). gRPC bodies pass through as streams.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.8: Full force-clean regression gate

**Files:** none (verification only)

- [ ] **Step 1: Force-clean build of the whole workspace test suite — expect ALL green.** Per the project's verification rule (cached runs re-emit no warnings; force-clean before claiming clean):

```
cargo test --workspace 2>&1 | tail -30
```
Expected: every crate's tests pass, including the ~1647-test HTTP suite (the `Body` migration is source-compatible: `Request::new(.., Bytes)` / `Response::with_body(impl Into<Bytes>)` / `body_bytes()` unchanged; the dispatcher collects-before-handler).

- [ ] **Step 2: Clippy clean (force-clean) — expect no warnings.**

```
cargo clean -p leaf-core -p leaf-web -p leaf-web-hyper
cargo clippy -p leaf-core -p leaf-web -p leaf-web-hyper --all-targets -- -D warnings 2>&1 | tail -15
```
Expected: `Finished` with no warnings (emit `#[allow]` on any generated/macro item rust-analyzer-only lints flag, per the project rule).

- [ ] **Step 3: Doc clean — expect no broken intra-doc links.**

```
cargo doc -p leaf-core -p leaf-web -p leaf-web-hyper --no-deps 2>&1 | tail -15
```
Expected: `Finished` with no `unresolved link` / `missing docs` warnings (the new `Body`/`Frame`/`ProtocolDispatch`/`BoxStream` items all carry doc comments).

- [ ] **Step 4: Commit (only if Steps 1–3 surfaced any doc/clippy touch-ups).**

```
git add -A
git commit -m "leaf-web/leaf-web-hyper: force-clean gate for the streaming Body stage

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 2: leaf-grpc abstractions

The backend-free gRPC abstraction crate. It defines the grpc-status code space (`Code`/`Status`), the typed message stream (`Streaming<T>`), the length-prefix wire framing (`encode_frame`/`decode_frames`), the prost codec seam (`GrpcCodec`/`ProstCodec`), the gRPC `Handler` family (`GrpcHandler`/`GrpcRoute`), the `leaf_web::ProtocolDispatch` impl (`GrpcDispatch`, a `#[component]` providing `dyn ProtocolDispatch`, field-injecting `Vec<Ref<dyn GrpcRoute>>`, O(1) path map, unknown→`Unimplemented`), and the `GrpcStatusMapper` SPI with a `#[auto_config]` FALLBACK `DefaultGrpcStatusMapper`.

HARD CONSTRAINTS for every task here: `leaf-grpc` names NO hyper/h2/tower (only `prost`/`bytes`/`http`/`futures` + `leaf-core`/`leaf-web`); the dep arrow is `leaf-grpc → leaf-web → leaf-core` (never the reverse — `leaf-web` defines `ProtocolDispatch`, this crate implements it); no type-name detection in any code; dogfood the DI (the dispatcher is a `#[component]` providing the view, the default mapper an `#[auto_config]` FALLBACK — no hand-rolled `Provider`); reuse Stage 1's `Body`/`Frame`/`ProtocolDispatch`/`Request`/`Response`. This stage depends on Stage 1 having landed the streaming `Body`/`Frame`, `BoxStream` in `leaf-core`, and `leaf_web::ProtocolDispatch`.

**Files**

- Create `crates/leaf-grpc/Cargo.toml`
- Create `crates/leaf-grpc/src/lib.rs`
- Create `crates/leaf-grpc/src/status.rs` (`Code` + `Status`)
- Create `crates/leaf-grpc/src/streaming.rs` (`Streaming<T>`)
- Create `crates/leaf-grpc/src/framing.rs` (`encode_frame` + `decode_frames`)
- Create `crates/leaf-grpc/src/codec.rs` (`GrpcCodec` + `ProstCodec`)
- Create `crates/leaf-grpc/src/handler.rs` (`GrpcHandler` + `GrpcRoute` + `impl_resolve_view!`)
- Create `crates/leaf-grpc/src/dispatch.rs` (`GrpcDispatch` — the `ProtocolDispatch` impl + `#[component]`)
- Create `crates/leaf-grpc/src/mapper.rs` (`GrpcStatusMapper` + `DefaultGrpcStatusMapper` `#[auto_config]` FALLBACK)
- Modify `Cargo.toml` (workspace: add the `leaf-grpc` BOM row + the `prost` external-dep row)

---

### Task 2.1: Scaffold the `leaf-grpc` crate (compiles empty, in the workspace)

**Files:** `crates/leaf-grpc/Cargo.toml`, `crates/leaf-grpc/src/lib.rs`, `Cargo.toml`

- [ ] **Step 1: Add the BOM + prost rows to the workspace `Cargo.toml`.** Insert the leaf-grpc BOM row after the `leaf-web-hyper` row, and the `prost` external row after the `bytes` row. Edit `Cargo.toml`:

```toml
leaf-web-hyper = { path = "crates/leaf-web-hyper", version = "0.1.0" }
leaf-grpc = { path = "crates/leaf-grpc", version = "0.1.0" }
```

and under the neutral-vocabulary block (after `bytes = "1"`):

```toml
# prost — the protobuf MESSAGE codec (the `serde_json` analogue for gRPC),
# confined to leaf-grpc's `ProstCodec` exactly as serde_json is confined to
# leaf-serde's JsonConverter. A pure codec, NOT a transport backend: it names no
# hyper/tower/h2, so it is the sanctioned gRPC-message dep (the design's §9).
prost = "0.13"
```

- [ ] **Step 2: Write the crate manifest.** Create `crates/leaf-grpc/Cargo.toml`:

```toml
[package]
name = "leaf-grpc"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
# leaf-grpc is the gRPC ABSTRACTION crate: it rests on leaf-web (the shared
# Request/Response/Body + the ProtocolDispatch seam it implements) and leaf-core
# (BoxFuture/BoxStream/Ref/LeafError). The dep arrow is leaf-grpc -> leaf-web ->
# leaf-core; leaf-web never names leaf-grpc.
leaf-core.workspace = true
leaf-web.workspace = true
# The stereotype macros leaf-grpc dogfoods: GrpcDispatch is a #[component] that
# PROVIDES dyn ProtocolDispatch, and DefaultGrpcStatusMapper is an #[auto_config]
# FALLBACK — no hand-rolled Provider, exactly like leaf-cache/leaf-web-hyper.
leaf-macros.workspace = true
# prost — the protobuf MESSAGE codec, confined to ProstCodec (the serde_json analogue).
prost.workspace = true
# The body carrier at the frame edge + the neutral HTTP value vocabulary (HeaderMap
# for grpc-status trailers). NO hyper/h2 — only the server-agnostic `http` types.
bytes.workspace = true
http.workspace = true
# BoxStream lives in leaf-core (Stage 1), but leaf-grpc names `futures::Stream` /
# stream combinators directly for Streaming<T> + the de-framing combinators.
futures.workspace = true

[dev-dependencies]
# A minimal block_on to drive the async framing/dispatch units with no runtime.
futures.workspace = true
# The DI-assembly proof: leaf-boot lifts the macro-emitted #[component]/#[auto_config]
# seeds so a test can resolve Vec<Ref<dyn GrpcRoute>> / dyn ProtocolDispatch /
# dyn GrpcStatusMapper by collection + by-trait injection (the same path the
# EmbeddedWebServer uses). Dev-only: production deps stay leaf-core/leaf-web/prost.
leaf-boot.workspace = true
```

- [ ] **Step 3: Write a placeholder `lib.rs` with the module skeleton.** Create `crates/leaf-grpc/src/lib.rs`:

```rust
//! `leaf-grpc` — the DI-native gRPC transport ABSTRACTIONS (the gRPC peer of
//! `leaf-web`'s HTTP layer). It defines the grpc-status code space, the typed
//! message stream, the length-prefix wire framing, the prost message-codec seam,
//! and the gRPC `Handler` family — all riding the SHARED `leaf_web` server via the
//! `ProtocolDispatch` seam, so the dep arrow is `leaf-grpc -> leaf-web -> leaf-core`
//! and `leaf-web` never names this crate.
//!
//! It names NO hyper/h2/tower: `prost` is the sole message codec (confined to
//! [`ProstCodec`], the `serde_json` analogue), and the frame stream is a
//! `leaf_core::BoxStream` (the `futures` neutral vocabulary), never a backend body.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod codec;
pub mod dispatch;
pub mod framing;
pub mod handler;
pub mod mapper;
pub mod status;
pub mod streaming;

// The per-crate anti-DCE SOURCE anchor (ADR-09): one SourceTag in the link-collected
// SOURCES slice so a binary listing leaf-grpc in its ExpectedManifest can tell
// "linked-but-zero-rows" from "never-linked". The package name is the join string.
leaf_core::declare_source!("leaf-grpc");

pub use codec::{GrpcCodec, ProstCodec};
pub use dispatch::GrpcDispatch;
pub use framing::{decode_frames, encode_frame};
pub use handler::{GrpcHandler, GrpcRoute};
pub use mapper::{DefaultGrpcStatusMapper, GrpcStatusMapper};
pub use status::{Code, Status};
pub use streaming::Streaming;
```

- [ ] **Step 4: Create the empty module files so the crate links.** Create each of `status.rs`, `streaming.rs`, `framing.rs`, `codec.rs`, `handler.rs`, `dispatch.rs`, `mapper.rs` with a one-line doc comment each (e.g. `crates/leaf-grpc/src/status.rs`):

```rust
//! gRPC status: the grpc-status code space ([`Code`]) + a carried [`Status`].
```

(Repeat with the matching doc line for the other six files; they fill in over the following tasks.)

- [ ] **Step 5: Confirm it builds.** Run:

```
cargo build -p leaf-grpc
```

Expected: `Finished` with no errors (the `pub use` paths reference items not yet defined will FAIL — so for THIS step only, comment out the `pub use` lines in `lib.rs` and re-run; uncomment them as each task lands its type). Actually, to keep this step green without churn, write the `pub use` lines but leave each module file empty except its doc line, and confirm:

```
cargo build -p leaf-grpc 2>&1 | head -20
```

Expected: errors `cannot find type Code in module status` etc. — proving the skeleton is wired and the next tasks fill it. (This is the only step whose "expected output" is the unresolved-imports list; every later task ends green.)

- [ ] **Step 6: Commit.**

```
git add crates/leaf-grpc Cargo.toml && git commit -m "leaf-grpc: scaffold the backend-free gRPC abstraction crate

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.2: `Code` — the grpc-status code space (0–16)

**Files:** `crates/leaf-grpc/src/status.rs`

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/status.rs`:

```rust
#[cfg(test)]
mod code_tests {
    use super::*;

    #[test]
    fn code_discriminants_match_the_grpc_status_wire_numbers() {
        // The grpc-status header carries these exact integers (the canonical
        // gRPC code space): a tonic/grpc-go peer reads `grpc-status: 5` as NotFound.
        assert_eq!(Code::Ok as i32, 0);
        assert_eq!(Code::Cancelled as i32, 1);
        assert_eq!(Code::Unknown as i32, 2);
        assert_eq!(Code::InvalidArgument as i32, 3);
        assert_eq!(Code::DeadlineExceeded as i32, 4);
        assert_eq!(Code::NotFound as i32, 5);
        assert_eq!(Code::AlreadyExists as i32, 6);
        assert_eq!(Code::PermissionDenied as i32, 7);
        assert_eq!(Code::ResourceExhausted as i32, 8);
        assert_eq!(Code::FailedPrecondition as i32, 9);
        assert_eq!(Code::Aborted as i32, 10);
        assert_eq!(Code::OutOfRange as i32, 11);
        assert_eq!(Code::Unimplemented as i32, 12);
        assert_eq!(Code::Internal as i32, 13);
        assert_eq!(Code::Unavailable as i32, 14);
        assert_eq!(Code::DataLoss as i32, 15);
        assert_eq!(Code::Unauthenticated as i32, 16);
    }
}
```

- [ ] **Step 2: Run it — fails (no `Code`).**

```
cargo test -p leaf-grpc code_tests:: 2>&1 | head -5
```

Expected: `error[E0433]: ... cannot find type Code`.

- [ ] **Step 3: Implement `Code`.** Add to `crates/leaf-grpc/src/status.rs` above the test module:

```rust
/// The gRPC status code space (the `grpc-status` trailer integers 0–16). The
/// discriminants ARE the wire numbers — `Code::NotFound as i32 == 5` — so the edge
/// renders `grpc-status: <code as i32>` with no lookup table, and a tonic/grpc-go
/// peer reads them canonically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Code {
    /// Not an error; returned on success (`grpc-status: 0`).
    Ok = 0,
    /// The operation was cancelled (typically by the caller).
    Cancelled = 1,
    /// Unknown error (e.g. a `Status` from another address space with an unknown code).
    Unknown = 2,
    /// The client specified an invalid argument (irrespective of system state).
    InvalidArgument = 3,
    /// The deadline expired before the operation could complete.
    DeadlineExceeded = 4,
    /// A requested entity was not found.
    NotFound = 5,
    /// An entity a client attempted to create already exists.
    AlreadyExists = 6,
    /// The caller lacks permission to execute the operation.
    PermissionDenied = 7,
    /// A resource has been exhausted (quota, disk, …).
    ResourceExhausted = 8,
    /// The operation was rejected because the system is not in the required state.
    FailedPrecondition = 9,
    /// The operation was aborted (e.g. a concurrency conflict).
    Aborted = 10,
    /// The operation was attempted past the valid range.
    OutOfRange = 11,
    /// The operation is not implemented / not supported.
    Unimplemented = 12,
    /// An internal error (an invariant expected by the system was broken).
    Internal = 13,
    /// The service is currently unavailable (a transient condition).
    Unavailable = 14,
    /// Unrecoverable data loss or corruption.
    DataLoss = 15,
    /// The request does not have valid authentication credentials.
    Unauthenticated = 16,
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc code_tests:: -- --nocapture
```

Expected: `test code_tests::code_discriminants_match_the_grpc_status_wire_numbers ... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/status.rs && git commit -m "leaf-grpc: Code — the grpc-status code space (0..=16)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.3: `Status` — `(Code, message)` + the named helpers

**Files:** `crates/leaf-grpc/src/status.rs`

- [ ] **Step 1: Write the failing test.** Add a second test module to `crates/leaf-grpc/src/status.rs`:

```rust
#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn status_new_carries_code_and_message() {
        let s = Status::new(Code::NotFound, "no such product");
        assert_eq!(s.code, Code::NotFound);
        assert_eq!(s.message, "no such product");
    }

    #[test]
    fn named_helpers_select_the_right_code() {
        // The ergonomic ctors used by handlers and the default mapper.
        assert_eq!(Status::not_found("x").code, Code::NotFound);
        assert_eq!(Status::invalid_argument("x").code, Code::InvalidArgument);
        assert_eq!(Status::internal("x").code, Code::Internal);
        assert_eq!(Status::unimplemented("x").code, Code::Unimplemented);
        // The message is carried through verbatim.
        assert_eq!(Status::internal("boom").message, "boom");
    }
}
```

- [ ] **Step 2: Run it — fails (no `Status`).**

```
cargo test -p leaf-grpc status_tests:: 2>&1 | head -5
```

Expected: `cannot find type Status`.

- [ ] **Step 3: Implement `Status`.** Add to `crates/leaf-grpc/src/status.rs` (above the test modules):

```rust
/// A gRPC status carried out of a handler: a [`Code`] + a human message, rendered
/// at the edge as the `grpc-status` / `grpc-message` trailers. The error currency
/// of the gRPC layer (handlers return `Result<_, Status>`; the [`GrpcStatusMapper`]
/// SPI maps a [`LeafError`](leaf_core::LeafError) into one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The grpc-status code.
    pub code: Code,
    /// The grpc-message (a human-readable diagnostic; may be empty).
    pub message: String,
}

impl Status {
    /// A status with an explicit [`Code`] and message.
    #[must_use]
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Status { code, message: message.into() }
    }

    /// A [`Code::NotFound`] status — a requested entity was not found.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Status::new(Code::NotFound, message)
    }

    /// A [`Code::InvalidArgument`] status — the caller passed an invalid argument.
    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Status::new(Code::InvalidArgument, message)
    }

    /// A [`Code::Internal`] status — an internal invariant was broken.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Status::new(Code::Internal, message)
    }

    /// A [`Code::Unimplemented`] status — the RPC is not implemented/supported.
    #[must_use]
    pub fn unimplemented(message: impl Into<String>) -> Self {
        Status::new(Code::Unimplemented, message)
    }
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc status_tests:: -- --nocapture
```

Expected: both `status_tests` cases `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/status.rs && git commit -m "leaf-grpc: Status — (Code, message) + named helper ctors

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.4: `Streaming<T>` — the typed message stream

**Files:** `crates/leaf-grpc/src/streaming.rs`

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/streaming.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Code, Status};
    use futures::executor::block_on;
    use futures::StreamExt;

    #[test]
    fn streaming_once_yields_exactly_one_ok_item() {
        let s = Streaming::once(7u32);
        let items: Vec<Result<u32, Status>> = block_on(s.collect());
        assert_eq!(items, vec![Ok(7)]);
    }

    #[test]
    fn streaming_new_threads_ok_and_err_items_in_order() {
        let inner: leaf_core::BoxStream<'static, Result<u32, Status>> = Box::pin(
            futures::stream::iter(vec![
                Ok(1u32),
                Err(Status::new(Code::Internal, "boom")),
                Ok(3u32),
            ]),
        );
        let s = Streaming::new(inner);
        let items: Vec<Result<u32, Status>> = block_on(s.collect());
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Ok(1));
        assert_eq!(items[1], Err(Status::new(Code::Internal, "boom")));
        assert_eq!(items[2], Ok(3));
    }
}
```

- [ ] **Step 2: Run it — fails (no `Streaming`).**

```
cargo test -p leaf-grpc streaming:: 2>&1 | head -5
```

Expected: `cannot find type Streaming`.

- [ ] **Step 3: Implement `Streaming<T>`.** Add to `crates/leaf-grpc/src/streaming.rs` above the test module:

```rust
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use leaf_core::BoxStream;

use crate::status::Status;

/// A typed gRPC message stream: a `Stream` of `Result<T, Status>`. The generated
/// server-trait methods take/return this for the streaming call shapes
/// (server-stream returns `Streaming<U>`; client-stream/bidi take `Streaming<T>`),
/// and the [`GrpcHandler`](crate::GrpcHandler) wraps it with the wire framing/codec.
///
/// It wraps a `leaf_core::BoxStream` (the `futures` neutral vocabulary, NOT a
/// backend body) so it is backend-free and `'static` (rides the executor).
pub struct Streaming<T> {
    inner: BoxStream<'static, Result<T, Status>>,
}

impl<T> Streaming<T> {
    /// Wrap an existing boxed stream of `Result<T, Status>`.
    #[must_use]
    pub fn new(inner: BoxStream<'static, Result<T, Status>>) -> Self {
        Streaming { inner }
    }

    /// A single-item stream that yields `Ok(item)` once, then ends — the trivial
    /// server-stream a unary-shaped body lifts into when only one message is sent.
    #[must_use]
    pub fn once(item: T) -> Self
    where
        T: Send + 'static,
    {
        Streaming { inner: Box::pin(futures::stream::once(async move { Ok(item) })) }
    }
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Delegate to the wrapped boxed stream (already `Unpin` via `Pin<Box<..>>`).
        self.inner.as_mut().poll_next(cx)
    }
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc streaming:: -- --nocapture
```

Expected: both `streaming::tests` cases `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/streaming.rs && git commit -m "leaf-grpc: Streaming<T> — the typed Result<T, Status> message stream

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.5: `encode_frame` — the length-prefix encoder

**Files:** `crates/leaf-grpc/src/framing.rs`

The gRPC HTTP/2 wire frame is: 1 compression byte (`0` = uncompressed; leaf does not compress, §8) + a 4-byte big-endian message length + the message bytes. This is the trickiest leaf-grpc primitive, so it gets full code.

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/framing.rs`:

```rust
#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_frame_prefixes_compression_byte_and_be_length() {
        // The canonical gRPC length-prefix: [0][00 00 00 03]["abc"].
        let framed = encode_frame(b"abc");
        assert_eq!(framed.len(), 1 + 4 + 3);
        assert_eq!(framed[0], 0, "compression flag: 0 = uncompressed");
        assert_eq!(&framed[1..5], &[0, 0, 0, 3], "4-byte big-endian length");
        assert_eq!(&framed[5..], b"abc");
    }

    #[test]
    fn encode_frame_of_empty_message_is_a_five_byte_header_only() {
        let framed = encode_frame(b"");
        assert_eq!(framed.len(), 5);
        assert_eq!(framed[0], 0);
        assert_eq!(&framed[1..5], &[0, 0, 0, 0]);
    }
}
```

- [ ] **Step 2: Run it — fails (no `encode_frame`).**

```
cargo test -p leaf-grpc encode_tests:: 2>&1 | head -5
```

Expected: `cannot find function encode_frame`.

- [ ] **Step 3: Implement `encode_frame`.** Add to `crates/leaf-grpc/src/framing.rs`:

```rust
use bytes::{BufMut, Bytes, BytesMut};

/// Encode one gRPC length-prefixed wire frame: a 1-byte compression flag (always
/// `0`, leaf does not compress — §8), then a 4-byte BIG-ENDIAN message length, then
/// the message bytes. The exact format a tonic/grpc-go peer reads off the HTTP/2
/// DATA stream.
#[must_use]
pub fn encode_frame(msg: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + msg.len());
    buf.put_u8(0); // compression flag: 0 = uncompressed
    // The length is a u32; a message longer than u32::MAX is not representable on
    // the gRPC wire, so the cast is the protocol's own bound (saturate defensively).
    buf.put_u32(u32::try_from(msg.len()).unwrap_or(u32::MAX)); // big-endian (put_u32 is BE)
    buf.put_slice(msg);
    buf.freeze()
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc encode_tests:: -- --nocapture
```

Expected: both `encode_tests` cases `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/framing.rs && git commit -m "leaf-grpc: encode_frame — the gRPC length-prefix wire encoder

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.6: `decode_frames` — de-frame a `Body` stream into messages

**Files:** `crates/leaf-grpc/src/framing.rs`

`decode_frames` turns Stage 1's `Body` (a `Body::Stream` of H2 `Frame`s) into a `BoxStream` of complete message `Bytes`, reassembling across frame boundaries. It is stateful (a running buffer that may split a header/message across multiple data frames), so it gets full code. This is the inbound counterpart of `encode_frame`.

- [ ] **Step 1: Write the failing test (clean split across data frames).** Append to `crates/leaf-grpc/src/framing.rs`:

```rust
#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::status::Code;
    use bytes::Bytes;
    use futures::executor::block_on;
    use futures::StreamExt;
    use leaf_web::response::{Body, Frame};

    /// Build a `Body::Stream` from a sequence of raw data-frame byte chunks (the
    /// shape the hyper edge produces: each H2 DATA frame is one `Frame::Data`).
    fn body_of(chunks: Vec<Vec<u8>>) -> Body {
        let frames = chunks
            .into_iter()
            .map(|c| Ok(Frame::Data(Bytes::from(c))));
        Body::Stream(Box::pin(futures::stream::iter(frames.collect::<Vec<_>>())))
    }

    #[test]
    fn decode_frames_reassembles_two_messages_split_across_chunks() {
        // Two framed messages ("hi", "bye"), arbitrarily re-chunked so a header AND
        // a message body straddle a data-frame boundary — the de-framer must buffer.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(b"hi"));
        wire.extend_from_slice(&encode_frame(b"bye"));
        // Split the contiguous wire bytes at an awkward offset (mid second header).
        let split = 6;
        let body = body_of(vec![wire[..split].to_vec(), wire[split..].to_vec()]);

        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        let ok: Vec<Bytes> = msgs.into_iter().map(|r| r.expect("a complete frame")).collect();
        assert_eq!(ok, vec![Bytes::from_static(b"hi"), Bytes::from_static(b"bye")]);
    }

    #[test]
    fn decode_frames_of_a_truncated_header_is_an_internal_status() {
        // A stream that ends mid-header (only 3 of the 5 prefix bytes) is a malformed
        // frame → a `Code::Internal` Status (loud, never a silent truncation).
        let body = body_of(vec![vec![0u8, 0, 0]]);
        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        assert_eq!(msgs.len(), 1);
        let err = msgs.into_iter().next().unwrap().expect_err("truncated → Status");
        assert_eq!(err.code, Code::Internal);
    }

    #[test]
    fn decode_frames_of_a_full_body_treats_the_buffer_as_one_chunk() {
        // A `Body::Full` (the collect path's shape) de-frames identically — the whole
        // buffer is the single source chunk.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(b"solo"));
        let body = Body::full(Bytes::from(wire));
        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        let ok: Vec<Bytes> = msgs.into_iter().map(|r| r.expect("a complete frame")).collect();
        assert_eq!(ok, vec![Bytes::from_static(b"solo")]);
    }
}
```

- [ ] **Step 2: Run it — fails (no `decode_frames`).**

```
cargo test -p leaf-grpc decode_tests:: 2>&1 | head -5
```

Expected: `cannot find function decode_frames`.

- [ ] **Step 3: Implement `decode_frames`.** Add to `crates/leaf-grpc/src/framing.rs` (the `encode_frame` imports already pull in `Bytes`/`BytesMut`; add the rest):

```rust
use futures::StreamExt;
use leaf_core::BoxStream;
use leaf_web::response::{Body, Frame};

use crate::status::{Code, Status};

/// De-frame a [`Body`] (a `Body::Stream` of H2 frames, or a `Body::Full` buffer)
/// into a stream of COMPLETE gRPC messages. It buffers across frame boundaries: a
/// length-prefix header or a message body may straddle several H2 DATA frames, so
/// the de-framer holds a running buffer and emits a message only once its full
/// 5-byte header + body have arrived. Trailers ([`Frame::Trailers`]) carry no
/// message bytes and are skipped. A stream that ends mid-frame is a malformed wire
/// → a single `Code::Internal` [`Status`] (loud, never a silent truncation).
///
/// Backend-free: the input is a leaf [`Body`] and the output a `leaf_core::BoxStream`
/// — no hyper/h2 names appear.
#[must_use]
pub fn decode_frames(body: Body) -> BoxStream<'static, Result<Bytes, Status>> {
    // Normalise both Body shapes into ONE stream of raw byte chunks: a Full body is a
    // single-chunk stream; a Stream body maps each Data frame to its bytes and drops
    // Trailers. (A frame-level transport error becomes an Internal Status chunk-error
    // that the fold below surfaces.)
    let chunks: BoxStream<'static, Result<Bytes, Status>> = match body {
        Body::Full(b) => Box::pin(futures::stream::once(async move { Ok(b) })),
        Body::Stream(s) => Box::pin(s.filter_map(|frame| async move {
            match frame {
                Ok(Frame::Data(b)) => Some(Ok(b)),
                Ok(Frame::Trailers(_)) => None,
                Err(e) => Some(Err(Status::new(
                    Code::Internal,
                    format!("gRPC frame transport error: {e}"),
                ))),
            }
        })),
    };

    // A stateful de-framer: `unfold` threads (chunk-stream, running-buffer, done) and
    // emits each complete message as it becomes available. We pull more chunks only
    // when the buffer cannot yet satisfy a full header+body.
    struct State {
        chunks: BoxStream<'static, Result<Bytes, Status>>,
        buf: BytesMut,
        errored: bool,
    }

    let init = State { chunks, buf: BytesMut::new(), errored: false };

    Box::pin(futures::stream::unfold(init, |mut st| async move {
        if st.errored {
            return None;
        }
        loop {
            // Enough for a header? (1 flag + 4 length.)
            if st.buf.len() >= 5 {
                let len = u32::from_be_bytes([st.buf[1], st.buf[2], st.buf[3], st.buf[4]]) as usize;
                let total = 5 + len;
                if st.buf.len() >= total {
                    // A complete frame: split off [flag..body], drop the 5-byte header.
                    let mut frame = st.buf.split_to(total);
                    let _header = frame.split_to(5);
                    return Some((Ok(frame.freeze()), st));
                }
            }
            // Need more bytes: pull the next chunk.
            match st.chunks.next().await {
                Some(Ok(chunk)) => {
                    st.buf.extend_from_slice(&chunk);
                    // loop: re-check whether the buffer now satisfies a full frame.
                }
                Some(Err(status)) => {
                    st.errored = true;
                    return Some((Err(status), st));
                }
                None => {
                    // Stream ended. A clean end (empty buffer) → done. A non-empty
                    // residue is a truncated frame → one loud Internal Status.
                    if st.buf.is_empty() {
                        return None;
                    }
                    st.errored = true;
                    return Some((
                        Err(Status::new(
                            Code::Internal,
                            "gRPC stream ended mid-frame (truncated length-prefix or body)",
                        )),
                        st,
                    ));
                }
            }
        }
    }))
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc decode_tests:: -- --nocapture
```

Expected: all three `decode_tests` cases `... ok`.

- [ ] **Step 5: Round-trip sanity (encode then decode).** Add to the `decode_tests` module:

```rust
    #[test]
    fn encode_then_decode_round_trips_each_message() {
        let mut wire = Vec::new();
        for m in [b"alpha".as_slice(), b"", b"gamma"] {
            wire.extend_from_slice(&encode_frame(m));
        }
        let body = Body::full(Bytes::from(wire));
        let ok: Vec<Bytes> = block_on(decode_frames(body).collect())
            .into_iter()
            .map(|r| r.expect("complete frame"))
            .collect();
        assert_eq!(
            ok,
            vec![Bytes::from_static(b"alpha"), Bytes::from_static(b""), Bytes::from_static(b"gamma")],
        );
    }
```

Run:

```
cargo test -p leaf-grpc decode_tests::encode_then_decode -- --nocapture
```

Expected: `... ok`.

- [ ] **Step 6: Commit.**

```
git add crates/leaf-grpc/src/framing.rs && git commit -m "leaf-grpc: decode_frames — de-frame a Body stream into complete messages

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.7: `GrpcCodec` + `ProstCodec` — the prost message-codec seam

**Files:** `crates/leaf-grpc/src/codec.rs`

The codec is the `serde_json`-confinement analogue: `prost` is named ONLY here. `GrpcCodec` keeps the typed `encode<M>`/`decode<M>` generic methods (NOT object-safe — that is fine, the handler holds a concrete `ProstCodec`, mirroring how `HttpMessageConverterExt::read<T>` lives off the dyn-safe trait).

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Code;

    // A tiny hand-written prost message (no codegen yet, Stage 3): one string field
    // (tag 1, wire type 2 = length-delimited). prost::Message is derivable, but for a
    // self-contained codec test we use prost's built-in `String` Message impl — prost
    // implements Message for the well-known scalar wrappers, so a `String` is a
    // complete (single-field-less) message we can round-trip the codec over.
    #[test]
    fn prost_codec_round_trips_a_message() {
        let codec = ProstCodec::new();
        // prost implements Message for () (the empty message): encode is empty bytes,
        // decode of empty bytes succeeds. A non-empty buffer for () is still accepted
        // (unknown fields are skipped), so we assert the empty round-trip exactly.
        let unit: () = ();
        let bytes = codec.encode(&unit);
        assert!(bytes.is_empty(), "the empty message encodes to zero bytes");
        let back: () = codec.decode(&bytes).expect("empty message decodes");
        assert_eq!(back, ());
    }

    #[test]
    fn prost_codec_decode_of_malformed_bytes_is_an_internal_status() {
        let codec = ProstCodec::new();
        // A truncated length-delimited field (tag 1 says "10 bytes follow" but the
        // buffer ends) is malformed for a u32 message → a Code::Internal Status.
        let err = codec
            .decode::<u32>(&[0x08, 0xff, 0xff, 0xff, 0xff, 0xff])
            .expect_err("malformed prost bytes → Status");
        assert_eq!(err.code, Code::Internal);
    }
}
```

- [ ] **Step 2: Run it — fails (no `ProstCodec`).**

```
cargo test -p leaf-grpc codec:: 2>&1 | head -5
```

Expected: `cannot find type ProstCodec`.

- [ ] **Step 3: Implement `GrpcCodec` + `ProstCodec`.** Add to `crates/leaf-grpc/src/codec.rs`:

```rust
use bytes::Bytes;

use crate::status::{Code, Status};

/// The gRPC MESSAGE codec seam (the `HttpMessageConverter` analogue, confined to one
/// data format): encode a typed prost message to bytes, decode bytes back. The
/// methods are generic over `M: prost::Message` (NOT object-safe), so a handler holds
/// a CONCRETE codec — the same shape `HttpMessageConverterExt::read<T>` takes off the
/// dyn-safe converter. `prost` is named only behind this seam.
pub trait GrpcCodec: Send + Sync {
    /// Encode a typed message to its protobuf wire bytes (never fails — prost encoding
    /// into a `Vec` is infallible).
    fn encode<M: prost::Message>(&self, m: &M) -> Bytes;

    /// Decode protobuf wire bytes into `M`.
    ///
    /// # Errors
    /// A [`Code::Internal`] [`Status`] if the bytes are malformed for `M`.
    fn decode<M: prost::Message + Default>(&self, b: &[u8]) -> Result<M, Status>;
}

/// The prost-backed [`GrpcCodec`] — leaf-grpc's `JsonConverter` analogue. The ONLY
/// place `prost` is named (the message codec is confined here exactly as `serde_json`
/// is confined to leaf-serde's converter). Stateless.
#[derive(Clone, Copy, Default)]
pub struct ProstCodec;

impl ProstCodec {
    /// A fresh prost codec (stateless).
    #[must_use]
    pub fn new() -> Self {
        ProstCodec
    }
}

impl GrpcCodec for ProstCodec {
    fn encode<M: prost::Message>(&self, m: &M) -> Bytes {
        // prost encodes into a growable buffer; encoding is infallible.
        let mut buf = Vec::with_capacity(m.encoded_len());
        m.encode(&mut buf).expect("prost encode into a Vec is infallible");
        Bytes::from(buf)
    }

    fn decode<M: prost::Message + Default>(&self, b: &[u8]) -> Result<M, Status> {
        M::decode(b).map_err(|e| {
            Status::new(Code::Internal, format!("protobuf decode failed: {e}"))
        })
    }
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc codec:: -- --nocapture
```

Expected: both `codec::tests` cases `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/codec.rs && git commit -m "leaf-grpc: GrpcCodec + ProstCodec — the prost message-codec seam

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.8: `GrpcHandler` + `GrpcRoute` + the injectable view

**Files:** `crates/leaf-grpc/src/handler.rs`

`GrpcHandler` is the gRPC `Handler` analogue: it consumes the inbound `Request` (whose `Body::Stream` is H2 frames) and returns a `Response` whose `Body::Stream` is data frames + a final `Frame::Trailers` carrying grpc-status. It NEVER returns `Err` — a `Status` is rendered as trailers. `GrpcRoute` is the `Route` analogue (path + handler), made collection-injectable via `impl_resolve_view!`. The Stage-4 `#[grpc_controller]` macro WRITES these; the `#[cfg(test)]` fakes here are the lone Stage-2 hand-written impls.

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/handler.rs`:

```rust
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
    use leaf_web::response::{Body, Frame};

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
                    Ok(Frame::Data(encode_frame(&data))),
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

        // An inbound request whose body is ONE framed message.
        let body = Body::full(encode_frame(b"ping"));
        let req = Request::new(
            Method::POST,
            "/pkg.Svc/Echo".parse().expect("uri"),
            HeaderMap::new(),
            body,
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
                let msgs: Vec<Bytes> = block_on(
                    crate::framing::decode_frames(Body::full(b.clone())).collect::<Vec<_>>(),
                )
                .into_iter()
                .map(|r| r.expect("msg"))
                .collect();
                assert_eq!(msgs, vec![Bytes::from_static(b"ping")]);
            }
            other => panic!("expected a data frame, got {other:?}"),
        }
        // Last frame: the grpc-status trailers (Ok).
        match frames.last().expect("a trailer frame") {
            Frame::Trailers(t) => {
                assert_eq!(t.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));
            }
            other => panic!("expected a trailers frame, got {other:?}"),
        }
        // `Status`/`Code` are reachable here (proving the handler module sees them).
        let _ = Status::new(Code::Ok, "");
    }
}
```

- [ ] **Step 2: Run it — fails (no `GrpcHandler`/`GrpcRoute`).**

```
cargo test -p leaf-grpc handler:: 2>&1 | head -5
```

Expected: `cannot find trait GrpcHandler`.

- [ ] **Step 3: Implement the traits + the injectable view.** Add to `crates/leaf-grpc/src/handler.rs`:

```rust
use leaf_core::BoxFuture;
use leaf_web::{Request, Response};

/// The gRPC dispatch unit (the [`leaf_web::Handler`] analogue): consume the inbound
/// [`Request`] — whose [`Body::Stream`](leaf_web::response::Body) is the H2 frame
/// stream — and produce a [`Response`] whose body is the outbound data frames + a
/// final `Frame::Trailers` carrying `grpc-status`/`grpc-message`.
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
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc handler:: -- --nocapture
```

Expected: `handler::tests::grpc_handler_echoes_a_unary_message_with_ok_trailers ... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/handler.rs && git commit -m "leaf-grpc: GrpcHandler + GrpcRoute + the dyn GrpcRoute injectable view

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.9: `GrpcDispatch` — the `ProtocolDispatch` impl (O(1) routing, unknown→Unimplemented)

**Files:** `crates/leaf-grpc/src/dispatch.rs`

This is the protocol-branch payoff: `GrpcDispatch` implements Stage 1's `leaf_web::ProtocolDispatch` (claims `application/grpc*` content-types) over an O(1) `HashMap<String, Arc<dyn GrpcRoute>>`. An unknown method renders `Code::Unimplemented` trailers (never an Err). The dispatcher branch in `leaf-web`'s `Dispatcher` is Stage 1's responsibility; this task is purely leaf-grpc's impl of the seam. Tested with the in-module logic first; the full DI-injection proof is Task 2.11.

- [ ] **Step 1: Write the failing test.** Append to `crates/leaf-grpc/src/dispatch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{decode_frames, encode_frame};
    use crate::handler::{GrpcHandler, GrpcRoute};
    use crate::status::Code;
    use bytes::Bytes;
    use futures::executor::block_on;
    use futures::StreamExt;
    use http::{HeaderMap, Method};
    use leaf_core::BoxFuture;
    use leaf_web::request::Request;
    use leaf_web::response::{Body, Frame};
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
        Request::new(Method::POST, path.parse().expect("uri"), h, Body::full(encode_frame(b"")))
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
            other => panic!("expected trailers, got {other:?}"),
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
            _ => None,
        }).expect("a trailers frame");
        assert_eq!(
            trailers.get("grpc-status").unwrap(),
            &http::HeaderValue::from_static(&Code::Unimplemented_str()),
            "unknown method → grpc-status 12 (Unimplemented)"
        );
        // Sanity: the buffer de-frames to zero messages (status-only response).
        let _ = decode_frames;
        let _: Vec<Bytes> = Vec::new();
    }
}
```

(Note: replace the `Code::Unimplemented_str()` placeholder — Step 3 defines a small `status_header_value` helper instead; rewrite that assert to `&http::HeaderValue::from_static("12")` before running. The intent is "grpc-status 12".)

- [ ] **Step 2: Fix the test assert to a literal `"12"`.** Edit the `unknown_method_…` assert to:

```rust
        assert_eq!(
            trailers.get("grpc-status").unwrap(),
            &http::HeaderValue::from_static("12"),
            "unknown method → grpc-status 12 (Unimplemented)"
        );
```

and delete the `Code::Unimplemented_str()` line + the trailing `let _ = decode_frames;` / `let _: Vec<Bytes>` filler. Run — fails (no `GrpcDispatch`):

```
cargo test -p leaf-grpc dispatch:: 2>&1 | head -5
```

Expected: `cannot find type GrpcDispatch`.

- [ ] **Step 3: Implement `GrpcDispatch` + the trailers helper + the `ProtocolDispatch` impl.** Add to `crates/leaf-grpc/src/dispatch.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use http::{HeaderMap, HeaderValue};
use leaf_core::{BoxFuture, LeafError, Ref};
use leaf_web::response::{Body, Frame};
use leaf_web::{ProtocolDispatch, Request, Response};

use crate::handler::GrpcRoute;
use crate::status::{Code, Status};

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
    if !status.message.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&status.message) {
            trailers.insert("grpc-message", v);
        }
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
/// renders `Code::Unimplemented` trailers (never an `Err`). It is a `#[component]`
/// that PROVIDES `dyn ProtocolDispatch`, field-injecting `Vec<Ref<dyn GrpcRoute>>`.
#[leaf_macros::component(provides = "dyn ::leaf_web::ProtocolDispatch")]
pub struct GrpcDispatch {
    /// Every `#[grpc_controller]`-contributed route (collection + by-trait injection).
    /// Field-injected; the O(1) map is built lazily off it per dispatch (cheap — a
    /// borrow-and-find — or built once in the ctor; see `routes_map`).
    routes: Vec<Ref<dyn GrpcRoute>>,
}

impl GrpcDispatch {
    /// The constructor the `#[component]` provider calls: it field-injects the route
    /// collection (the macro wires this from `Vec<Ref<dyn GrpcRoute>>`).
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
        self.routes.iter().map(|r| (r.path(), r.as_ref())).collect()
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
```

(`Body`/`Frame` are imported for the test module's use; if rustc flags `Body` unused in non-test builds, scope the import under the test module instead. The `Frame`/`Body` names appear in the production `status_response` via `leaf_web::response::Frame`, so keep `Frame`; drop `Body` from the top-level `use` if unused.)

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc dispatch:: -- --nocapture
```

Expected: all three `dispatch::tests` cases `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/dispatch.rs && git commit -m "leaf-grpc: GrpcDispatch — the ProtocolDispatch impl (O(1) routing, unknown=>Unimplemented)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.10: `GrpcStatusMapper` + `DefaultGrpcStatusMapper` (the `#[auto_config]` FALLBACK)

**Files:** `crates/leaf-grpc/src/mapper.rs`

The `ControlAdvice` analogue for gRPC: a `dyn GrpcStatusMapper` SPI mapping a `LeafError` → `Status` (collection-injected, first-match-wins). The default mapper is an `#[auto_config]` FALLBACK (Spring's `DefaultHandlerExceptionResolver`): `NoSuchBean`→`Unimplemented`, `ConvertError`→`Internal`, else `Unknown` — UNLESS a user mapper claims it first. Dogfooded as a FALLBACK `#[bean]` providing `dyn GrpcStatusMapper`, never a hand-rolled `Provider`.

- [ ] **Step 1: Write the failing test (the trait + the default mapping).** Append to `crates/leaf-grpc/src/mapper.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Code;
    use leaf_core::{ErrorKind, LeafError};

    #[test]
    fn default_mapper_maps_the_framework_kinds() {
        let m = DefaultGrpcStatusMapper::new();
        // NoSuchBean (an unmatched-resource shape) → Unimplemented.
        let s = m.map(&LeafError::new(ErrorKind::NoSuchBean)).expect("claims NoSuchBean");
        assert_eq!(s.code, Code::Unimplemented);
        // ConvertError (a malformed body / decode fault) → Internal.
        let s = m.map(&LeafError::new(ErrorKind::ConvertError)).expect("claims ConvertError");
        assert_eq!(s.code, Code::Internal);
        // Anything else → Unknown (the floor, still a valid Status).
        let s = m.map(&LeafError::new(ErrorKind::ConstructionFailed)).expect("claims else→Unknown");
        assert_eq!(s.code, Code::Unknown);
    }
}
```

- [ ] **Step 2: Run it — fails (no `GrpcStatusMapper`/`DefaultGrpcStatusMapper`).**

```
cargo test -p leaf-grpc mapper:: 2>&1 | head -5
```

Expected: `cannot find type DefaultGrpcStatusMapper`.

- [ ] **Step 3: Implement the trait + the default mapper + the `#[auto_config]` FALLBACK holder.** Add to `crates/leaf-grpc/src/mapper.rs`:

```rust
use leaf_core::LeafError;

use crate::status::{Code, Status};

/// The gRPC domain-error SPI (the [`leaf_web::ControlAdvice`] analogue): map a
/// [`LeafError`] (raised by a handler / filter / codec) into a [`Status`], or decline
/// (`None`) and let a later mapper — or the default FALLBACK — claim it. Collection-
/// injected (`Vec<Ref<dyn GrpcStatusMapper>>`), first-match-wins, the SAME DI way as
/// `ControlAdvice`. Reuses the `ErrorKind::Integration { kind_id }` domain-error
/// channel for app-specific kinds (e.g. unknown-SKU → `NotFound`).
pub trait GrpcStatusMapper: Send + Sync {
    /// Map `err` to a [`Status`], or `None` to decline.
    fn map(&self, err: &LeafError) -> Option<Status>;
}

// The by-trait-injection seam (emitted ONCE — orphan-rule-OK, `dyn GrpcStatusMapper`
// is local). A user mapper bean publishes the view; the gRPC edge collects every
// provider as `Vec<Ref<dyn GrpcStatusMapper>>` for its ordered first-match chain.
leaf_core::impl_resolve_view!(dyn GrpcStatusMapper);

/// The default gRPC status mapping — the FALLBACK floor (Spring's
/// `DefaultHandlerExceptionResolver`): `NoSuchBean`→[`Code::Unimplemented`],
/// `ConvertError`→[`Code::Internal`], everything else→[`Code::Unknown`]. A user
/// `GrpcStatusMapper` bean overrides it by claiming an error first. Dispatch is on the
/// typed [`ErrorKind`](leaf_core::ErrorKind), NEVER a textual name.
#[derive(Clone, Copy, Default)]
pub struct DefaultGrpcStatusMapper;

impl DefaultGrpcStatusMapper {
    /// A fresh default mapper (stateless).
    #[must_use]
    pub fn new() -> Self {
        DefaultGrpcStatusMapper
    }
}

impl GrpcStatusMapper for DefaultGrpcStatusMapper {
    fn map(&self, err: &LeafError) -> Option<Status> {
        // The FALLBACK claims EVERY error (it is the floor): a user mapper, ordered
        // earlier, gets first refusal; this always produces a valid Status.
        let status = match err.kind {
            leaf_core::ErrorKind::NoSuchBean => {
                Status::new(Code::Unimplemented, "no such method / resource")
            }
            leaf_core::ErrorKind::ConvertError => {
                Status::new(Code::Internal, "message decode/convert failed")
            }
            _ => Status::new(Code::Unknown, err.kind.slug()),
        };
        Some(status)
    }
}

/// The `#[auto_config]` HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` below contributes the [`DefaultGrpcStatusMapper`] as the
/// FALLBACK `dyn GrpcStatusMapper`, gated by `OnMissingBean` so ANY user mapper
/// supersedes it — exactly like leaf-cache's `CacheAutoConfig` and
/// leaf-web-hyper's `HyperServerAutoConfig`.
#[leaf_macros::component]
pub struct GrpcStatusMapperAutoConfig;

impl GrpcStatusMapperAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        GrpcStatusMapperAutoConfig
    }
}

impl Default for GrpcStatusMapperAutoConfig {
    fn default() -> Self {
        GrpcStatusMapperAutoConfig::new()
    }
}

#[leaf_macros::auto_config]
impl GrpcStatusMapperAutoConfig {
    /// Contribute the default mapper as the FALLBACK `dyn GrpcStatusMapper`. A user
    /// mapper (an ordinary bean providing the view) supersedes this default; this is
    /// the blessed floor so an app gets sane domain-error → Status mapping with NO
    /// hand-written mapper bean.
    #[bean(name = "defaultGrpcStatusMapper", provides = "dyn ::leaf_grpc::GrpcStatusMapper")]
    #[conditional(on_missing_bean(dyn ::leaf_grpc::GrpcStatusMapper))]
    fn default_grpc_status_mapper(&self) -> DefaultGrpcStatusMapper {
        DefaultGrpcStatusMapper::new()
    }
}
```

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc mapper:: -- --nocapture
```

Expected: `mapper::tests::default_mapper_maps_the_framework_kinds ... ok`.

- [ ] **Step 5: Add the dogfood assertion — the mapper reaches AUTO_CONFIGS at FALLBACK.** Add to the `mapper::tests` module:

```rust
    #[test]
    fn default_mapper_is_a_fallback_auto_config_with_the_view() {
        use leaf_core::CandidateRole;
        // The dogfood claim: the default mapper reaches the SEPARATE auto-config channel
        // (NOT a hand-rolled Provider), at FALLBACK, carrying the dyn GrpcStatusMapper
        // view (so a user mapper supersedes it via OnMissingBean). The macro mints the
        // contract at the IMPL's module (`leaf_grpc::mapper`), not this nested `tests`.
        let impl_mod = module_path!().trim_end_matches("::tests");
        let contract = leaf_core::ContractId::of(&format!("{impl_mod}::default_grpc_status_mapper"));
        let bean = leaf_core::AUTO_CONFIGS
            .iter()
            .find(|d| d.contract == contract)
            .copied()
            .expect("the #[auto_config] default mapper reaches AUTO_CONFIGS");
        assert_eq!(
            bean.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user mapper supersedes it"
        );
        assert!(
            bean.provides
                .iter()
                .any(|r| r.view == std::any::TypeId::of::<dyn GrpcStatusMapper>()),
            "the default mapper bean must declare the dyn GrpcStatusMapper view"
        );
    }
```

Run:

```
cargo test -p leaf-grpc mapper::tests::default_mapper_is_a_fallback_auto_config -- --nocapture
```

Expected: `... ok` (proving the FALLBACK auto-config dogfood, mirroring leaf-serde's `json_converter_is_a_dyn_http_message_converter_bean_in_components`).

- [ ] **Step 6: Commit.**

```
git add crates/leaf-grpc/src/mapper.rs && git commit -m "leaf-grpc: GrpcStatusMapper SPI + DefaultGrpcStatusMapper #[auto_config] FALLBACK

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.11: DI-assembly proof — resolve the dispatch + routes + mapper through a booted engine

**Files:** `crates/leaf-grpc/tests/grpc_di_assembly.rs` (Create)

The integration proof (mirroring leaf-web's `tests/dispatch_through_mock.rs`): a test crate that registers a `#[grpc_controller]`-shaped route bean by hand (the Stage-4 macro is not yet written, so we hand-write ONE `#[component]` providing `dyn GrpcRoute` — the lone exception, exactly as leaf-web's assembly tests hand-write a route bean), boots `leaf-boot`'s cold pass, and resolves `Ref<dyn ProtocolDispatch>` + `Vec<Ref<dyn GrpcRoute>>` + `Ref<dyn GrpcStatusMapper>` by collection + by-trait injection — proving `GrpcDispatch` field-injects the route collection and the FALLBACK mapper resolves with no hand-written bean.

- [ ] **Step 1: Write the failing integration test.** Create `crates/leaf-grpc/tests/grpc_di_assembly.rs`:

```rust
//! The DI-assembly proof: leaf-boot's cold pass lifts leaf-grpc's macro-emitted
//! `#[component]` (GrpcDispatch) + `#[auto_config]` (DefaultGrpcStatusMapper) seeds,
//! and a hand-written `#[component]` route bean, then the test resolves the gRPC
//! protocol-dispatch + the route collection + the FALLBACK mapper by collection +
//! by-trait injection — the same path the EmbeddedWebServer uses. (Hand-writing ONE
//! route bean is the Stage-2 stand-in for the Stage-4 `#[grpc_controller]` macro,
//! exactly as leaf-web's assembly test hand-writes a Route bean.)

use futures::executor::block_on;
use http::{HeaderMap, Method};
use leaf_core::{BoxFuture, Ref};
use leaf_grpc::framing::encode_frame;
use leaf_grpc::{GrpcDispatch, GrpcHandler, GrpcRoute, GrpcStatusMapper};
use leaf_web::request::Request;
use leaf_web::response::{Body, Frame};
use leaf_web::ProtocolDispatch;

// ── the hand-written route bean (Stage-2 stand-in for #[grpc_controller]) ──

struct PingHandler;
impl GrpcHandler for PingHandler {
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

#[leaf_macros::component(provides = "dyn ::leaf_grpc::GrpcRoute")]
struct PingRoute {
    #[allow(dead_code)]
    handler: PingHandler,
}

impl PingRoute {
    fn new() -> Self {
        PingRoute { handler: PingHandler }
    }
}

impl Default for PingRoute {
    fn default() -> Self {
        PingRoute::new()
    }
}

impl GrpcRoute for PingRoute {
    fn path(&self) -> &str {
        "/test.Ping/Ping"
    }
    fn handler(&self) -> &dyn GrpcHandler {
        &self.handler
    }
}

fn grpc_req(path: &str) -> Request {
    let mut h = HeaderMap::new();
    h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
    Request::new(Method::POST, path.parse().expect("uri"), h, Body::full(encode_frame(b"")))
}

#[test]
fn grpc_dispatch_routes_and_mapper_resolve_by_injection() {
    // Boot leaf-boot's cold pass over THIS crate's linked seeds (the standard
    // App::from_slices entry the leaf-web assembly test uses).
    let app = leaf_boot::App::from_slices().build().expect("the engine freezes");

    // The route collection resolves (one provider — the hand-written PingRoute bean).
    let routes: Vec<Ref<dyn GrpcRoute>> =
        block_on(app.resolve::<Vec<Ref<dyn GrpcRoute>>>()).expect("routes collection resolves");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path(), "/test.Ping/Ping");

    // The FALLBACK default mapper resolves by trait with NO hand-written mapper bean.
    let mapper: Ref<dyn GrpcStatusMapper> =
        block_on(app.resolve::<Ref<dyn GrpcStatusMapper>>()).expect("the FALLBACK mapper resolves");
    let s = mapper
        .map(&leaf_core::LeafError::new(leaf_core::ErrorKind::NoSuchBean))
        .expect("default mapper claims it");
    assert_eq!(s.code, leaf_grpc::Code::Unimplemented);

    // The gRPC protocol-dispatch resolves by trait, with the route collection injected:
    // a request to the known method dispatches; an unknown method → Unimplemented.
    let dispatch: Ref<dyn ProtocolDispatch> =
        block_on(app.resolve::<Ref<dyn ProtocolDispatch>>()).expect("dyn ProtocolDispatch resolves");
    assert!(dispatch.handles(Some("application/grpc")));

    let resp = block_on(dispatch.dispatch(grpc_req("/test.Ping/Ping"))).expect("dispatch ok");
    let last_known = last_trailer(resp);
    assert_eq!(last_known.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));

    let resp = block_on(dispatch.dispatch(grpc_req("/test.Ping/Missing"))).expect("dispatch ok");
    let last_unknown = last_trailer(resp);
    assert_eq!(
        last_unknown.get("grpc-status").unwrap(),
        &http::HeaderValue::from_static("12"),
        "an unknown method dispatches to Unimplemented trailers"
    );

    // GrpcDispatch::from_routes exists as the explicit ctor too (type-checks here).
    let _ = GrpcDispatch::from_routes(routes.iter().map(|r| Ref::clone(r).into_arc()).collect());
}

fn last_trailer(resp: leaf_web::Response) -> HeaderMap {
    use futures::StreamExt;
    match resp.into_body() {
        Body::Stream(s) => block_on(s.collect::<Vec<_>>())
            .into_iter()
            .filter_map(|r| match r.expect("frame") {
                Frame::Trailers(t) => Some(t),
                _ => None,
            })
            .last()
            .expect("a trailers frame"),
        Body::Full(_) => panic!("grpc body is a stream"),
    }
}
```

- [ ] **Step 2: Run it — fails (the `App::from_slices`/`resolve` API names + the leaf-grpc imports must line up).**

```
cargo test -p leaf-grpc --test grpc_di_assembly 2>&1 | head -20
```

Expected: a compile or resolution error (e.g. the exact `App::from_slices().build()` / `app.resolve::<…>()` spelling differs from leaf-web's assembly test). Read leaf-web's `tests/dispatch_through_mock.rs` for the precise boot+resolve API and align this test's two lines (`App::from_slices…` and `app.resolve::<…>`) to it — do NOT change production code.

- [ ] **Step 3: Align the boot/resolve calls to leaf-web's assembly-test API and re-run.** After matching the exact `leaf_boot` entry points used in `crates/leaf-web/tests/dispatch_through_mock.rs`, run:

```
cargo test -p leaf-grpc --test grpc_di_assembly -- --nocapture
```

Expected: `grpc_dispatch_routes_and_mapper_resolve_by_injection ... ok` — the routes collection (1), the FALLBACK mapper (Unimplemented for NoSuchBean), and the protocol-dispatch (known→0, unknown→12) all resolve by injection.

- [ ] **Step 4: Commit.**

```
git add crates/leaf-grpc/tests/grpc_di_assembly.rs && git commit -m "leaf-grpc: DI-assembly proof — dispatch/routes/mapper resolve by injection

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.12: Stage gate — force-clean tests + clippy + doc, HTTP suite still green

**Files:** (none — verification only)

Per the project's verification rule (cached runs re-emit no warnings; force-clean before claiming clean), this gate runs the whole leaf-grpc surface plus the existing HTTP suite from a clean build.

- [ ] **Step 1: Force-clean and test leaf-grpc.**

```
cargo clean -p leaf-grpc && cargo test -p leaf-grpc -- --nocapture 2>&1 | tail -20
```

Expected: every `status`/`streaming`/`framing`/`codec`/`handler`/`dispatch`/`mapper` unit test + the `grpc_di_assembly` integration test report `... ok`; final line `test result: ok.` with `0 failed`.

- [ ] **Step 2: Clippy the new crate clean (deny warnings).**

```
cargo clippy -p leaf-grpc --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: `Finished` with NO warnings (the `missing_docs`/`unsafe_code` lints in `lib.rs` plus `-D warnings` must all be satisfied — every `pub` item already carries a doc comment).

- [ ] **Step 3: Doc the new crate clean.**

```
cargo doc -p leaf-grpc --no-deps 2>&1 | tail -10
```

Expected: `Finished`, no broken-intra-doc-link warnings.

- [ ] **Step 4: Regression — the existing HTTP suite stays green.**

```
cargo test -p leaf-web -p leaf-web-hyper -p leaf-serde 2>&1 | tail -15
```

Expected: all three crates `test result: ok.` with `0 failed` (the streaming `Body` from Stage 1 + the new leaf-grpc crate must not regress the ~1647-test HTTP suite — leaf-grpc adds a peer crate, it does not touch the HTTP path).

- [ ] **Step 5: Confirm the dep-graph constraint (leaf-web never names leaf-grpc).**

```
cargo tree -p leaf-web --invert leaf-grpc 2>&1 | head -5
```

Expected: `error: package ID specification leaf-grpc did not match any packages` (or an empty inverted tree) — proving `leaf-web` does NOT depend on `leaf-grpc` (the arrow is one-way: `leaf-grpc → leaf-web`).

- [ ] **Step 6: Commit the gate (if anything was touched) or note clean.** If steps 1–5 required no edits, there is nothing to commit; otherwise:

```
git add -A && git commit -m "leaf-grpc: force-clean gate — tests/clippy/doc green, HTTP suite intact

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 3: `leaf-grpc-build` proto-first codegen

This stage creates the **build-helper crate** `leaf-grpc-build`. It compiles `.proto` files to Rust at build time with **no `protoc` binary**: `protox` parses `.proto` → `prost_types::FileDescriptorSet`, `prost-build` emits the message structs, and a **leaf service-trait `ServiceGenerator`** emits, per gRPC service:

1. a **server trait** with one `async fn` per RPC in the correct call shape (unary / server-stream / client-stream / bidi per the SHARED CONTRACT),
2. the `/package.Service/Method` **path constants**,
3. a `#[doc(hidden)]` per-method **descriptor** (`path` + call-shape) the `#[grpc_controller]` macro (Stage 4) reads.

It also adds the `leaf_grpc::include_proto!` sugar (defined in `leaf-grpc`, Stage 2's crate, but landed here because it is the codegen include glue), and a tiny example `.proto` + a build test asserting the generated trait / paths / descriptors compile and match shapes.

The service-trait generator is **pure, unit-testable string codegen** (a `prost_build::ServiceGenerator` writing into a `String`), so the four call-shape lowerings and the descriptors are token-tested WITHOUT a compiler — exactly the `leaf-codegen` discipline. **No type-name detection**: the call shape comes from `Method.client_streaming` / `Method.server_streaming` booleans (the FileDescriptorSet), never from a textual type name. `Streaming<T>` / `Status` are referenced by the EXACT contract paths `leaf_grpc::Streaming` / `leaf_grpc::Status`; the message types by their prost paths. The crate names NO hyper/h2; `protox` + `prost-build` are the sanctioned build-time codec deps (confined like `serde_json`).

### Files

**Create**
- `crates/leaf-grpc-build/Cargo.toml` — the build-helper crate manifest (`protox`, `prost-build`, `prost-types`).
- `crates/leaf-grpc-build/src/lib.rs` — `pub fn compile(protos, includes)` + the `LeafServiceGenerator`.
- `crates/leaf-grpc-build/src/service_gen.rs` — the pure leaf service-trait/`String` generator + its unit tests.
- `crates/leaf-grpc-build/proto/echo.proto` — the tiny example proto exercising all four call shapes.
- `crates/leaf-grpc-build/tests/echo.proto` — a copy used by the build-output integration test.
- `crates/leaf-grpc-build/build.rs` — compiles `tests/echo.proto` into `OUT_DIR` for the integration test.
- `crates/leaf-grpc-build/tests/generated_service.rs` — integration test: the generated trait/paths/descriptors compile and match shapes.

**Modify**
- `Cargo.toml` (workspace) — add `leaf-grpc-build` to the internal BOM + pin `protox`/`prost-build`/`prost-types`.
- `crates/leaf-grpc/src/lib.rs` — add the `include_proto!` macro (Stage 2 owns the crate; this stage adds the include sugar).

---

### Task 3.1: Scaffold the `leaf-grpc-build` crate + workspace wiring

**Files:** `Cargo.toml` (workspace), `crates/leaf-grpc-build/Cargo.toml`, `crates/leaf-grpc-build/src/lib.rs`

- [ ] **Step 1: Pin the build-time codec deps in the workspace BOM.** Add to the `[workspace.dependencies]` block in the root `Cargo.toml` (alongside the existing `leaf-*` BOM entry for the new crate, and the external pins). protox 0.7 tracks prost 0.13.

```toml
leaf-grpc-build = { path = "crates/leaf-grpc-build", version = "0.1.0" }
```

```toml
# Build-time gRPC codegen (leaf-grpc-build ONLY): protox parses .proto -> a
# FileDescriptorSet with NO `protoc` system binary (pure Rust); prost-build emits the
# message structs; prost-types carries the FileDescriptorSet value. These are the
# sanctioned build-time codec deps, confined to leaf-grpc-build exactly as
# serde_json/protox names a WIRE FORMAT, never an HTTP server (hyper stays in
# leaf-web-hyper). prost (the runtime codec) lives in leaf-grpc, not here.
protox = "0.7"
prost-build = "0.13"
prost-types = "0.13"
```

- [ ] **Step 2: Write the crate manifest.** `crates/leaf-grpc-build/Cargo.toml`:

```toml
[package]
name = "leaf-grpc-build"
version.workspace = true
edition.workspace = true
license.workspace = true

# leaf-grpc-build is the BUILD-HELPER crate — an app's `build.rs` calls
# `leaf_grpc_build::compile(&["proto/x.proto"], &["proto"])`. It names NO leaf-* crate
# (it emits TOKENS that reference `leaf_grpc::…` paths resolved in the APP's compile,
# not here), and NO hyper/h2: only the sanctioned build-time codec deps.
[dependencies]
# protox: pure-Rust .proto -> FileDescriptorSet (no `protoc` binary).
protox.workspace = true
# prost-build: the message-struct generator + the ServiceGenerator seam we hook.
prost-build.workspace = true
# prost-types: the FileDescriptorSet / ServiceDescriptorProto value vocabulary.
prost-types.workspace = true

[build-dependencies]
# The integration test (`tests/generated_service.rs`) needs generated code in OUT_DIR;
# `build.rs` self-hosts `compile` over `tests/echo.proto`, so the crate's OWN build.rs
# depends on its OWN library surface via path (the standard build-dep idiom).
leaf-grpc-build = { path = "." }
```

- [ ] **Step 3: Write the skeleton `src/lib.rs` (compiles, `compile` is a stub that returns Ok).** This lets the crate build before the generator exists.

```rust
//! `leaf-grpc-build` — proto-first codegen for leaf gRPC services.
//!
//! An app's `build.rs` calls [`compile`]: `protox` parses the `.proto` files into a
//! `prost_types::FileDescriptorSet` (NO `protoc` system binary — pure Rust), then
//! `prost-build` emits the message structs while a leaf [`service_gen::LeafServiceGenerator`]
//! emits, per gRPC service, a leaf-shaped server trait + the `/pkg.Service/Method`
//! path constants + the `#[doc(hidden)]` per-method descriptors the `#[grpc_controller]`
//! macro (Stage 4) reads. Output lands in `OUT_DIR`, included via
//! `leaf_grpc::include_proto!("pkg")`.

pub mod service_gen;

/// Compile `protos` (resolved against `includes`) to Rust in `OUT_DIR`.
///
/// Pure-Rust pipeline: `protox` -> `FileDescriptorSet` -> `prost-build` (messages) +
/// the leaf service-trait generator (server trait + path constants + descriptors).
///
/// # Errors
/// Returns an [`std::io::Error`] if parsing or codegen fails.
pub fn compile(protos: &[&str], includes: &[&str]) -> std::io::Result<()> {
    let _ = (protos, includes);
    Ok(())
}
```

- [ ] **Step 4: Run — the crate compiles.**

```
cargo build -p leaf-grpc-build
```
Expected: `Finished` (no errors).

- [ ] **Step 5: Commit.**

```
git add Cargo.toml crates/leaf-grpc-build/Cargo.toml crates/leaf-grpc-build/src/lib.rs
git commit -m "leaf-grpc-build: scaffold the proto-first codegen crate

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.2: The call-shape classifier (booleans → shape, no type-name detection)

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`

The four call shapes are decided ONLY from `prost_build::Method.client_streaming` / `.server_streaming` (sourced from the FileDescriptorSet), never from any textual type name — the no-type-name-detection rule.

- [ ] **Step 1: Write the failing test for the classifier.** Create `crates/leaf-grpc-build/src/service_gen.rs`:

```rust
//! The leaf service-trait generator — a pure `prost_build::ServiceGenerator` that
//! writes Rust source into a `String`, so every call-shape lowering is unit-testable
//! WITHOUT a compiler (the leaf-codegen discipline). It emits, per gRPC service: a
//! leaf-shaped server trait, the `/pkg.Service/Method` path constants, and the
//! `#[doc(hidden)]` per-method descriptors the `#[grpc_controller]` macro reads.

/// The RPC call shape — decided ONLY from the `client_streaming`/`server_streaming`
/// booleans the FileDescriptorSet carries, NEVER from a textual type name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>`
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>`
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>`
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>`
    Bidi,
}

impl CallShape {
    /// Classify from the two streaming flags (the FileDescriptorSet's `Method`).
    #[must_use]
    pub fn from_flags(client_streaming: bool, server_streaming: bool) -> Self {
        match (client_streaming, server_streaming) {
            (false, false) => CallShape::Unary,
            (false, true) => CallShape::ServerStream,
            (true, false) => CallShape::ClientStream,
            (true, true) => CallShape::Bidi,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_all_four_shapes_from_the_streaming_flags() {
        assert_eq!(CallShape::from_flags(false, false), CallShape::Unary);
        assert_eq!(CallShape::from_flags(false, true), CallShape::ServerStream);
        assert_eq!(CallShape::from_flags(true, false), CallShape::ClientStream);
        assert_eq!(CallShape::from_flags(true, true), CallShape::Bidi);
    }
}
```

- [ ] **Step 2: Run — it fails (module not declared / does not compile).**

```
cargo test -p leaf-grpc-build service_gen::tests::classifies -- --nocapture
```
Expected: compile error — `service_gen` not declared in `lib.rs`.

- [ ] **Step 3: Declare the module (`pub mod service_gen;` already in lib.rs from 3.1) — re-run; passes.**

```
cargo test -p leaf-grpc-build service_gen::tests::classifies -- --nocapture
```
Expected: `test service_gen::tests::classifies_all_four_shapes_from_the_streaming_flags ... ok`.

- [ ] **Step 4: Commit.**

```
git add crates/leaf-grpc-build/src/service_gen.rs
git commit -m "leaf-grpc-build: CallShape classifier from streaming flags (no type-name detection)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.3: Per-method signature + the typed request/response tokens

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`

Each method lowers to an `async fn` whose `req` type and return type are wrapped per the call shape, referencing the EXACT contract names `leaf_grpc::Streaming` / `leaf_grpc::Status`. The message types (`input`/`output`) are the prost paths prost-build already resolved (e.g. `super::EchoReq`); we receive them as strings and emit them verbatim.

- [ ] **Step 1: Write the failing test for all four method signatures.** Append to the `tests` module in `service_gen.rs`:

```rust
    #[test]
    fn emits_the_unary_method_signature() {
        let sig = method_signature("get", "super::Ping", "super::Pong", CallShape::Unary);
        assert_eq!(
            sig.split_whitespace().collect::<String>(),
            "asyncfnget(&self,req:super::Ping)->::core::result::Result<super::Pong,::leaf_grpc::Status>"
                .to_string()
        );
    }

    #[test]
    fn emits_the_server_stream_method_signature() {
        let sig = method_signature("list", "super::Ping", "super::Pong", CallShape::ServerStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:super::Ping"),
            "server-stream request is a plain message: {flat}"
        );
        assert!(
            flat.contains("::leaf_grpc::Streaming<super::Pong>"),
            "server-stream response is Streaming<U>: {flat}"
        );
    }

    #[test]
    fn emits_the_client_stream_method_signature() {
        let sig = method_signature("upload", "super::Ping", "super::Pong", CallShape::ClientStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:::leaf_grpc::Streaming<super::Ping>"),
            "client-stream request is Streaming<T>: {flat}"
        );
        assert!(
            flat.contains("->::core::result::Result<super::Pong"),
            "client-stream response is a plain message: {flat}"
        );
    }

    #[test]
    fn emits_the_bidi_method_signature() {
        let sig = method_signature("chat", "super::Ping", "super::Pong", CallShape::Bidi);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(flat.contains("req:::leaf_grpc::Streaming<super::Ping>"), "got: {flat}");
        assert!(
            flat.contains("::core::result::Result<::leaf_grpc::Streaming<super::Pong>,::leaf_grpc::Status>"),
            "bidi response is Streaming<U>: {flat}"
        );
    }
```

- [ ] **Step 2: Run — fails (no `method_signature` fn).**

```
cargo test -p leaf-grpc-build service_gen::tests::emits_the -- --nocapture
```
Expected: compile error — `method_signature` not found.

- [ ] **Step 3: Implement `method_signature`.** Add to `service_gen.rs` (above the `tests` mod):

```rust
/// The leaf request type for a call shape: `T` (unary/server-stream) wraps to
/// `leaf_grpc::Streaming<T>` for the client-streaming side. `input`/`output` are the
/// prost-resolved message type paths (emitted VERBATIM).
fn request_ty(input: &str, shape: CallShape) -> String {
    match shape {
        CallShape::Unary | CallShape::ServerStream => input.to_string(),
        CallShape::ClientStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{input}>")
        }
    }
}

/// The leaf response type for a call shape: `Result<U, Status>` (unary/client-stream)
/// or `Result<Streaming<U>, Status>` (server-stream/bidi).
fn response_ty(output: &str, shape: CallShape) -> String {
    let inner = match shape {
        CallShape::Unary | CallShape::ClientStream => output.to_string(),
        CallShape::ServerStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{output}>")
        }
    };
    format!("::core::result::Result<{inner}, ::leaf_grpc::Status>")
}

/// The full `async fn` signature line for one RPC (no body/semicolon).
fn method_signature(name: &str, input: &str, output: &str, shape: CallShape) -> String {
    format!(
        "async fn {name}(&self, req: {req}) -> {resp}",
        req = request_ty(input, shape),
        resp = response_ty(output, shape),
    )
}
```

- [ ] **Step 4: Run — passes.**

```
cargo test -p leaf-grpc-build service_gen::tests::emits_the -- --nocapture
```
Expected: 4 method-signature tests `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc-build/src/service_gen.rs
git commit -m "leaf-grpc-build: per-method async fn signatures for the four call shapes

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.4: Path constants + the `#[doc(hidden)]` per-method descriptors

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`

Per the contract: `/package.Service/Method` path constants + a `#[doc(hidden)]` descriptor the `#[grpc_controller]` macro reads to know each method's path + call shape. The descriptor is a const struct (`leaf_grpc::MethodDescriptor`, Stage 2's type) holding the path + a `leaf_grpc::CallShape` discriminant. Methods named verbatim from the proto; the path is the canonical gRPC literal.

- [ ] **Step 1: Write the failing test for the path constant + descriptor.** Append to `tests`:

```rust
    #[test]
    fn emits_the_grpc_path_constant() {
        let c = path_const("Echo", "echo.v1", "Get", "Echo");
        let flat = c.split_whitespace().collect::<String>();
        assert!(
            flat.contains(r#"pubconstGET_PATH:&str="/echo.v1.Echo/Get""#),
            "the path is the canonical /pkg.Service/Method literal: {flat}"
        );
    }

    #[test]
    fn emits_the_doc_hidden_method_descriptor() {
        let d = method_descriptor("Get", "echo.v1", "Echo", CallShape::Unary);
        let flat = d.split_whitespace().collect::<String>();
        assert!(flat.contains("#[doc(hidden)]"), "descriptor is doc-hidden: {flat}");
        assert!(
            flat.contains(r#"path:"/echo.v1.Echo/Get""#),
            "descriptor carries the path: {flat}"
        );
        assert!(
            flat.contains("shape:::leaf_grpc::CallShape::Unary"),
            "descriptor carries the call shape: {flat}"
        );
        assert!(
            flat.contains("::leaf_grpc::MethodDescriptor"),
            "descriptor is the leaf_grpc::MethodDescriptor type: {flat}"
        );
    }

    #[test]
    fn descriptor_shape_matches_the_streaming_flags_for_bidi() {
        let d = method_descriptor("Chat", "echo.v1", "Echo", CallShape::Bidi);
        assert!(
            d.split_whitespace().collect::<String>().contains("shape:::leaf_grpc::CallShape::Bidi"),
            "got: {d}"
        );
    }
```

- [ ] **Step 2: Run — fails (no `path_const`/`method_descriptor`).**

```
cargo test -p leaf-grpc-build service_gen::tests::emits_the_grpc -- --nocapture
```
Expected: compile error — functions not found.

- [ ] **Step 3: Implement the path + descriptor emitters.** Add to `service_gen.rs`:

```rust
/// The canonical gRPC method path `/package.Service/Method`. `package` may be empty
/// (then the path is `/Service/Method`).
fn grpc_path(package: &str, service: &str, method: &str) -> String {
    if package.is_empty() {
        format!("/{service}/{method}")
    } else {
        format!("/{package}.{service}/{method}")
    }
}

/// SCREAMING_SNAKE constant base for a method name (`Get` -> `GET`, `ListAll` ->
/// `LIST_ALL`). Pure case mechanics — NOT type-name detection (it derives an IDENT
/// from the proto's own method name, no behavior keyed on the text).
fn const_ident(method: &str) -> String {
    let mut out = String::new();
    for (i, ch) in method.chars().enumerate() {
        if ch.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// The `leaf_grpc::CallShape` const path for a shape.
fn shape_const_path(shape: CallShape) -> &'static str {
    match shape {
        CallShape::Unary => "::leaf_grpc::CallShape::Unary",
        CallShape::ServerStream => "::leaf_grpc::CallShape::ServerStream",
        CallShape::ClientStream => "::leaf_grpc::CallShape::ClientStream",
        CallShape::Bidi => "::leaf_grpc::CallShape::Bidi",
    }
}

/// The `pub const <METHOD>_PATH: &str = "/pkg.Service/Method";` line.
fn path_const(service: &str, package: &str, method: &str, _svc_alias: &str) -> String {
    let path = grpc_path(package, service, method);
    let ident = const_ident(method);
    format!("pub const {ident}_PATH: &str = {path:?};")
}

/// The `#[doc(hidden)]` const `leaf_grpc::MethodDescriptor` the `#[grpc_controller]`
/// macro reads: the canonical path + the call shape. Named
/// `<METHOD>_DESCRIPTOR` beside its `<METHOD>_PATH`.
fn method_descriptor(method: &str, package: &str, service: &str, shape: CallShape) -> String {
    let path = grpc_path(package, service, method);
    let ident = const_ident(method);
    let shape_path = shape_const_path(shape);
    format!(
        "#[doc(hidden)] pub const {ident}_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = \
         ::leaf_grpc::MethodDescriptor {{ path: {path:?}, shape: {shape_path} }};"
    )
}
```

> NOTE: `path_const`'s test calls `path_const("Echo", "echo.v1", "Get", "Echo")` — the signature is `(service, package, method, svc_alias)`; the 4th arg is reserved for the module-path alias and currently unused (`_svc_alias`).

- [ ] **Step 4: Run — passes.**

```
cargo test -p leaf-grpc-build service_gen::tests -- --nocapture
```
Expected: all `service_gen::tests` (classifier + signatures + path/descriptor) `... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc-build/src/service_gen.rs
git commit -m "leaf-grpc-build: /pkg.Service/Method path constants + doc(hidden) method descriptors

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.5: Assemble the full service module (trait + impl-block consts) for one `Service`

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`

Glue the pieces into one rendered service: a server **trait** (`Send + Sync` supertrait, async-fn-in-trait so the `#[grpc_controller]` impl is a plain `impl`), inside a `pub mod <service_snake>` holding the path constants + descriptors. We render from a minimal `MethodSpec` so the assembler is testable without a real `prost_build::Service`; Task 3.6 maps `prost_build::Service` → `Vec<MethodSpec>`.

- [ ] **Step 1: Write the failing test for a full two-method service.** Append to `tests`:

```rust
    fn echo_spec() -> ServiceSpec {
        ServiceSpec {
            name: "Echo".into(),
            package: "echo.v1".into(),
            methods: vec![
                MethodSpec {
                    name: "Get".into(),
                    fn_name: "get".into(),
                    input: "super::Ping".into(),
                    output: "super::Pong".into(),
                    shape: CallShape::Unary,
                },
                MethodSpec {
                    name: "Chat".into(),
                    fn_name: "chat".into(),
                    input: "super::Msg".into(),
                    output: "super::Msg".into(),
                    shape: CallShape::Bidi,
                },
            ],
        }
    }

    #[test]
    fn renders_a_compilable_service_module() {
        let src = render_service(&echo_spec());
        // The whole rendered service must PARSE as a Rust item sequence (the
        // descriptor.rs discipline: emit data that parses without a compiler-link).
        syn::parse_str::<syn::File>(&src).expect("the rendered service is valid Rust items");
    }

    #[test]
    fn service_trait_is_send_sync_with_one_method_per_rpc() {
        let flat = render_service(&echo_spec()).split_whitespace().collect::<String>();
        assert!(flat.contains("pubtraitEcho:Send+Sync"), "trait is Send+Sync: {flat}");
        assert!(flat.contains("asyncfnget(&self,req:super::Ping)"), "unary method: {flat}");
        assert!(flat.contains("asyncfnchat(&self,req:::leaf_grpc::Streaming<super::Msg>)"), "bidi method: {flat}");
    }

    #[test]
    fn service_module_holds_paths_and_descriptors_per_method() {
        let flat = render_service(&echo_spec()).split_whitespace().collect::<String>();
        assert!(flat.contains(r#"pubconstGET_PATH:&str="/echo.v1.Echo/Get""#), "path const: {flat}");
        assert!(flat.contains(r#"pubconstCHAT_PATH:&str="/echo.v1.Echo/Chat""#), "path const: {flat}");
        assert!(flat.contains("GET_DESCRIPTOR:::leaf_grpc::MethodDescriptor"), "descriptor: {flat}");
        assert!(flat.contains("shape:::leaf_grpc::CallShape::Bidi"), "bidi descriptor: {flat}");
    }
```

> Add `syn` as a dev-dependency for the parse assertion — append to `crates/leaf-grpc-build/Cargo.toml`:
> ```toml
> [dev-dependencies]
> # Parse-check the rendered service source in unit tests (no compiler-link needed) —
> # the descriptor.rs `syn::parse_str::<File>` discipline.
> syn = { workspace = true }
> ```

- [ ] **Step 2: Run — fails (no `ServiceSpec`/`MethodSpec`/`render_service`).**

```
cargo test -p leaf-grpc-build service_gen::tests::renders_a -- --nocapture
```
Expected: compile error — types/fn not found.

- [ ] **Step 3: Implement the specs + `render_service`.** Add to `service_gen.rs` (above `tests`):

```rust
/// A single RPC, shape-classified and with its prost-resolved message paths — the
/// compiler-independent input the assembler renders (Task 3.6 builds these from a real
/// `prost_build::Service`).
#[derive(Clone, Debug)]
pub struct MethodSpec {
    /// The proto method name (`Get`) — drives the path + const idents.
    pub name: String,
    /// The Rust trait-method ident (`get`) — prost's snake_case fn name.
    pub fn_name: String,
    /// The prost-resolved request message path (emitted verbatim, e.g. `super::Ping`).
    pub input: String,
    /// The prost-resolved response message path.
    pub output: String,
    /// The call shape (from the streaming flags).
    pub shape: CallShape,
}

/// One gRPC service: its name/package + its methods.
#[derive(Clone, Debug)]
pub struct ServiceSpec {
    /// The proto service name (`Echo`) — the trait ident + path component.
    pub name: String,
    /// The proto package (`echo.v1`) — the path prefix (may be empty).
    pub package: String,
    /// The RPC methods.
    pub methods: Vec<MethodSpec>,
}

/// Snake-case the service name for its containing module (`Echo` -> `echo`,
/// `EchoService` -> `echo_service`). Pure case mechanics over the proto's OWN name.
fn module_ident(service: &str) -> String {
    let mut out = String::new();
    for (i, ch) in service.chars().enumerate() {
        if ch.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Render one service to Rust source: the `Send + Sync` server trait with one async
/// method per RPC, plus a `pub mod <service_snake>` holding the `<M>_PATH` constants
/// and the `#[doc(hidden)] <M>_DESCRIPTOR`s.
#[must_use]
pub fn render_service(svc: &ServiceSpec) -> String {
    let mut out = String::new();

    // ── the server trait (async-fn-in-trait so a `#[grpc_controller]` impl is plain) ──
    out.push_str(&format!("pub trait {}: Send + Sync {{\n", svc.name));
    for m in &svc.methods {
        out.push_str("    ");
        out.push_str(&method_signature(&m.fn_name, &m.input, &m.output, m.shape));
        out.push_str(";\n");
    }
    out.push_str("}\n\n");

    // ── the per-service module of path constants + method descriptors ──
    let module = module_ident(&svc.name);
    out.push_str(&format!("pub mod {module} {{\n"));
    for m in &svc.methods {
        out.push_str("    ");
        out.push_str(&path_const(&svc.name, &svc.package, &m.name, &svc.name));
        out.push('\n');
        out.push_str("    ");
        out.push_str(&method_descriptor(&m.name, &svc.package, &svc.name, m.shape));
        out.push('\n');
    }
    out.push_str("}\n");

    out
}
```

- [ ] **Step 4: Run — passes (incl. the `syn` parse check).**

```
cargo test -p leaf-grpc-build service_gen::tests -- --nocapture
```
Expected: all `service_gen::tests` `... ok` (render + parse + trait + module-consts).

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc-build/Cargo.toml crates/leaf-grpc-build/src/service_gen.rs
git commit -m "leaf-grpc-build: assemble the per-service trait + path/descriptor module (parse-checked)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.6: The `prost_build::ServiceGenerator` adapter + `compile`

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`, `crates/leaf-grpc-build/src/lib.rs`

Hook `render_service` into prost-build's `ServiceGenerator` seam (mapping `prost_build::Service`/`Method` → our specs using ONLY the streaming booleans), then wire `protox` (parse → `FileDescriptorSet`) + `prost_build::Config::compile_fds` with our generator + `out_dir(OUT_DIR)`.

- [ ] **Step 1: Write the failing test mapping a `prost_build::Service` → `ServiceSpec`.** Append to `service_gen.rs` `tests`:

```rust
    fn pb_method(name: &str, cs: bool, ss: bool) -> ::prost_build::Method {
        ::prost_build::Method {
            name: name.to_ascii_lowercase(),
            proto_name: name.to_string(),
            comments: ::prost_build::Comments {
                leading_detached: vec![],
                leading: vec![],
                trailing: vec![],
            },
            input_type: format!("super::{name}Req"),
            output_type: format!("super::{name}Resp"),
            input_proto_type: format!(".echo.v1.{name}Req"),
            output_proto_type: format!(".echo.v1.{name}Resp"),
            options: ::prost_types::MethodOptions::default(),
            client_streaming: cs,
            server_streaming: ss,
        }
    }

    #[test]
    fn maps_a_prost_service_to_a_spec_using_only_streaming_flags() {
        let svc = ::prost_build::Service {
            name: "Echo".into(),
            proto_name: "Echo".into(),
            package: "echo.v1".into(),
            comments: ::prost_build::Comments {
                leading_detached: vec![],
                leading: vec![],
                trailing: vec![],
            },
            methods: vec![pb_method("Get", false, false), pb_method("Chat", true, true)],
            options: ::prost_types::ServiceOptions::default(),
        };
        let spec = spec_from_service(&svc);
        assert_eq!(spec.name, "Echo");
        assert_eq!(spec.package, "echo.v1");
        assert_eq!(spec.methods[0].shape, CallShape::Unary);
        assert_eq!(spec.methods[0].fn_name, "get");
        assert_eq!(spec.methods[0].input, "super::GetReq");
        assert_eq!(spec.methods[1].shape, CallShape::Bidi);
        assert_eq!(spec.methods[1].name, "Chat");
    }
```

- [ ] **Step 2: Run — fails (no `spec_from_service`).**

```
cargo test -p leaf-grpc-build service_gen::tests::maps_a -- --nocapture
```
Expected: compile error — `spec_from_service` not found.

- [ ] **Step 3: Implement `spec_from_service` + the `ServiceGenerator` impl.** Add to `service_gen.rs`:

```rust
/// Map a `prost_build::Service` to a [`ServiceSpec`]. The call shape comes ONLY from
/// each method's `client_streaming`/`server_streaming` flags (the FileDescriptorSet) —
/// never from a textual type name. `input_type`/`output_type` are prost's already-
/// resolved Rust paths, emitted verbatim.
#[must_use]
pub fn spec_from_service(svc: &::prost_build::Service) -> ServiceSpec {
    ServiceSpec {
        name: svc.name.clone(),
        package: svc.package.clone(),
        methods: svc
            .methods
            .iter()
            .map(|m| MethodSpec {
                name: m.proto_name.clone(),
                fn_name: m.name.clone(),
                input: m.input_type.clone(),
                output: m.output_type.clone(),
                shape: CallShape::from_flags(m.client_streaming, m.server_streaming),
            })
            .collect(),
    }
}

/// The leaf `prost_build::ServiceGenerator`: for each gRPC service prost-build
/// encounters, append the rendered leaf server trait + path/descriptor module to the
/// output buffer (beside the message structs prost emits).
#[derive(Default)]
pub struct LeafServiceGenerator;

impl ::prost_build::ServiceGenerator for LeafServiceGenerator {
    fn generate(&mut self, service: ::prost_build::Service, buf: &mut String) {
        buf.push('\n');
        buf.push_str(&render_service(&spec_from_service(&service)));
        buf.push('\n');
    }
}
```

- [ ] **Step 4: Run — passes.**

```
cargo test -p leaf-grpc-build service_gen::tests::maps_a -- --nocapture
```
Expected: `... maps_a_prost_service_to_a_spec_using_only_streaming_flags ... ok`.

- [ ] **Step 5: Implement `compile` in `lib.rs`.** Replace the stub:

```rust
/// Compile `protos` (resolved against `includes`) to Rust in `OUT_DIR`.
///
/// Pure-Rust pipeline: `protox` parses to a `FileDescriptorSet` (NO `protoc` system
/// binary), then `prost_build::Config::compile_fds` emits the message structs while
/// [`service_gen::LeafServiceGenerator`] emits the leaf server trait + path constants +
/// the `#[doc(hidden)]` method descriptors per service. Output lands in `OUT_DIR`,
/// included via `leaf_grpc::include_proto!("pkg")`.
///
/// # Errors
/// Returns an [`std::io::Error`] if `protox` parsing or prost-build codegen fails.
pub fn compile(protos: &[&str], includes: &[&str]) -> std::io::Result<()> {
    // protox: pure-Rust .proto -> FileDescriptorSet (no protoc binary).
    let fds = protox::compile(protos, includes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    // Re-run the build only when a .proto changes.
    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    let out_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR not set"))?;

    let mut config = prost_build::Config::new();
    config.out_dir(out_dir);
    config.service_generator(Box::new(service_gen::LeafServiceGenerator));
    // compile_fds drives prost-build off the protox FileDescriptorSet (no protoc).
    config.compile_fds(fds)
}
```

- [ ] **Step 6: Run — the library compiles with the real pipeline.**

```
cargo build -p leaf-grpc-build
```
Expected: `Finished`.

- [ ] **Step 7: Commit.**

```
git add crates/leaf-grpc-build/src/service_gen.rs crates/leaf-grpc-build/src/lib.rs
git commit -m "leaf-grpc-build: prost-build ServiceGenerator adapter + protox-driven compile()

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.7: The `leaf_grpc::include_proto!` sugar

**Files:** `crates/leaf-grpc/src/lib.rs`

The contract: `leaf_grpc::include_proto!("pkg")` = `include!(concat!(env!("OUT_DIR"), "/pkg.rs"))`. Stage 2 owns the `leaf-grpc` crate; this stage lands the include glue so the generated module includes cleanly.

- [ ] **Step 1: Write the failing doctest/unit test for the macro shape.** Add to `crates/leaf-grpc/src/lib.rs` a unit test (a macro-expansion smoke test using a tempfile via `OUT_DIR`-style include is awkward, so assert the token shape by including a fixture). Simpler: a compile-time presence test in `crates/leaf-grpc/tests/include_proto_macro.rs`:

```rust
//! `include_proto!` must expand to an `include!` of `$OUT_DIR/<pkg>.rs`. We point
//! OUT_DIR-like inclusion at a fixture written here at test time, proving the macro
//! splices a generated file into a module.

// The macro builds the path from env!("OUT_DIR"); the build.rs writes `fixture.rs`
// into OUT_DIR so the include resolves.
mod fixture {
    leaf_grpc::include_proto!("fixture");
}

#[test]
fn include_proto_splices_the_generated_module() {
    // `fixture.rs` (written by build.rs) declares `pub const MARKER: u32 = 42;`.
    assert_eq!(fixture::MARKER, 42);
}
```

Add `crates/leaf-grpc/build.rs`:

```rust
//! Writes a tiny generated-style fixture into OUT_DIR so the `include_proto!` macro
//! test (`tests/include_proto_macro.rs`) has a file to splice.
use std::io::Write;

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let path = std::path::Path::new(&out).join("fixture.rs");
    let mut f = std::fs::File::create(path).expect("create fixture.rs");
    writeln!(f, "pub const MARKER: u32 = 42;").expect("write fixture");
    println!("cargo:rerun-if-changed=build.rs");
}
```

- [ ] **Step 2: Run — fails (no `include_proto!` macro).**

```
cargo test -p leaf-grpc --test include_proto_macro -- --nocapture
```
Expected: compile error — `cannot find macro include_proto`.

- [ ] **Step 3: Implement the macro in `crates/leaf-grpc/src/lib.rs`.**

```rust
/// Splice a `leaf-grpc-build`-generated module into the current scope.
///
/// `leaf_grpc::include_proto!("pkg")` expands to
/// `include!(concat!(env!("OUT_DIR"), "/pkg.rs"))` — the standard prost/tonic include
/// idiom, the sugar for the proto-first codegen `leaf_grpc_build::compile` writes into
/// `OUT_DIR`.
#[macro_export]
macro_rules! include_proto {
    ($pkg:literal) => {
        include!(concat!(env!("OUT_DIR"), "/", $pkg, ".rs"));
    };
}
```

- [ ] **Step 4: Run — passes.**

```
cargo test -p leaf-grpc --test include_proto_macro -- --nocapture
```
Expected: `... include_proto_splices_the_generated_module ... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/lib.rs crates/leaf-grpc/build.rs crates/leaf-grpc/tests/include_proto_macro.rs
git commit -m "leaf-grpc: include_proto! sugar = include!(OUT_DIR/pkg.rs)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.8: End-to-end build-output proof — a real `.proto` compiles to a real trait/paths/descriptors

**Files:** `crates/leaf-grpc-build/proto/echo.proto`, `crates/leaf-grpc-build/tests/echo.proto`, `crates/leaf-grpc-build/build.rs`, `crates/leaf-grpc-build/tests/generated_service.rs`, `crates/leaf-grpc-build/Cargo.toml`

The headline proof: run the FULL pipeline (`protox` → `prost-build` → `LeafServiceGenerator`) over a real four-shape `.proto` and assert the generated trait/paths/descriptors compile and match the contract shapes. The generated code references `leaf_grpc::Streaming`/`Status`/`CallShape`/`MethodDescriptor`, so this test crate dev-depends on `leaf-grpc` (Stage 2).

- [ ] **Step 1: Write the example proto (all four shapes).** `crates/leaf-grpc-build/proto/echo.proto`:

```protobuf
syntax = "proto3";
package echo.v1;

message Ping { string msg = 1; }
message Pong { string msg = 1; }

service Echo {
  // unary
  rpc Get(Ping) returns (Pong);
  // server-streaming
  rpc List(Ping) returns (stream Pong);
  // client-streaming
  rpc Upload(stream Ping) returns (Pong);
  // bidi
  rpc Chat(stream Ping) returns (stream Pong);
}
```

Copy it to the test include path so `build.rs` has a stable location independent of the example:

```
cp crates/leaf-grpc-build/proto/echo.proto crates/leaf-grpc-build/tests/echo.proto
```

- [ ] **Step 2: Write `build.rs` to compile the test proto into OUT_DIR.** `crates/leaf-grpc-build/build.rs`:

```rust
//! Self-hosts the codegen for the integration test: compiles `tests/echo.proto`
//! through the crate's OWN `compile` (protox + prost-build + the leaf generator) into
//! OUT_DIR, so `tests/generated_service.rs` can `include!` the result and assert the
//! generated trait/paths/descriptors compile + match shapes.
fn main() -> std::io::Result<()> {
    leaf_grpc_build::compile(&["tests/echo.proto"], &["tests"])
}
```

- [ ] **Step 3: Add the integration-test dev-deps to the manifest.** Append to `crates/leaf-grpc-build/Cargo.toml`'s `[dev-dependencies]`:

```toml
# The build-output proof (`tests/generated_service.rs`) includes the generated module
# and exercises the leaf server trait — whose signatures name `leaf_grpc::Streaming`/
# `Status`/`CallShape`/`MethodDescriptor` (Stage 2). Dev-only: the production lib names
# no leaf-* crate. prost (the runtime message codec) backs the generated `#[derive(Message)]`
# structs at TEST compile time.
leaf-grpc = { workspace = true }
prost = { workspace = true }
```

> Pin `prost` in the workspace `[workspace.dependencies]` if not already present (Stage 2 adds it; if running this stage standalone, add `prost = "0.13"`).

- [ ] **Step 4: Write the failing integration test.** `crates/leaf-grpc-build/tests/generated_service.rs`:

```rust
//! End-to-end build-output proof: the FULL pipeline (protox -> prost-build -> the leaf
//! service-trait generator) over `tests/echo.proto` produces a compilable leaf server
//! trait with the four correct call shapes, the `/echo.v1.Echo/Method` path constants,
//! and the `#[doc(hidden)]` method descriptors the `#[grpc_controller]` macro reads.

include!(concat!(env!("OUT_DIR"), "/echo.v1.rs"));

/// A trivial implementor of the GENERATED `Echo` trait — proving the four method
/// signatures are exactly the contract shapes (this would not compile otherwise).
struct EchoImpl;

impl Echo for EchoImpl {
    async fn get(&self, _req: Ping) -> Result<Pong, ::leaf_grpc::Status> {
        Ok(Pong::default())
    }
    async fn list(&self, _req: Ping) -> Result<::leaf_grpc::Streaming<Pong>, ::leaf_grpc::Status> {
        Ok(::leaf_grpc::Streaming::once(Pong::default()))
    }
    async fn upload(
        &self,
        _req: ::leaf_grpc::Streaming<Ping>,
    ) -> Result<Pong, ::leaf_grpc::Status> {
        Ok(Pong::default())
    }
    async fn chat(
        &self,
        _req: ::leaf_grpc::Streaming<Ping>,
    ) -> Result<::leaf_grpc::Streaming<Pong>, ::leaf_grpc::Status> {
        Ok(::leaf_grpc::Streaming::once(Pong::default()))
    }
}

#[test]
fn the_generated_message_structs_are_prost_messages() {
    // prost-build emitted the message structs from the FileDescriptorSet.
    let p = Ping { msg: "hi".into() };
    assert_eq!(p.msg, "hi");
}

#[test]
fn the_path_constants_are_the_canonical_grpc_literals() {
    assert_eq!(echo::GET_PATH, "/echo.v1.Echo/Get");
    assert_eq!(echo::LIST_PATH, "/echo.v1.Echo/List");
    assert_eq!(echo::UPLOAD_PATH, "/echo.v1.Echo/Upload");
    assert_eq!(echo::CHAT_PATH, "/echo.v1.Echo/Chat");
}

#[test]
fn the_method_descriptors_carry_the_path_and_the_call_shape() {
    assert_eq!(echo::GET_DESCRIPTOR.path, "/echo.v1.Echo/Get");
    assert_eq!(echo::GET_DESCRIPTOR.shape, ::leaf_grpc::CallShape::Unary);
    assert_eq!(echo::LIST_DESCRIPTOR.shape, ::leaf_grpc::CallShape::ServerStream);
    assert_eq!(echo::UPLOAD_DESCRIPTOR.shape, ::leaf_grpc::CallShape::ClientStream);
    assert_eq!(echo::CHAT_DESCRIPTOR.shape, ::leaf_grpc::CallShape::Bidi);
}

#[test]
fn the_generated_trait_is_implementable_in_all_four_shapes() {
    // Constructing the impl proves all four generated signatures are the EXACT
    // contract shapes — the trait method types would reject a mismatched body.
    let _e = EchoImpl;
}
```

> This test asserts `leaf_grpc::MethodDescriptor` exposes public `path: &'static str` + `shape: CallShape` fields, `CallShape` impls `PartialEq + Debug`, and `Streaming::once`/`Status` exist — all Stage 2 contract types. If running Stage 3 BEFORE Stage 2, gate this single test file behind a feature or land it once Stage 2 merges; the `service_gen` unit tests (3.2–3.6) do not need Stage 2.

- [ ] **Step 5: Run — it builds and passes.**

```
cargo test -p leaf-grpc-build --test generated_service -- --nocapture
```
Expected: 4 tests `... ok` (messages / paths / descriptors / trait-implementable).

- [ ] **Step 6: Run the crate's whole suite (unit + integration) clean.**

```
cargo test -p leaf-grpc-build
```
Expected: all `service_gen::tests` + `generated_service` tests `... ok`; `0 failed`.

- [ ] **Step 7: Commit.**

```
git add crates/leaf-grpc-build/proto/echo.proto crates/leaf-grpc-build/tests/echo.proto crates/leaf-grpc-build/build.rs crates/leaf-grpc-build/tests/generated_service.rs crates/leaf-grpc-build/Cargo.toml
git commit -m "leaf-grpc-build: end-to-end proof — echo.proto -> leaf trait + paths + descriptors (4 shapes)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.9: Force-clean gate for the stage (tests + clippy + doc)

**Files:** none (verification only)

The project's verification rule: cached `cargo` runs re-emit no warnings, so force-clean before claiming clean. rustc skips macro-gen naming lints but rust-analyzer does not — the generator emits a `pub mod`/`pub trait` whose idents come from the proto, so confirm clippy is clean.

- [ ] **Step 1: Force-clean + test the new crate and the include sugar.**

```
cargo clean -p leaf-grpc-build -p leaf-grpc && cargo test -p leaf-grpc-build -p leaf-grpc
```
Expected: all tests pass; `0 failed`.

- [ ] **Step 2: Clippy the new crate under `-D warnings`.**

```
cargo clippy -p leaf-grpc-build -p leaf-grpc --all-targets -- -D warnings
```
Expected: `Finished` with no warnings.

- [ ] **Step 3: Doc-build the new crate (no broken intra-doc links).**

```
cargo doc -p leaf-grpc-build --no-deps
```
Expected: `Finished`, `Generated …/leaf_grpc_build/index.html`, zero warnings.

- [ ] **Step 4: Confirm the existing HTTP suite is untouched (Stage 3 added a crate only; no leaf-web/leaf-core change).**

```
cargo test -p leaf-web -p leaf-web-hyper
```
Expected: the existing HTTP suite stays green (the ~1647-test budget unchanged by this stage).

- [ ] **Step 5: Final commit (only if any lint/doc fix was needed; otherwise skip).**

```
git commit -am "leaf-grpc-build: force-clean gate green (test + clippy -D warnings + doc)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 4: `#[grpc_controller]` stereotype

Add `#[grpc_controller]` in `leaf-macros` + its lowering in `leaf-codegen` (`grpc_controller.rs`). Dual-form, EXACTLY mirroring `#[rest_controller]`:

- on a **struct**: the controller BEAN — structurally a `#[component]` (field injection of collaborators) that ALSO emits a `::leaf_grpc::GrpcControllerKind` marker (the dual-form consistency guard, the gRPC twin of `::leaf_web::ControllerKind`).
- on an inherent **impl block** `#[grpc_controller] impl pkg::Catalog for CatalogController { async fn get(&self, req: ProductReq) -> Result<Product, Status> { .. } }`: the per-method ITERATOR — each RPC method is desugared (native async, in-macro, NO `#[async_impl]`) and lowered to ONE `#[doc(hidden)]` `GrpcRoute` bean that PROVIDES `dyn ::leaf_grpc::GrpcRoute`, field-injects `controller: Ref<Controller>` + `codec: Ref<dyn ::leaf_grpc::GrpcCodec>`, and wraps the typed method with framing/codec selected by the **call shape** read from the Stage-3 descriptor (a trait associated-const seam — NO type-name detection).

**HARD CONSTRAINTS baked into this stage:** the macro reads the per-method `path()` + call shape from the Stage-3 generated server trait's associated consts (`<Trait>::__leaf_grpc_path(<method-name>)` / `__leaf_grpc_shape(<method-name>)`) — it NEVER decides shape from the textual type of `req`/the return. The four shape-dispatch wrappers (`call_unary`/`call_server_stream`/`call_client_stream`/`call_bidi`) are Stage-2 leaf-grpc library fns; the codegen only references them. Dep arrow stays `leaf-grpc → leaf-web → leaf-core`; codegen emits absolute `::leaf_grpc::` paths. `#[grpc_controller]` is exported in the umbrella/prelude behind the `grpc` feature. Reuse `descriptor::emit` (the exact currency `#[rest_controller]`/`#[web_filter]` use). Keep the ~1647-test HTTP suite green (no changes to `web_controller.rs`).

### Files

- **Create** `crates/leaf-codegen/src/grpc_controller.rs` — `expand_grpc_controller_impl`, `emit_grpc_route_bean`, `emit_grpc_controller_kind`, the `grpc_controller_kind_guard`, the call-shape dispatch lowering.
- **Modify** `crates/leaf-codegen/src/lib.rs` — `pub mod grpc_controller;`.
- **Modify** `crates/leaf-macros/src/lib.rs` — the dual-form `#[grpc_controller]` proc-macro (struct = bean+`GrpcControllerKind`; impl = strip nothing, lower the trait impl).
- **Modify** `crates/leaf/src/lib.rs` + `crates/leaf/src/prelude.rs` — export `grpc_controller` behind `#[cfg(feature = "grpc")]`.

---

### Task 4.1: the call-shape model + the Stage-3 descriptor seam

The macro must learn each method's gRPC path + call shape WITHOUT inspecting the textual type of the parameter/return (no type-name detection). The Stage-3 service-trait generator (`leaf-grpc-build`) emits, per service trait `T`, two `#[doc(hidden)]` associated consts that map a method NAME (a string the macro already has from `sig.ident`) to its path + shape. `grpc_controller.rs` owns the `CallShape` enum + the token for each wrapper.

**Files:** `crates/leaf-codegen/src/grpc_controller.rs`, `crates/leaf-codegen/src/lib.rs`

- [ ] **Step 1: register the module (failing build).** Add to `crates/leaf-codegen/src/lib.rs` next to `pub mod web_controller;`:

```rust
pub mod grpc_controller;
```

- [ ] **Step 2: run it — fails (no file yet).**

```
cargo build -p leaf-codegen
```
-> `error[E0583]: file not found for module grpc_controller`.

- [ ] **Step 3: write the module skeleton + the `CallShape` model.** Create `crates/leaf-codegen/src/grpc_controller.rs`:

```rust
//! The `#[grpc_controller]` controller-impl ITERATOR (Stage 4): lower each RPC method of
//! a `#[grpc_controller] impl ServiceTrait for Bean` block into ONE generated `GrpcRoute`
//! bean — the SECOND `Handler` family, collected by DI exactly like the HTTP `#[rest_controller]`
//! per-method `Route` beans.
//!
//! ## What it emits, per RPC method
//!
//! `#[grpc_controller] impl catalog::Catalog for CatalogController {
//!     async fn get(&self, req: ProductReq) -> Result<Product, Status> { .. } }`
//! lowers `get` to:
//!
//! - a `#[doc(hidden)]` generated `GrpcRoute` STRUCT (`__LeafGrpcRoute_CatalogController_get`)
//!   holding the DI'd `controller: Ref<CatalogController>` (field injection) + the injected
//!   `codec: Ref<dyn ::leaf_grpc::GrpcCodec>` (prost),
//! - its `impl ::leaf_grpc::GrpcRoute` (`path()` = the `/pkg.Service/Method` constant read
//!   from the Stage-3 trait seam; `handler()` = `self`),
//! - its `impl ::leaf_grpc::GrpcHandler` whose `call` wraps the typed method with
//!   framing/codec via the CALL-SHAPE wrapper (`call_unary`/`call_server_stream`/
//!   `call_client_stream`/`call_bidi`) — the shape read from the Stage-3 trait seam, NEVER
//!   from the textual type of `req`/the return (the no-type-names rule),
//! - the `#[component]`-equivalent bean registration (one const `::leaf_core::Descriptor`
//!   into `COMPONENTS`, via the SAME [`crate::descriptor::emit`] currency the stereotypes
//!   use) that `provides` the `dyn ::leaf_grpc::GrpcRoute` view, so `GrpcDispatch`'s
//!   `Vec<Ref<dyn GrpcRoute>>` collection injection finds it.
//!
//! The controller bean itself stays a plain `#[grpc_controller]` struct (the struct macro
//! registered it + its `GrpcControllerKind` marker); this iterator only contributes the
//! per-method `GrpcRoute` beans. Async methods are desugared NATIVELY here (no separate
//! `#[async_impl]`) and the original RPC impl block is RE-EMITTED unchanged by the macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ImplItemFn, ItemImpl, ItemStruct, Type};

use crate::descriptor::{self, BeanInput, Dependency, EmitError, FieldShape, Scope, ServiceView, Slice};
use crate::stereotype::{self, Stereotype};

/// The four gRPC call shapes (§5). The shape selects WHICH framing/codec wrapper the
/// generated `GrpcHandler::call` invokes around the typed user method — read from the
/// Stage-3 trait seam, NEVER inferred from the textual type of `req`/the return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>`.
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>`.
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>`.
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>`.
    Bidi,
}
```

- [ ] **Step 4: run it — module compiles.**

```
cargo build -p leaf-codegen
```
-> builds clean (the `use`s for symbols not yet used will warn; that is fine until the impl lands — or add `#[allow(unused_imports)]` temporarily; we wire them in 4.2).

---

### Task 4.2: lower ONE unary RPC method to a `GrpcRoute` bean

The headline lowering. The generated `GrpcHandler::call` reads the controller via `&self.controller`, the codec via `&*self.codec`, the path + shape from the Stage-3 trait seam, and wraps the typed method through `::leaf_grpc::call_unary`. The path is read by NAME (`__leaf_grpc_path("get")`), never spelled.

**Files:** `crates/leaf-codegen/src/grpc_controller.rs`

- [ ] **Step 1: write the failing unary test.** Add at the bottom of `grpc_controller.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn impl_item(src: &str) -> ItemImpl {
        syn::parse_str(src).expect("a valid impl block")
    }

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn a_unary_rpc_method_emits_a_grpc_route_bean() {
        // The headline Stage-4 lowering: a unary `async fn get(&self, req: ProductReq)
        // -> Result<Product, Status>` on a `#[grpc_controller] impl catalog::Catalog`
        // lowers to a generated `GrpcRoute` bean that
        //   (a) provides the `dyn ::leaf_grpc::GrpcRoute` view (so GrpcDispatch collects it),
        //   (b) reports `path()` read from the Stage-3 trait seam BY METHOD NAME,
        //   (c) field-injects the controller Ref + the codec Ref,
        //   (d) wraps the typed method with the UNARY framing/codec wrapper, the shape read
        //       from the trait seam — never inferred from the type of `req`/the return.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        // The whole emitted artifact must PARSE as a Rust item sequence.
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);

        // (a) the generated bean PROVIDES the `dyn ::leaf_grpc::GrpcRoute` view.
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_grpc::GrpcRoute>()"),
            "the GrpcRoute bean must declare the `dyn ::leaf_grpc::GrpcRoute` provides[] view: {s}"
        );
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the GrpcRoute bean is a COMPONENTS row: {s}"
        );

        // (b) `path()` is read from the Stage-3 trait seam BY METHOD NAME (no spelled literal).
        assert!(
            s.contains("<catalog::Catalogas::leaf_grpc::GrpcService>::__leaf_grpc_path(\"get\")"),
            "path() reads the Stage-3 trait seam by method name: {s}"
        );

        // (c) the controller Ref + the codec Ref are field-injected.
        assert!(
            s.contains("controller:::leaf_core::Ref<CatalogController>"),
            "the controller is field-injected as Ref<Controller>: {s}"
        );
        assert!(
            s.contains("codec:::leaf_core::Ref<dyn::leaf_grpc::GrpcCodec>"),
            "the prost codec is field-injected as Ref<dyn GrpcCodec>: {s}"
        );

        // (d) the typed method is wrapped through the UNARY framing/codec wrapper.
        assert!(
            s.contains("::leaf_grpc::call_unary("),
            "a unary method wraps through ::leaf_grpc::call_unary: {s}"
        );
        // The controller method is invoked inside the wrapper (by NAME, on the injected Ref).
        assert!(s.contains(".get(") && s.contains(".await"), "invokes the controller method: {s}");
    }
}
```

- [ ] **Step 2: run it — fails (no fn).**

```
cargo test -p leaf-codegen grpc_controller::tests::a_unary_rpc_method -- --nocapture
```
-> `error[E0425]: cannot find function expand_grpc_controller_impl`.

- [ ] **Step 3: write `expand_grpc_controller_impl` + `emit_grpc_route_bean` (unary only).** Add above the `#[cfg(test)]` block:

```rust
/// Lower a `#[grpc_controller] impl ServiceTrait for Bean` block to its per-RPC `GrpcRoute`
/// beans (one const `Descriptor` + the generated `GrpcRoute`/`GrpcHandler` impls per RPC
/// method, through the SAME [`descriptor::emit`] currency the stereotypes use). The macro
/// re-emits the original impl block (with async desugared); this function emits the sibling
/// `GrpcRoute` registration rows a method-position attr alone cannot.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic, NOT a trait impl (a
/// `#[grpc_controller]` impl implements the Stage-3 service trait), or an RPC method takes
/// no `self` receiver.
pub fn expand_grpc_controller_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    let service_trait = service_trait_of(item)?;
    let self_ty = (*item.self_ty).clone();
    let controller_ident = type_ident(&self_ty);
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}` is a generic `#[grpc_controller]` impl: a generic \
                 controller has no single concrete type to mint its per-method `GrpcRoute` \
                 beans. Make the controller concrete."
            ),
        });
    }

    let mut rows = TokenStream::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        rows.extend(emit_grpc_route_bean(&self_ty, &service_trait, &controller_ident, func)?);
    }
    // The dual-form consistency guard: assert the controller STRUCT carries the
    // `GrpcControllerKind` marker the struct stereotype emits (so a `#[grpc_controller] impl`
    // on a struct never annotated `#[grpc_controller]` fails loudly). Keyed on the trait, not
    // a spelled type name.
    rows.extend(grpc_controller_kind_guard(&self_ty));
    Ok(rows)
}

/// Emit ONE generated `GrpcRoute` bean for an RPC method: the `#[doc(hidden)]` route struct
/// (holding the DI'd controller + the codec), its `GrpcRoute` + `GrpcHandler` trait impls,
/// and the `#[component]`-equivalent const registration that `provides` the
/// `dyn ::leaf_grpc::GrpcRoute` view.
fn emit_grpc_route_bean(
    self_ty: &Type,
    service_trait: &syn::Path,
    controller_ident: &str,
    method: &ImplItemFn,
) -> Result<TokenStream, EmitError> {
    let method_ident = &method.sig.ident;
    let method_name = method_ident.to_string();

    if !has_self_receiver(method) {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}::{method_name}` is a `#[grpc_controller]` RPC method but \
                 takes no `self` receiver: a handler method threads the controller bean \
                 through `&self`."
            ),
        });
    }

    // The generated route struct: `__LeafGrpcRoute_<Controller>_<method>`. Unique per
    // (controller, method) so two RPCs in one module never collide.
    let route_struct_ident = format_ident!("__LeafGrpcRoute_{controller_ident}_{method_name}");
    let route_struct_ty: Type = parse_type(&route_struct_ident.to_string())?;

    // The `/pkg.Service/Method` path: read from the Stage-3 generated service trait by method
    // NAME (a const seam), NEVER a spelled literal — so the macro carries no proto knowledge
    // and an aliased message type is irrelevant.
    let path_expr = quote! {
        <#service_trait as ::leaf_grpc::GrpcService>::__leaf_grpc_path(#method_name)
    };
    // The CALL-SHAPE wrapper token: which framing/codec adapter `call` wraps the typed method
    // with (unary/server/client/bidi). Read from the Stage-3 trait seam — never inferred from
    // the textual type of `req`/the return.
    let dispatch = shape_dispatch(service_trait, &method_name, self_ty, method_ident);

    // The struct's injected fields (field injection through `Injectable`): the controller bean
    // + the prost `GrpcCodec`. `&*self.controller` / `&*self.codec` deref the `Ref<…>` to the
    // value/trait object inside the wrapper.
    let deps = vec![
        Dependency {
            name: "controller".into(),
            ty: parse_type(&format!("::leaf_core::Ref<{}>", quote!(#self_ty)))?,
        },
        Dependency {
            name: "codec".into(),
            ty: parse_type("::leaf_core::Ref<dyn ::leaf_grpc::GrpcCodec>")?,
        },
    ];

    let items = quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        pub struct #route_struct_ident {
            controller: ::leaf_core::Ref<#self_ty>,
            codec: ::leaf_core::Ref<dyn ::leaf_grpc::GrpcCodec>,
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_grpc::GrpcRoute for #route_struct_ident {
            fn path(&self) -> &str {
                #path_expr
            }
            fn handler(&self) -> &dyn ::leaf_grpc::GrpcHandler {
                self
            }
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_grpc::GrpcHandler for #route_struct_ident {
            fn call<'__a>(
                &'__a self,
                __req: ::leaf_web::Request,
            ) -> ::leaf_core::BoxFuture<'__a, ::leaf_web::Response> {
                ::std::boxed::Box::pin(async move {
                    let __controller = &*self.controller;
                    let __codec: &dyn ::leaf_grpc::GrpcCodec = &*self.codec;
                    #dispatch
                })
            }
        }
    };

    // The `#[component]`-equivalent bean registration providing the `dyn ::leaf_grpc::GrpcRoute`
    // view, FIELD-injected through `Injectable` (the SAME descriptor currency + field-default
    // construction the stereotypes use — NOT a hand-written Provider).
    let meta = crate::annotation::resolve(&Stereotype::Component.annotation())
        .map_err(|e| EmitError { message: e.to_string() })?;
    let mut input =
        BeanInput::new(route_struct_ty, route_struct_ident.to_string(), route_struct_ident.to_string());
    input.module_qualified = true;
    input.scope = Scope::Singleton;
    input.meta = meta;
    input.slice = Slice::Components;
    input.deps = deps;
    input.inject_via_trait = true;
    input.field_shape = FieldShape::Named;
    input.provides = vec![ServiceView { dyn_ty: parse_type("dyn ::leaf_grpc::GrpcRoute")? }];
    let registration = descriptor::emit(&input)?;

    Ok(quote! { #items #registration })
}

/// The CALL-SHAPE dispatch expression: wrap the typed user method with the framing/codec
/// adapter selected by the method's gRPC shape (unary/server/client/bidi). Stage 4 lowers
/// UNARY here; 4.3 fills the streaming arms. The shape is read from the Stage-3 trait seam
/// at MACRO time (`__leaf_grpc_shape` is a `const fn` the macro evaluates), never from the
/// textual type of the parameter/return.
fn shape_dispatch(
    service_trait: &syn::Path,
    method_name: &str,
    _self_ty: &Type,
    method_ident: &syn::Ident,
) -> TokenStream {
    // The typed invocation: `__controller.get(__decoded).await` — referenced by method name,
    // on the injected controller Ref. The wrapper supplies `__decoded` (the decoded T) and
    // consumes the method's returned future.
    let invoke = quote! { __controller.#method_ident(__msg).await };
    // UNARY: the `call_unary` wrapper de-frames + decodes the single request `T`, calls the
    // typed method, encodes the single `U`, frames it + the grpc-status trailers. Selected by
    // the trait-seam shape (asserted equal to Unary at macro time below), NOT a type check.
    let _ = (service_trait, method_name);
    quote! {
        ::leaf_grpc::call_unary(__req, __codec, |__msg| async move { #invoke }).await
    }
}

/// `true` iff the method takes a `self`/`&self`/`&mut self` receiver.
fn has_self_receiver(func: &ImplItemFn) -> bool {
    func.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)))
}

/// The service-trait PATH a `#[grpc_controller] impl Trait for Bean` block implements.
///
/// # Errors
/// [`EmitError`] for an inherent impl — `#[grpc_controller]` lowers a `impl ServiceTrait for
/// Controller { .. }` block (the Stage-3 generated server trait the controller satisfies).
fn service_trait_of(item: &ItemImpl) -> Result<syn::Path, EmitError> {
    match &item.trait_ {
        Some((_, path, _)) => Ok(path.clone()),
        None => Err(EmitError {
            message: "`#[grpc_controller]` applies to a `impl ServiceTrait for Controller { .. }` \
                      trait impl (the Stage-3 generated gRPC server trait), not an inherent impl."
                .into(),
        }),
    }
}

/// The leading-ident name of the `Self` type (`CatalogController` / `Repo<u32>` →
/// `CatalogController`/`Repo`), the per-method route-struct identity base + diagnostics.
fn type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "Self".into()),
        _ => "Self".into(),
    }
}

/// Parse a type-expression string into a [`syn::Type`] (the generated field/view types are
/// built from leaf-absolute path strings).
fn parse_type(s: &str) -> Result<Type, EmitError> {
    syn::parse_str(s).map_err(|e| EmitError {
        message: format!("internal: could not parse generated type `{s}`: {e}"),
    })
}
```

  Also add the `grpc_controller_kind_guard` stub (filled in 4.4) so this compiles — add it now:

```rust
/// The dual-form consistency guard: filled in Task 4.4.
fn grpc_controller_kind_guard(_self_ty: &Type) -> TokenStream {
    TokenStream::new()
}
```

- [ ] **Step 4: run it — passes.**

```
cargo test -p leaf-codegen grpc_controller::tests::a_unary_rpc_method -- --nocapture
```
-> PASS.

- [ ] **Step 5: commit.**

```
git add crates/leaf-codegen/src/grpc_controller.rs crates/leaf-codegen/src/lib.rs
git commit -m "leaf-codegen: #[grpc_controller] unary RPC lowering to a GrpcRoute bean

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.3: the three streaming call shapes (server / client / bidi)

The shape decides which of the four leaf-grpc wrappers `call` invokes — selected at macro time from the Stage-3 trait seam via a `const`-evaluable `__leaf_grpc_shape(name)`, NOT from the textual type of `req`/the return (the no-type-names rule). Each wrapper has the same `(req, codec, body_closure)` shape; the closure's `__msg` is `T` for unary/server-stream and `Streaming<T>` for client-stream/bidi (the wrapper supplies the right thing).

**Files:** `crates/leaf-codegen/src/grpc_controller.rs`

- [ ] **Step 1: write the three failing shape tests.** Add to the `tests` module:

```rust
    #[test]
    fn a_server_stream_rpc_wraps_through_call_server_stream() {
        // Server-stream: `async fn list(&self, req: ListReq) -> Result<Streaming<Product>, Status>`.
        // The shape is read from the Stage-3 trait seam (a const the macro evaluates), so the
        // codegen wraps through `call_server_stream` — never inferred from the `Streaming<U>`
        // RETURN type (no type-name detection).
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn list(&self, req: ListReq) -> Result<Streaming<Product>, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_server_stream("),
            "a server-stream method wraps through call_server_stream: {s}"
        );
        assert!(s.contains(".list(") && s.contains(".await"), "invokes the controller method: {s}");
    }

    #[test]
    fn a_client_stream_rpc_wraps_through_call_client_stream() {
        // Client-stream: `async fn upload(&self, reqs: Streaming<Chunk>) -> Result<Summary, Status>`.
        // The wrapper de-frames the inbound stream into a `Streaming<T>` and hands it to the
        // method; the shape is read from the trait seam, never from the `Streaming<T>` PARAM type.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn upload(&self, reqs: Streaming<Chunk>) -> Result<Summary, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_client_stream("),
            "a client-stream method wraps through call_client_stream: {s}"
        );
    }

    #[test]
    fn a_bidi_rpc_wraps_through_call_bidi() {
        // Bidi: `async fn chat(&self, reqs: Streaming<Msg>) -> Result<Streaming<Msg>, Status>`.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn chat(&self, reqs: Streaming<Msg>) -> Result<Streaming<Msg>, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_bidi("),
            "a bidi method wraps through call_bidi: {s}"
        );
    }
```

- [ ] **Step 2: run them — fail.**

```
cargo test -p leaf-codegen grpc_controller::tests::a_server_stream grpc_controller::tests::a_client_stream grpc_controller::tests::a_bidi -- --nocapture
```
-> the three new tests fail (all emit `call_unary`).

- [ ] **Step 3: make `shape_dispatch` shape-aware via the trait seam.** Replace the whole `shape_dispatch` fn body. The macro reads the shape at macro time but the macro never *parses the type* — instead it emits a `const`-evaluated `match` over the trait seam so the COMPILER picks the wrapper, with all four arms present (so there is literally no type inspection in the macro). Replace:

```rust
fn shape_dispatch(
    service_trait: &syn::Path,
    method_name: &str,
    _self_ty: &Type,
    method_ident: &syn::Ident,
) -> TokenStream {
    // The typed invocation, referenced by method name on the injected controller Ref. The
    // wrapper supplies `__msg` (T for unary/server-stream, Streaming<T> for client-stream/bidi)
    // and consumes the method's returned future. ONE invocation form serves all four shapes —
    // the wrapper differs, not the call.
    let invoke = quote! { __controller.#method_ident(__msg).await };
    // The shape is read from the Stage-3 trait seam (a `const fn` the macro names, never a type
    // check): the emitted `match` lets the COMPILER pick the wrapper from the const shape, so
    // the macro inspects NO type. All four arms are present; the const-folded `match` keeps one.
    let shape = quote! {
        <#service_trait as ::leaf_grpc::GrpcService>::__leaf_grpc_shape(#method_name)
    };
    quote! {
        match #shape {
            ::leaf_grpc::CallShape::Unary => {
                ::leaf_grpc::call_unary(__req, __codec, |__msg| async move { #invoke }).await
            }
            ::leaf_grpc::CallShape::ServerStream => {
                ::leaf_grpc::call_server_stream(__req, __codec, |__msg| async move { #invoke }).await
            }
            ::leaf_grpc::CallShape::ClientStream => {
                ::leaf_grpc::call_client_stream(__req, __codec, |__msg| async move { #invoke }).await
            }
            ::leaf_grpc::CallShape::Bidi => {
                ::leaf_grpc::call_bidi(__req, __codec, |__msg| async move { #invoke }).await
            }
        }
    }
}
```

  > NOTE for Stage 2: `call_unary`/`call_server_stream`/`call_client_stream`/`call_bidi` are leaf-grpc library fns each accepting `(Request, &dyn GrpcCodec, closure) -> impl Future<Output = Response>`; `closure` receives the decoded `T`/`Streaming<T>` and returns the method's `Result<U/Streaming<U>, Status>` future. `::leaf_grpc::CallShape` is the runtime mirror of this module's `CallShape` and `__leaf_grpc_shape` is a `const fn` on the generated `GrpcService` trait. The four-arm `match` is monomorphized per generated method so each wrapper sees the concrete `T`/`U`; the const shape collapses it to one arm.

- [ ] **Step 4: run the four shape tests + the unary one — all pass.**

```
cargo test -p leaf-codegen grpc_controller:: -- --nocapture
```
-> the unary, server-stream, client-stream, and bidi tests PASS.

- [ ] **Step 5: commit.**

```
git add crates/leaf-codegen/src/grpc_controller.rs
git commit -m "leaf-codegen: #[grpc_controller] all four call-shape wrappers via the trait seam

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.4: the struct form — the controller BEAN + the `GrpcControllerKind` marker + the dual-form guard

The struct form is structurally a `#[component]` (field injection) that ALSO emits `impl ::leaf_grpc::GrpcControllerKind for Bean {}` — the gRPC twin of `::leaf_web::ControllerKind`. The impl form appends a `const _: ()` guard asserting the struct carries that marker, so a `#[grpc_controller] impl` on a struct never annotated `#[grpc_controller]` is a hard compile error (the same shape `controller_kind_guard` uses for HTTP).

**Files:** `crates/leaf-codegen/src/grpc_controller.rs`

- [ ] **Step 1: write the failing struct-form + guard tests.** Add to the `tests` module:

```rust
    fn struct_item(src: &str) -> ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    #[test]
    fn a_grpc_controller_struct_is_a_component_and_emits_the_kind_marker() {
        // The struct form: `#[grpc_controller] struct CatalogController { .. }` is a
        // `#[component]`-equivalent bean (field injection of collaborators) that ALSO emits
        // the `GrpcControllerKind` marker the impl-form guard asserts.
        let rows = stereotype::emit_struct(
            &struct_item("struct CatalogController { repo: ::leaf_core::Ref<Repo> }"),
            Stereotype::Component,
            TokenStream::new(),
        )
        .expect("emits");
        let kind = emit_grpc_controller_kind(&struct_item("struct CatalogController;"));
        let ts = quote! { #rows #kind };
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the controller bean is a COMPONENTS row: {s}"
        );
        // The collaborator is field-injected through the Injectable trait (no hand-rolled ctor).
        assert!(
            s.contains("<::leaf_core::Ref<Repo>as::leaf_core::Injectable>::inject(__cx).await?"),
            "the controller's collaborator is field-injected: {s}"
        );
        // The dual-form marker is emitted.
        assert!(
            s.contains("impl::leaf_grpc::GrpcControllerKindforCatalogController"),
            "the struct emits the GrpcControllerKind marker: {s}"
        );
    }

    #[test]
    fn a_grpc_controller_impl_emits_the_kind_mismatch_guard() {
        // Every RPC impl appends a compile-time guard asserting the controller struct carries
        // the `GrpcControllerKind` marker — so a `#[grpc_controller] impl` on a struct never
        // annotated `#[grpc_controller]` (which lacks the marker) is a hard compile error.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let s = flat(&expand_grpc_controller_impl(&item).expect("emits"));
        assert!(
            s.contains("<CatalogControlleras::leaf_grpc::GrpcControllerKind>::IS_GRPC_CONTROLLER"),
            "the impl asserts the struct carries the GrpcControllerKind marker: {s}"
        );
    }
```

- [ ] **Step 2: run them — fail.**

```
cargo test -p leaf-codegen grpc_controller::tests::a_grpc_controller_struct grpc_controller::tests::a_grpc_controller_impl_emits -- --nocapture
```
-> `cannot find function emit_grpc_controller_kind`, and the guard test fails (the stub is empty).

- [ ] **Step 3: implement `emit_grpc_controller_kind` + the real guard.** Add the emitter (next to `emit_grpc_route_bean`) and REPLACE the `grpc_controller_kind_guard` stub:

```rust
/// Emit the controller STRUCT's `::leaf_grpc::GrpcControllerKind` marker — the gRPC twin of
/// `::leaf_web::ControllerKind`. Emitted ALONGSIDE the stereotype rows on the controller
/// struct so the matching RPC impl can assert the struct really is a `#[grpc_controller]`
/// (see [`grpc_controller_kind_guard`]).
pub fn emit_grpc_controller_kind(item: &ItemStruct) -> TokenStream {
    let ident = &item.ident;
    let (impl_g, ty_g, where_c) = item.generics.split_for_impl();
    quote! {
        #[automatically_derived]
        #[doc(hidden)]
        impl #impl_g ::leaf_grpc::GrpcControllerKind for #ident #ty_g #where_c {
            const IS_GRPC_CONTROLLER: bool = true;
        }
    }
}

/// The compile-time guard an RPC impl emits: assert the controller struct carries the
/// `GrpcControllerKind` marker (the struct stereotype emits it), turning a `#[grpc_controller]
/// impl` on a non-`#[grpc_controller]` struct into a clear `compile_error`. Keyed on the
/// trait/const — never a spelled type name. See [`emit_grpc_controller_kind`].
fn grpc_controller_kind_guard(self_ty: &Type) -> TokenStream {
    quote! {
        const _: () = {
            ::core::assert!(
                <#self_ty as ::leaf_grpc::GrpcControllerKind>::IS_GRPC_CONTROLLER,
                "gRPC controller stereotype mismatch: this `impl` block lowers RPC methods but \
                 its controller struct is not a `#[grpc_controller]`. Put `#[grpc_controller]` \
                 on BOTH the struct AND the `impl ServiceTrait for Controller` block."
            );
        };
    }
}
```

  > NOTE for Stage 2: `::leaf_grpc::GrpcControllerKind { const IS_GRPC_CONTROLLER: bool; }` is a `#[doc(hidden)]` marker trait in leaf-grpc (the gRPC twin of `leaf_web::ControllerKind`).

- [ ] **Step 4: run them — pass (and the whole module stays green).**

```
cargo test -p leaf-codegen grpc_controller:: -- --nocapture
```
-> all `grpc_controller::` tests PASS.

- [ ] **Step 5: commit.**

```
git add crates/leaf-codegen/src/grpc_controller.rs
git commit -m "leaf-codegen: #[grpc_controller] struct kind marker + dual-form guard

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.5: the error paths (generic impl, inherent impl, no-self method)

Mirror the HTTP controller's hard-error surface so misuse is a clear `compile_error!`.

**Files:** `crates/leaf-codegen/src/grpc_controller.rs`

- [ ] **Step 1: write the three failing error tests.** Add to the `tests` module:

```rust
    #[test]
    fn a_generic_grpc_controller_impl_is_a_hard_error() {
        let item = impl_item(
            r#"impl<T> catalog::Catalog for CatalogController<T> {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("a generic grpc-controller impl hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    #[test]
    fn an_inherent_grpc_controller_impl_is_a_hard_error() {
        // `#[grpc_controller]` lowers a `impl ServiceTrait for Controller` trait impl (the
        // Stage-3 generated server trait). An inherent `impl Controller { .. }` has no trait
        // to read the path/shape seam from, so it is a loud error.
        let item = impl_item(
            r#"impl CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("an inherent grpc-controller impl hard-errors");
        assert!(err.message.contains("trait impl"), "got: {}", err.message);
    }

    #[test]
    fn an_rpc_method_without_a_self_receiver_is_an_error() {
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("an RPC method needs a self receiver");
        assert!(err.message.contains("self"), "got: {}", err.message);
    }
```

- [ ] **Step 2: run them — pass already** (the guards are in `expand_grpc_controller_impl` / `service_trait_of` / `emit_grpc_route_bean` from 4.2). Confirm:

```
cargo test -p leaf-codegen grpc_controller:: -- --nocapture
```
-> all PASS. (If `a_generic_...` fails because the generic check runs after `service_trait_of`, no fix needed — both are errors; the assertion only checks the message substring "generic", which the generic arm provides.)

- [ ] **Step 3: commit.**

```
git add crates/leaf-codegen/src/grpc_controller.rs
git commit -m "leaf-codegen: #[grpc_controller] hard-error surface (generic/inherent/no-self)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.6: the thin `#[grpc_controller]` proc-macro (dual-form dispatch)

Add the dual-form macro to `leaf-macros`, mirroring `expand_controller` EXACTLY: a struct = the `#[component]` bean + the `GrpcControllerKind` marker; an `impl` = re-emit the trait impl with **async desugared** (native, via the existing `leaf_codegen::async_impl::expand`) PLUS the lowered `GrpcRoute` rows. No method-position attrs to strip (the RPC methods are plain `async fn`s).

**Files:** `crates/leaf-macros/src/lib.rs`

- [ ] **Step 1: add the macro after `rest_controller` / `expand_controller`.** Insert:

```rust
/// `#[grpc_controller]` — the gRPC controller-family stereotype (the inbound-RPC handler
/// family, alongside `#[controller]`/`#[rest_controller]`). TWO forms, EXACTLY like
/// `#[rest_controller]`:
///
/// - on a STRUCT: the controller BEAN — structurally a `#[component]` (so the controller is
///   registered + resolvable, its collaborators field-injected) that ALSO emits the
///   `::leaf_grpc::GrpcControllerKind` marker (the dual-form consistency anchor).
/// - on an inherent IMPL of the Stage-3 server trait (`#[grpc_controller] impl
///   catalog::Catalog for CatalogController { async fn get(&self, req: ProductReq) ->
///   Result<Product, Status> {..} }`): the per-RPC ITERATOR. The macro DESUGARS each native
///   `async fn` (no separate `#[async_impl]`) and re-emits the trait impl, PLUS lowers each
///   RPC method to a `#[doc(hidden)]` `GrpcRoute` bean (the second `Handler` family) that
///   provides `dyn ::leaf_grpc::GrpcRoute`, field-injects `Ref<Controller>` + the codec, and
///   wraps the typed method with framing/codec for its call shape (read from the Stage-3
///   trait seam — NO type-name detection).
#[proc_macro_attribute]
pub fn grpc_controller(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as Item);
    match parsed {
        Item::Impl(item_impl) => {
            // Re-emit the trait impl with async desugared (native async, in-macro — the SAME
            // BoxFuture desugar `#[async_impl]` performs), then append the lowered GrpcRoute
            // rows. The RPC methods are plain `async fn`s, so there is nothing to STRIP.
            let desugared = leaf_codegen::async_impl::expand(&item_impl);
            match leaf_codegen::grpc_controller::expand_grpc_controller_impl(&item_impl) {
                Ok(rows) => quote! { #desugared #rows }.into(),
                Err(err) => {
                    let error = compile_error(&err);
                    quote! { #desugared #error }.into()
                }
            }
        }
        Item::Struct(item_struct) => {
            match stereotype::emit_struct(&item_struct, Stereotype::Component, attr.into()) {
                Ok(rows) => {
                    let kind = leaf_codegen::grpc_controller::emit_grpc_controller_kind(&item_struct);
                    quote! { #item_struct #rows #kind }.into()
                }
                Err(err) => {
                    let error = compile_error(&err);
                    quote! { #item_struct #error }.into()
                }
            }
        }
        other => quote! {
            #other
            ::core::compile_error!(
                "#[grpc_controller] applies to a `struct` (the controller bean) or an \
                 `impl ServiceTrait for Controller` block (its RPC methods)"
            );
        }
        .into(),
    }
}
```

  > The struct form uses `Stereotype::Component` (a gRPC controller carries no @ResponseBody axis — the marker is `GrpcControllerKind`, not a new component-marker variant), so no `stereotype.rs` change is needed and the no-type-name-detection rule holds (the marker is emitted, not inferred).

- [ ] **Step 2: build — fails until leaf-grpc exists, but the macro CRATE itself compiles** (it only references `leaf_codegen`, which is already green).

```
cargo build -p leaf-macros
```
-> builds clean (the emitted `::leaf_grpc::` paths are checked at the USER crate, not here).

- [ ] **Step 3: add a token test in leaf-macros' integration-free unit surface** — leaf-macros is a proc-macro crate (no in-crate unit tests of expansion), so the EXPANSION is covered by the `leaf-codegen` tests (4.2–4.5). Verify the dispatch wiring compiles + the whole codegen suite stays green:

```
cargo test -p leaf-codegen grpc_controller:: -- --nocapture && cargo build -p leaf-macros
```
-> PASS + clean build.

- [ ] **Step 4: commit.**

```
git add crates/leaf-macros/src/lib.rs
git commit -m "leaf-macros: thin dual-form #[grpc_controller] (struct bean + RPC iterator)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.7: export `#[grpc_controller]` in the umbrella + prelude behind the `grpc` feature

Mirror the `web`-feature export of `rest_controller`/`web_filter`. The umbrella's `grpc` feature (Stage 6 / the starter) pulls `leaf-grpc` + re-exports its surface; this stage adds the MACRO export so `use leaf::prelude::*;` brings `grpc_controller` into scope behind the feature.

**Files:** `crates/leaf/src/prelude.rs`, `crates/leaf/src/lib.rs`

- [ ] **Step 1: add the prelude export.** In `crates/leaf/src/prelude.rs`, after the `#[cfg(feature = "web")]` macro block, add:

```rust
// The gRPC controller-family stereotype (Stage 4), brought into scope flat behind the
// `grpc` capability feature — the gRPC twin of the `web`-gated `rest_controller`/`web_filter`
// exports. The macro emits ABSOLUTE `::leaf_grpc::` paths that resolve through the umbrella's
// `grpc`-gated re-exports + the `extern crate leaf as leaf_grpc;` facade alias. Present iff
// the `grpc` feature pulled the bundle in.
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_macros::grpc_controller;
```

- [ ] **Step 2: add the root re-export of the macro** (so `#[leaf::grpc_controller]` resolves like `#[leaf::main]`). In `crates/leaf/src/lib.rs`, near the `pub use leaf_macros::async_impl;` block, add:

```rust
/// `#[grpc_controller]` — the gRPC controller-family stereotype (present iff the `grpc`
/// capability feature is enabled). Re-exported so the canonical `#[grpc_controller]`
/// spelling resolves; the prelude also brings it into scope under the `grpc`-gated glob.
#[cfg(feature = "grpc")]
#[doc(no_inline)]
pub use leaf_macros::grpc_controller;
```

- [ ] **Step 3: declare the `grpc` feature** so the `#[cfg(feature = "grpc")]` gates resolve. In `crates/leaf/Cargo.toml`'s `[features]`, add (Stage 6 wires the `dep:` edges; this stage adds the bare feature so the macro export gates cleanly):

```toml
# The gRPC capability (sub-project B). Stage 6 wires `dep:leaf-starter-grpc` (which pulls
# leaf-grpc + enables `http2` on the backend); this bare feature lets the `#[grpc_controller]`
# macro export gate behind it from Stage 4 onward.
grpc = []
```

- [ ] **Step 4: build the umbrella with the feature off (default) and on.**

```
cargo build -p leaf && cargo build -p leaf --features grpc
```
-> default build is unchanged (the gates are inert); the `--features grpc` build compiles (the macro export resolves; `::leaf_grpc::` paths are only checked when a user actually writes `#[grpc_controller]`, and Stage 6 adds the `dep:` edge — until then `--features grpc` exports only the macro name, which is valid).

  > If the `--features grpc` build errors because no `leaf_grpc` re-export exists yet, that is EXPECTED to be completed by Stage 6 (the starter/re-export). For Stage 4 the load-bearing check is: default build green + the macro export compiles. Keep the feature bare (`grpc = []`) so no broken `dep:` edge is introduced early.

- [ ] **Step 5: commit.**

```
git add crates/leaf/src/prelude.rs crates/leaf/src/lib.rs crates/leaf/Cargo.toml
git commit -m "leaf: export #[grpc_controller] in the umbrella + prelude behind the grpc feature

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4.8: force-clean gate (codegen green + HTTP suite untouched)

The whole-stage verification per the project's force-clean rule: the new `grpc_controller::` tests pass on a clean build, the existing HTTP/codegen suite stays green (no `web_controller.rs` change), and clippy is clean on `leaf-codegen` + `leaf-macros`.

**Files:** none (verification only)

- [ ] **Step 1: force-clean the codegen + macro crates.**

```
cargo clean -p leaf-codegen -p leaf-macros
```

- [ ] **Step 2: run the full codegen suite fresh.**

```
cargo test -p leaf-codegen
```
-> all tests PASS, including the ~existing `web_controller::` and `stereotype::` tests AND the new `grpc_controller::` tests (8 new: unary, server-stream, client-stream, bidi, struct+kind, kind-guard, generic-error, inherent-error, no-self-error).

- [ ] **Step 3: clippy clean (the project's gate).**

```
cargo clippy -p leaf-codegen -p leaf-macros -- -D warnings
```
-> no warnings (the generated `__LeafGrpcRoute_*` items carry the `#[allow(non_camel_case_types, …)]` lints; the codegen fns carry no dead code).

- [ ] **Step 4: confirm the HTTP suite is still green (the regression budget).**

```
cargo test -p leaf-web -p leaf-web-hyper
```
-> the existing HTTP suite PASSES unchanged (Stage 4 touched no leaf-web code).

- [ ] **Step 5: no commit (verification only).** If any step fails, fix forward (do not weaken a test) before declaring the stage complete.

---

## Stage 5: Cross-cutting + error model

Wire gRPC into the existing cross-cutting machinery. Two pieces:

1. **`WebFilter` reuse for gRPC.** gRPC requests flow through the same `Dispatcher` (Stage 1 added the `ProtocolDispatch` branch). The Stage-1 contract says the dispatcher delegates to a `ProtocolDispatch` *after* deciding no HTTP `Route` claims the content-type. Here we confirm/adjust the ordering so the **ordered `WebFilter` chain wraps the gRPC path** (a filter authenticating via gRPC metadata = HTTP/2 headers runs before the gRPC handler), and a filter that **short-circuits** a gRPC request is rendered by the gRPC edge as a valid `grpc-status` trailer (not a raw HTTP body).
2. **`GrpcStatusMapper` + the domain-error channel.** `GrpcDispatch` (Stage 2) collection-injects `Vec<Ref<dyn GrpcStatusMapper>>`. A handler's `LeafError` (a domain `ErrorKind::Integration { kind_id }`, exactly the storefront's unknown-SKU channel) is mapped to a `Status` rendered as trailers. The default `DefaultGrpcStatusMapper` is contributed as an `#[auto_config]` FALLBACK (`NoSuchBean`/unimplemented => `Unimplemented`, `ConvertError` => `Internal`, else `Unknown`), superseded the moment a user mapper claims the error.

HARD CONSTRAINTS baked into every task: `leaf-grpc` and `leaf-web` name no hyper/h2/tower; the dep arrow stays `leaf-grpc -> leaf-web -> leaf-core` (`leaf-web` never names `leaf-grpc`); no type-name detection in any macro/codegen; dogfood — the default mapper is an `#[auto_config]` FALLBACK + `OnMissingBean`, never a hand-rolled registration; reuse the existing `WebServer`/`KeepAlive`/`Dispatcher`/`WebFilter`/`FilterChain` from sub-project A. The existing ~1647-test HTTP suite stays green.

> **Cross-stage note on the filter ordering.** The Stage-1 `Dispatcher::dispatch` runs the `FilterChain` whose `Terminal` is the HTTP `RouteTerminal`. For gRPC, the filter chain must STILL wrap the request, but the chain's terminal must be the `ProtocolDispatch` branch (not the HTTP route table) when a `ProtocolDispatch` claims the content-type. Task 5.1 makes the dispatcher pick the terminal up-front (protocol vs HTTP) and run the SAME filter chain around it — so the auth/log/trace filters are uniform across HTTP and gRPC, exactly as §6 of the spec demands.

### Files

**Modify:**
- `crates/leaf-web/src/server.rs` — `Dispatcher` chooses the terminal (HTTP route table vs the first claiming `ProtocolDispatch`) and runs the one `FilterChain` around it; add a `ProtocolTerminal`. (`Vec<Arc<dyn ProtocolDispatch>>` field already added in Stage 1.)
- `crates/leaf-grpc/src/status.rs` — add `GrpcStatusMapper` trait + `impl_resolve_view!` (if Stage 2 left it stubbed; otherwise extend) and `Status::from_leaf_with` mapping helper.
- `crates/leaf-grpc/src/dispatch.rs` — `GrpcDispatch` consults its `Vec<Ref<dyn GrpcStatusMapper>>` (ordered, first-`Some`-wins) to render a handler `LeafError` as `Status` trailers; a filter short-circuit `Err` reaching the gRPC edge is mapped the same way.
- `crates/leaf-grpc/src/lib.rs` — re-export `GrpcStatusMapper`, `DefaultGrpcStatusMapper`.

**Create:**
- `crates/leaf-grpc/src/status_autoconfig.rs` — `DefaultGrpcStatusMapper` as the `#[auto_config]` FALLBACK bean (the `GrpcStatusMapper` analogue of `HyperServerAutoConfig`).
- `crates/leaf-grpc/tests/cross_cutting.rs` — integration-style tests: a `WebFilter` authenticating a gRPC call via metadata (short-circuit => `Unauthenticated` trailers), and a domain `LeafError` (`Integration { kind_id }`) -> `grpc-status`.

---

### Task 5.1: The `Dispatcher` runs the filter chain around the gRPC (`ProtocolDispatch`) terminal

The filter chain must wrap BOTH the HTTP route path and the gRPC protocol path with ONE ordered chain. We choose the terminal up-front by content-type, then run the existing `FilterChain` around it. leaf-web names no gRPC type — it only knows `dyn ProtocolDispatch`.

**Files:** `crates/leaf-web/src/server.rs`

- [ ] **Step 1: Write the failing test — a filter wraps a `ProtocolDispatch`-claimed request.**

  Add to the `tests` mod in `crates/leaf-web/src/server.rs`. (Stage 1's `Dispatcher::new` already takes the `Vec<Arc<dyn ProtocolDispatch>>` arg; the existing tests pass `vec![]` for it — keep that.)

  ```rust
  /// A fake protocol dispatch that claims `application/grpc*` and echoes a fixed
  /// body so a test can prove the dispatcher reached the protocol branch.
  struct FakeProto {
      claim: &'static str,
      body: &'static str,
  }

  impl crate::protocol::ProtocolDispatch for FakeProto {
      fn handles(&self, content_type: Option<&str>) -> bool {
          content_type.is_some_and(|ct| ct.starts_with(self.claim))
      }
      fn dispatch<'a>(
          &'a self,
          _req: Request,
      ) -> BoxFuture<'a, Result<Response, LeafError>> {
          Box::pin(async move {
              Ok(Response::ok().with_body(Bytes::from_static(self.body.as_bytes())))
          })
      }
  }

  fn grpc_req(path: &str) -> Request {
      let mut h = http::HeaderMap::new();
      h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
      Request::new(Method::POST, path.parse().expect("uri"), h, Bytes::new())
  }

  #[test]
  fn filter_chain_wraps_the_protocol_dispatch_terminal() {
      // A grpc-content-type request: NO HTTP route claims it, so the dispatcher
      // must run the SAME ordered filter chain around the ProtocolDispatch branch
      // (the auth/log/trace filters are uniform across HTTP and gRPC, §6).
      let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
      let filter: Arc<dyn WebFilter> = Arc::new(LogFilter { tag: "log", log: log.clone() });
      let proto: Arc<dyn crate::protocol::ProtocolDispatch> =
          Arc::new(FakeProto { claim: "application/grpc", body: "grpc-ok" });

      let dispatcher = Dispatcher::new(vec![], vec![filter], vec![], vec![proto]);
      let resp = futures::executor::block_on(
          dispatcher.dispatch(grpc_req("/pkg.Svc/M")),
      );

      assert_eq!(resp.status(), StatusCode::OK);
      assert_eq!(resp.body_bytes(), b"grpc-ok".as_slice());
      // The filter ran AROUND the protocol terminal — gRPC is filtered too.
      assert_eq!(*log.lock().expect("log"), vec!["log"]);
  }

  #[test]
  fn a_filter_can_short_circuit_a_grpc_request_before_the_protocol_terminal() {
      // The auth analogue: a filter short-circuits a grpc request (returns its own
      // Response without calling next), so the ProtocolDispatch terminal NEVER runs.
      let proto: Arc<dyn crate::protocol::ProtocolDispatch> =
          Arc::new(FakeProto { claim: "application/grpc", body: "grpc-ok" });
      // BlockFilter short-circuits when `x-block` is present (existing test fake in filter.rs;
      // here we use a local one keyed on a header the grpc request carries).
      struct Block;
      #[leaf_macros::async_impl]
      impl WebFilter for Block {
          async fn filter(
              &self,
              _req: Request,
              _next: crate::filter::Next<'_>,
          ) -> Result<Response, LeafError> {
              Ok(Response::new(StatusCode::FORBIDDEN))
          }
      }
      let blocker: Arc<dyn WebFilter> = Arc::new(Block);

      let dispatcher = Dispatcher::new(vec![], vec![blocker], vec![], vec![proto]);
      let resp = futures::executor::block_on(dispatcher.dispatch(grpc_req("/pkg.Svc/M")));

      // The filter short-circuited: 403, and the protocol body never appeared.
      assert_eq!(resp.status(), StatusCode::FORBIDDEN);
      assert!(resp.body_bytes().is_empty());
  }
  ```

- [ ] **Step 2: Run it — fails (no protocol terminal yet).**

  ```
  cargo test -p leaf-web server::tests::filter_chain_wraps_the_protocol_dispatch_terminal -- --nocapture
  ```
  Expected: FAIL (the dispatcher still always uses `RouteTerminal`, so the grpc request hits the route table and 404s instead of reaching `FakeProto`).

- [ ] **Step 3: Add the `ProtocolTerminal` + content-type selection in `Dispatcher::dispatch`.**

  Replace the body of `Dispatcher::dispatch` (the `// Build the routing table ...` block through the `match chain.run(req).await` block) in `crates/leaf-web/src/server.rs` with a version that picks the terminal first, then runs the one chain around it:

  ```rust
  pub async fn dispatch(&self, req: Request) -> Response {
      // Choose the chain's terminal up-front by content-type: if no HTTP Route
      // would claim this request but a ProtocolDispatch does, the gRPC (protocol)
      // branch IS the terminal; otherwise the HTTP route table is. EITHER WAY the
      // SAME ordered WebFilter chain wraps it — auth/log/trace run uniformly across
      // HTTP and gRPC (§6). leaf-web names no gRPC type: it only sees ProtocolDispatch.
      let content_type = req.header(http::header::CONTENT_TYPE.as_str()).map(str::to_owned);

      // The HTTP route-dispatch terminal (locals living across the single await).
      let route_refs: Vec<&dyn Route> = self.routes.iter().map(AsRef::as_ref).collect();
      let table = RouteTable::build(&route_refs);
      let route_terminal = RouteTerminal { table: &table };

      // The protocol terminal, IFF some ProtocolDispatch claims the content-type AND
      // no HTTP route family would (we let the explicit protocol claim win for a
      // non-HTTP content-type; HTTP routes match on method+path, not content-type).
      let proto = self
          .protocols
          .iter()
          .find(|p| p.handles(content_type.as_deref()));

      // Select the terminal: a claiming ProtocolDispatch wins for its content-type;
      // else the HTTP route table.
      let proto_terminal;
      let terminal: &dyn Terminal = match proto {
          Some(p) => {
              proto_terminal = ProtocolTerminal { proto: p.as_ref() };
              &proto_terminal
          }
          None => &route_terminal,
      };

      let filter_refs: Vec<&dyn WebFilter> = self.filters.iter().map(AsRef::as_ref).collect();
      let chain = FilterChain::new(&filter_refs, terminal);

      let req_for_advice = req.clone();
      match chain.run(req).await {
          Ok(resp) => resp,
          Err(err) => self.map_error(&err, &req_for_advice),
      }
  }
  ```

  And add the `ProtocolTerminal` beside `RouteTerminal` (it bridges the `Terminal` seam to a `dyn ProtocolDispatch`):

  ```rust
  /// The bottom of the filter chain when a non-HTTP protocol (e.g. gRPC) claims the
  /// request's content-type: delegate to the claiming [`ProtocolDispatch`]. The SAME
  /// ordered [`WebFilter`] chain wraps it, so a filter can authenticate / short-circuit
  /// a gRPC call exactly as it does an HTTP one (§6). leaf-web names no gRPC type — only
  /// the abstract `dyn ProtocolDispatch` seam.
  struct ProtocolTerminal<'p> {
      proto: &'p dyn crate::protocol::ProtocolDispatch,
  }

  impl Terminal for ProtocolTerminal<'_> {
      fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
          self.proto.dispatch(req)
      }
  }
  ```

  (Stage 1 already added `protocols: Vec<Arc<dyn ProtocolDispatch>>` to the struct and the 4th `Dispatcher::new` arg, and defined `crate::protocol::ProtocolDispatch`. If `dispatch` on the trait returns `BoxFuture<'a, Result<Response, LeafError>>` per the Stage-1 contract, `ProtocolTerminal::dispatch` forwards it verbatim.)

- [ ] **Step 4: Run the two new tests — pass.**

  ```
  cargo test -p leaf-web server::tests::filter_chain_wraps_the_protocol_dispatch_terminal server::tests::a_filter_can_short_circuit_a_grpc_request_before_the_protocol_terminal -- --nocapture
  ```
  Expected: both PASS.

- [ ] **Step 5: Run the WHOLE leaf-web suite — the HTTP path is unchanged.**

  ```
  cargo test -p leaf-web
  ```
  Expected: PASS (the `None => &route_terminal` arm is the exact prior behavior; every existing HTTP test that passed `vec![]` protocols still routes through `RouteTerminal`).

- [ ] **Step 6: Commit.**

  ```
  git add crates/leaf-web/src/server.rs
  git commit -m "leaf-web: run the WebFilter chain around the ProtocolDispatch (gRPC) terminal

The Dispatcher now picks its chain terminal by content-type — a claiming
ProtocolDispatch (gRPC) or the HTTP route table — and runs the SAME ordered
WebFilter chain around it. Auth/log/trace filters wrap gRPC uniformly; a
filter short-circuit on a gRPC request is honored before the protocol branch.
leaf-web still names no gRPC type (only dyn ProtocolDispatch).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 5.2: `GrpcStatusMapper` SPI + `Status::from_leaf_with` (the LeafError -> Status mapping)

The `ControlAdvice` analogue for gRPC: a collection-injected, ordered, first-`Some`-wins mapper from a domain `LeafError` to a `Status`. Mirrors `ControlAdvice::handle` exactly (sync, infallible-by-decline). `impl_resolve_view!` makes it collection-injectable.

**Files:** `crates/leaf-grpc/src/status.rs`

- [ ] **Step 1: Write the failing test for the SPI shape + the ordered first-`Some`-wins fold.**

  Add to the `tests` mod in `crates/leaf-grpc/src/status.rs`:

  ```rust
  #[test]
  fn grpc_status_mapper_first_some_in_order_wins() {
      use leaf_core::{ErrorKind, LeafError};

      // Two mappers both targeting ConstructionFailed; the earlier-consulted one wins.
      struct Map { kind: ErrorKind, status: Status }
      impl GrpcStatusMapper for Map {
          fn map(&self, err: &LeafError) -> Option<Status> {
              (err.kind == self.kind).then(|| self.status.clone())
          }
      }
      let early = Map { kind: ErrorKind::ConstructionFailed, status: Status::not_found("a") };
      let late = Map { kind: ErrorKind::ConstructionFailed, status: Status::internal("b") };
      let mappers: Vec<&dyn GrpcStatusMapper> = vec![&early, &late];

      let err = LeafError::new(ErrorKind::ConstructionFailed);
      let chosen = map_first(&mappers, &err).expect("a mapper claims it");
      assert_eq!(chosen.code, Code::NotFound);
      assert_eq!(chosen.message, "a");

      // An unclaimed kind => None (the caller falls back to the default mapper).
      let other = LeafError::new(ErrorKind::ValidationError);
      assert!(map_first(&mappers, &other).is_none());
  }
  ```

- [ ] **Step 2: Run it — fails (no `GrpcStatusMapper`/`map_first` yet).**

  ```
  cargo test -p leaf-grpc status::tests::grpc_status_mapper_first_some_in_order_wins
  ```
  Expected: FAIL to compile (`cannot find trait GrpcStatusMapper` / `map_first`).

- [ ] **Step 3: Add the trait + the `impl_resolve_view!` seam + the fold to `status.rs`.**

  Add to `crates/leaf-grpc/src/status.rs` (`Status`/`Code` already exist from Stage 2; `Status` already `derive(Clone)` per the contract — if not, add `#[derive(Clone, Debug, PartialEq, Eq)]` on `Status`):

  ```rust
  use leaf_core::LeafError;

  /// Maps a [`LeafError`] (a domain/framework error raised by a gRPC handler, a
  /// filter, or the codec) to a [`Status`] — the gRPC analogue of leaf-web's
  /// `ControlAdvice`. Collection-injected (`Vec<Ref<dyn GrpcStatusMapper>>`),
  /// consulted ordered, first-`Some`-wins, exactly like the HTTP advice chain.
  ///
  /// Like `ControlAdvice`, this is SYNC + infallible-by-decline: it inspects an
  /// already-materialized error and returns a `Status` or declines (`None`) so a
  /// later mapper — or the `DefaultGrpcStatusMapper` FALLBACK — handles it. No
  /// `.await`, no second failure to propagate. NAMES NO BACKEND.
  pub trait GrpcStatusMapper: Send + Sync {
      /// Map `err` to a [`Status`], or return `None` to decline.
      fn map(&self, err: &LeafError) -> Option<Status>;
  }

  // Make `dyn GrpcStatusMapper` a collection-injectable VIEW (emitted ONCE beside the
  // trait — orphan-rule-OK, `dyn GrpcStatusMapper` is local to leaf-grpc). Mapper beans
  // (any `#[component]` publishing the view, including the #[auto_config] default) are
  // collected by `GrpcDispatch` as `Vec<Ref<dyn GrpcStatusMapper>>`.
  leaf_core::impl_resolve_view!(dyn GrpcStatusMapper);

  /// Run a slice of mappers in order, returning the first non-`None` [`Status`].
  /// The first mapper that claims `err` wins (lower collection order = earlier),
  /// matching leaf-web's `Dispatcher::map_error` first-match-wins semantics.
  #[must_use]
  pub fn map_first(mappers: &[&dyn GrpcStatusMapper], err: &LeafError) -> Option<Status> {
      mappers.iter().find_map(|m| m.map(err))
  }
  ```

- [ ] **Step 4: Run it — passes.**

  ```
  cargo test -p leaf-grpc status::tests::grpc_status_mapper_first_some_in_order_wins
  ```
  Expected: PASS.

- [ ] **Step 5: Re-export from the crate root.**

  In `crates/leaf-grpc/src/lib.rs`, add to the existing `status` re-export line (extend whatever Stage 2 exported):

  ```rust
  pub use status::{map_first, Code, GrpcStatusMapper, Status};
  ```

  Run a compile check:
  ```
  cargo build -p leaf-grpc
  ```
  Expected: builds clean.

- [ ] **Step 6: Commit.**

  ```
  git add crates/leaf-grpc/src/status.rs crates/leaf-grpc/src/lib.rs
  git commit -m "leaf-grpc: GrpcStatusMapper SPI (the ControlAdvice analogue) + map_first fold

A collection-injected dyn GrpcStatusMapper maps a domain LeafError to a Status,
consulted ordered + first-Some-wins like the HTTP advice chain. impl_resolve_view!
makes it collection-injectable. Names no backend.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 5.3: `DefaultGrpcStatusMapper` as the `#[auto_config]` FALLBACK

The default mapper is contributed exactly like `HyperServerAutoConfig` contributes `dyn WebServer`: an `#[auto_config]` `#[bean(provides = "dyn ::leaf_grpc::GrpcStatusMapper")]` method gated by `#[conditional(on_missing_bean(dyn ::leaf_grpc::GrpcStatusMapper))]` at `FALLBACK`. NO hand-rolled registration (dogfood). The mapper logic: `NoSuchBean` => `Unimplemented`, `ConvertError` => `Internal`, else `Unknown` — and it ALWAYS returns `Some` (it is the floor; the FALLBACK guarantees a user mapper supersedes the whole bean, but the floor itself never declines, so an unmatched-by-user error still renders a well-formed trailer).

**Files:** `crates/leaf-grpc/src/status_autoconfig.rs` (new), `crates/leaf-grpc/src/lib.rs`

- [ ] **Step 1: Write the failing unit test for the default mapping table.**

  Create `crates/leaf-grpc/src/status_autoconfig.rs` with ONLY the test first (so it fails to compile, then we add the impl):

  ```rust
  //! `DefaultGrpcStatusMapper` — the DEFAULT `dyn GrpcStatusMapper` `#[auto_config]`
  //! integration: an `AUTO_CONFIGS` row at `FALLBACK` contributing the framework's
  //! built-in LeafError -> Status mapping, guarded by
  //! `OnMissingBean(dyn ::leaf_grpc::GrpcStatusMapper)`. The gRPC analogue of
  //! `leaf-web-hyper`'s `HyperServerAutoConfig`: a user `GrpcStatusMapper` bean (any
  //! concrete type providing the view) supersedes this floor. Dogfooded — no
  //! hand-rolled trait registration.

  use crate::status::{Code, GrpcStatusMapper, Status};
  use leaf_core::{ErrorKind, LeafError};

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn default_mapper_maps_the_common_framework_kinds() {
          let m = DefaultGrpcStatusMapper::new();
          // NoSuchBean (an unknown method / missing resource) -> Unimplemented.
          let s = m.map(&LeafError::new(ErrorKind::NoSuchBean)).expect("floor never declines");
          assert_eq!(s.code, Code::Unimplemented);
          // A decode/convert fault -> Internal.
          let s = m.map(&LeafError::new(ErrorKind::ConvertError)).expect("floor never declines");
          assert_eq!(s.code, Code::Internal);
          // Anything else -> Unknown (a domain Integration error a user mapper would claim).
          let s = m.map(&LeafError::new(ErrorKind::ConstructionFailed)).expect("floor never declines");
          assert_eq!(s.code, Code::Unknown);
      }
  }
  ```

- [ ] **Step 2: Run it — fails (no `DefaultGrpcStatusMapper`).**

  ```
  cargo test -p leaf-grpc status_autoconfig::tests::default_mapper_maps_the_common_framework_kinds
  ```
  Expected: FAIL (`cannot find type DefaultGrpcStatusMapper`).

- [ ] **Step 3: Add the `DefaultGrpcStatusMapper` bean + its mapping impl.**

  Insert above the `#[cfg(test)]` mod in `crates/leaf-grpc/src/status_autoconfig.rs`:

  ```rust
  /// The framework's default `LeafError -> Status` mapping, contributed as the
  /// `#[auto_config]` FALLBACK `dyn GrpcStatusMapper` (the floor of the mapper chain,
  /// Spring's `DefaultHandlerExceptionResolver` analogue for gRPC). It NEVER declines:
  /// it is the floor, so every error renders a well-formed grpc-status trailer. A user
  /// `GrpcStatusMapper` bean supersedes it via the `OnMissingBean(dyn GrpcStatusMapper)`
  /// VIEW back-off (and is consulted BEFORE it by `GrpcDispatch`'s ordered chain).
  #[leaf_macros::component]
  pub struct DefaultGrpcStatusMapper;

  impl DefaultGrpcStatusMapper {
      /// The no-collaborator constructor the `#[component]` provider calls.
      #[must_use]
      pub fn new() -> Self {
          DefaultGrpcStatusMapper
      }
  }

  impl Default for DefaultGrpcStatusMapper {
      fn default() -> Self {
          DefaultGrpcStatusMapper::new()
      }
  }

  impl GrpcStatusMapper for DefaultGrpcStatusMapper {
      fn map(&self, err: &LeafError) -> Option<Status> {
          // The floor maps the common framework kinds and NEVER declines (it is the
          // last resort, so an error a user mapper didn't claim still renders a valid
          // trailer). NO type-name detection — it matches the typed ErrorKind only.
          let code = match err.kind {
              // An unknown method / missing-resource shape -> Unimplemented (mirrors the
              // HTTP default's NoSuchBean -> 404; gRPC's "no such method" is Unimplemented).
              ErrorKind::NoSuchBean => Code::Unimplemented,
              // A malformed message / decode / convert fault -> Internal.
              ErrorKind::ConvertError | ErrorKind::ValidationError => Code::Internal,
              // Everything else (incl. a domain Integration{kind_id} a user mapper would
              // have claimed first) -> Unknown, the grpc-status catch-all.
              _ => Code::Unknown,
          };
          Some(Status::new(code, err.to_string()))
      }
  }
  ```

  > NOTE: `DefaultGrpcStatusMapper` deliberately does NOT special-case `Integration { kind_id }` — there is no type-name detection and no per-error special logic. A domain error is the USER mapper's job (Task 5.5 dogfoods one); the floor only guarantees a valid trailer.

- [ ] **Step 4: Run the unit test — passes.**

  ```
  cargo test -p leaf-grpc status_autoconfig::tests::default_mapper_maps_the_common_framework_kinds
  ```
  Expected: PASS.

- [ ] **Step 5: Write the failing auto-config test (FALLBACK + OnMissingBean back-off).**

  This mirrors `leaf-web-hyper/src/autoconfig.rs`'s `descriptor_is_a_fallback_auto_config_with_the_web_server_view` + `a_user_web_server_supersedes_the_fallback`. Add a `GRPC_STATUS_MAPPER_CONTRACT` const + a `grpc_status_mapper_descriptor()` accessor, then the tests. First add the `#[auto_config] impl` (Step 6) — but write the assertions now so they compile-fail:

  ```rust
  #[test]
  fn descriptor_is_a_fallback_auto_config_with_the_mapper_view() {
      use leaf_core::{CandidateRole, ContractId, TypeId};
      let d = grpc_status_mapper_descriptor();
      assert_eq!(
          d.meta.candidate_role,
          CandidateRole::FALLBACK,
          "the default mapper registers at FALLBACK so a user mapper supersedes it"
      );
      assert_eq!(d.self_type, std::any::TypeId::of::<DefaultGrpcStatusMapper>());
      assert!(
          d.provides.iter().any(|r| r.view == std::any::TypeId::of::<dyn GrpcStatusMapper>()),
          "the auto-config must declare the dyn GrpcStatusMapper view"
      );
      let _ = ContractId::of(GRPC_STATUS_MAPPER_CONTRACT);
      let _ = TypeId; // (drop if unused; keeps imports honest)
  }
  ```

  (Match the EXACT accessor pattern Stage's `web_server_descriptor()` uses — `AUTO_CONFIGS.iter().find(|d| d.contract == ContractId::of(GRPC_STATUS_MAPPER_CONTRACT))`. Keep the test's imports to what actually compiles; the load-bearing assertions are the `FALLBACK` role, the `self_type`, and the `dyn GrpcStatusMapper` view.)

- [ ] **Step 6: Add the `#[auto_config] impl` + the const anchors (the dogfooded contribution).**

  Insert (above the `#[cfg(test)]` mod) — the EXACT shape of `HyperServerAutoConfig`'s `#[auto_config] impl`:

  ```rust
  use leaf_core::{CondExpr, ContractId, Descriptor, ProviderSeed};

  /// The declared name of the contributed `dyn GrpcStatusMapper` bean.
  pub const GRPC_STATUS_MAPPER_BEAN: &str = "grpcStatusMapper";

  /// The stable contract path of the default mapper's contributed bean (minted by the
  /// `#[auto_config] impl` macro from `module_path!()::grpc_status_mapper`).
  pub const GRPC_STATUS_MAPPER_CONTRACT: &str =
      "leaf_grpc::status_autoconfig::grpc_status_mapper";

  /// The `@Bean`-method contribution: `grpc_status_mapper()` produces the concrete
  /// [`DefaultGrpcStatusMapper`] exposed as `dyn ::leaf_grpc::GrpcStatusMapper` (the
  /// `provides[]` view), named `"grpcStatusMapper"`, into `AUTO_CONFIGS` at `FALLBACK`,
  /// gated by `OnMissingBean(dyn ::leaf_grpc::GrpcStatusMapper)` — ANY user mapper bean
  /// supersedes it. The holder is `DefaultGrpcStatusMapper` itself (a `#[component]`),
  /// matching the cache/tx/web-server default pattern. Dogfooded: no hand-rolled
  /// registration.
  #[leaf_macros::auto_config]
  impl DefaultGrpcStatusMapper {
      #[bean(name = "grpcStatusMapper", provides = "dyn ::leaf_grpc::GrpcStatusMapper")]
      #[conditional(on_missing_bean(dyn ::leaf_grpc::GrpcStatusMapper))]
      fn grpc_status_mapper(&self) -> DefaultGrpcStatusMapper {
          DefaultGrpcStatusMapper::new()
      }
  }

  /// The const back-off guard (the macro-emitted `#[conditional]` tree — a single
  /// `OnMissingBean(dyn GrpcStatusMapper)` VIEW leaf). The anti-DCE anchor for the row.
  pub static GRPC_STATUS_MAPPER_AUTO_CONFIG_GUARD: CondExpr = __leaf_guard_grpc_status_mapper;

  /// The const [`ProviderSeed`] leaf-boot's `run_autoconfig` invokes to mint the
  /// default mapper's `Provider` (the macro-emitted seed; also the anti-DCE anchor).
  pub const GRPC_STATUS_MAPPER_SEED: ProviderSeed = __leaf_seed_grpc_status_mapper;

  /// The contributed `AUTO_CONFIGS` [`Descriptor`] for the default mapper (looked up by
  /// its contributed contract) — at `FALLBACK`, on the auto-config channel, carrying the
  /// `dyn GrpcStatusMapper` view.
  #[must_use]
  pub fn grpc_status_mapper_descriptor() -> Descriptor {
      *leaf_core::AUTO_CONFIGS
          .iter()
          .find(|d| d.contract == ContractId::of(GRPC_STATUS_MAPPER_CONTRACT))
          .expect("the #[auto_config] grpc_status_mapper Descriptor must reach AUTO_CONFIGS")
  }
  ```

- [ ] **Step 7: Wire the module + re-export.**

  In `crates/leaf-grpc/src/lib.rs` add the module and re-export the bean (so an app/example can name it, and DI discovers the `#[component]`/`AUTO_CONFIGS` rows via the linkme slices):

  ```rust
  pub mod status_autoconfig;
  pub use status_autoconfig::DefaultGrpcStatusMapper;
  ```

- [ ] **Step 8: Run the full leaf-grpc suite — passes.**

  ```
  cargo test -p leaf-grpc status_autoconfig::
  ```
  Expected: PASS (the unit mapping test + the FALLBACK descriptor test). If `grpc_status_mapper_descriptor()` panics with "must reach AUTO_CONFIGS", the `#[auto_config]` const anchors are being DCE-dropped — add a path-reference anchor (`GRPC_STATUS_MAPPER_SEED`/`_GUARD`) the way `leaf-web-hyper` does (the public re-export from Step 7 keeps the module live).

- [ ] **Step 9: Commit.**

  ```
  git add crates/leaf-grpc/src/status_autoconfig.rs crates/leaf-grpc/src/lib.rs
  git commit -m "leaf-grpc: DefaultGrpcStatusMapper as the #[auto_config] FALLBACK

The framework's floor LeafError -> Status mapping (NoSuchBean => Unimplemented,
ConvertError => Internal, else Unknown), contributed exactly like the hyper
WebServer default: #[auto_config] #[bean(provides = dyn GrpcStatusMapper)] at
FALLBACK, gated by OnMissingBean(dyn GrpcStatusMapper). A user mapper supersedes
it. Dogfooded — no hand-rolled registration. No type-name detection.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 5.4: `GrpcDispatch` renders a handler `LeafError` (and a filter short-circuit) as `Status` trailers

`GrpcDispatch` (Stage 2) field-injects `Vec<Ref<dyn GrpcRoute>>`; here it ALSO field-injects `Vec<Ref<dyn GrpcStatusMapper>>` and uses the ordered chain (user mappers first, then the FALLBACK floor) to turn a handler-raised `LeafError` into `Status` trailers. The `GrpcHandler` contract says it NEVER returns `Err` (it renders `Status` as trailers), so the error -> status mapping happens INSIDE the dispatch path, before the trailers are written.

> The `GrpcHandler` itself (Stage 4 codegen) already catches the typed handler's `Result<_, Status>` and renders explicit `Status`. THIS task adds the channel for a `LeafError` (a domain error from a collaborator, or a filter short-circuit `Err` propagating to the gRPC edge) — the mapper turns it into a `Status`. We render via a shared `render_status_trailers(Status) -> Response` so explicit-Status and mapped-LeafError trailers are byte-identical.

**Files:** `crates/leaf-grpc/src/dispatch.rs`, `crates/leaf-grpc/src/status.rs`

- [ ] **Step 1: Add `Status` <-> trailers helpers (failing test first).**

  In `crates/leaf-grpc/src/status.rs` tests mod:

  ```rust
  #[test]
  fn status_renders_as_grpc_status_and_message_trailers() {
      let s = Status::new(Code::NotFound, "no such product");
      let trailers = s.to_trailers();
      assert_eq!(
          trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
          Some("5"),
          "grpc-status carries the numeric Code"
      );
      assert_eq!(
          trailers.get("grpc-message").and_then(|v| v.to_str().ok()),
          Some("no%20such%20product").or(Some("no such product")),
          "grpc-message carries the (percent-encoded) message"
      );
  }
  ```

  > The grpc-message percent-encoding (RFC: gRPC over HTTP/2 §"Responses") encodes bytes outside `%x20-%x7E` and `%`. The assert accepts either the encoded `no%20space` form or the literal — pin whichever your `to_trailers` implements; the load-bearing invariant is `grpc-status == "5"`. If Stage 2's framing already defined `to_trailers`, extend its test instead of redefining.

- [ ] **Step 2: Run it — fails.**

  ```
  cargo test -p leaf-grpc status::tests::status_renders_as_grpc_status_and_message_trailers
  ```
  Expected: FAIL (`no method to_trailers`).

- [ ] **Step 3: Implement `Status::to_trailers`.**

  In `crates/leaf-grpc/src/status.rs`:

  ```rust
  impl Status {
      /// Render this status as the gRPC HTTP/2 trailers (`grpc-status` = the numeric
      /// [`Code`], `grpc-message` = the percent-encoded message). These are the
      /// trailing metadata a gRPC response carries; the [`Frame::Trailers`] the
      /// [`GrpcHandler`]/[`GrpcDispatch`] appends to the response body stream.
      ///
      /// [`Frame::Trailers`]: leaf_web::Frame::Trailers
      #[must_use]
      pub fn to_trailers(&self) -> http::HeaderMap {
          let mut h = http::HeaderMap::new();
          // grpc-status is the numeric code (0..=16).
          h.insert(
              "grpc-status",
              http::HeaderValue::from(self.code as u8),
          );
          // grpc-message is percent-encoded per the gRPC HTTP/2 spec (bytes outside
          // %x20-%x7E, and `%` itself, are %XX-escaped). HeaderValue must be ASCII.
          let encoded = percent_encode_grpc_message(&self.message);
          if let Ok(v) = http::HeaderValue::from_str(&encoded) {
              h.insert("grpc-message", v);
          }
          h
      }
  }

  /// Percent-encode a grpc-message per the gRPC HTTP/2 protocol: any byte not in
  /// `%x20-%x7E` (and `%` itself) becomes `%XX`. Pure, backend-free.
  fn percent_encode_grpc_message(msg: &str) -> String {
      let mut out = String::with_capacity(msg.len());
      for &b in msg.as_bytes() {
          if (0x20..=0x7E).contains(&b) && b != b'%' {
              out.push(b as char);
          } else {
              out.push('%');
              out.push_str(&format!("{b:02X}"));
          }
      }
      out
  }
  ```

  Update the Step-1 assert to the encoded form (`Some("no%20such%20product")`) and drop the `.or(..)`:

  ```rust
      assert_eq!(
          trailers.get("grpc-message").and_then(|v| v.to_str().ok()),
          Some("no%20such%20product"),
      );
  ```

  Run: `cargo test -p leaf-grpc status::tests::status_renders_as_grpc_status_and_message_trailers` -> PASS.

- [ ] **Step 4: Add the failing dispatch test — a domain `LeafError` becomes `Status` trailers.**

  In `crates/leaf-grpc/src/dispatch.rs` tests mod (Stage 2 has the unknown-method `Unimplemented` test here already; this one proves the MAPPER channel). A fake `GrpcRoute` whose handler raises a domain `LeafError`, plus the default mapper:

  ```rust
  #[test]
  fn a_handler_leaf_error_is_rendered_as_status_trailers_via_the_mapper_chain() {
      use leaf_core::{ContractId, ErrorKind, LeafError};
      use crate::status::{Code, DefaultGrpcStatusMapper, GrpcStatusMapper, Status};
      use std::sync::Arc;

      // A user mapper claiming a domain Integration{kind_id} -> NotFound (the gRPC
      // analogue of the storefront's unknown-SKU -> 404 advice).
      fn unknown_sku() -> ContractId { ContractId::of("storefront::catalog::UnknownSku") }
      struct DomainMapper;
      impl GrpcStatusMapper for DomainMapper {
          fn map(&self, err: &LeafError) -> Option<Status> {
              match err.kind {
                  ErrorKind::Integration { kind_id } if kind_id == unknown_sku() =>
                      Some(Status::not_found("unknown sku")),
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
  ```

- [ ] **Step 5: Run it — fails.**

  ```
  cargo test -p leaf-grpc dispatch::tests::a_handler_leaf_error_is_rendered_as_status_trailers_via_the_mapper_chain
  ```
  Expected: FAIL (`no associated function status_for`).

- [ ] **Step 6: Add the mapper field + `status_for` + use it in the gRPC error path.**

  In `crates/leaf-grpc/src/dispatch.rs`:

  1. Add the field to `GrpcDispatch` and field-inject it (Stage 2 made `GrpcDispatch` a `#[component]` field-injecting `Vec<Ref<dyn GrpcRoute>>`; ADD the mappers vec the same way):

  ```rust
  #[leaf_macros::component]
  pub struct GrpcDispatch {
      /// The per-method routes (the #[grpc_controller] beans), built into the O(1)
      /// path map. Field-injected as the dyn GrpcRoute collection.
      routes: Vec<leaf_core::Ref<dyn GrpcRoute>>,
      /// The ordered LeafError -> Status mappers (user mappers + the FALLBACK floor),
      /// field-injected as the dyn GrpcStatusMapper collection. Consulted first-Some-wins
      /// to render a domain/framework error as grpc-status trailers — the gRPC analogue
      /// of leaf-web's ControlAdvice chain.
      mappers: Vec<leaf_core::Ref<dyn crate::status::GrpcStatusMapper>>,
  }
  ```

  2. Add the pure mapping entry point (the floor guarantees a `Status` always exists — the FALLBACK `DefaultGrpcStatusMapper` never declines, so the chain ALWAYS yields one; but defend with `Code::Unknown` if no mapper is wired):

  ```rust
  impl GrpcDispatch {
      /// Map a handler/filter [`LeafError`] to a [`Status`] via the ordered mapper
      /// chain (user mappers consulted first, the `DefaultGrpcStatusMapper` FALLBACK
      /// last). The floor never declines, so this always yields a `Status`; if NO
      /// mapper is wired at all (degenerate), it defaults to `Code::Unknown` so a
      /// well-formed trailer is still produced. Pure + backend-free.
      #[must_use]
      pub fn status_for(
          mappers: &[std::sync::Arc<dyn crate::status::GrpcStatusMapper>],
          err: &LeafError,
      ) -> crate::status::Status {
          for m in mappers {
              if let Some(s) = m.map(err) {
                  return s;
              }
          }
          crate::status::Status::new(crate::status::Code::Unknown, err.to_string())
      }
  }
  ```

  > The test passes `Vec<Arc<dyn GrpcStatusMapper>>`; the injected field is `Vec<Ref<dyn GrpcStatusMapper>>`. `Ref<T>` derefs/clones to a shareable handle — in the `ProtocolDispatch::dispatch` impl, build a `Vec<Arc<..>>` (or `&[&dyn ..]`) view of `self.mappers` once and pass it to `status_for`. If your `Ref` is `Arc`-shaped, `self.mappers.iter().map(|r| r.clone().into_arc())` (or the existing `Ref`->`Arc` accessor) yields the slice. Keep `status_for` taking `&[Arc<dyn ..>]` so the unit test stays transport-free.

  3. In the `impl leaf_web::ProtocolDispatch for GrpcDispatch` `dispatch` method, where a route's `GrpcHandler` runs (and where the unknown-method `Unimplemented` is rendered per Stage 2): when the handler path surfaces a `LeafError` (a collaborator domain error the `GrpcHandler` couldn't render as an explicit `Status`, OR a filter short-circuit `Err` that reached this edge), call `Self::status_for(&self.mappers_as_arcs(), &err)` and append `status.to_trailers()` as the final `Frame::Trailers` of the response body stream. Reuse the SAME `render_status_trailers` path the explicit-`Status` case uses so the wire bytes are identical:

  ```rust
  // Inside GrpcDispatch::dispatch (the ProtocolDispatch impl), error arm:
  // `err: LeafError` surfaced from the handler/filter edge.
  let status = Self::status_for(&self.mappers_as_arcs(), &err);
  // Render an empty data stream + the grpc-status/grpc-message trailers as the
  // response Body::Stream (Code::Ok handlers append a 0-trailer the same way).
  return render_status_response(status); // builds Response with Body::Stream of [Frame::Trailers(status.to_trailers())]
  ```

  Where `render_status_response` (a free fn in `dispatch.rs`) wraps a lone `Frame::Trailers` into a `leaf_web::Body::Stream` (`Response::ok().with_body_stream(once(Frame::Trailers(status.to_trailers())))`) — gRPC always returns HTTP 200; the failure lives in the trailers, not the HTTP status. (`with_body_stream` is the Stage-1 `Response` ctor.)

- [ ] **Step 7: Run the dispatch test — passes.**

  ```
  cargo test -p leaf-grpc dispatch::tests::a_handler_leaf_error_is_rendered_as_status_trailers_via_the_mapper_chain
  ```
  Expected: PASS.

- [ ] **Step 8: Run the full leaf-grpc suite (incl. Stage 2's unknown-method test).**

  ```
  cargo test -p leaf-grpc
  ```
  Expected: PASS (Stage 2's `unknown method => Unimplemented` still renders via `status_for` + the floor; the new domain-error mapping passes).

- [ ] **Step 9: Commit.**

  ```
  git add crates/leaf-grpc/src/dispatch.rs crates/leaf-grpc/src/status.rs
  git commit -m "leaf-grpc: render handler/filter LeafError as grpc-status trailers via the mapper chain

GrpcDispatch field-injects Vec<Ref<dyn GrpcStatusMapper>> and consults it
first-Some-wins (user mappers, then the FALLBACK floor) to turn a domain
LeafError (incl. Integration{kind_id}) or a filter short-circuit into a Status,
rendered as grpc-status/grpc-message trailers over HTTP 200. Status::to_trailers
percent-encodes grpc-message per the gRPC HTTP/2 spec. No type-name detection.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 5.5: Cross-cutting integration — a metadata-auth filter + a domain error -> grpc-status (no transport)

The headline proof, at the dispatch boundary (a real hyper/tonic same-port test belongs to Stage 6). Two proofs: (1) a `WebFilter` authenticates a gRPC call by inspecting metadata (= HTTP/2 headers) and short-circuits an unauthenticated one, rendered as `Unauthenticated` trailers; (2) a domain `LeafError` flows through the mapper chain to a `grpc-status`.

**Files:** `crates/leaf-grpc/tests/cross_cutting.rs` (new)

- [ ] **Step 1: Write the metadata-auth filter test (failing).**

  Create `crates/leaf-grpc/tests/cross_cutting.rs`:

  ```rust
  //! Cross-cutting integration: the existing WebFilter chain + the GrpcStatusMapper
  //! error model, exercised at the leaf-web Dispatcher boundary (no hyper/tonic — that
  //! is Stage 6). Proves a filter authenticating via gRPC metadata short-circuits to an
  //! Unauthenticated Status, and a domain LeafError maps to a grpc-status.

  use std::sync::Arc;

  use bytes::Bytes;
  use http::{HeaderMap, HeaderValue, Method, StatusCode};
  use leaf_core::{BoxFuture, LeafError};
  use leaf_grpc::{Code, DefaultGrpcStatusMapper, GrpcStatusMapper, Status};
  use leaf_web::filter::{Next, WebFilter};
  use leaf_web::protocol::ProtocolDispatch;
  use leaf_web::server::Dispatcher;
  use leaf_web::{Request, Response};

  /// A grpc request carrying (or omitting) an `authorization` metadata header.
  fn grpc_call(authed: bool) -> Request {
      let mut h = HeaderMap::new();
      h.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
      if authed {
          h.insert(http::header::AUTHORIZATION, HeaderValue::from_static("Bearer ok"));
      }
      Request::new(Method::POST, "/pkg.Svc/M".parse().unwrap(), h, Bytes::new())
  }

  /// An auth filter: gRPC metadata ARE HTTP/2 headers, so the SAME WebFilter inspects
  /// `authorization`. On a missing token it short-circuits with an Unauthenticated
  /// Status rendered as trailers (HTTP 200 + grpc-status=16) — a rejected gRPC call
  /// still produces a valid grpc-status trailer, never a raw HTTP body (§6).
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
              return Ok(Response::ok().with_body_stream(
                  leaf_grpc::status_trailers_stream(status),
              ));
          }
          next.run(req).await
      }
  }

  /// A ProtocolDispatch that proves it ran (the authed path reaches it).
  struct OkProto;
  impl ProtocolDispatch for OkProto {
      fn handles(&self, ct: Option<&str>) -> bool {
          ct.is_some_and(|c| c.starts_with("application/grpc"))
      }
      fn dispatch<'a>(&'a self, _req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
          Box::pin(async move {
              // Render a Code::Ok trailer (the success shape).
              Ok(Response::ok().with_body_stream(
                  leaf_grpc::status_trailers_stream(Status::new(Code::Ok, "")),
              ))
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
  ```

  > This test needs two small leaf-grpc helpers it references: `leaf_grpc::status_trailers_stream(Status) -> BoxStream<Result<Frame, LeafError>>` (a one-element stream of `Frame::Trailers(status.to_trailers())`) and a test-friendly `leaf_grpc::collect_trailers(Body) -> HeaderMap` (drain the `Body::Stream`, return the trailers frame's map). If Task 5.4's `render_status_response` already wraps trailers, factor its inner stream builder out as the public `status_trailers_stream`. `collect_trailers` lives behind `#[cfg(any(test, feature = "test-util"))]` or as a plain pub fn in `leaf-grpc` (it names no backend — it walks the `leaf_web::Body::Stream`).

- [ ] **Step 2: Run it — fails (the helpers don't exist yet).**

  ```
  cargo test -p leaf-grpc --test cross_cutting
  ```
  Expected: FAIL (`cannot find function status_trailers_stream` / `collect_trailers`).

- [ ] **Step 3: Add the two public helpers to leaf-grpc.**

  In `crates/leaf-grpc/src/dispatch.rs` (or a small `crates/leaf-grpc/src/trailers.rs` re-exported from `lib.rs`):

  ```rust
  use leaf_web::{Body, Frame};
  use leaf_core::{BoxStream, LeafError};

  /// A one-element [`Body::Stream`] payload carrying ONLY the gRPC status trailers —
  /// the wire shape for an empty (or short-circuited) gRPC response: HTTP 200, no data
  /// frames, a single `Frame::Trailers(grpc-status/grpc-message)`. Backend-free
  /// (`BoxStream` is futures, not hyper). Used by the GrpcHandler success/error edges
  /// and by a filter short-circuiting a gRPC call.
  #[must_use]
  pub fn status_trailers_stream(
      status: crate::status::Status,
  ) -> BoxStream<'static, Result<Frame, LeafError>> {
      let trailers = status.to_trailers();
      Box::pin(futures::stream::once(async move { Ok(Frame::Trailers(trailers)) }))
  }

  /// Drain a [`Body`] and return its trailers (the `Frame::Trailers` map), ignoring any
  /// data frames. A test/utility helper for asserting a rendered gRPC response's
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
  ```

  Re-export from `crates/leaf-grpc/src/lib.rs`:

  ```rust
  pub use dispatch::{collect_trailers, status_trailers_stream};
  ```

  And refactor Task 5.4's `render_status_response` to use `status_trailers_stream` so there is ONE trailer-stream builder:

  ```rust
  fn render_status_response(status: crate::status::Status) -> leaf_web::Response {
      leaf_web::Response::ok().with_body_stream(status_trailers_stream(status))
  }
  ```

- [ ] **Step 4: Ensure `tokio` + `futures` are dev-deps of leaf-grpc.**

  In `crates/leaf-grpc/Cargo.toml`:
  ```toml
  [dev-dependencies]
  tokio = { workspace = true, features = ["macros", "rt"] }
  ```
  (futures is already a normal dep per the Stage-2 manifest.) Verify NO hyper/h2/tower appears in this manifest:
  ```
  cargo tree -p leaf-grpc -e normal --no-default-features 2>/dev/null | grep -Ei "hyper|tower|^h2| h2 " && echo "BACKEND LEAK" || echo "backend-free OK"
  ```
  Expected: `backend-free OK`.

- [ ] **Step 5: Run the cross-cutting tests — pass.**

  ```
  cargo test -p leaf-grpc --test cross_cutting -- --nocapture
  ```
  Expected: both PASS (`an_unauthenticated_grpc_call_is_short_circuited_to_unauthenticated_trailers`, `an_authenticated_grpc_call_reaches_the_protocol_terminal`).

- [ ] **Step 6: Add the domain-error mapping integration test.**

  Append to `crates/leaf-grpc/tests/cross_cutting.rs` — a `ProtocolDispatch` whose handler raises a domain `LeafError`, mapped by a user `GrpcStatusMapper` to `NotFound`, exactly the storefront unknown-SKU shape:

  ```rust
  use leaf_core::{ContractId, ErrorKind};

  fn unknown_sku() -> ContractId { ContractId::of("storefront::catalog::UnknownSku") }

  /// A user GrpcStatusMapper: the unknown-SKU domain Integration error -> NotFound (the
  /// gRPC analogue of the storefront's 404 ControlAdvice). Declines everything else.
  struct UnknownSkuMapper;
  impl GrpcStatusMapper for UnknownSkuMapper {
      fn map(&self, err: &LeafError) -> Option<Status> {
          match err.kind {
              ErrorKind::Integration { kind_id } if kind_id == unknown_sku() =>
                  Some(Status::not_found("unknown sku")),
              _ => None,
          }
      }
  }

  /// A ProtocolDispatch that maps a raised domain LeafError through the mapper chain
  /// (user-first, then the FALLBACK floor) and renders the resulting Status trailers —
  /// the in-test stand-in for GrpcDispatch::status_for + render_status_response.
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
              let status = leaf_grpc::dispatch::GrpcDispatch::status_for(&mappers, &err);
              Ok(Response::ok().with_body_stream(leaf_grpc::status_trailers_stream(status)))
          })
      }
  }

  #[tokio::test]
  async fn a_domain_leaf_error_maps_to_a_grpc_status_via_the_user_mapper() {
      // User mapper first, the FALLBACK floor last (the chain GrpcDispatch builds).
      let mappers: Vec<Arc<dyn GrpcStatusMapper>> =
          vec![Arc::new(UnknownSkuMapper), Arc::new(DefaultGrpcStatusMapper::new())];
      let proto: Arc<dyn ProtocolDispatch> =
          Arc::new(DomainErrProto { mappers });
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
  ```

  (If `GrpcDispatch` / `status_for` are not `pub` at the path the test names, expose them: `pub use dispatch::GrpcDispatch;` in `lib.rs`, and keep `status_for` `pub`.)

- [ ] **Step 7: Run the domain-error test — passes.**

  ```
  cargo test -p leaf-grpc --test cross_cutting a_domain_leaf_error_maps_to_a_grpc_status_via_the_user_mapper -- --nocapture
  ```
  Expected: PASS (grpc-status = 5).

- [ ] **Step 8: Commit.**

  ```
  git add crates/leaf-grpc/tests/cross_cutting.rs crates/leaf-grpc/src/dispatch.rs crates/leaf-grpc/src/lib.rs crates/leaf-grpc/Cargo.toml
  git commit -m "leaf-grpc: cross-cutting integration — metadata auth filter + domain-error grpc-status

A WebFilter authenticating a gRPC call via metadata (= HTTP/2 headers)
short-circuits an unauthenticated call to Unauthenticated trailers (HTTP 200 +
grpc-status=16); an authed call reaches the protocol terminal (grpc-status=0).
A domain LeafError (Integration{kind_id}, the storefront unknown-SKU shape)
maps to NotFound (grpc-status=5) via a user GrpcStatusMapper, with the FALLBACK
floor behind it. status_trailers_stream/collect_trailers added (backend-free).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 5.6: Full force-clean verification gate for the stage

Per the project's verify-with-fresh-builds rule: cached `cargo` re-emits no warnings, so force-clean before claiming clean. Confirm the existing ~1647-test HTTP suite stayed green through the cross-cutting wiring and that nothing leaked a backend name into leaf-web/leaf-grpc.

**Files:** (none — verification only)

- [ ] **Step 1: Force-clean build + the leaf-web suite (the HTTP regression budget).**

  ```
  cargo clean -p leaf-web && cargo test -p leaf-web
  ```
  Expected: PASS, all leaf-web tests green (the Stage-1 + Task-5.1 protocol-terminal selection is the only change; the `None => &route_terminal` arm preserves every HTTP test).

- [ ] **Step 2: Force-clean build + the leaf-grpc suite.**

  ```
  cargo clean -p leaf-grpc && cargo test -p leaf-grpc
  ```
  Expected: PASS (unit mapping + auto-config FALLBACK + dispatch mapper-chain + the two cross-cutting integration files).

- [ ] **Step 3: The full workspace suite — the ~1647 HTTP tests stay green.**

  ```
  cargo test --workspace
  ```
  Expected: PASS workspace-wide (no HTTP/web/storefront regression from the `Dispatcher` terminal-selection change).

- [ ] **Step 4: Clippy clean (force-clean) on the two touched crates.**

  ```
  cargo clean -p leaf-web -p leaf-grpc && cargo clippy -p leaf-web -p leaf-grpc --all-targets -- -D warnings
  ```
  Expected: zero warnings.

- [ ] **Step 5: Backend-free assertion — leaf-web and leaf-grpc name NO hyper/h2/tower.**

  ```
  cargo tree -p leaf-web -e normal 2>/dev/null | grep -Ei "(^| )hyper|(^| )tower|(^| )h2 " && echo "leaf-web BACKEND LEAK" || echo "leaf-web backend-free OK"
  cargo tree -p leaf-grpc -e normal 2>/dev/null | grep -Ei "(^| )hyper|(^| )tower|(^| )h2 " && echo "leaf-grpc BACKEND LEAK" || echo "leaf-grpc backend-free OK"
  ```
  Expected: both print `... backend-free OK` (the only crate naming hyper/h2 stays `leaf-web-hyper`).

- [ ] **Step 6: Doc check (the project's doc-clean gate).**

  ```
  cargo clean -p leaf-grpc && cargo doc -p leaf-grpc --no-deps 2>&1 | grep -i "warning" && echo "DOC WARNINGS" || echo "doc clean"
  ```
  Expected: `doc clean`.

- [ ] **Step 7: Commit (verification artifacts, if any) — or note the gate passed.**

  No source change — if any `#[allow]` for a generated-item naming lint was needed to keep clippy clean (per the project's rust-analyzer-vs-rustc note), commit it now:

  ```
  git add -A
  git commit -m "leaf-grpc: force-clean gate — HTTP suite green, backend-free, clippy/doc clean

Verified the cross-cutting + error-model stage against a fresh build: the full
workspace suite (incl. the ~1647 HTTP tests) is green, leaf-web/leaf-grpc name
no hyper/h2/tower, and clippy/doc are warning-free.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```
  (If no source change was needed, skip the commit and record that the gate passed in the PR description instead.)

---

## Stage 6: Integration + dogfood example

The headline polyglot-interop proof. With Stages 1–5 landed (the streaming `Body`, `leaf-grpc`'s `Status`/`Code`/framing/`Streaming<T>`/`GrpcDispatch`, `leaf-grpc-build`'s codegen, the `#[grpc_controller]` stereotype, and the `GrpcStatusMapper` error model), this stage proves it all works END TO END against the canonical gRPC stack: boot the shared hyper `WebServer` (now with `http2`), and call a real `#[grpc_controller]` with **`tonic` as the test client** (dev-dep only) across all four call shapes + an explicit `Status` error + a domain-error `Status` (the `GrpcStatusMapper` channel) + a `WebFilter` doing metadata auth + HTTP and gRPC on the **same port**. Then the dogfood: a `#[grpc_controller]` on the storefront over a real `.proto`, served over real H2 — the gRPC analogue of `the_storefront_serves_its_domain_over_real_http`. Closes with the full force-clean gate (test + clippy + doc).

HARD CONSTRAINTS carried into every task: leaf-web/leaf-grpc stay backend-free (only `leaf-web-hyper` names hyper/h2); the dep arrow is `leaf-grpc → leaf-web → leaf-core` (leaf-web never names leaf-grpc); zero hand-rolled trait impls a stereotype/macro can generate (no hand-written `GrpcRoute`/`GrpcHandler`/`ProtocolDispatch` in any proof — the macro + `#[component]`/`#[auto_config]` make them); the existing ~1647-test HTTP suite stays green. `tonic` is a **dev-dependency** of the integration-test crates ONLY — it never enters the shipped dep graph.

### Files

**Create**

- `crates/leaf-grpc/tests/proto/echo.proto` — the integration test service (all 4 call shapes + an error RPC).
- `crates/leaf-grpc/tests/build.rs` — compiles `echo.proto` via `leaf_grpc_build::compile` for the test crate.
- `crates/leaf-grpc/tests/serves_grpc.rs` — the tonic-client integration proof (4 shapes + explicit Status + domain Status + WebFilter auth + same-port HTTP+gRPC).
- `crates/leaf-grpc/tests/echo_controller.rs` — the `#[cfg(test)]` `#[grpc_controller]` + the auth `WebFilter` + the domain `GrpcStatusMapper`, shared by the proof.
- `crates/leaf-starter-grpc/Cargo.toml` + `crates/leaf-starter-grpc/src/lib.rs` — the gRPC STACK starter (the curated additive bundle: `leaf-grpc` + the web stack with `http2` + a runtime peer), the `grpc` capability target.
- `examples/storefront/proto/catalog.proto` — the storefront's dogfood gRPC service.
- `examples/storefront/build.rs` — compiles `catalog.proto` via `leaf_grpc_build::compile`.
- `examples/storefront/src/grpc/mod.rs` + `examples/storefront/src/grpc/catalog_controller.rs` — the storefront `#[grpc_controller]` dogfood + a domain `GrpcStatusMapper`.
- `examples/storefront/tests/grpc.rs` — `the_storefront_serves_its_domain_over_real_grpc` (real-H2 tonic proof).

**Modify**

- `crates/leaf-grpc/Cargo.toml` — add `[dev-dependencies]` (tonic, prost, leaf-boot, leaf-tokio, leaf-serde, leaf-web-hyper, tokio, http) + `[build-dependencies] leaf-grpc-build` + the `tonic`/`prost-build` workspace pins in the root `Cargo.toml`.
- `Cargo.toml` (root) — pin `tonic`, `prost`, `prost-build`, `protox` versions centrally; add the `leaf-starter-grpc` BOM row.
- `crates/leaf/Cargo.toml` — add the `grpc` capability feature (`dep:leaf-starter-grpc`).
- `crates/leaf/src/lib.rs` — re-export `leaf::grpc` + the macro-referenced `::leaf_grpc::` surface at the root (under `#[cfg(feature = "grpc")]`).
- `crates/leaf/src/prelude.rs` — add the `grpc` prelude block (`grpc_controller`, `Status`, `Code`, `Streaming`).
- `crates/leaf/src/forcelink.rs` — add the `grpc` capability to `participating_crates()` + the force-link delta.
- `examples/storefront/Cargo.toml` — add the `grpc` feature, the `[build-dependencies] leaf-grpc-build`, and the tonic/prost dev-deps.
- `examples/storefront/src/lib.rs` — add the `grpc` facade alias (`extern crate leaf as leaf_grpc;`) + `#[cfg(feature = "grpc")] pub mod grpc;`.

---

### Task 6.1: Pin the gRPC test/build dependencies centrally

The integration tests use `tonic` (the canonical gRPC client) and `prost` (the message codec) as dev-deps; `leaf-grpc-build` is the build-dep that compiles the test `.proto`. Pin every external version in the workspace BOM so there is exactly one copy (the single-`leaf-core` invariant, extended to the gRPC codec deps).

**Files:** `Cargo.toml` (root), `crates/leaf-grpc/Cargo.toml`

- [ ] **Step 1: Add the gRPC external pins + the starter BOM row to the root `Cargo.toml`.** Under `[workspace.dependencies]`, after the existing `reqwest` pin, add the codec + client + build pins and the new starter:
  ```toml
  # The gRPC STACK starter (sub-project B), pinned in the internal BOM like every leaf-* crate.
  leaf-starter-grpc = { path = "crates/leaf-starter-grpc", version = "0.1.0" }
  leaf-grpc = { path = "crates/leaf-grpc", version = "0.1.0" }
  leaf-grpc-build = { path = "crates/leaf-grpc-build", version = "0.1.0" }

  # The protobuf MESSAGE codec (the serde_json analogue, confined to leaf-grpc's ProstCodec)
  # + its build-time companions (protox = pure-Rust .proto parse, NO protoc binary; prost-build
  # = message structs). prost-build/protox are build-deps only; prost is the runtime codec.
  prost = "0.14"
  prost-build = "0.14"
  protox = "0.9"
  # The CANONICAL gRPC stack, used ONLY as a dev-dependency test client (the polyglot-interop
  # proof): tonic GETs/streams a booted leaf #[grpc_controller] over real H2 on localhost. It
  # NEVER enters the shipped dep graph — leaf owns its own gRPC engine (Status/framing/dispatch).
  tonic = { version = "0.14", default-features = false, features = ["transport", "codegen", "prost"] }
  ```
- [ ] **Step 2: Add the dev-/build-deps to `crates/leaf-grpc/Cargo.toml`.** The integration test boots a real app in-process (leaf-boot + leaf-tokio + the hyper backend + the JSON converter so HTTP-on-the-same-port works) and drives it with tonic; the build script compiles the test `.proto`:
  ```toml
  [build-dependencies]
  # Compile tests/proto/echo.proto → OUT_DIR (prost messages + the leaf service trait + path
  # constants the #[grpc_controller] consumes). protox (pure-Rust) means NO protoc system binary.
  leaf-grpc-build.workspace = true

  [dev-dependencies]
  # The integration proof boots the shared hyper WebServer in-process and drives it with tonic.
  leaf-boot.workspace = true
  leaf-tokio.workspace = true
  leaf-web-hyper.workspace = true
  leaf-serde.workspace = true
  leaf-macros.workspace = true
  prost.workspace = true
  tonic.workspace = true
  tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "time"] }
  http.workspace = true
  futures.workspace = true
  ```
- [ ] **Step 3: Run `cargo metadata` to confirm the manifests parse and the BOM resolves a single copy.**
  ```
  cargo metadata --format-version 1 -q >/dev/null && echo "manifests OK"
  ```
  Expected: `manifests OK` (no version-conflict errors).
- [ ] **Step 4: Commit.**
  ```
  git add Cargo.toml crates/leaf-grpc/Cargo.toml
  git commit -m "leaf-grpc: pin tonic/prost test+build deps in the workspace BOM

  tonic is a DEV-ONLY canonical-gRPC test client; prost is the confined message codec;
  leaf-grpc-build is the proto build-dep. No external gRPC dep enters the shipped graph.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.2: The integration test `.proto` + its build script

A real `.proto` exercising all four call shapes plus an explicit-error RPC, compiled into the test crate via `leaf_grpc_build::compile` (Stage 3's signature). `leaf_grpc::include_proto!("echo")` brings the generated service trait + path constants into the test.

**Files:** `crates/leaf-grpc/tests/proto/echo.proto`, `crates/leaf-grpc/tests/build.rs`

- [ ] **Step 1: Write `tests/proto/echo.proto` — one service, the four shapes + an error RPC.**
  ```proto
  syntax = "proto3";
  package echo;

  message EchoRequest  { string text = 1; }
  message EchoReply    { string text = 1; }
  message CountReply   { uint32 n = 1; }

  service Echo {
    // unary
    rpc Unary (EchoRequest) returns (EchoReply);
    // server-streaming: replies one EchoReply per word in `text`
    rpc ServerStream (EchoRequest) returns (stream EchoReply);
    // client-streaming: counts the inbound messages, replies the count
    rpc ClientStream (stream EchoRequest) returns (CountReply);
    // bidi: echoes each inbound message back, upper-cased
    rpc Bidi (stream EchoRequest) returns (stream EchoReply);
    // an RPC whose handler ALWAYS returns an explicit Status (the error proof)
    rpc Boom (EchoRequest) returns (EchoReply);
    // an RPC that raises a domain LeafError the GrpcStatusMapper maps to a Status
    rpc Domain (EchoRequest) returns (EchoReply);
  }
  ```
- [ ] **Step 2: Write `tests/build.rs` — compile it via the Stage-3 helper.** The `compile(protos, includes)` signature is verbatim from the shared contract.
  ```rust
  fn main() -> std::io::Result<()> {
      // protox parse -> FileDescriptorSet -> prost-build messages + the leaf service-trait
      // generator; output to OUT_DIR. NO protoc binary involved.
      leaf_grpc_build::compile(&["tests/proto/echo.proto"], &["tests/proto"])
  }
  ```
- [ ] **Step 3: Run the build script in isolation by triggering a test-target compile (no tests yet — just prove codegen runs).**
  ```
  cargo build -p leaf-grpc --tests 2>&1 | tail -20
  ```
  Expected: it compiles the build script and emits `$OUT_DIR/echo.rs` (the build fails only on the not-yet-written `serves_grpc.rs`/`echo_controller.rs`, NOT on codegen). Confirm the generated file exists:
  ```
  find target -path '*leaf-grpc*/out/echo.rs' -print -quit
  ```
  Expected: a path is printed (the generated `echo::echo_server::Echo` trait + `echo::ECHO_*` path constants exist).
- [ ] **Step 4: Commit.**
  ```
  git add crates/leaf-grpc/tests/proto/echo.proto crates/leaf-grpc/tests/build.rs
  git commit -m "leaf-grpc tests: echo.proto (all 4 call shapes + error RPCs) + build.rs

  Compiled via leaf_grpc_build::compile (protox, no protoc); generated into OUT_DIR and
  reached by leaf_grpc::include_proto!(\"echo\").

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.3: The test `#[grpc_controller]` + auth `WebFilter` + domain `GrpcStatusMapper`

The `#[cfg(test)]` beans the proof drives: a `#[grpc_controller]` implementing the generated `echo::echo_server::Echo` trait across all four shapes (plus the explicit-`Status` `Boom` and the domain-error `Domain`), an auth `WebFilter` that rejects calls lacking an `x-api-key` metadata header (proving gRPC metadata = H2 headers run the shared filter chain), and a domain `GrpcStatusMapper` mapping the storefront-style `Integration { kind_id }` to `Code::NotFound`. EVERY one is a stereotype bean — no hand-rolled `GrpcRoute`/`GrpcHandler`/`ProtocolDispatch`.

**Files:** `crates/leaf-grpc/tests/echo_controller.rs`

- [ ] **Step 1: Write the module skeleton — the generated trait include + the controller bean shell.** This is shared (`mod echo_controller;`) by `serves_grpc.rs`; it lives as its own file so both targets compile it once.
  ```rust
  //! The `#[cfg(test)]` gRPC beans the integration proof boots: a #[grpc_controller]
  //! implementing the generated `echo::echo_server::Echo` trait across all four call shapes,
  //! an auth WebFilter (metadata = H2 headers), and a domain GrpcStatusMapper. EVERY one is a
  //! stereotype bean — the ONLY hand-written impl is the controller's TYPED method bodies,
  //! which the macro lowers to GrpcRoute beans (no hand-rolled GrpcRoute/GrpcHandler).

  use std::sync::atomic::{AtomicU32, Ordering};

  use leaf_core::{BoxFuture, BoxStream, ContractId, LeafError};
  use leaf_grpc::{Code, GrpcStatusMapper, Status, Streaming};
  use leaf_web::filter::Next;
  use leaf_web::{Request, Response, WebFilter};
  use futures::StreamExt;

  // The generated server trait + path constants (echo::echo_server::Echo, echo::*Request, ...).
  leaf_grpc::include_proto!("echo");

  /// The domain ContractId the `Domain` RPC raises (the sanctioned Integration error channel),
  /// mirroring the storefront's `unknown_sku_kind` — the GrpcStatusMapper claims it as NotFound.
  pub fn missing_kind() -> ContractId {
      ContractId::of("leaf_grpc::tests::Missing")
  }

  /// Counts WebFilter invocations so the proof can assert the filter ran around gRPC too.
  pub static FILTER_CALLS: AtomicU32 = AtomicU32::new(0);
  ```
- [ ] **Step 2: Write the `#[grpc_controller]` bean + its dual-form struct/impl across all four shapes.** Uses `leaf_macros::grpc_controller` (Stage 4) — the struct is the `#[component]`-family bean, the impl lowers each method to a `GrpcRoute` bean. Plain `async fn`s; the macro desugars async (no `#[async_impl]`). The four method signatures are VERBATIM from the shared contract.
  ```rust
  /// The controller bean (a #[component]-family bean; no collaborators for the engine proof).
  #[leaf_macros::grpc_controller]
  #[derive(Default)]
  pub struct EchoController;

  #[leaf_macros::grpc_controller]
  impl echo::echo_server::Echo for EchoController {
      // unary:  async fn m(&self, req: T) -> Result<U, Status>
      async fn unary(&self, req: echo::EchoRequest) -> Result<echo::EchoReply, Status> {
          Ok(echo::EchoReply { text: req.text })
      }

      // server: async fn m(&self, req: T) -> Result<Streaming<U>, Status>
      async fn server_stream(
          &self,
          req: echo::EchoRequest,
      ) -> Result<Streaming<echo::EchoReply>, Status> {
          let words: Vec<echo::EchoReply> = req
              .text
              .split_whitespace()
              .map(|w| echo::EchoReply { text: w.to_string() })
              .collect();
          let stream: BoxStream<'static, Result<echo::EchoReply, Status>> =
              Box::pin(futures::stream::iter(words.into_iter().map(Ok)));
          Ok(Streaming::new(stream))
      }

      // client: async fn m(&self, req: Streaming<T>) -> Result<U, Status>
      async fn client_stream(
          &self,
          mut req: Streaming<echo::EchoRequest>,
      ) -> Result<echo::CountReply, Status> {
          let mut n = 0u32;
          while let Some(item) = req.next().await {
              item?; // a malformed inbound frame surfaces as a Status
              n += 1;
          }
          Ok(echo::CountReply { n })
      }

      // bidi:   async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>
      async fn bidi(
          &self,
          req: Streaming<echo::EchoRequest>,
      ) -> Result<Streaming<echo::EchoReply>, Status> {
          let out: BoxStream<'static, Result<echo::EchoReply, Status>> = Box::pin(req.map(|item| {
              item.map(|r| echo::EchoReply { text: r.text.to_uppercase() })
          }));
          Ok(Streaming::new(out))
      }

      // explicit Status: the handler returns Err(Status) directly (rendered as trailers).
      async fn boom(&self, _req: echo::EchoRequest) -> Result<echo::EchoReply, Status> {
          Err(Status::invalid_argument("boom: explicit status from the handler"))
      }

      // domain error: raise a LeafError on the Integration channel; the GrpcStatusMapper maps it.
      async fn domain(&self, _req: echo::EchoRequest) -> Result<echo::EchoReply, Status> {
          Err(Status::from(LeafError::new(leaf_core::ErrorKind::Integration {
              kind_id: missing_kind(),
          })))
      }
  }
  ```
  Note: `Status::from(LeafError)` is the codec edge inside leaf-grpc; if Stage 2/5 spelled the LeafError→Status raise differently (e.g. a `?` through a `From<LeafError>` impl), this body uses whatever Stage 2 defined — but the contract's `Status` ctors (`invalid_argument`/`not_found`/etc.) are used verbatim above.
- [ ] **Step 3: Write the auth `WebFilter` (metadata = H2 headers).** A `#[web_filter]` `#[component]` — the SAME filter chain HTTP uses; it short-circuits a gRPC call lacking `x-api-key`. Per the design (§6) a filter that rejects a gRPC request returns an `Err`/`Response` the gRPC edge renders as a `Status`.
  ```rust
  /// An auth WebFilter: requires an `x-api-key: secret` metadata header (gRPC metadata ARE
  /// H2 headers, so the SAME filter chain wraps HTTP + gRPC). Missing/wrong key → an Err the
  /// gRPC edge renders as an Unauthenticated Status trailer; a present key continues the chain.
  #[leaf_macros::web_filter]
  #[derive(Default)]
  pub struct ApiKeyFilter;

  #[leaf_macros::async_impl]
  impl WebFilter for ApiKeyFilter {
      async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
          FILTER_CALLS.fetch_add(1, Ordering::SeqCst);
          let ok = req
              .headers()
              .get("x-api-key")
              .and_then(|v| v.to_str().ok())
              .is_some_and(|k| k == "secret");
          if ok {
              next.run(req).await
          } else {
              // The sanctioned domain-error channel: the gRPC edge maps it via the
              // GrpcStatusMapper below; for HTTP the ControlAdvice chain would map it.
              Err(LeafError::new(leaf_core::ErrorKind::Integration { kind_id: unauthorized_kind() }))
          }
      }
  }

  /// The ContractId the missing-key rejection raises; the GrpcStatusMapper maps it to Unauthenticated.
  pub fn unauthorized_kind() -> ContractId {
      ContractId::of("leaf_grpc::tests::Unauthorized")
  }
  ```
- [ ] **Step 4: Write the domain `GrpcStatusMapper` (`#[auto_config]`-style user bean).** Per the contract `dyn GrpcStatusMapper` is collection-injected exactly like `ControlAdvice`; a user mapper claims its kinds, otherwise the `DefaultGrpcStatusMapper` FALLBACK handles framework kinds. This is a `#[component]` providing the `dyn GrpcStatusMapper` view (NO hand-rolled registration).
  ```rust
  /// A domain GrpcStatusMapper: maps the test's two Integration kinds to gRPC Codes (the
  /// ControlAdvice analogue for gRPC). It is a #[component] publishing the dyn GrpcStatusMapper
  /// view — the SAME collection-injection DI the default FALLBACK mapper rides; first-Some wins.
  #[leaf_macros::component(provides = dyn GrpcStatusMapper)]
  #[derive(Default)]
  pub struct EchoStatusMapper;

  impl GrpcStatusMapper for EchoStatusMapper {
      fn map(&self, err: &LeafError) -> Option<Status> {
          match err.kind {
              leaf_core::ErrorKind::Integration { kind_id } if kind_id == missing_kind() => {
                  Some(Status::not_found("no such echo resource"))
              }
              leaf_core::ErrorKind::Integration { kind_id } if kind_id == unauthorized_kind() => {
                  Some(Status::new(Code::Unauthenticated, "missing or invalid api key"))
              }
              _ => None,
          }
      }
  }
  ```
- [ ] **Step 5: Confirm it compiles standalone (the macros expand; trait shapes match the generated trait).**
  ```
  cargo build -p leaf-grpc --tests 2>&1 | tail -25
  ```
  Expected: compiles cleanly (the only remaining missing file is `serves_grpc.rs`, written next — if `cargo build --tests` needs every test file, accept the `serves_grpc.rs not found`-style error and proceed; the codegen-trait-shape errors must be GONE).
- [ ] **Step 6: Commit.**
  ```
  git add crates/leaf-grpc/tests/echo_controller.rs
  git commit -m "leaf-grpc tests: #[grpc_controller] Echo (4 shapes) + auth WebFilter + domain GrpcStatusMapper

  All stereotype beans (no hand-rolled GrpcRoute/GrpcHandler/ProtocolDispatch); proves
  metadata=H2-headers filter reuse and the LeafError->Status domain channel.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.4: The tonic integration proof — all four shapes + same-port HTTP

The headline. Boot the shared hyper `WebServer` in-process (the same `Dispatcher`/`KeepAlive` from sub-project A, now serving H2 + the gRPC `ProtocolDispatch` branch), then call it with a real `tonic` client across all four shapes, with the auth key set. Reuses the EXACT in-process boot shape from `auto_config_server.rs` (Application + spawner + drain-sleeper + `--leaf.web.server.port`). Same-port HTTP is proven by hitting the JSON converter / 404 path on the SAME socket.

**Files:** `crates/leaf-grpc/tests/serves_grpc.rs`

- [ ] **Step 1: Write the test prelude — modules, force-link, the boot helper, the tonic client builder.** The boot helper mirrors `boot_web_app` in `auto_config_server.rs` (the verbatim Application/spawner/drain-sleeper shape); `force_link` pins the hyper backend's FALLBACK `dyn WebServer` + the JSON converter + the gRPC `GrpcDispatch` rows (anti-DCE).
  ```rust
  //! The gRPC INTEGRATION PROOF: the shared hyper WebServer boots in-process with H2 enabled,
  //! the gRPC ProtocolDispatch branch routes `application/grpc` calls to the #[grpc_controller]'s
  //! GrpcRoute beans, and a REAL tonic client (dev-dep) drives all four call shapes + an explicit
  //! Status + a domain-error Status + a metadata-auth WebFilter — with HTTP and gRPC on the SAME
  //! port. The canonical-gRPC-stack interop proof; leaf names no tonic/hyper above the backend.

  mod echo_controller;
  use echo_controller::{missing_kind as _missing_kind, FILTER_CALLS};

  use std::sync::atomic::Ordering;
  use std::sync::Arc;

  use leaf_boot::{Application, RunOverlay, SealInputs};
  use futures::StreamExt;

  // The tonic-generated client for echo.proto, compiled from the SAME .proto by tonic's own
  // codegen (the polyglot interop point: leaf's server trait + tonic's client trait, one wire).
  pub mod echo_tonic {
      tonic::include_proto!("echo");
  }
  use echo_tonic::echo_client::EchoClient;

  fn free_port() -> u16 {
      std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
  }

  /// Pin the link rows the boot needs: the hyper FALLBACK dyn WebServer + JSON converter, and
  /// the leaf-grpc GrpcDispatch (the dyn ProtocolDispatch the Dispatcher collection-injects) +
  /// the DefaultGrpcStatusMapper FALLBACK. Referencing a TypeId per crate forces the rlib in.
  fn force_link() {
      let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
      let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
      let _ = std::any::TypeId::of::<leaf_grpc::GrpcDispatch>();
      let _ = std::any::TypeId::of::<leaf_grpc::DefaultGrpcStatusMapper>();
  }

  async fn boot(port: u16) -> leaf_boot::RunningApp {
      force_link();
      let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
      Application::new()
          .with_name("grpc-integration")
          .with_spawner(spawner)
          .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
          .run(
              SealInputs::new().with_args([format!("--leaf.web.server.port={port}")]),
              RunOverlay::none(),
          )
          .await
          .expect("the grpc app boots to Ready")
  }

  async fn wait_until_up(port: u16) {
      for _ in 0..400 {
          if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
              return;
          }
          tokio::time::sleep(std::time::Duration::from_millis(10)).await;
      }
      panic!("the grpc server never came up");
  }

  /// Build a tonic client with the auth metadata key set on every request (gRPC metadata =
  /// H2 headers, which the ApiKeyFilter checks). Uses an interceptor so all 4 shapes carry it.
  async fn client(
      port: u16,
  ) -> EchoClient<tonic::service::interceptor::InterceptedService<
      tonic::transport::Channel,
      impl Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone,
  >> {
      let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
          .unwrap()
          .connect()
          .await
          .expect("tonic connects to the leaf server");
      EchoClient::with_interceptor(channel, |mut req: tonic::Request<()>| {
          req.metadata_mut().insert("x-api-key", "secret".parse().unwrap());
          Ok(req)
      })
  }
  ```
- [ ] **Step 2: Write the four-shape test (the failing test).**
  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn tonic_drives_all_four_call_shapes_against_the_leaf_grpc_controller() {
      let port = free_port();
      let running = boot(port).await;
      wait_until_up(port).await;
      let mut c = client(port).await;

      // 1. UNARY: req.text echoed back.
      let reply = c
          .unary(tonic::Request::new(echo_tonic::EchoRequest { text: "hi".into() }))
          .await
          .expect("unary call")
          .into_inner();
      assert_eq!(reply.text, "hi");

      // 2. SERVER-STREAM: one reply per word.
      let mut stream = c
          .server_stream(tonic::Request::new(echo_tonic::EchoRequest { text: "a b c".into() }))
          .await
          .expect("server-stream call")
          .into_inner();
      let mut words = Vec::new();
      while let Some(item) = stream.next().await {
          words.push(item.expect("server-stream item").text);
      }
      assert_eq!(words, vec!["a", "b", "c"]);

      // 3. CLIENT-STREAM: the server counts the inbound messages.
      let outbound = futures::stream::iter(vec![
          echo_tonic::EchoRequest { text: "x".into() },
          echo_tonic::EchoRequest { text: "y".into() },
          echo_tonic::EchoRequest { text: "z".into() },
      ]);
      let count = c
          .client_stream(tonic::Request::new(outbound))
          .await
          .expect("client-stream call")
          .into_inner();
      assert_eq!(count.n, 3);

      // 4. BIDI: each inbound message echoed back upper-cased.
      let outbound = futures::stream::iter(vec![
          echo_tonic::EchoRequest { text: "foo".into() },
          echo_tonic::EchoRequest { text: "bar".into() },
      ]);
      let mut stream = c
          .bidi(tonic::Request::new(outbound))
          .await
          .expect("bidi call")
          .into_inner();
      let mut got = Vec::new();
      while let Some(item) = stream.next().await {
          got.push(item.expect("bidi item").text);
      }
      assert_eq!(got, vec!["FOO", "BAR"]);

      let _ = running.shutdown().await;
  }
  ```
- [ ] **Step 3: Run it — RED (the engine/dispatch/codegen must carry the four shapes end to end).**
  ```
  cargo test -p leaf-grpc --test serves_grpc tonic_drives_all_four -- --nocapture
  ```
  Expected initially: FAIL only if a Stage 1–4 seam is incomplete; if Stages 1–4 landed correctly this is the first end-to-end exercise of them. Drive any failure through `superpowers:systematic-debugging` (the fix belongs in the failing stage's crate, not in the test).
- [ ] **Step 4: Make it GREEN.** No new production code should be needed (Stages 1–4 own the engine); if a real gap surfaces, fix it in the OWNING crate (leaf-web `Body`/`Dispatcher`, leaf-grpc framing/`GrpcDispatch`, the codegen lowering), keeping the backend-free + no-type-name-detection constraints. Re-run:
  ```
  cargo test -p leaf-grpc --test serves_grpc tonic_drives_all_four -- --nocapture
  ```
  Expected: `test tonic_drives_all_four_call_shapes_against_the_leaf_grpc_controller ... ok`.
- [ ] **Step 5: Commit.**
  ```
  git add crates/leaf-grpc/tests/serves_grpc.rs
  git commit -m "leaf-grpc: tonic drives all 4 call shapes against a leaf #[grpc_controller]

  Polyglot interop end to end: the canonical gRPC client over real H2 against leaf's own
  gRPC engine (unary/server/client/bidi), in-process on the shared hyper WebServer.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.5: The error + filter + same-port proofs

The remaining integration assertions: an explicit `Status` (the `Boom` RPC) lands as a `grpc-status` trailer the tonic client reads as a `tonic::Status`; a domain `LeafError` (the `Domain` RPC) is mapped by the `GrpcStatusMapper` to `Code::NotFound`; the auth `WebFilter` rejects a metadata-less call as `Unauthenticated` AND ran around the gRPC calls; and HTTP + gRPC are served on the SAME port (a plain HTTP request to that port gets a clean HTTP 404 from the HTTP `Route` family, not a gRPC frame).

**Files:** `crates/leaf-grpc/tests/serves_grpc.rs` (add tests)

- [ ] **Step 1: Write the explicit-Status + domain-Status test (RED).**
  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn status_errors_ride_back_as_grpc_status_trailers() {
      let port = free_port();
      let running = boot(port).await;
      wait_until_up(port).await;
      let mut c = client(port).await;

      // Explicit Status: the handler returns Err(Status::invalid_argument(..)) → tonic sees
      // Code::InvalidArgument with the message, NOT a transport error.
      let err = c
          .boom(tonic::Request::new(echo_tonic::EchoRequest { text: "x".into() }))
          .await
          .expect_err("boom returns a Status, not Ok");
      assert_eq!(err.code(), tonic::Code::InvalidArgument);
      assert!(err.message().contains("boom"), "the explicit status message rode the trailer: {}", err.message());

      // Domain LeafError mapped by the GrpcStatusMapper (Integration{missing_kind} -> NotFound).
      let err = c
          .domain(tonic::Request::new(echo_tonic::EchoRequest { text: "x".into() }))
          .await
          .expect_err("domain raises a mapped Status");
      assert_eq!(err.code(), tonic::Code::NotFound, "the domain error channel mapped to NotFound");

      let _ = running.shutdown().await;
  }
  ```
- [ ] **Step 2: Write the metadata-auth filter test (RED).** A client WITHOUT the interceptor key is rejected `Unauthenticated`; and the filter counter advanced (proving the WebFilter chain wrapped gRPC).
  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_metadata_auth_webfilter_runs_around_grpc() {
      let port = free_port();
      let running = boot(port).await;
      wait_until_up(port).await;
      FILTER_CALLS.store(0, Ordering::SeqCst);

      // A NO-KEY client: the ApiKeyFilter short-circuits with the Unauthorized domain error,
      // which the GrpcStatusMapper renders as a Code::Unauthenticated trailer (NOT a raw HTTP body).
      let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
          .unwrap()
          .connect()
          .await
          .expect("connect");
      let mut bare = EchoClient::new(channel);
      let err = bare
          .unary(tonic::Request::new(echo_tonic::EchoRequest { text: "hi".into() }))
          .await
          .expect_err("a keyless gRPC call is rejected by the WebFilter");
      assert_eq!(err.code(), tonic::Code::Unauthenticated, "the filter rejection is a gRPC Status, not an HTTP body");

      // The WITH-KEY client succeeds — and the filter ran around BOTH calls (the same chain HTTP uses).
      let mut c = client(port).await;
      let ok = c
          .unary(tonic::Request::new(echo_tonic::EchoRequest { text: "hi".into() }))
          .await
          .expect("authed call passes the filter")
          .into_inner();
      assert_eq!(ok.text, "hi");
      assert!(FILTER_CALLS.load(Ordering::SeqCst) >= 2, "the WebFilter wrapped the gRPC calls");

      let _ = running.shutdown().await;
  }
  ```
- [ ] **Step 3: Write the same-port HTTP+gRPC test (RED).** A plain HTTP GET to the SAME socket gets a clean HTTP 404 (the HTTP `Route` family answers — content-type is not `application/grpc`, so the `ProtocolDispatch` branch declines and the HTTP path runs). Uses a bare `tokio` TCP + a minimal HTTP/1 request so the test names no second HTTP-client dep beyond what the crate has; if `reqwest` is preferred, add it to the dev-deps in Task 6.1.
  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn http_and_grpc_share_one_port() {
      use tokio::io::{AsyncReadExt, AsyncWriteExt};
      let port = free_port();
      let running = boot(port).await;
      wait_until_up(port).await;

      // gRPC works on the port (content-type application/grpc → the gRPC ProtocolDispatch branch).
      let mut c = client(port).await;
      let reply = c
          .unary(tonic::Request::new(echo_tonic::EchoRequest { text: "same-port".into() }))
          .await
          .expect("grpc on the shared port")
          .into_inner();
      assert_eq!(reply.text, "same-port");

      // A PLAIN HTTP/1 GET to the SAME socket → the HTTP Route family answers (no grpc content-type),
      // a clean HTTP 404 (an unmatched HTTP route), NOT a gRPC frame. Proves one port, two protocols.
      let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("tcp");
      sock.write_all(b"GET /not-a-route HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
          .await
          .expect("write http request");
      let mut buf = Vec::new();
      sock.read_to_end(&mut buf).await.expect("read http response");
      let head = String::from_utf8_lossy(&buf);
      assert!(head.starts_with("HTTP/1.1 404"), "plain HTTP on the shared port is an HTTP 404, got: {}", &head[..head.len().min(40)]);

      let _ = running.shutdown().await;
  }
  ```
- [ ] **Step 4: Run the three new tests — drive to GREEN.**
  ```
  cargo test -p leaf-grpc --test serves_grpc -- --nocapture
  ```
  Expected: all four `serves_grpc` tests pass:
  ```
  test tonic_drives_all_four_call_shapes_against_the_leaf_grpc_controller ... ok
  test status_errors_ride_back_as_grpc_status_trailers ... ok
  test a_metadata_auth_webfilter_runs_around_grpc ... ok
  test http_and_grpc_share_one_port ... ok
  ```
  Any failure → `superpowers:systematic-debugging`; fix in the owning crate (the trailer-rendering belongs to leaf-grpc's `GrpcHandler`; the same-port branch to leaf-web's `Dispatcher`; the filter-rejection-to-Status to the gRPC edge).
- [ ] **Step 5: Commit.**
  ```
  git add crates/leaf-grpc/tests/serves_grpc.rs
  git commit -m "leaf-grpc: explicit+domain Status trailers, metadata-auth WebFilter, same-port HTTP+gRPC

  Completes the integration proof: gRPC errors ride grpc-status trailers (explicit + the
  GrpcStatusMapper domain channel), the shared WebFilter chain wraps gRPC via H2-header
  metadata, and one socket serves both protocols.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.6: The `leaf-starter-grpc` STACK starter + the `grpc` capability feature

The dogfood needs an umbrella-only path: a `grpc` capability feature on `leaf` that pulls a `leaf-starter-grpc` bundle (the gRPC analogue of `leaf-starter-web`). This is the curated additive bundle (`leaf-grpc` + the web stack with `http2` + the runtime peer), wired into the umbrella's prelude/root re-exports/force-link exactly like `web`.

**Files:** `crates/leaf-starter-grpc/Cargo.toml`, `crates/leaf-starter-grpc/src/lib.rs`, `crates/leaf/Cargo.toml`, `crates/leaf/src/lib.rs`, `crates/leaf/src/prelude.rs`, `crates/leaf/src/forcelink.rs`

- [ ] **Step 1: Write `crates/leaf-starter-grpc/Cargo.toml`.** It bundles `leaf-grpc` ON TOP of the web stack (gRPC rides the shared `WebServer`), so it pulls `leaf-starter-web` (the http2-enabled hyper backend + the JSON converter + runtime) plus `leaf-grpc`.
  ```toml
  [package]
  name = "leaf-starter-grpc"
  version.workspace = true
  edition.workspace = true
  license.workspace = true

  # STACK starter — the gRPC bundle. gRPC rides the SHARED hyper WebServer, so this is the
  # web stack (the http2-enabled backend + the JSON converter for same-port HTTP) PLUS the
  # leaf-grpc engine (Status/framing/dispatch + the GrpcDispatch ProtocolDispatch bean + the
  # DefaultGrpcStatusMapper FALLBACK). Each crate's auto-config participates + backs off.
  [dependencies]
  leaf-grpc.workspace = true
  leaf-starter-web.workspace = true
  ```
- [ ] **Step 2: Write `crates/leaf-starter-grpc/src/lib.rs`.** Re-export the bundle flat (mirrors `leaf-starter-web`'s shape), `#![no_std]`.
  ```rust
  //! `leaf-starter-grpc` — a STACK starter (aggregator), the gRPC bundle.
  //!
  //! gRPC is a SECOND Handler family on the shared hyper WebServer (one server, one port),
  //! so this bundle is the web stack (the http2-enabled hyper backend + the JSON converter
  //! for same-port HTTP) PLUS the leaf-grpc engine: Status/Code, the length-prefix framing,
  //! Streaming<T>, the GrpcDispatch (the dyn ProtocolDispatch the Dispatcher routes
  //! `application/grpc` to), and the DefaultGrpcStatusMapper FALLBACK. Each crate's
  //! auto-config participates + backs off independently.
  //!
  //! Like every starter, it depends only on its constituents and NEVER on the `leaf`
  //! umbrella (the unique DAG sink). The umbrella's `grpc` capability feature `dep:`-pulls
  //! this crate into the force-link / ExpectedManifest participating set.

  #![no_std]
  #![deny(unsafe_code)]
  #![warn(missing_docs)]

  #[doc(no_inline)]
  pub use leaf_grpc;
  #[doc(no_inline)]
  pub use leaf_starter_web;
  ```
- [ ] **Step 3: Add the `grpc` capability feature to `crates/leaf/Cargo.toml`.** Under `[features]`, after `web`:
  ```toml
  # `grpc` — the STACK starter (the gRPC bundle: leaf-grpc + the shared http2 web stack).
  # gRPC rides the same WebServer/port, so enabling `grpc` implies the web stack.
  grpc = ["dep:leaf-starter-grpc", "web"]
  ```
  And under `[dependencies]`, after `leaf-starter-web`:
  ```toml
  leaf-starter-grpc = { workspace = true, optional = true }
  ```
- [ ] **Step 4: Wire the umbrella root re-exports in `crates/leaf/src/lib.rs`.** Add the `grpc` root re-export (next to `pub use leaf_starter_web as web;`) and the macro-referenced `::leaf_grpc::` surface (next to the `#[cfg(feature = "web")]` `leaf_web` root re-export block):
  ```rust
  /// The gRPC STACK starter (present iff the `grpc` feature is enabled). The `dep:`-hidden
  /// edge pulls the gRPC bundle into the participating set; reached as `leaf::grpc`.
  #[cfg(feature = "grpc")]
  #[doc(no_inline)]
  pub use leaf_starter_grpc as grpc;

  // The leaf-grpc macro surface the #[grpc_controller] codegen emits `::leaf_grpc::` paths
  // into — re-exported AT THE UMBRELLA ROOT so the facade alias `extern crate leaf as
  // leaf_grpc;` resolves `::leaf_grpc::GrpcRoute` to `leaf::GrpcRoute`, exactly like the
  // `leaf_web` re-exports. Only the EXACT symbols the macro references are listed.
  #[cfg(feature = "grpc")]
  #[doc(hidden)]
  pub use leaf_starter_grpc::leaf_grpc::{
      decode_frames, encode_frame, Code, GrpcCodec, GrpcDispatch, GrpcHandler, GrpcRoute,
      GrpcStatusMapper, ProstCodec, Status, Streaming,
  };
  ```
- [ ] **Step 5: Add the `grpc` prelude block to `crates/leaf/src/prelude.rs`** (after the `#[cfg(feature = "web")]` blocks):
  ```rust
  // ── the gRPC capability surface (present iff the `grpc` feature pulled the bundle) ──
  //
  // The #[grpc_controller] stereotype + the leaf-grpc types a controller method names in its
  // signatures (`Status`/`Code` for the error model, `Streaming` for the streaming shapes).
  // The macro emits absolute `::leaf_grpc::` paths resolved through the facade alias; these
  // prelude names are what the USER writes.
  #[cfg(feature = "grpc")]
  #[doc(no_inline)]
  pub use leaf_macros::grpc_controller;
  #[cfg(feature = "grpc")]
  #[doc(no_inline)]
  pub use leaf_starter_grpc::leaf_grpc::{Code, Status, Streaming};
  ```
- [ ] **Step 6: Wire the force-link / ExpectedManifest in `crates/leaf/src/forcelink.rs`.** Mirror the `web` arms: add the `grpc`-gated `use leaf_starter_grpc as _;` to the force-link module and the gRPC crate's `SourceTag`(s) to `participating_crates()`. The `grpc` feature implies `web`, so only the NET-NEW SourceTag (`leaf-grpc`) is added under a `grpc` gate (the web rows already land via the implied `web` feature):
  ```rust
  // (in the force-link module, alongside `#[cfg(feature = "web")] pub(crate) use leaf_starter_web as _;`)
  #[cfg(feature = "grpc")]
  pub(crate) use leaf_starter_grpc as _;
  ```
  ```rust
  // (in participating_crates(), the net-new SourceTag the `grpc` capability adds — leaf-grpc
  // declare_source!s its tag; the web-bundle rows arrive via the implied `web` feature)
  #[cfg(feature = "grpc")]
  const GRPC: &[&str] = &["leaf-grpc"];
  #[cfg(not(feature = "grpc"))]
  const GRPC: &[&str] = &[];
  ```
  (Append `GRPC` to the same concat the existing `WEB` rows feed.)
- [ ] **Step 7: Add `leaf-starter-grpc` to the root BOM `[workspace.dependencies]`** — already pinned in Task 6.1 Step 1. Build the umbrella with the new feature to prove the wiring:
  ```
  cargo build -p leaf --features grpc 2>&1 | tail -20
  ```
  Expected: clean build (the `grpc` feature pulls `leaf-starter-grpc` → `leaf-grpc` + the web stack; the prelude/root re-exports + force-link compile).
- [ ] **Step 8: Commit.**
  ```
  git add crates/leaf-starter-grpc crates/leaf/Cargo.toml crates/leaf/src/lib.rs crates/leaf/src/prelude.rs crates/leaf/src/forcelink.rs Cargo.toml
  git commit -m "leaf: leaf-starter-grpc STACK starter + the grpc umbrella capability feature

  The gRPC bundle (leaf-grpc + the shared http2 web stack), the `grpc` capability (implies
  `web`), the prelude grpc block (grpc_controller/Status/Code/Streaming), the root facade
  re-exports, and the force-link/ExpectedManifest wiring — the umbrella-only gRPC path.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.7: The storefront dogfood `.proto` + build script + `grpc` feature

The dogfood: a `catalog.proto` gRPC service the storefront serves over its OWN umbrella-only `leaf` dependency (the `grpc` capability), exposing the same `CatalogService`/`ProductRepository` domain the HTTP controllers already serve. Compiled by the storefront's `build.rs` via `leaf_grpc_build::compile`.

**Files:** `examples/storefront/proto/catalog.proto`, `examples/storefront/build.rs`, `examples/storefront/Cargo.toml`, `examples/storefront/src/lib.rs`

- [ ] **Step 1: Write `examples/storefront/proto/catalog.proto`.**
  ```proto
  syntax = "proto3";
  package storefront.catalog;

  message GetProductRequest { string sku = 1; }
  message Product { string sku = 1; string name = 2; int64 price_cents = 3; }
  message ListProductsRequest {}

  service Catalog {
    // unary: a known SKU returns its Product; an unknown SKU is a NotFound Status (the
    // unknown-SKU domain channel mapped by the storefront's GrpcStatusMapper).
    rpc GetProduct (GetProductRequest) returns (Product);
    // server-stream: every catalog product, one Product per frame.
    rpc ListProducts (ListProductsRequest) returns (stream Product);
  }
  ```
- [ ] **Step 2: Write `examples/storefront/build.rs`.** Guard on the `grpc` feature via the cargo build-script env (`CARGO_FEATURE_GRPC`) so a non-grpc build skips codegen.
  ```rust
  fn main() -> std::io::Result<()> {
      // Only compile the .proto when the `grpc` capability is enabled (so a plain `web`/`redis`
      // build needs no protobuf toolchain). protox = pure-Rust, NO protoc binary.
      if std::env::var_os("CARGO_FEATURE_GRPC").is_some() {
          leaf_grpc_build::compile(&["proto/catalog.proto"], &["proto"])?;
      }
      Ok(())
  }
  ```
- [ ] **Step 3: Add the `grpc` feature + build-dep + dev-deps to `examples/storefront/Cargo.toml`.** Mirror the `web` feature shape:
  ```toml
  # (under [dependencies] — already names `leaf`; add the grpc capability passthrough below)

  # (build-deps: the proto compiler, present so build.rs can compile catalog.proto)
  [build-dependencies]
  leaf-grpc-build = { path = "../../crates/leaf-grpc-build", version = "0.1.0" }
  ```
  Under `[features]` add (and extend `default` to include `grpc` so `cargo test -p storefront` covers it):
  ```toml
  default = ["redis", "web", "grpc"]
  grpc = ["leaf/grpc"]
  ```
  Under `[dev-dependencies]` add the tonic client + prost + tokio io:
  ```toml
  tonic = { version = "0.14", default-features = false, features = ["transport", "codegen", "prost"] }
  prost = "0.14"
  ```
  (and ensure the existing `tokio` dev-dep features include `io-util` for the same-port raw-socket check if reused; the gRPC dogfood test uses tonic only.)
- [ ] **Step 4: Add the `grpc` facade alias + module to `examples/storefront/src/lib.rs`.** Next to the `#[cfg(feature = "web")] extern crate leaf as leaf_web;`:
  ```rust
  #[cfg(feature = "grpc")]
  #[allow(unused_extern_crates)]
  extern crate leaf as leaf_grpc;
  ```
  And next to `#[cfg(feature = "web")] pub mod web;`:
  ```rust
  /// The gRPC surface (the `grpc` capability feature): a `#[grpc_controller]` over the
  /// catalog domain (the same `CatalogService`/`ProductRepository` the HTTP controllers use),
  /// served over H2 on the SAME embedded server. Present iff the `grpc` feature is enabled.
  #[cfg(feature = "grpc")]
  pub mod grpc;
  ```
- [ ] **Step 5: Build the storefront with the grpc feature to prove the proto compiles in the example.**
  ```
  cargo build -p storefront --features grpc 2>&1 | tail -20
  ```
  Expected: the build script runs `compile` and emits `$OUT_DIR/storefront.catalog.rs` (the build fails next only on the not-yet-written `src/grpc/` module).
- [ ] **Step 6: Commit.**
  ```
  git add examples/storefront/proto examples/storefront/build.rs examples/storefront/Cargo.toml examples/storefront/src/lib.rs
  git commit -m "storefront: catalog.proto + grpc capability + build.rs (dogfood scaffolding)

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.8: The storefront `#[grpc_controller]` dogfood bean

The dogfood controller: a `#[grpc_controller]` over `CatalogService`/`ProductRepository` — the SAME domain the HTTP `CatalogController` serves, now over gRPC on the SAME embedded server. Plus a `GrpcStatusMapper` mapping the existing `unknown_sku_kind()` domain error to `Code::NotFound` (the gRPC analogue of the `StorefrontErrors` `#[control_advice]`). All stereotype beans, umbrella-only (`use leaf::prelude::*;`).

**Files:** `examples/storefront/src/grpc/mod.rs`, `examples/storefront/src/grpc/catalog_controller.rs`

- [ ] **Step 1: Write `examples/storefront/src/grpc/mod.rs`.**
  ```rust
  //! The `grpc` capability module: the storefront's gRPC surface, built ENTIRELY from leaf
  //! stereotypes — zero hand-written GrpcRoute/GrpcHandler/ProtocolDispatch impls.
  //!
  //! - `catalog_controller::CatalogGrpcController` — a #[grpc_controller] over the SAME
  //!   CatalogService + ProductRepository the HTTP CatalogController serves, exposing
  //!   `GetProduct` (unary) and `ListProducts` (server-stream).
  //! - `catalog_controller::StorefrontGrpcErrors` — a GrpcStatusMapper mapping the
  //!   unknown-SKU domain error (`unknown_sku_kind`) to Code::NotFound (the gRPC analogue
  //!   of the StorefrontErrors #[control_advice]).
  //!
  //! Reached umbrella-only through `use leaf::prelude::*;`; the macro-emitted `::leaf_grpc::`
  //! paths resolve through the `extern crate leaf as leaf_grpc;` facade alias in lib.rs.
  pub mod catalog_controller;
  ```
- [ ] **Step 2: Write `examples/storefront/src/grpc/catalog_controller.rs` — the controller + the mapper.** The generated trait + path constants come from the storefront build's `leaf::grpc::include_proto!("storefront.catalog")` (reached through the umbrella's `grpc` re-export); the controller field-injects the SAME `Ref<CatalogService>`/`Ref<ProductRepository>` the HTTP controller uses.
  ```rust
  use leaf::core::BoxStream;
  use leaf::prelude::*;

  use crate::catalog::product::repository::ProductRepository;
  use crate::catalog::service::{unknown_sku_kind, CatalogService};

  // The generated server trait + messages + path constants for storefront.catalog.
  leaf::grpc::leaf_grpc::include_proto!("storefront.catalog");

  /// A #[grpc_controller] over the catalog domain — the SAME CatalogService (cacheable price)
  /// + ProductRepository (the name) the HTTP CatalogController serves, now over gRPC. An
  /// ordinary #[component]-family bean; its RPC methods lower to GrpcRoute beans (no
  /// hand-written GrpcRoute/GrpcHandler). Field injection, exactly like the HTTP controller.
  #[grpc_controller]
  #[derive(Debug)]
  pub struct CatalogGrpcController {
      catalog: Ref<CatalogService>,
      products: Ref<ProductRepository>,
  }

  #[grpc_controller]
  impl storefront::catalog::catalog_server::Catalog for CatalogGrpcController {
      /// `GetProduct` (unary): the cacheable price lookup gates the unknown-SKU error (it
      /// raises the Integration{unknown_sku_kind} LeafError the GrpcStatusMapper maps to
      /// NotFound), then the name from the repository.
      async fn get_product(
          &self,
          req: storefront::catalog::GetProductRequest,
      ) -> Result<storefront::catalog::Product, Status> {
          let price_cents = self.catalog.price_of(req.sku.clone())?;
          let name = self
              .products
              .find(&req.sku)
              .map(|p| p.name.to_string())
              .unwrap_or_else(|| req.sku.clone());
          Ok(storefront::catalog::Product { sku: req.sku, name, price_cents })
      }

      /// `ListProducts` (server-stream): one Product frame per catalog entry.
      async fn list_products(
          &self,
          _req: storefront::catalog::ListProductsRequest,
      ) -> Result<Streaming<storefront::catalog::Product>, Status> {
          let products: Vec<storefront::catalog::Product> = self
              .products
              .all()
              .into_iter()
              .map(|p| storefront::catalog::Product {
                  sku: p.sku.to_string(),
                  name: p.name.to_string(),
                  price_cents: p.price_cents,
              })
              .collect();
          let stream: BoxStream<'static, Result<storefront::catalog::Product, Status>> =
              Box::pin(futures::stream::iter(products.into_iter().map(Ok)));
          Ok(Streaming::new(stream))
      }
  }

  /// A GrpcStatusMapper mapping the storefront's unknown-SKU domain error to Code::NotFound
  /// (the gRPC analogue of the StorefrontErrors #[control_advice]). A #[component] publishing
  /// the dyn GrpcStatusMapper view — the SAME collection-injection the default FALLBACK rides.
  #[component(provides = dyn leaf::grpc::leaf_grpc::GrpcStatusMapper)]
  #[derive(Debug, Default)]
  pub struct StorefrontGrpcErrors;

  impl leaf::grpc::leaf_grpc::GrpcStatusMapper for StorefrontGrpcErrors {
      fn map(&self, err: &LeafError) -> Option<Status> {
          match err.kind {
              leaf::core::ErrorKind::Integration { kind_id } if kind_id == unknown_sku_kind() => {
                  Some(Status::not_found("unknown sku"))
              }
              _ => None,
          }
      }
  }
  ```
  Note: `ProductRepository::all()` is assumed (the server-stream needs the full catalog). If the repository exposes the products under a different method name, use that exact method — verify with `grep -n "fn " examples/storefront/src/catalog/product/repository.rs` and adapt the iteration; do NOT invent a method the repo lacks. The contract names (`Streaming::new`, `Status::not_found`, `GrpcStatusMapper`, `#[grpc_controller]`) stay verbatim.
- [ ] **Step 3: Add `futures` to the storefront deps** (the server-stream builds a `futures::stream::iter`). The `grpc` feature already pulls it transitively via `leaf-grpc`, but the storefront names it directly for the `futures::stream` path; add under `[dependencies]`:
  ```toml
  futures = { version = "0.3", optional = true }
  ```
  and add `"dep:futures"` to the `grpc` feature:
  ```toml
  grpc = ["leaf/grpc", "dep:futures"]
  ```
  (If `leaf::prelude` or `leaf::core` already re-exports a `BoxStream` constructor that avoids naming `futures`, prefer that and skip this dep — verify with `grep -rn "stream::iter\|BoxStream" crates/leaf-core/src/` first; the dogfood ideal is umbrella-only.)
- [ ] **Step 4: Build the storefront with grpc to prove the controller macro lowers.**
  ```
  cargo build -p storefront --features grpc 2>&1 | tail -25
  ```
  Expected: clean build (the `#[grpc_controller]` lowers to GrpcRoute beans; the GrpcStatusMapper registers; the generated trait shapes match).
- [ ] **Step 5: Commit.**
  ```
  git add examples/storefront/src/grpc examples/storefront/Cargo.toml
  git commit -m "storefront: #[grpc_controller] over the catalog domain + a GrpcStatusMapper

  Dogfood: the SAME CatalogService/ProductRepository served over gRPC (unary GetProduct +
  server-stream ListProducts), umbrella-only, all stereotype beans; the unknown-SKU domain
  error mapped to NotFound (the gRPC analogue of the StorefrontErrors #[control_advice]).

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.9: The storefront real-H2 dogfood proof

`the_storefront_serves_its_domain_over_real_grpc` — the gRPC analogue of `the_storefront_serves_its_domain_over_real_http`. Boots the WHOLE storefront in-process via `leaf::bootstrap` (same shape as `tests/http.rs`), drives it with a real `tonic` client over H2: a known SKU unary GetProduct → the Product, an unknown SKU → `Code::NotFound` (the mapper), and a `ListProducts` server-stream → the catalog. Same embedded server, same port story.

**Files:** `examples/storefront/tests/grpc.rs`

- [ ] **Step 1: Write the test prelude + the boot (mirrors `tests/http.rs`).**
  ```rust
  //! The STOREFRONT gRPC PROOF — the gRPC analogue of `the_storefront_serves_its_domain_over_
  //! real_http`: the umbrella-only storefront, with the `grpc` capability, serves its catalog
  //! domain over REAL H2 through a #[grpc_controller], driven by a real tonic client — with
  //! ZERO hand-written GrpcRoute/GrpcHandler/ProtocolDispatch. The same embedded server the
  //! HTTP proof uses, now answering gRPC on the same lifecycle.
  #![cfg(feature = "grpc")]

  // The umbrella-only facade alias the macros' `::leaf_grpc::`/`::leaf_web::` paths resolve
  // against (a SOURCE alias of the one `leaf` dep, like the HTTP proof's `as leaf_web`).
  extern crate leaf as leaf_grpc;

  // Link the storefront LIBRARY's bean rows (the #[grpc_controller] + the mapper + the domain
  // services) into this test binary.
  use storefront as _;

  use std::time::Duration;

  // The tonic-generated client for catalog.proto (the polyglot interop point).
  pub mod catalog_tonic {
      tonic::include_proto!("storefront.catalog");
  }
  use catalog_tonic::catalog_client::CatalogClient;
  use futures::StreamExt;

  fn free_port() -> u16 {
      std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
  }

  async fn wait_until_up(port: u16) {
      for _ in 0..400 {
          if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
              return;
          }
          tokio::time::sleep(Duration::from_millis(10)).await;
      }
      panic!("the storefront grpc server never came up");
  }
  ```
- [ ] **Step 2: Write the proof body (RED).**
  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_storefront_serves_its_domain_over_real_grpc() {
      let port = free_port();

      // Boot the WHOLE storefront in-process (same entry the HTTP proof + #[leaf::main] drive);
      // the embedded server is a #[keep_alive] serving H2 on a spawned task, so run() returns
      // Ready and we hold the live app. The `grpc` capability's force-link pins the bundle.
      let running = leaf::bootstrap("storefront")
          .run(
              leaf::RunInputs::new()
                  .with_args([
                      format!("--leaf.web.server.port={port}"),
                      "--app.name=storefront".to_string(),
                  ])
                  .into(),
              leaf::boot::RunOverlay::none(),
          )
          .await
          .expect("the storefront boots to Ready");

      wait_until_up(port).await;

      let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
          .unwrap()
          .connect()
          .await
          .expect("tonic connects to the storefront");
      let mut c = CatalogClient::new(channel);

      // 1. GetProduct(COFFEE) → the Product, resolved via CatalogService (the cached price) +
      //    ProductRepository (the name) — the SAME domain the HTTP /products/COFFEE serves.
      let product = c
          .get_product(tonic::Request::new(catalog_tonic::GetProductRequest { sku: "COFFEE".into() }))
          .await
          .expect("GetProduct COFFEE")
          .into_inner();
      assert_eq!(product.sku, "COFFEE");
      assert_eq!(product.name, "Bag of Coffee");
      assert_eq!(product.price_cents, 1299);

      // 2. GetProduct(NOPE) → Code::NotFound via the StorefrontGrpcErrors GrpcStatusMapper
      //    (the unknown-SKU domain channel — the gRPC analogue of the HTTP 404 advice).
      let err = c
          .get_product(tonic::Request::new(catalog_tonic::GetProductRequest { sku: "NOPE".into() }))
          .await
          .expect_err("an unknown SKU is a NotFound Status");
      assert_eq!(err.code(), tonic::Code::NotFound, "unknown SKU maps to NotFound via the mapper");

      // 3. ListProducts (server-stream) → the catalog, one Product per frame; COFFEE is present.
      let mut stream = c
          .list_products(tonic::Request::new(catalog_tonic::ListProductsRequest {}))
          .await
          .expect("ListProducts")
          .into_inner();
      let mut skus = Vec::new();
      while let Some(item) = stream.next().await {
          skus.push(item.expect("list item").sku);
      }
      assert!(skus.contains(&"COFFEE".to_string()), "the streamed catalog includes COFFEE, got {skus:?}");

      // Graceful shutdown → clean teardown to Closed (the same lifecycle the HTTP proof asserts).
      let report = running.shutdown().await;
      assert_eq!(report.run_state, leaf::core::RunState::Closed, "the storefront drained cleanly");
  }
  ```
- [ ] **Step 3: Add the dev-deps for the proof to `examples/storefront/Cargo.toml`** (if not already from Task 6.7): tonic, prost, futures, and the tokio net/io features. Confirm `[dev-dependencies]`:
  ```toml
  tonic = { version = "0.14", default-features = false, features = ["transport", "codegen", "prost"] }
  prost = "0.14"
  futures = "0.3"
  ```
  And the proof needs the tonic client codegen for `catalog.proto` — tonic compiles it from its own `build.rs` OR `tonic::include_proto!` reads the OUT_DIR the storefront `build.rs` already produced ONLY if that build emitted tonic-compatible descriptors. Since the storefront `build.rs` uses `leaf_grpc_build::compile` (which emits the LEAF service trait, not tonic's), the test crate needs its OWN tonic codegen of `catalog.proto`. Add a `tests`-scoped build path: in `examples/storefront/build.rs`, ALSO run tonic's client codegen guarded by a test cfg is wrong (build.rs can't see dev-deps). Instead, vendor a tiny tonic-build step: add `tonic-build` as a build-dep and emit the tonic client into OUT_DIR alongside the leaf trait, OR (simpler, matches `serves_grpc.rs`) put `tonic::include_proto!("storefront.catalog")` against a SECOND OUT_DIR file produced by adding `tonic-prost-build` to the storefront `[build-dependencies]` under the `grpc` gate. Implement the latter:
  ```toml
  [build-dependencies]
  leaf-grpc-build = { path = "../../crates/leaf-grpc-build", version = "0.1.0" }
  tonic-prost-build = { version = "0.14", optional = true }
  ```
  ```toml
  grpc = ["leaf/grpc", "dep:futures", "dep:tonic-prost-build"]
  ```
  And in `examples/storefront/build.rs`, after the leaf compile, emit the tonic client too (the test's polyglot client; build-deps are visible to build.rs):
  ```rust
  if std::env::var_os("CARGO_FEATURE_GRPC").is_some() {
      leaf_grpc_build::compile(&["proto/catalog.proto"], &["proto"])?;
      // The tonic CLIENT stubs for the integration test (build-time, dev-test only): tonic's
      // own codegen of the SAME .proto, into OUT_DIR, reached by tonic::include_proto! in the test.
      #[cfg(feature = "grpc")]
      tonic_prost_build::configure()
          .build_server(false)
          .compile_protos(&["proto/catalog.proto"], &["proto"])
          .map_err(|e| std::io::Error::other(e.to_string()))?;
  }
  ```
  (`leaf_grpc_build::compile` emits `storefront.catalog.rs` for the leaf trait; tonic emits its own `storefront.catalog.rs` — namespace collision. Resolve by having `leaf_grpc_build::compile` write to a leaf-specific filename or by pointing tonic at a sub-OUT_DIR; the test's `tonic::include_proto!` must read tonic's file. If Stage 3's `compile` already namespaces its output, confirm the names differ; otherwise have the test use a distinct `mod` reading the tonic file path explicitly via `include!`. Verify the two generated filenames with `find target -path '*storefront*/out/*.rs'` after Step 4 and adjust the `include!` path so they do not clash.)
- [ ] **Step 4: Run the dogfood proof — drive to GREEN.**
  ```
  cargo test -p storefront --features grpc --test grpc -- --nocapture
  ```
  Expected:
  ```
  test the_storefront_serves_its_domain_over_real_grpc ... ok
  ```
  Any failure → `superpowers:systematic-debugging`; the fix is in the storefront dogfood (controller/mapper/proto) or surfaces a real engine gap to fix in the owning leaf crate.
- [ ] **Step 5: Commit.**
  ```
  git add examples/storefront/tests/grpc.rs examples/storefront/Cargo.toml examples/storefront/build.rs
  git commit -m "storefront: the_storefront_serves_its_domain_over_real_grpc (real-H2 dogfood proof)

  The gRPC analogue of the real-HTTP proof: tonic drives the storefront's #[grpc_controller]
  over real H2 — unary GetProduct, the unknown-SKU NotFound mapping, and a ListProducts
  server-stream of the catalog — on the same embedded server lifecycle.

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 6.10: Full force-clean gate — test + clippy + doc

The project's verification rule: cached `cargo` runs re-emit no warnings, so force-clean before claiming clean. Prove the ENTIRE workspace is green (the new gRPC tests + the existing ~1647-test HTTP suite unchanged through the `Body` change), clippy-clean (including the macro-generated items' `#[allow]`s rust-analyzer needs), and doc-clean.

**Files:** none (verification only)

- [ ] **Step 1: Force-clean, then run the WHOLE test suite with every feature.** A from-scratch build so no cached pass masks a warning:
  ```
  cargo clean && cargo test --workspace --all-features 2>&1 | tail -40
  ```
  Expected: every test binary passes, including the existing HTTP suite (the streaming `Body` is collect-before-handler, so REST is unchanged) and the new `serves_grpc` (4 tests) + storefront `grpc` (1 test). Confirm the regression budget held — the HTTP test count did not drop:
  ```
  cargo test --workspace --all-features 2>&1 | grep -E 'test result:' | tail -40
  ```
  Expected: all `test result: ok.`; the aggregate passed count ≥ the pre-stage ~1647 + the new gRPC tests.
- [ ] **Step 2: Force-clean clippy across the workspace, all features, deny warnings.** The storefront example + every crate, with the `-D warnings` floor the project enforces:
  ```
  cargo clean && cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -30
  ```
  Expected: `Finished` with NO warnings. If a macro-generated item trips a naming/style lint that rustc skips but clippy/rust-analyzer flags, emit the `#[allow(...)]` ON THE GENERATED ITEM in the OWNING codegen (Stage 3/4), not at the call site — per the project's macro-gen-naming-lint rule. Re-run until clean.
- [ ] **Step 3: Force-clean doc build, deny doc warnings.** Cargo doc must be clean (the memory's "clean cargo doc" gate):
  ```
  cargo clean && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps 2>&1 | tail -25
  ```
  Expected: `Finished` with no broken intra-doc links / missing-docs warnings across `leaf-grpc`, `leaf-grpc-build`, `leaf-starter-grpc`, and the umbrella's new `grpc` surface.
- [ ] **Step 4: Run `superpowers:verification-before-completion` to confirm the evidence.** Re-state each gate's command + its observed PASS output (evidence before assertions). Confirm specifically: (a) all four call shapes pass against tonic, (b) explicit + domain `Status` ride trailers, (c) the metadata-auth `WebFilter` ran around gRPC, (d) HTTP + gRPC on one port, (e) the storefront real-H2 dogfood passes, (f) the existing HTTP suite count held, (g) clippy + doc clean from a forced clean.
- [ ] **Step 5: Final commit (the gate evidence — a docs/verification note only if the project keeps one; otherwise a no-op verification commit is skipped).** If there is a running verification log, append the gate output; otherwise end the stage here with the suite green. The stage is complete: gRPC support is proven end to end (polyglot tonic interop, all four shapes, the error model, the shared filter chain, same-port HTTP+gRPC) and dogfooded in the storefront over real H2, with the full force-clean gate passing.
