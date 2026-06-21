//! The gRPC length-prefix wire framing: [`encode_frame`] + [`decode_frames`].

use bytes::{BufMut, Bytes, BytesMut};
use futures::StreamExt;
use leaf_core::BoxStream;
use leaf_web::{Body, Frame};

use crate::status::{Code, Status};

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

/// De-frame a [`Body`] (a `Body::Stream` of H2 frames, or a `Body::Full` buffer)
/// into a stream of COMPLETE gRPC messages. It buffers across frame boundaries: a
/// length-prefix header or a message body may straddle several H2 DATA frames, so
/// the de-framer holds a running buffer and emits a message only once its full
/// 5-byte header + body have arrived. Trailers ([`Frame::Trailers`]) carry no
/// message bytes and are skipped. A stream that ends mid-frame is a malformed wire
/// → a single `Code::Internal` [`Status`] (loud, never a silent truncation).
///
/// Backend-free: the input is a leaf [`Body`] and the output a `leaf_core::BoxStream`
/// — no hyper/h2 names appear.
#[must_use]
pub fn decode_frames(body: Body) -> BoxStream<'static, Result<Bytes, Status>> {
    // Normalise both Body shapes into ONE stream of raw byte chunks: a Full body is a
    // single-chunk stream; a Stream body maps each Data frame to its bytes and drops
    // Trailers. (A frame-level transport error becomes an Internal Status chunk-error
    // that the fold below surfaces.)
    let chunks: BoxStream<'static, Result<Bytes, Status>> = match body {
        Body::Full(b) => Box::pin(futures::stream::once(async move { Ok(b) })),
        Body::Stream(s) => Box::pin(s.filter_map(|frame| async move {
            match frame {
                Ok(Frame::Data(b)) => Some(Ok(b)),
                Ok(Frame::Trailers(_)) => None,
                Err(e) => Some(Err(Status::new(
                    Code::Internal,
                    format!("gRPC frame transport error: {e}"),
                ))),
            }
        })),
    };

    // A stateful de-framer: `unfold` threads (chunk-stream, running-buffer, done) and
    // emits each complete message as it becomes available. We pull more chunks only
    // when the buffer cannot yet satisfy a full header+body.
    struct State {
        chunks: BoxStream<'static, Result<Bytes, Status>>,
        buf: BytesMut,
        errored: bool,
    }

    let init = State { chunks, buf: BytesMut::new(), errored: false };

    Box::pin(futures::stream::unfold(init, |mut st| async move {
        if st.errored {
            return None;
        }
        loop {
            // Enough for a header? (1 flag + 4 length.)
            if st.buf.len() >= 5 {
                let len = u32::from_be_bytes([st.buf[1], st.buf[2], st.buf[3], st.buf[4]]) as usize;
                let total = 5 + len;
                if st.buf.len() >= total {
                    // A complete frame: split off [flag..body], drop the 5-byte header.
                    let mut frame = st.buf.split_to(total);
                    let _header = frame.split_to(5);
                    return Some((Ok(frame.freeze()), st));
                }
            }
            // Need more bytes: pull the next chunk.
            match st.chunks.next().await {
                Some(Ok(chunk)) => {
                    st.buf.extend_from_slice(&chunk);
                    // loop: re-check whether the buffer now satisfies a full frame.
                }
                Some(Err(status)) => {
                    st.errored = true;
                    return Some((Err(status), st));
                }
                None => {
                    // Stream ended. A clean end (empty buffer) → done. A non-empty
                    // residue is a truncated frame → one loud Internal Status.
                    if st.buf.is_empty() {
                        return None;
                    }
                    st.errored = true;
                    return Some((
                        Err(Status::new(
                            Code::Internal,
                            "gRPC stream ended mid-frame (truncated length-prefix or body)",
                        )),
                        st,
                    ));
                }
            }
        }
    }))
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

#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::status::Code;
    use bytes::Bytes;
    use futures::executor::block_on;
    use futures::StreamExt;
    use leaf_web::{Body, Frame};

    /// Build a `Body::Stream` from a sequence of raw data-frame byte chunks (the
    /// shape the hyper edge produces: each H2 DATA frame is one `Frame::Data`).
    fn body_of(chunks: Vec<Vec<u8>>) -> Body {
        let frames = chunks
            .into_iter()
            .map(|c| Ok(Frame::Data(Bytes::from(c))));
        Body::Stream(Box::pin(futures::stream::iter(frames.collect::<Vec<_>>())))
    }

    #[test]
    fn decode_frames_reassembles_two_messages_split_across_chunks() {
        // Two framed messages ("hi", "bye"), arbitrarily re-chunked so a header AND
        // a message body straddle a data-frame boundary — the de-framer must buffer.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(b"hi"));
        wire.extend_from_slice(&encode_frame(b"bye"));
        // Split the contiguous wire bytes at an awkward offset (mid second header).
        let split = 6;
        let body = body_of(vec![wire[..split].to_vec(), wire[split..].to_vec()]);

        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        let ok: Vec<Bytes> = msgs.into_iter().map(|r| r.expect("a complete frame")).collect();
        assert_eq!(ok, vec![Bytes::from_static(b"hi"), Bytes::from_static(b"bye")]);
    }

    #[test]
    fn decode_frames_of_a_truncated_header_is_an_internal_status() {
        // A stream that ends mid-header (only 3 of the 5 prefix bytes) is a malformed
        // frame → a `Code::Internal` Status (loud, never a silent truncation).
        let body = body_of(vec![vec![0u8, 0, 0]]);
        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        assert_eq!(msgs.len(), 1);
        let err = msgs.into_iter().next().unwrap().expect_err("truncated → Status");
        assert_eq!(err.code, Code::Internal);
    }

    #[test]
    fn decode_frames_of_a_full_body_treats_the_buffer_as_one_chunk() {
        // A `Body::Full` (the collect path's shape) de-frames identically — the whole
        // buffer is the single source chunk.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode_frame(b"solo"));
        let body = Body::full(Bytes::from(wire));
        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        let ok: Vec<Bytes> = msgs.into_iter().map(|r| r.expect("a complete frame")).collect();
        assert_eq!(ok, vec![Bytes::from_static(b"solo")]);
    }

    #[test]
    fn encode_then_decode_round_trips_each_message() {
        let mut wire = Vec::new();
        for m in [b"alpha".as_slice(), b"", b"gamma"] {
            wire.extend_from_slice(&encode_frame(m));
        }
        let body = Body::full(Bytes::from(wire));
        let msgs: Vec<Result<Bytes, _>> = block_on(decode_frames(body).collect());
        let ok: Vec<Bytes> = msgs
            .into_iter()
            .map(|r| r.expect("complete frame"))
            .collect();
        assert_eq!(
            ok,
            vec![Bytes::from_static(b"alpha"), Bytes::from_static(b""), Bytes::from_static(b"gamma")],
        );
    }
}
