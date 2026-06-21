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

/// The leaf request type for a call shape: `T` (unary/server-stream) wraps to
/// `leaf_grpc::Streaming<T>` for the client-streaming side. `input`/`output` are the
/// prost-resolved message type paths (emitted VERBATIM).
// (Task 3.3: consumed by `render_service` in Task 3.5 — `allow` removed there.)
#[allow(dead_code)]
fn request_ty(input: &str, shape: CallShape) -> String {
    match shape {
        CallShape::Unary | CallShape::ServerStream => input.to_string(),
        CallShape::ClientStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{input}>")
        }
    }
}

/// The leaf response type for a call shape: `Result<U, Status>` (unary/client-stream)
/// or `Result<Streaming<U>, Status>` (server-stream/bidi).
#[allow(dead_code)]
fn response_ty(output: &str, shape: CallShape) -> String {
    let inner = match shape {
        CallShape::Unary | CallShape::ClientStream => output.to_string(),
        CallShape::ServerStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{output}>")
        }
    };
    format!("::core::result::Result<{inner}, ::leaf_grpc::Status>")
}

/// The full `async fn` signature line for one RPC (no body/semicolon).
#[allow(dead_code)]
fn method_signature(name: &str, input: &str, output: &str, shape: CallShape) -> String {
    format!(
        "async fn {name}(&self, req: {req}) -> {resp}",
        req = request_ty(input, shape),
        resp = response_ty(output, shape),
    )
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

    #[test]
    fn emits_the_unary_method_signature() {
        let sig = method_signature("get", "super::Ping", "super::Pong", CallShape::Unary);
        assert_eq!(
            sig.split_whitespace().collect::<String>(),
            "asyncfnget(&self,req:super::Ping)->::core::result::Result<super::Pong,::leaf_grpc::Status>"
                .to_string()
        );
    }

    #[test]
    fn emits_the_server_stream_method_signature() {
        let sig = method_signature("list", "super::Ping", "super::Pong", CallShape::ServerStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:super::Ping"),
            "server-stream request is a plain message: {flat}"
        );
        assert!(
            flat.contains("::leaf_grpc::Streaming<super::Pong>"),
            "server-stream response is Streaming<U>: {flat}"
        );
    }

    #[test]
    fn emits_the_client_stream_method_signature() {
        let sig = method_signature("upload", "super::Ping", "super::Pong", CallShape::ClientStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:::leaf_grpc::Streaming<super::Ping>"),
            "client-stream request is Streaming<T>: {flat}"
        );
        assert!(
            flat.contains("->::core::result::Result<super::Pong"),
            "client-stream response is a plain message: {flat}"
        );
    }

    #[test]
    fn emits_the_bidi_method_signature() {
        let sig = method_signature("chat", "super::Ping", "super::Pong", CallShape::Bidi);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(flat.contains("req:::leaf_grpc::Streaming<super::Ping>"), "got: {flat}");
        assert!(
            flat.contains("::core::result::Result<::leaf_grpc::Streaming<super::Pong>,::leaf_grpc::Status>"),
            "bidi response is Streaming<U>: {flat}"
        );
    }
}
