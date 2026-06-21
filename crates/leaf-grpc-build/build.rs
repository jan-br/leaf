//! Self-hosts the codegen for the integration test: compiles `tests/echo.proto`
//! through the SAME pipeline (protox + prost-build + the leaf generator) the library's
//! `compile` runs, writing the result into OUT_DIR so `tests/generated_service.rs` can
//! `include!` it and assert the generated trait/paths/descriptors compile + match shapes.
//!
//! NOTE (deviation from plan): current Cargo rejects a crate naming ITSELF as a
//! build-dependency (`cyclic package dependency`), so `build.rs` cannot call
//! `leaf_grpc_build::compile` via a path self-dep. Instead it `include!`s the generator
//! (`src/service_gen.rs`) and inlines the tiny protox->prost-build pipeline here, over
//! the same build-time codec deps the library uses. The library's own `compile` (and its
//! unit tests) remain the canonical surface; this is a faithful re-host of it.

// The pure service-trait generator, loaded as a FILE module via `#[path]` (not
// `include!`) so its leading `//!` module-doc lines are valid (a file-module's head).
// Its `#[cfg(test)] mod tests` is inert here (build scripts compile without `cfg(test)`).
#[path = "src/service_gen.rs"]
mod service_gen;
use service_gen::LeafServiceGenerator;

fn main() -> std::io::Result<()> {
    let protos = ["tests/echo.proto"];
    let includes = ["tests"];

    // protox: pure-Rust .proto -> FileDescriptorSet (no protoc binary).
    let fds = protox::compile(protos, includes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    let out_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR not set"))?;

    let mut config = prost_build::Config::new();
    config.out_dir(out_dir);
    config.service_generator(Box::new(LeafServiceGenerator));
    config.compile_fds(fds)
}
