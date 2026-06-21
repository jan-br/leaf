# gRPC Support (sub-project B) — Design

**Status:** approved design, pre-plan.
**Goal:** add full gRPC support to leaf — unary + server/client/bidi streaming — as a *second `Handler` family on the shared `WebServer`*, driven by a `#[grpc_controller]` controller-family stereotype, proto-first, with leaf owning the gRPC layer (prost for the message codec only). One server, one port, shared lifecycle.

**Anchors (decided during brainstorming):**
1. **Engine:** leaf OWNS the gRPC abstraction (dispatch, framing, status, the stereotype). `prost` is the protobuf message codec — the `serde_json` analogue, confined to one place. Runs on the existing hyper `WebServer` with HTTP/2 enabled. Never exposes tonic/tower/hyper.
2. **Stereotype:** `#[grpc_controller]` (the controller family — inbound-request handlers — alongside `#[controller]`/`#[rest_controller]`). `#[service]` stays for domain logic the controller delegates to.
3. **Scope:** full — unary + server-streaming + client-streaming + bidi. This makes a streaming `Body` a first-class leaf primitive.
4. **Contract:** proto-first. `.proto` → build codegen (`protox` + `prost-build`, **no `protoc` binary**) → prost message structs + a leaf-shaped server trait + path constants. `#[grpc_controller]` implements that trait.
5. **Client:** OUT of scope. Server-only (`#[grpc_controller]` server handlers). A gRPC client (generated stubs, channels, connection management) is its own later spec, mirroring the deferred HTTP client.

---

## 1. Architecture & crate topology

gRPC slots in as a peer of the HTTP layer, reusing transport + lifecycle + DI rather than running a parallel server.

- **`leaf-grpc`** (NEW abstraction crate, backend-free — like `leaf-web`; deps: `leaf-core`, `leaf-web`, `prost`, `bytes`, `http`, `futures`): the gRPC `Handler` family (`GrpcRoute`/`GrpcHandler`), the message framing (the 5-byte length-prefix), `Status` + `Code` (the grpc-status code space, 0–16), the typed `Streaming<T>` message-stream type, the `GrpcCodec` (prost) seam, and the `GrpcStatusMapper` SPI. Names NO hyper/h2/tower.
- **`leaf-grpc-build`** (NEW build-helper crate): wraps `protox` (pure-Rust `.proto` → `FileDescriptorSet`) + `prost-build` (+ a leaf service-trait code generator) so an app's `build.rs` compiles `.proto` → generated Rust. No `protoc` system binary.
- **`#[grpc_controller]`** in `leaf-macros` (+ lowering in `leaf-codegen`): lowers a `#[grpc_controller] impl ServiceTrait for Bean` block to one `GrpcRoute` bean per RPC method (the second Handler family), collected by DI exactly like HTTP routes. Field-injects the controller's collaborators (`Ref<…>`), same as `#[rest_controller]`.
- **`leaf-web` (shared core, MODIFIED):** the streaming `Body` (§2) + a **protocol-dispatch seam** in the `Dispatcher`. leaf-web defines an ABSTRACT `trait ProtocolDispatch { fn handles(&self, content_type: Option<&str>) -> bool; fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Response>; }` and the `Dispatcher` collection-injects `Vec<Ref<dyn ProtocolDispatch>>`. On a request whose `content-type` no built-in HTTP path claims, the dispatcher delegates to the first `ProtocolDispatch` whose `handles(..)` returns true; otherwise the HTTP `Route` family runs (unchanged). This is how leaf-web routes to gRPC WITHOUT ever naming `leaf-grpc`: the gRPC family is ONE `dyn ProtocolDispatch` impl contributed by `leaf-grpc` (matching `application/grpc*`), so the dep arrow stays `leaf-grpc → leaf-web`, never the reverse. (WebSocket etc. would later plug in the same way.) `WebServer`/`KeepAlive`/`Request`/`Response` remain the shared currency; the embedded server (the §A `KeepAlive` bean) now also injects `Vec<Ref<dyn ProtocolDispatch>>` into the `Dispatcher`.
- **`leaf-web-hyper` (MODIFIED):** enable the `http2` feature on `hyper`/`hyper-util`; the existing `auto::Builder` already negotiates H1+H2. One server, one port. Map hyper's `Incoming` ↔ the leaf streaming `Body` at the edge (§2).
- **`leaf-starter-grpc`** (NEW) + a `grpc` capability feature on the `leaf` umbrella (pulls `leaf-grpc` + the starter; enables `http2` on the backend).

Dep direction stays acyclic: `leaf-grpc → leaf-web → leaf-core`; `leaf-web-hyper → leaf-web`; the umbrella sinks them all. `leaf-web` never names `leaf-grpc`.

## 2. The streaming `Body` (core change)

Full streaming means `Request`/`Response` bodies can no longer be only buffered `Bytes`. **Chosen approach (A):**

