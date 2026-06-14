//! The async-across-`dyn` boxing standard.
//!
//! Every `dyn` seam in leaf returns a [`BoxFuture`] because async-fn-in-trait
//! (AFIT) and `-> impl Future` (RPITIT) are **not** `dyn`-compatible — true
//! regardless of nightly (TOOLKIT.md, ADR-01 ownership-model line 35). Boxing
//! at the `dyn` boundary is the single, uniform answer the whole kernel pins to.

use std::future::Future;
use std::pin::Pin;

/// The one boxed-future shape returned at every `dyn` seam in leaf.
///
/// `Send + 'a` is mandatory: shared beans are `Send + Sync + 'static`, so the
/// futures that construct them must be `Send` to ride the executor across
/// threads. This mirrors `futures::future::BoxFuture` exactly but is defined
/// here so the kernel ABI does not leak the `futures` crate at its surface.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time smoke test: a `BoxFuture` is constructible from any
    // `Send` future and is itself `Send` (so it rides the executor).
    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn box_future_is_constructible_and_send() {
        let fut: BoxFuture<'static, i32> = Box::pin(async { 21 + 21 });
        assert_send(&fut);
        let out = futures::executor::block_on(fut);
        assert_eq!(out, 42);
    }

    #[test]
    fn box_future_can_borrow_for_a_lifetime() {
        let data = [1, 2, 3];
        let fut: BoxFuture<'_, usize> = Box::pin(async { data.len() });
        assert_eq!(futures::executor::block_on(fut), 3);
    }
}
