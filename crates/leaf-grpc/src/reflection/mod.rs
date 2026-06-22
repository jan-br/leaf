//! gRPC server-reflection support (sub-project C), the version-agnostic core.
//!
//! [`ReflectionIndex`] is PLAIN Rust — NOT a gRPC service. It decodes the
//! [`crate::REFLECTED_FILE_DESCRIPTOR_SETS`] discovery slice (each row an encoded
//! `prost_types::FileDescriptorSet`) into descriptor maps and answers the reflection
//! queries (`list_services`, `file_by_filename`, `file_containing_symbol`,
//! `file_containing_extension`, `all_extension_numbers_of_type`). The two version
//! `#[grpc_controller]` adapters (`grpc.reflection.v1` / `v1alpha`, Stage 3) drive
//! this one index.
//!
//! It keys on the FDS WIRE symbol strings (the gRPC fully-qualified identifiers,
//! e.g. `storefront.catalog.Catalog`) — NEVER on a Rust type name (the
//! no-type-name-detection rule). The `file_*` queries return the matched file PLUS
//! the transitive closure of its `dependency` imports, deduped (the reflection spec:
//! a client needs the full set to rebuild the type).

mod index;

pub use index::ReflectionIndex;
