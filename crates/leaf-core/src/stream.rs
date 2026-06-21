//! The boxed-stream standard — the streaming analogue of [`BoxFuture`](crate::BoxFuture).
//!
//! A `dyn` seam that yields a SEQUENCE of values (a streaming request/response body,
//! a gRPC message stream) returns a [`BoxStream`] for the same reason `dyn`-async
//! returns a `BoxFuture`: `impl Stream` is not `dyn`-compatible. This mirrors
//! `futures::stream::BoxStream` exactly but is defined here so the kernel ABI does not
//! leak the `futures` crate at its surface (the leaf-web `Body` names `leaf_core::BoxStream`,
//! never `futures::...`).

use std::pin::Pin;

use futures::Stream;

/// The one boxed-stream shape returned at a streaming `dyn` seam in leaf.
///
/// `Send + 'a` mirrors [`BoxFuture`](crate::BoxFuture): a streaming body rides the
/// executor across threads, so the stream it wraps must be `Send`.
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn box_stream_is_constructible_send_and_collectible() {
        let s: BoxStream<'static, i32> = Box::pin(futures::stream::iter([1, 2, 3]));
        assert_send(&s);
        let out: Vec<i32> = futures::executor::block_on(s.collect());
        assert_eq!(out, vec![1, 2, 3]);
    }
}
