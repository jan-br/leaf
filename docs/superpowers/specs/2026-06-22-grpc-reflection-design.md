# gRPC Server Reflection (sub-project C) — Design

**Status:** approved design, pre-plan.
**Goal:** add **opt-in gRPC Server Reflection** to leaf — the standard `grpc.reflection.v1` (+ `v1alpha`) `ServerReflection` service — so tools like grpcurl can discover an app's gRPC services and message schemas at runtime with no `.proto` on hand. Shipped with `leaf-grpc`, discovering **every** registered `#[grpc_controller]`'s descriptors automatically, implemented as a dogfooded leaf-provided `#[grpc_controller]`.

**Anchors (decided during brainstorming):**
1. **Scope: reflection ONLY.** Health checking (`grpc.health.v1`) and channelz are deferred to their own later specs. The gRPC client stays deferred (it is NOT required for bidi streaming — bidi is a server-side call shape already built + proven in sub-project B).
2. **Posture: opt-in.** Reflection is OFF by default; the app enables it with `leaf.grpc.reflection.enabled = true`. Safer-by-default (no API-schema exposure unless asked), matching tonic. Expressed in leaf's idiom as a `#[conditional(on_property = "leaf.grpc.reflection.enabled")]` gate on the reflection auto-config.
3. **Discovery: automatic.** Every proto the app compiles via `leaf-grpc-build` contributes its encoded `FileDescriptorSet` to a `leaf-grpc` discovery slice — no app wiring; every served service is reflectable.
4. **Dogfood.** Reflection is a real gRPC service: `leaf-grpc` ships `reflection.proto`, `leaf-grpc-build` compiles it, and a leaf-provided `#[grpc_controller]` serves the bidi `ServerReflectionInfo` RPC over the existing streaming/framing machinery — no hand-rolled `GrpcRoute`/`GrpcHandler`.
5. **Versions: both `v1` and `v1alpha`** under one shared index/logic core — older grpcurl/tools speak `v1alpha`, newer ones `v1`; both = maximum tool compatibility.

Builds on sub-project B (gRPC) — see the gRPC design at `docs/superpowers/specs/2026-06-22-grpc-support-design.md`.

---

## 1. Discovery: collecting the FileDescriptorSets

Server reflection answers with `FileDescriptorProto` bytes, so the server must hold the descriptor sets of every served service at runtime.

- **`leaf-grpc-build` change:** in addition to the message structs + server trait + path/`MethodDescriptor` consts it already emits, `compile()` now also writes each proto's **encoded `FileDescriptorSet`** (via `prost_build::Config::file_descriptor_set_path`) and emits it into the generated module as `pub const FILE_DESCRIPTOR_SET: &[u8] = &[…]` (the raw encoded bytes embedded with `include_bytes!`/a byte const).
- **Auto-registration:** the generated code contributes that const to a `leaf-grpc` `linkme` discovery slice:
  ```rust
  // leaf-grpc
  #[linkme::distributed_slice]
  pub static REFLECTED_FILE_DESCRIPTOR_SETS: [&'static [u8]] = [..];
  ```
  via a generated `#[distributed_slice(::leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS)] static …: &[u8] = FILE_DESCRIPTOR_SET;` row — emitted once per compiled proto, automatically, for every app that runs `leaf_grpc_build::compile`. This is leaf's established discovery-channel pattern (mirrors `COMPONENTS`/`AUTO_CONFIGS`), keyed through `leaf-core`'s re-exported `linkme`. The FDS bytes are inert static data whether or not reflection is enabled.
- **No app wiring:** an app that already builds a `#[grpc_controller]` (so its build.rs calls `leaf_grpc_build::compile`) becomes reflectable the moment reflection is enabled — it contributes its FDS automatically.

## 2. Opt-in gating

The two reflection `#[grpc_controller]`s wear `#[conditional(on_property = "leaf.grpc.reflection.enabled")]`. Default (property unset/false): neither the controller struct beans NOR their generated `GrpcRoute` beans register → a reflection request hits no route → `Code::Unimplemented` (the normal unknown-method path). Set `leaf.grpc.reflection.enabled = true`: the reflection services register and serve.

**Condition propagation (a small, general codegen addition this sub-project needs):** today the `#[grpc_controller]` lowering emits the controller struct bean PLUS separate `#[doc(hidden)]` `GrpcRoute` beans (each injecting `Ref<Controller>`). For a `#[conditional]` controller to gate cleanly, the controller's `#[conditional]`/`#[profile]` guard attributes must be PROPAGATED onto its generated `GrpcRoute` beans — otherwise the routes would register while the controller is conditioned out (and fail to resolve `Ref<Controller>`). So the `#[grpc_controller]` codegen will copy the controller struct's condition/profile attributes onto each emitted route bean, registering the service as a unit. This is general (it also makes a conditional HTTP `#[rest_controller]` gate correctly) and is the chosen mechanism — preferred over a runtime "registered-but-refuses" check, which would leave a phantom service in the route table. (The FDS discovery slice is collected regardless; only the service that READS it is gated.)

## 3. The reflection service (dogfooded)

