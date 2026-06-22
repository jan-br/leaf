//! The storefront build script. When the `grpc` capability is enabled it compiles
//! `proto/catalog.proto` into BOTH sides of the wire from the SAME `.proto` (so a plain
//! `web`/`redis` build needs no protobuf toolchain):
//!
//! 1. leaf's server trait + path/descriptor module + prost messages, via
//!    `leaf_grpc_build::compile` (protox -> prost-build + the leaf service generator),
//!    written to `$OUT_DIR/storefront.catalog.rs` and reached by
//!    `leaf::grpc::leaf_grpc::include_proto!("storefront.catalog")` in the controller.
//! 2. tonic's OWN client stub (the dev-test polyglot client), via `tonic-build` over the
//!    SAME protox-parsed descriptors (NO protoc), written to a SEPARATE
//!    `$OUT_DIR/tonic/storefront.catalog.rs` (so it does NOT collide with leaf's file) and
//!    reached by the integration test's own `include!`. tonic/tonic-build are BUILD/DEV-only
//!    — they never enter the storefront's normal dep graph.
//!
//! DRIFT vs the plan: the workspace pins tonic 0.13 (riding prost 0.13), so the client
//! codegen is `tonic_build`'s `compile_fds` (the 0.13 API), mirroring `leaf-grpc/build.rs`,
//! NOT the plan-text's 0.14 `tonic-prost-build`.
fn main() -> std::io::Result<()> {
    // Only compile the .proto when the `grpc` capability is enabled (so a plain `web`/`redis`
    // build needs no protobuf toolchain). protox = pure-Rust, NO protoc binary.
    if std::env::var_os("CARGO_FEATURE_GRPC").is_some() {
        // (1) leaf's server trait + path/descriptor module + prost messages -> $OUT_DIR.
        leaf_grpc_build::compile(&["proto/catalog.proto"], &["proto"])?;

        // (2) tonic's CLIENT stub from the SAME catalog.proto -> $OUT_DIR/tonic/. protox
        // parses the FileDescriptorSet (pure Rust, NO protoc), fed to tonic-build's
        // `compile_fds`. Client-only: leaf provides the server, tonic the test client.
        #[cfg(feature = "grpc")]
        {
            let out = std::env::var("OUT_DIR").expect("OUT_DIR");
            let fds = protox::compile(["proto/catalog.proto"], ["proto"])
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            let tonic_out = std::path::Path::new(&out).join("tonic");
            std::fs::create_dir_all(&tonic_out)?;
            tonic_build::configure()
                .build_server(false)
                .build_client(true)
                .out_dir(&tonic_out)
                .compile_fds(fds)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        println!("cargo:rerun-if-changed=proto/catalog.proto");
        println!("cargo:rerun-if-changed=build.rs");
    }
    Ok(())
}
