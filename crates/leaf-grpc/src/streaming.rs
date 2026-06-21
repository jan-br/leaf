//! `Streaming<T>` — the typed gRPC message stream (`Stream` of `Result<T, Status>`).

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use leaf_core::BoxStream;

use crate::status::Status;

/// A typed gRPC message stream: a `Stream` of `Result<T, Status>`. The generated
/// server-trait methods take/return this for the streaming call shapes
/// (server-stream returns `Streaming<U>`; client-stream/bidi take `Streaming<T>`),
/// and the [`GrpcHandler`](crate::GrpcHandler) wraps it with the wire framing/codec.
///
/// It wraps a `leaf_core::BoxStream` (the `futures` neutral vocabulary, NOT a
/// backend body) so it is backend-free and `'static` (rides the executor).
pub struct Streaming<T> {
    inner: BoxStream<'static, Result<T, Status>>,
}

impl<T> Streaming<T> {
    /// Wrap an existing boxed stream of `Result<T, Status>`.
    #[must_use]
    pub fn new(inner: BoxStream<'static, Result<T, Status>>) -> Self {
        Streaming { inner }
    }

    /// A single-item stream that yields `Ok(item)` once, then ends — the trivial
    /// server-stream a unary-shaped body lifts into when only one message is sent.
    #[must_use]
    pub fn once(item: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        Streaming { inner: Box::pin(futures::stream::once(async move { Ok(item) })) }
    }
}

impl<T> Stream for Streaming<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Delegate to the wrapped boxed stream (already `Unpin` via `Pin<Box<..>>`).
        self.inner.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Code, Status};
    use futures::executor::block_on;
    use futures::StreamExt;

    #[test]
    fn streaming_once_yields_exactly_one_ok_item() {
        let s = Streaming::once(7u32);
        let items: Vec<Result<u32, Status>> = block_on(s.collect());
        assert_eq!(items, vec![Ok(7)]);
    }

    #[test]
    fn streaming_new_threads_ok_and_err_items_in_order() {
        let inner: leaf_core::BoxStream<'static, Result<u32, Status>> = Box::pin(
            futures::stream::iter(vec![
                Ok(1u32),
                Err(Status::new(Code::Internal, "boom")),
                Ok(3u32),
            ]),
        );
        let s = Streaming::new(inner);
        let items: Vec<Result<u32, Status>> = block_on(s.collect());
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Ok(1));
        assert_eq!(items[1], Err(Status::new(Code::Internal, "boom")));
        assert_eq!(items[2], Ok(3));
    }
}