- **`leaf-grpc` ships `reflection.proto`** for both `grpc.reflection.v1` and `grpc.reflection.v1alpha` (the upstream gRPC protos). `leaf-grpc-build` compiles them into the leaf service traits (the `ServerReflectionInfo` bidi-streaming RPC) the same way app protos are compiled.
- **A version-agnostic index core** (`leaf-grpc`, plain Rust — not a gRPC service itself): on first use it decodes the collected `REFLECTED_FILE_DESCRIPTOR_SETS` into `prost_types::FileDescriptorProto`s and builds:
  - `services: Vec<String>` — every fully-qualified service name (for `list_services`),
  - `by_filename: HashMap<String, FileDescriptorProto>`,
  - `by_symbol: HashMap<String, file_name>` — every message/enum/service/method fully-qualified symbol → its defining file,
  - `by_extension: HashMap<(extendee, number), file_name>`.
  It answers each reflection query and, for `file_*` queries, returns the matched file **plus the transitive closure of its `dependency` imports** (per the reflection spec — a client needs the full set to build the type). A not-found symbol/file yields the reflection `ErrorResponse { error_code = NOT_FOUND }`.
- **Two thin `#[grpc_controller]`s** (`ReflectionV1` and `ReflectionV1alpha`), each implementing its version's generated `ServerReflection` trait, adapting that version's `ServerReflectionRequest`/`Response` types to/from the shared index core. Both field-inject the same index (built from the collected slice). The bidi RPC maps each inbound `ServerReflectionRequest` in the stream to one `ServerReflectionResponse` (`Streaming<Req> -> Streaming<Resp>`), reusing the existing streaming machinery.

## 4. Error handling

Reflection-level "not found" is a normal `ServerReflectionResponse::ErrorResponse` (NOT a transport error) — the index returns it for unknown symbols/files. A malformed request variant maps to `INVALID_ARGUMENT` in the error response. The RPC itself only fails (a `Status`) on a genuine internal error (e.g. a corrupt FDS that fails to decode → `Internal`), via the normal gRPC `Status` path.

## 5. Testing

- **Unit (`leaf-grpc`):** the index over a sample `FileDescriptorSet` — `list_services` returns the service names; `file_containing_symbol` returns the defining file **and its dependency closure**; `file_by_filename`; an unknown symbol → the `NOT_FOUND` ErrorResponse.
- **Integration (the headline proof):** boot a test app (and/or the storefront) with `leaf.grpc.reflection.enabled = true` on the shared hyper `WebServer`; use a **reflection client** — the `reflection.proto`'s tonic-generated client, dev-only (no external `grpcurl` binary) — over real H2 to `list_services` (asserts the app's catalog service appears) and `file_containing_symbol` for a request message (asserts the descriptor + deps come back).
- **Opt-in proof:** with reflection OFF (default), a reflection request → `Code::Unimplemented` (the service isn't registered); flipping `leaf.grpc.reflection.enabled = true` makes the same request succeed.
- **Regression:** the existing ~1733-test suite stays green; force-clean gate (test + clippy + doc).

## 6. Hard constraints (carried from the charter)

- **Backend-free `leaf-grpc`** (only `leaf-web-hyper` names hyper/h2); reflection is pure leaf-grpc + `prost`/`prost-types`.
- **No type-name detection** in any macro/codegen (the reflection index keys on the runtime FDS symbol strings — which ARE the gRPC wire identifiers, not Rust type names — never on a Rust type's spelled name).
- **Dogfood:** reflection is `#[grpc_controller]` beans + an `#[auto_config]`; the FDS registration is a generated discovery-slice row; no hand-rolled `GrpcRoute`/`GrpcHandler`/`Provider`.
- **Dep graph** unchanged: `leaf-grpc → leaf-web → leaf-core`; `leaf-grpc-build` is the build-helper; `tonic`/`tonic-build` stay dev/build-only (the reflection-client test peer).

## 7. Out of scope (own later specs)

Health checking (`grpc.health.v1`); channelz; the gRPC client; generating reflection for non-`leaf-grpc-build` protos. The discovery slice + index are shaped so health (which also enumerates services) can reuse the same service list later.

## 8. Suggested implementation staging (for the plan)

1. `leaf-grpc-build` emits the encoded `FileDescriptorSet` const + the discovery-slice registration row; the `REFLECTED_FILE_DESCRIPTOR_SETS` slice in `leaf-grpc`.
2. The version-agnostic reflection index core (decode FDS → maps; the queries + dependency-closure) with unit tests.
3. The `#[grpc_controller]` condition-propagation codegen addition (copy the controller's `#[conditional]`/`#[profile]` guards onto its generated `GrpcRoute` beans) + a token/wiring test that a conditioned controller gates as a unit. Then `reflection.proto` (v1 + v1alpha) shipped + compiled; the two thin `#[grpc_controller]`s adapting to the index, each gated `#[conditional(on_property = "leaf.grpc.reflection.enabled")]`.
4. Integration: the tonic reflection-client proof (list_services + file_containing_symbol) + the opt-in on/off proof + the storefront becoming reflectable; full force-clean gate.
