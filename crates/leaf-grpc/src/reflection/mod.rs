//! gRPC Server Reflection (opt-in). The version-agnostic core plus the two thin
//! `#[grpc_controller]` adapters.
//!
//! [`ReflectionIndex`] is PLAIN Rust — NOT a gRPC service. It decodes the
//! [`crate::REFLECTED_FILE_DESCRIPTOR_SETS`] discovery slice (each row an encoded
//! `prost_types::FileDescriptorSet`) into descriptor maps and answers the reflection
//! queries (`list_services`, `file_by_filename`, `file_containing_symbol`,
//! `file_containing_extension`, `all_extension_numbers_of_type`).
//!
//! Ships the upstream grpc.reflection.v1 / v1alpha protos (compiled by leaf-grpc-build to
//! leaf `ServerReflection` traits) and the two thin `#[grpc_controller]` beans
//! ([`ReflectionV1`] / [`ReflectionV1alpha`]) serving the bidi `ServerReflectionInfo` RPC
//! over the shared `ReflectionIndex` (built once from `REFLECTED_FILE_DESCRIPTOR_SETS`).
//! Each controller is `#[conditional(on_property = "leaf.grpc.reflection.enabled")]` — OFF
//! by default; a reflection request then hits no route (`Code::Unimplemented`).
//!
//! Backend-free: pure leaf-grpc + prost/prost-types. NO type-name detection — the index
//! keys on the FDS WIRE symbol strings (the gRPC fully-qualified identifiers, e.g.
//! `storefront.catalog.Catalog`) — NEVER on a Rust type name (the no-type-name-detection
//! rule). The `file_*` queries return the matched file PLUS the transitive closure of its
//! `dependency` imports, deduped (the reflection spec: a client needs the full set to
//! rebuild the type).

mod index;

/// The generated grpc.reflection.v1 module (server trait + prost messages + FDS const).
pub mod v1 {
    // The generated module is spliced verbatim into a crate that `#![deny(unsafe_code)]` +
    // `#![warn(missing_docs)]`. Allow the GENERATED-code lints at the splice site: the prost
    // structs/oneofs carry no docs, the oneof variants share a `Response` suffix, and the
    // Stage-1 FDS-registration row is a `linkme` `#[link_section]` static (which `unsafe_code`
    // flags) — this is framework-emitted wiring, not hand-written unsafe.
    #![allow(clippy::enum_variant_names, missing_docs, unsafe_code)]
    leaf_grpc::include_proto!("grpc.reflection.v1");
}
/// The generated grpc.reflection.v1alpha module.
pub mod v1alpha {
    #![allow(clippy::enum_variant_names, missing_docs, unsafe_code)]
    leaf_grpc::include_proto!("grpc.reflection.v1alpha");
}

mod service;

pub use index::ReflectionIndex;
// `ReflectionV1`/`ReflectionV1alpha` are added in Task 3.4 — re-exported then.
pub use service::Answer;