```rust
// leaf-web
pub enum Body {
    Full(Bytes),
    Stream(BoxStream<'static, Result<Frame, LeafError>>),
}
pub enum Frame {
    Data(Bytes),
    Trailers(HeaderMap),   // trailers are first-class — gRPC needs grpc-status trailers
}
```

- `Request`/`Response` hold a `Body` (replacing the `body: Bytes` field). Backend-free (`BoxStream` is `futures`, not hyper).
- **HTTP stays ergonomic:** the `Dispatcher` COLLECTS the body to `Bytes` (a `Body::collect() -> Result<Bytes, LeafError>` helper) *before* invoking an HTTP `Route` handler, so every existing `#[rest_controller]`, extractor, and `Request::body_bytes()` call is unchanged. `Response` keeps a `with_body(Bytes)` / `Body::Full` default; the body-size limit (T6a) applies when collecting.
- **gRPC handlers consume/produce the frame stream directly** — no buffering of a streaming RPC.
- **Hyper edge:** `to_leaf_request` builds a `Body::Stream` from hyper's `Incoming` (data + trailers frames), bounded by the existing `max_request_body_bytes` for the collect path; the out-conversion writes `Body::Full` as today and `Body::Stream` as an H2 frame stream with trailers.
- **Regression budget:** the entire existing HTTP suite must stay green; REST behavior is unchanged in practice (collect-before-handler).

## 3. Proto-first codegen pipeline

- `.proto` files live in the crate (e.g. `proto/catalog.proto`). The app's `build.rs` calls `leaf_grpc_build::compile(&["proto/catalog.proto"], &["proto"])`.
- That helper runs `protox` (parse → `FileDescriptorSet`) then `prost-build` (message structs) plus a leaf service-trait generator that emits, per service:
  - a **server trait** with one method per RPC in the correct call shape (§5),
  - the `/package.Service/Method` **path constants**,
  - a `#[doc(hidden)]` descriptor the `#[grpc_controller]` macro consumes to know each method's path + call shape.
- Generated code is included via the standard `include!(concat!(env!("OUT_DIR"), "/pkg.rs"))` idiom (or a `leaf_grpc::include_proto!("pkg")` sugar).
- The user writes `#[grpc_controller] impl catalog::Catalog for CatalogController { … }` with plain `async fn`s; the macro desugars async (like `#[rest_controller]` does for handler methods — no separate `#[async_impl]`) and field-injects collaborators.

## 4. Dispatch & the gRPC Handler family

- `leaf-web`'s `Dispatcher` routes by `content-type` through the abstract `dyn ProtocolDispatch` seam (§1): `application/grpc` / `application/grpc+proto` is claimed by leaf-grpc's `ProtocolDispatch` impl; everything else stays on the HTTP `Route` family. leaf-web names no gRPC type.
- INSIDE leaf-grpc's `ProtocolDispatch` impl (the `GrpcDispatch` bean): gRPC method paths are full literals (`/pkg.Service/Method`), so it holds a `HashMap<String, Arc<dyn GrpcRoute>>` built once from the collection-injected `Vec<Ref<dyn GrpcRoute>>` (the `#[grpc_controller]` beans) — no pattern matching, O(1) lookup. An unknown method → `Code::Unimplemented` rendered as trailers.
- `trait GrpcRoute { fn path(&self) -> &str; fn handler(&self) -> &dyn GrpcHandler; }` and a `GrpcHandler` that takes the inbound frame stream + metadata and yields the outbound frame stream + status. `impl_resolve_view!(dyn GrpcRoute)` makes it collection-injectable.
- The `GrpcHandler` wraps the user's typed method: H2 body frames → length-prefix de-framing → `GrpcCodec` (prost) decode `T` → user handler → prost encode `U` → length-prefixed data frames + `grpc-status`/`grpc-message` trailers. All framing/codec lives in `leaf-grpc`; the user only sees typed messages.
- gRPC rides the same `KeepAlive` embedded server + graceful shutdown built in sub-project A; an in-flight streaming RPC drains within the configured grace.

## 5. `#[grpc_controller]` + the four call shapes

`Streaming<T>` is leaf-grpc's typed `Stream<Result<T, Status>>`. The generated trait methods:

- **Unary:** `async fn get(&self, req: ProductReq) -> Result<Product, Status>`
- **Server-stream:** `async fn list(&self, req: ListReq) -> Result<Streaming<Product>, Status>`
- **Client-stream:** `async fn upload(&self, reqs: Streaming<Chunk>) -> Result<Summary, Status>`
- **Bidi:** `async fn chat(&self, reqs: Streaming<Msg>) -> Result<Streaming<Msg>, Status>`

