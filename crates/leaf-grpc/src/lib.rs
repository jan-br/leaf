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

// A self-alias so the `REFLECTED_FILE_DESCRIPTOR_SETS` slice DECLARATION below — and the
// `leaf-grpc-build`-generated FDS registration rows when they land in THIS crate's own
// test targets — can resolve the linkme attribute macro through leaf-grpc's
// `pub use leaf_core::linkme;` re-export (`::leaf_grpc::linkme::distributed_slice`) rather
// than naming a bare `linkme` dependency. Attribute-macro paths resolve against extern
// crates / the crate root, and edition 2024 provides no implicit `extern crate self`, so
// the alias is explicit (the same trick `leaf-validation` uses for its derive paths).
extern crate self as leaf_grpc;

pub mod caller;
pub mod codec;
pub mod descriptor;
pub mod dispatch;
pub mod framing;
pub mod handler;
pub mod mapper;
pub mod reflection;
pub mod status;
pub mod streaming;

// The per-crate anti-DCE SOURCE anchor (ADR-09): one SourceTag in the link-collected
// SOURCES slice so a binary listing leaf-grpc in its ExpectedManifest can tell
// "linked-but-zero-rows" from "never-linked". The package name is the join string.
leaf_core::declare_source!("leaf-grpc");

// The crate's public re-exports land here as each task lands its type (Tasks 2.2–2.10):
//   pub use codec::{GrpcCodec, ProstCodec};
//   pub use dispatch::GrpcDispatch;
//   pub use framing::{decode_frames, encode_frame};
//   pub use handler::{GrpcHandler, GrpcRoute};
//   pub use mapper::{DefaultGrpcStatusMapper, GrpcStatusMapper};
pub use caller::{GrpcControllerKind, GrpcRecv, GrpcSend};
pub use codec::{GrpcCodec, ProstCodec};
pub use dispatch::{collect_trailers, status_trailers_stream, GrpcDispatch};
pub use framing::{decode_frames, encode_frame};
pub use handler::{GrpcHandler, GrpcRoute};
pub use mapper::{map_first, DefaultGrpcStatusMapper, GrpcStatusMapper};
pub use reflection::ReflectionIndex;
pub use descriptor::{CallShape, MethodDescriptor};
pub use status::{Code, Status};
pub use streaming::Streaming;

// The prost message codec, re-exported so a proto-first downstream resolves the absolute
// `::prost::` paths the `leaf-grpc-build`-generated message structs emit
// (`#[derive(::prost::Message)]`, `::prost::alloc::*`, `::prost::bytes::*`) WITHOUT naming
// `prost` as a direct dependency: an umbrella-only app aliases `extern crate leaf as prost;`
// (the same facade trick as `leaf_grpc`/`leaf_web`), reaching prost through the one `leaf`
// dep. prost is leaf-grpc's normal (runtime) codec dependency — the re-export only exposes it.
#[doc(no_inline)]
pub use prost;

// prost-types — the descriptor value types the reflection index decodes the discovery
// slice into; re-exported (the same umbrella-facade trick as `prost`) so a proto-first
// downstream resolves `::leaf_grpc::prost_types::FileDescriptorProto` through the one dep.
#[doc(no_inline)]
pub use prost_types;

// `linkme` re-exported THROUGH leaf-core (which does `pub use linkme;`), so the
// `leaf-grpc-build`-generated FDS registration rows resolve `::leaf_grpc::linkme`
// without leaf-grpc naming a bare `linkme` dependency — the same indirection the
// COMPONENTS/AUTO_CONFIGS rows use for `::leaf_core::linkme`.
#[doc(no_inline)]
pub use leaf_core::linkme;

/// The gRPC reflection discovery channel: every proto compiled by
/// `leaf_grpc_build::compile` contributes its encoded `prost_types::FileDescriptorSet`
/// bytes here via a generated `#[distributed_slice]` row — no app wiring. Mirrors
/// leaf-core's `COMPONENTS`/`AUTO_CONFIGS` channels (collected at link time). The bytes
/// are inert static data whether or not reflection is enabled; the reflection index
/// (a later stage) is the only reader.
#[::leaf_grpc::linkme::distributed_slice]
#[linkme(crate = ::leaf_grpc::linkme)]
pub static REFLECTED_FILE_DESCRIPTOR_SETS: [&'static [u8]] = [..];

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
