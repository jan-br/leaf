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
