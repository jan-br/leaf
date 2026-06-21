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
