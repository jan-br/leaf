//! The reflection protos compile through leaf-grpc-build to leaf server traits + the
//! Stage-1 FDS registration. A smoke proof that `grpc.reflection.v1` and
//! `grpc.reflection.v1alpha` each yield a `ServerReflection` server trait + the
//! ServerReflectionRequest/Response prost types, included from OUT_DIR.

mod gen_v1 {
    // The generated module is spliced but its `ServerReflection` trait is not impl'd here
    // (this is a structural smoke test, not a controller), and prost's oneof variants share
    // a `Response` suffix — both are clippy lints on GENERATED code, allowed at the splice site.
    #![allow(dead_code, clippy::enum_variant_names)]
    leaf_grpc::include_proto!("grpc.reflection.v1");
}
mod gen_v1alpha {
    #![allow(dead_code, clippy::enum_variant_names)]
    leaf_grpc::include_proto!("grpc.reflection.v1alpha");
}

#[test]
fn the_reflection_protos_yield_server_reflection_traits_and_messages() {
    // The prost message types exist (constructed via Default) — a structural proof the
    // protos compiled.
    let _v1_req = gen_v1::ServerReflectionRequest::default();
    let _v1_resp = gen_v1::ServerReflectionResponse::default();
    let _v1a_req = gen_v1alpha::ServerReflectionRequest::default();
    let _v1a_resp = gen_v1alpha::ServerReflectionResponse::default();
    // The FDS const Stage-1 emits per proto package is present (a `&[u8]` the discovery
    // slice points at).
    let v1_fds: &[u8] = gen_v1::FILE_DESCRIPTOR_SET;
    assert!(!v1_fds.is_empty(), "the v1 reflection FDS const is non-empty");
}
