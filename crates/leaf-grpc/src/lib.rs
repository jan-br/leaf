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

// The crate's public re-exports land here as each task lands its type (Tasks 2.2–2.10):
//   pub use codec::{GrpcCodec, ProstCodec};
//   pub use dispatch::GrpcDispatch;
//   pub use framing::{decode_frames, encode_frame};
//   pub use handler::{GrpcHandler, GrpcRoute};
//   pub use mapper::{DefaultGrpcStatusMapper, GrpcStatusMapper};
//   pub use streaming::Streaming;
pub use status::{Code, Status};
