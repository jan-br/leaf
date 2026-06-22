//! The gRPC SERVER REFLECTION integration proof (Stage 4): the shared hyper WebServer
//! boots in-process with H2; a tonic-generated reflection client (the reflection_v1.proto's
//! own client, dev-only, NO external grpcurl) drives ServerReflectionInfo over real H2.
//! Opt-in: OFF (default) -> Code::Unimplemented; ON -> list_services + file_containing_symbol.

// The tonic-generated CLIENT for the SHIPPED grpc.reflection.v1 reflection_v1.proto, compiled
// by tonic's own codegen into a SEPARATE $OUT_DIR/tonic/ dir (so it never collides with the
// leaf-grpc-build server trait). The polyglot reflection peer; leaf names no tonic above dev.
pub mod reflection_tonic {
    include!(concat!(env!("OUT_DIR"), "/tonic/grpc.reflection.v1.rs"));
}

#[test]
fn the_tonic_reflection_client_stub_is_generated() {
    // Compiling this file at all proves the include! resolved; name the client type so the
    // module is not dead-code-eliminated before the include is type-checked.
    let _ = std::any::type_name::<
        reflection_tonic::server_reflection_client::ServerReflectionClient<()>,
    >();
}
