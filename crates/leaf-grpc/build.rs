//! The leaf-grpc test build script. Two jobs:
//!
//! 1. The `include_proto!` macro test (`tests/include_proto_macro.rs`) needs a tiny
//!    generated-style fixture in OUT_DIR to splice — written first.
//! 2. The tonic INTEGRATION proof (`tests/serves_grpc.rs`) needs BOTH sides of the wire
//!    generated from the SAME `tests/proto/echo.proto`:
//!    - leaf's server trait + path/descriptor module, via `leaf_grpc_build::compile`
//!      (protox -> prost-build + the leaf service generator), written to `$OUT_DIR/echo.rs`
//!      and reached by `leaf_grpc::include_proto!("echo")`;
//!    - tonic's OWN client stub, via `tonic-build` over the SAME descriptors (no protoc:
//!      protox parses the FileDescriptorSet, fed to `compile_fds`), written to a SEPARATE
//!      `$OUT_DIR/tonic/echo.rs` (so it does NOT collide with leaf's `echo.rs`) and reached
//!      by the test's own `include!`. This is the polyglot interop point: leaf's server +
//!      tonic's client speak one wire.
//!
//! Cargo runs ONE build script per crate (at the crate root), so all of this lives here —
//! a deviation from the plan's `tests/build.rs` (per-target build scripts do not exist).

use std::io::Write;

fn main() -> std::io::Result<()> {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");

    // (1) The include_proto! macro test fixture.
    let fixture = std::path::Path::new(&out).join("fixture.rs");
    let mut f = std::fs::File::create(fixture).expect("create fixture.rs");
    writeln!(f, "pub const MARKER: u32 = 42;").expect("write fixture");

    // (2a) leaf's server trait + path/descriptor module from echo.proto -> $OUT_DIR/echo.rs.
    leaf_grpc_build::compile(&["tests/proto/echo.proto"], &["tests/proto"])?;

    // (3) leaf-grpc ships the upstream gRPC reflection protos (grpc.reflection.v1 +
    // grpc.reflection.v1alpha) -> $OUT_DIR/grpc.reflection.v1.rs / .v1alpha.rs. compile()
    // also writes each proto's encoded FileDescriptorSet const + the Stage-1
    // REFLECTED_FILE_DESCRIPTOR_SETS auto-registration row (inert unless reflection reads it).
    leaf_grpc_build::compile(
        &["proto/reflection_v1.proto", "proto/reflection_v1alpha.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/reflection_v1.proto");
    println!("cargo:rerun-if-changed=proto/reflection_v1alpha.proto");

    // (2b) tonic's CLIENT stub from the SAME echo.proto -> $OUT_DIR/tonic/echo.rs. protox
    // parses the FileDescriptorSet (pure Rust, NO protoc), fed to tonic-build's `compile_fds`.
    let fds = protox::compile(["tests/proto/echo.proto"], ["tests/proto"])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let tonic_out = std::path::Path::new(&out).join("tonic");
    std::fs::create_dir_all(&tonic_out)?;
    tonic_build::configure()
        // Client-only: leaf provides the server; tonic provides the test client.
        .build_server(false)
        .build_client(true)
        // No transport codegen needed here (the test builds its own Channel) — but the
        // `transport` feature on the tonic dep supplies it anyway; keep the stub minimal.
        .out_dir(&tonic_out)
        .compile_fds(fds)?;

    // (2c) tonic's CLIENT stub for the SHIPPED grpc.reflection.v1 proto -> $OUT_DIR/tonic/
    // grpc.reflection.v1.rs. protox parses its FileDescriptorSet (pure Rust, NO protoc), fed
    // to tonic-build's `compile_fds`. Client-only: leaf SERVES reflection (the dogfooded
    // #[grpc_controller]s); tonic provides the dev-test reflection client peer for the H2
    // proof (tests/reflection_over_h2.rs). tonic/tonic-build stay build/dev-only.
    let refl_fds = protox::compile(["proto/reflection_v1.proto"], ["proto"])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .out_dir(&tonic_out)
        .compile_fds(refl_fds)?;

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tests/proto/echo.proto");
    Ok(())
}
