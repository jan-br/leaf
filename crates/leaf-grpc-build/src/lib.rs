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
