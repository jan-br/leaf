//! End-to-end build-output proof: the FULL pipeline (protox -> prost-build -> the leaf
//! service-trait generator) over `tests/echo.proto` produces a compilable leaf server
//! trait with the four correct call shapes, the `/echo.v1.Echo/Method` path constants,
//! and the `#[doc(hidden)]` method descriptors the `#[grpc_controller]` macro reads.

include!(concat!(env!("OUT_DIR"), "/echo.v1.rs"));

/// A trivial implementor of the GENERATED `Echo` trait — proving the four method
/// signatures are exactly the contract shapes (this would not compile otherwise).
struct EchoImpl;

impl Echo for EchoImpl {
    async fn get(&self, _req: Ping) -> Result<Pong, ::leaf_grpc::Status> {
        Ok(Pong::default())
    }
    async fn list(&self, _req: Ping) -> Result<::leaf_grpc::Streaming<Pong>, ::leaf_grpc::Status> {
        Ok(::leaf_grpc::Streaming::once(Pong::default()))
    }
    async fn upload(
        &self,
        _req: ::leaf_grpc::Streaming<Ping>,
    ) -> Result<Pong, ::leaf_grpc::Status> {
        Ok(Pong::default())
    }
    async fn chat(
        &self,
        _req: ::leaf_grpc::Streaming<Ping>,
    ) -> Result<::leaf_grpc::Streaming<Pong>, ::leaf_grpc::Status> {
        Ok(::leaf_grpc::Streaming::once(Pong::default()))
    }
}

#[test]
fn the_generated_message_structs_are_prost_messages() {
    // prost-build emitted the message structs from the FileDescriptorSet.
    let p = Ping { msg: "hi".into() };
    assert_eq!(p.msg, "hi");
}

#[test]
fn the_path_constants_are_the_canonical_grpc_literals() {
    assert_eq!(echo::GET_PATH, "/echo.v1.Echo/Get");
    assert_eq!(echo::LIST_PATH, "/echo.v1.Echo/List");
    assert_eq!(echo::UPLOAD_PATH, "/echo.v1.Echo/Upload");
    assert_eq!(echo::CHAT_PATH, "/echo.v1.Echo/Chat");
}

#[test]
fn the_method_descriptors_carry_the_path_and_the_call_shape() {
    assert_eq!(echo::GET_DESCRIPTOR.path, "/echo.v1.Echo/Get");
    assert_eq!(echo::GET_DESCRIPTOR.shape, ::leaf_grpc::CallShape::Unary);
    assert_eq!(echo::LIST_DESCRIPTOR.shape, ::leaf_grpc::CallShape::ServerStream);
    assert_eq!(echo::UPLOAD_DESCRIPTOR.shape, ::leaf_grpc::CallShape::ClientStream);
    assert_eq!(echo::CHAT_DESCRIPTOR.shape, ::leaf_grpc::CallShape::Bidi);
}

#[test]
fn the_generated_trait_is_implementable_in_all_four_shapes() {
    // Constructing the impl proves all four generated signatures are the EXACT
    // contract shapes — the trait method types would reject a mismatched body.
    let _e = EchoImpl;
}

#[test]
fn the_generated_module_exposes_the_encoded_file_descriptor_set() {
    // The FDS const is the package's encoded FileDescriptorSet, embedded from echo.v1.fds.
    assert!(
        !FILE_DESCRIPTOR_SET.is_empty(),
        "the FDS const carries the encoded bytes"
    );
}

#[test]
fn the_fds_const_equals_the_sibling_fds_file_and_decodes_to_the_echo_package() {
    use ::prost::Message;
    // Equals the bytes compile() wrote to OUT_DIR/echo.v1.fds.
    let on_disk = include_bytes!(concat!(env!("OUT_DIR"), "/echo.v1.fds"));
    assert_eq!(
        FILE_DESCRIPTOR_SET, on_disk,
        "the const embeds the .fds verbatim"
    );

    let decoded = ::prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
        .expect("the FDS const decodes as a prost FileDescriptorSet");
    let packages: Vec<_> = decoded.file.iter().filter_map(|f| f.package.clone()).collect();
    assert!(
        packages.iter().any(|p| p == "echo.v1"),
        "the decoded FDS names the echo.v1 package, got {packages:?}"
    );
    // The service the leaf trait was generated from is present in the descriptor.
    let services: Vec<_> = decoded
        .file
        .iter()
        .flat_map(|f| f.service.iter().filter_map(|s| s.name.clone()))
        .collect();
    assert!(
        services.iter().any(|s| s == "Echo"),
        "the Echo service is in the FDS, got {services:?}"
    );
}
