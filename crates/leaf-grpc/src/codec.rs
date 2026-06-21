//! The gRPC message-codec seam: [`GrpcCodec`] + the prost-backed [`ProstCodec`].

use bytes::Bytes;

use crate::status::{Code, Status};

/// The gRPC MESSAGE codec seam (the `HttpMessageConverter` analogue, confined to one
/// data format): encode a typed prost message to bytes, decode bytes back. The
/// methods are generic over `M: prost::Message` (NOT object-safe), so a handler holds
/// a CONCRETE codec — the same shape `HttpMessageConverterExt::read<T>` takes off the
/// dyn-safe converter. `prost` is named only behind this seam.
pub trait GrpcCodec: Send + Sync {
    /// Encode a typed message to its protobuf wire bytes (never fails — prost encoding
    /// into a `Vec` is infallible).
    fn encode<M: prost::Message>(&self, m: &M) -> Bytes;

    /// Decode protobuf wire bytes into `M`.
    ///
    /// # Errors
    /// A [`Code::Internal`] [`Status`] if the bytes are malformed for `M`.
    fn decode<M: prost::Message + Default>(&self, b: &[u8]) -> Result<M, Status>;
}

/// The prost-backed [`GrpcCodec`] — leaf-grpc's `JsonConverter` analogue. The ONLY
/// place `prost` is named (the message codec is confined here exactly as `serde_json`
/// is confined to leaf-serde's converter). Stateless.
///
/// Registered as a managed `#[component]` singleton (a no-collaborator ZST bean) so the
/// `#[grpc_controller]` per-method `GrpcRoute` beans can field-inject `Ref<ProstCodec>` —
/// the CONCRETE codec, since [`GrpcCodec`] is not object-safe (its methods are generic over
/// `M: prost::Message`). The dogfooded registration replaces a hand-rolled provider, exactly
/// like `GrpcDispatchConfig`.
#[leaf_macros::component]
#[derive(Clone, Copy, Default)]
pub struct ProstCodec;

impl ProstCodec {
    /// A fresh prost codec (stateless).
    #[must_use]
    pub fn new() -> Self {
        ProstCodec
    }
}

impl GrpcCodec for ProstCodec {
    fn encode<M: prost::Message>(&self, m: &M) -> Bytes {
        // prost encodes into a growable buffer; encoding is infallible.
        let mut buf = Vec::with_capacity(m.encoded_len());
        m.encode(&mut buf).expect("prost encode into a Vec is infallible");
        Bytes::from(buf)
    }

    fn decode<M: prost::Message + Default>(&self, b: &[u8]) -> Result<M, Status> {
        M::decode(b).map_err(|e| {
            Status::new(Code::Internal, format!("protobuf decode failed: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Code;

    // A tiny hand-written prost message (no codegen yet, Stage 3): one string field
    // (tag 1, wire type 2 = length-delimited). prost::Message is derivable, but for a
    // self-contained codec test we use prost's built-in `String` Message impl — prost
    // implements Message for the well-known scalar wrappers, so a `String` is a
    // complete (single-field-less) message we can round-trip the codec over.
    #[test]
    fn prost_codec_round_trips_a_message() {
        let codec = ProstCodec::new();
        // prost implements Message for () (the empty message): encode is empty bytes,
        // decode of empty bytes succeeds. A non-empty buffer for () is still accepted
        // (unknown fields are skipped), so we assert the empty round-trip exactly.
        let unit: () = ();
        let bytes = codec.encode(&unit);
        assert!(bytes.is_empty(), "the empty message encodes to zero bytes");
        let back: () = codec.decode(&bytes).expect("empty message decodes");
        assert_eq!(back, ());
    }

    #[test]
    fn prost_codec_decode_of_malformed_bytes_is_an_internal_status() {
        let codec = ProstCodec::new();
        // A truncated length-delimited field (tag 1 says "10 bytes follow" but the
        // buffer ends) is malformed for a u32 message → a Code::Internal Status.
        let err = codec
            .decode::<u32>(&[0x08, 0xff, 0xff, 0xff, 0xff, 0xff])
            .expect_err("malformed prost bytes → Status");
        assert_eq!(err.code, Code::Internal);
    }
}
