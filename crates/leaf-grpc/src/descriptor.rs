//! The per-RPC-method descriptor seam the `leaf-grpc-build` codegen emits and the
//! `#[grpc_controller]` macro (Stage 4) reads — the gRPC analogue of leaf-web's route
//! metadata.
//!
//! `leaf-grpc-build` renders, beside each generated server trait, a `#[doc(hidden)]`
//! `pub const <METHOD>_DESCRIPTOR: leaf_grpc::MethodDescriptor` carrying the canonical
//! `/pkg.Service/Method` path + the [`CallShape`]. The `#[grpc_controller]` lowering
//! consults the SHAPE here (NOT the textual type of `req`/the return) to pick the
//! framing/codec wrapper, so the no-type-name-detection rule holds end to end.

/// The RPC call shape — the streaming arity of an RPC, decided at codegen time ONLY
/// from the FileDescriptorSet's `client_streaming`/`server_streaming` flags (never from
/// a textual message-type name). The four protobuf method shapes:
///
/// - [`Unary`](CallShape::Unary): `T -> Result<U, Status>`
/// - [`ServerStream`](CallShape::ServerStream): `T -> Result<Streaming<U>, Status>`
/// - [`ClientStream`](CallShape::ClientStream): `Streaming<T> -> Result<U, Status>`
/// - [`Bidi`](CallShape::Bidi): `Streaming<T> -> Result<Streaming<U>, Status>`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>` — one request, one response.
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>` — one request, a
    /// response stream.
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>` — a request stream,
    /// one response.
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>` — both
    /// sides stream.
    Bidi,
}

/// The compile-time descriptor of one RPC method: its canonical gRPC path + its
/// [`CallShape`]. Emitted as a `#[doc(hidden)]` `const` by `leaf-grpc-build` beside each
/// generated server trait, and read by the `#[grpc_controller]` macro to wire the
/// per-method `GrpcRoute` with the right framing/codec wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodDescriptor {
    /// The canonical gRPC method path `/package.Service/Method` (the HTTP/2 `:path`).
    pub path: &'static str,
    /// The RPC call shape (the streaming arity) — the wrapper-selection key.
    pub shape: CallShape,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_descriptor_is_a_const_carrying_path_and_shape() {
        const D: MethodDescriptor =
            MethodDescriptor { path: "/echo.v1.Echo/Get", shape: CallShape::Unary };
        assert_eq!(D.path, "/echo.v1.Echo/Get");
        assert_eq!(D.shape, CallShape::Unary);
    }

    #[test]
    fn call_shape_is_eq_and_debug() {
        assert_eq!(CallShape::Bidi, CallShape::Bidi);
        assert_ne!(CallShape::Unary, CallShape::ServerStream);
        assert_eq!(format!("{:?}", CallShape::ClientStream), "ClientStream");
    }
}
