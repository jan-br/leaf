//! The storefront build script. Compiles `proto/catalog.proto` into the leaf gRPC
//! surface (the server trait + path/descriptor module + prost messages) ONLY when the
//! `grpc` capability is enabled, so a plain `web`/`redis` build needs no protobuf
//! toolchain. `leaf_grpc_build::compile` runs protox (pure Rust — NO protoc binary).
fn main() -> std::io::Result<()> {
    // Only compile the .proto when the `grpc` capability is enabled (so a plain `web`/`redis`
    // build needs no protobuf toolchain). protox = pure-Rust, NO protoc binary.
    if std::env::var_os("CARGO_FEATURE_GRPC").is_some() {
        leaf_grpc_build::compile(&["proto/catalog.proto"], &["proto"])?;
    }
    Ok(())
}
