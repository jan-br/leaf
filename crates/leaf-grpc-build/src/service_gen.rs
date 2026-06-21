//! The leaf service-trait generator — a pure `prost_build::ServiceGenerator` that
//! writes Rust source into a `String`, so every call-shape lowering is unit-testable
//! WITHOUT a compiler (the leaf-codegen discipline). It emits, per gRPC service: a
//! leaf-shaped server trait, the `/pkg.Service/Method` path constants, and the
//! `#[doc(hidden)]` per-method descriptors the `#[grpc_controller]` macro reads.

/// The RPC call shape — decided ONLY from the `client_streaming`/`server_streaming`
/// booleans the FileDescriptorSet carries, NEVER from a textual type name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>`
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>`
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>`
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>`
    Bidi,
}

impl CallShape {
    /// Classify from the two streaming flags (the FileDescriptorSet's `Method`).
    #[must_use]
    pub fn from_flags(client_streaming: bool, server_streaming: bool) -> Self {
        match (client_streaming, server_streaming) {
            (false, false) => CallShape::Unary,
            (false, true) => CallShape::ServerStream,
            (true, false) => CallShape::ClientStream,
            (true, true) => CallShape::Bidi,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_all_four_shapes_from_the_streaming_flags() {
        assert_eq!(CallShape::from_flags(false, false), CallShape::Unary);
        assert_eq!(CallShape::from_flags(false, true), CallShape::ServerStream);
        assert_eq!(CallShape::from_flags(true, false), CallShape::ClientStream);
        assert_eq!(CallShape::from_flags(true, true), CallShape::Bidi);
    }
}
