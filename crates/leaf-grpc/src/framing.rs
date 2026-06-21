//! The gRPC length-prefix wire framing: [`encode_frame`] + [`decode_frames`].

use bytes::{BufMut, Bytes, BytesMut};

/// Encode one gRPC length-prefixed wire frame: a 1-byte compression flag (always
/// `0`, leaf does not compress — §8), then a 4-byte BIG-ENDIAN message length, then
/// the message bytes. The exact format a tonic/grpc-go peer reads off the HTTP/2
/// DATA stream.
#[must_use]
pub fn encode_frame(msg: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + msg.len());
    buf.put_u8(0); // compression flag: 0 = uncompressed
    // The length is a u32; a message longer than u32::MAX is not representable on
    // the gRPC wire, so the cast is the protocol's own bound (saturate defensively).
    buf.put_u32(u32::try_from(msg.len()).unwrap_or(u32::MAX)); // big-endian (put_u32 is BE)
    buf.put_slice(msg);
    buf.freeze()
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_frame_prefixes_compression_byte_and_be_length() {
        // The canonical gRPC length-prefix: [0][00 00 00 03]["abc"].
        let framed = encode_frame(b"abc");
        assert_eq!(framed.len(), 1 + 4 + 3);
        assert_eq!(framed[0], 0, "compression flag: 0 = uncompressed");
        assert_eq!(&framed[1..5], &[0, 0, 0, 3], "4-byte big-endian length");
        assert_eq!(&framed[5..], b"abc");
    }

    #[test]
    fn encode_frame_of_empty_message_is_a_five_byte_header_only() {
        let framed = encode_frame(b"");
        assert_eq!(framed.len(), 5);
        assert_eq!(framed[0], 0);
        assert_eq!(&framed[1..5], &[0, 0, 0, 0]);
    }
}