The macro lowers each method to a `GrpcRoute` bean: the path constant + a `GrpcHandler` that performs the framing/codec around the typed method, selecting the right shape (decode one vs decode a stream; encode one vs encode a stream). The streaming variants ride the §2 `Body` frame stream. The controller bean itself is a normal `#[component]`-family bean (field injection); the per-method `GrpcRoute`s are `#[doc(hidden)]` generated beans, exactly like `#[rest_controller]`'s per-method `Route`s. (If the stereotype splits across `struct` + `impl`, apply the same dual-form `ControllerKind`-style consistency guard the HTTP controllers use.)

## 6. Cross-cutting & error model

- **Transport cross-cutting reuses `WebFilter`.** gRPC metadata *are* HTTP/2 headers, so auth / logging / tracing filters run uniformly across HTTP and gRPC (one ordered chain). A filter that short-circuits a gRPC request returns an `Err`/`Response` the gRPC edge renders as a `Status` (so a rejected gRPC call still produces a valid grpc-status trailer, not a raw HTTP body).
- **Errors:** handlers return `Result<_, Status>` over the grpc-status code space (`Code::NotFound`/`InvalidArgument`/`Internal`/…), rendered as `grpc-status`/`grpc-message` trailers.
- **Domain errors:** a `dyn GrpcStatusMapper` SPI (the `ControlAdvice` analogue, collection-injected the same DI way) maps a `LeafError` → `Status`, reusing the `ErrorKind::Integration { kind_id }` domain-error channel — exactly how the storefront maps unknown-SKU to a 404 for HTTP. A default mapper covers the common framework kinds (e.g. `NoSuchBean`/unimplemented → `Unimplemented`, `ConvertError` → `Internal`).

## 7. Testing strategy

- **Unit:** length-prefix framing encode/decode (incl. partial/over-cap frames), prost codec roundtrip, `Status`/`Code` mapping (incl. the domain-error channel), and codegen token tests for all four call-shape lowerings + the dual-form guard.
- **Dispatch:** the protocol branch (grpc vs HTTP on one port), O(1) routing by `/pkg.Service/Method`, unknown method → `Unimplemented`.
- **Integration (headline proof):** boot the shared hyper server and call leaf's `#[grpc_controller]` with **`tonic` as the test client** (dev-dependency only) — proving real polyglot interop with the canonical gRPC stack, exercising all four call types + a `Status` error (domain + explicit) + a `WebFilter` (auth via metadata) + HTTP and gRPC served on the **same port**.
- **Dogfood example:** a `#[grpc_controller]` added to the storefront (or a small example) implementing a `.proto`-defined service, served over real H2 — the gRPC analogue of `the_storefront_serves_its_domain_over_real_http`.
- **Regression:** the entire existing ~1647-test HTTP suite stays green through the `Body` change; force-clean gate (tests + clippy + doc) per the project's verification rule.

## 8. Out of scope (own later specs)

gRPC client (stubs/channels/connection management); reflection / health / server-side LB; non-protobuf codecs; compression; `leaf-actors`; generating `.proto` from Rust (code-first). The abstractions here are shaped so a client slots in later by reusing the codec + framing + `Status` types.

## 9. Hard constraints (carried from the project charter)

- **Backend-free:** `leaf-grpc` and `leaf-web` name no hyper/tower/h2 in deps or public API; only `leaf-web-hyper` may. `prost` (a pure codec) and `protox`/`prost-build` (build-time) are the sanctioned codec deps, confined like `serde_json`/`leaf-serde`.
- **No type-name detection** in any macro/codegen.
- **Dogfood:** no hand-rolled trait impls a stereotype/macro can generate; `#[grpc_controller]` beans, the default `GrpcStatusMapper` as an `#[auto_config]` FALLBACK, etc.
- **Dep graph stays acyclic**, `leaf-grpc → leaf-web → leaf-core`; `leaf-web` never names `leaf-grpc`.
- **One stack:** gRPC reuses the `WebServer`/`KeepAlive`/`Dispatcher`/`WebFilter`/DI — not a parallel server.

## 10. Suggested implementation staging (for the plan)

1. Streaming `Body` core change in `leaf-web` (+ hyper edge map) + enable `http2`; re-verify the HTTP suite.
2. `leaf-grpc` abstractions: `Status`/`Code`, framing, `Streaming<T>`, `GrpcRoute`/`GrpcHandler`, `GrpcCodec` (prost), the `Dispatcher` protocol branch + gRPC family.
3. `leaf-grpc-build` proto-first codegen (protox + prost-build + the service-trait generator).
4. `#[grpc_controller]` stereotype (lower the 4 call shapes to `GrpcRoute` beans; DI; async desugar; dual-form guard).
5. Cross-cutting + errors (`WebFilter` reuse for gRPC; `GrpcStatusMapper` + the domain-error channel; default FALLBACK mapper).
6. Integration + dogfood (tonic test client; all 4 call types; same-port HTTP+gRPC; status error; filter; storefront example) + full force-clean gate.
