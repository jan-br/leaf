//! Global error handling: the [`ControlAdvice`] seam (Spring's
//! `@ControllerAdvice` + `@ExceptionHandler`), expressed in leaf's DI.
//!
//! When a handler (or a filter, or an extractor) returns `Err(LeafError)`, the
//! server's `Dispatcher` (see [`crate::server`]) walks the ordered
//! `Vec<Ref<dyn ControlAdvice>>` it collected from the container and asks each to
//! map the error to a [`Response`]. The first advice that returns `Some` wins;
//! if none match, a built-in default mapping applies (a `500`). Advice beans are
//! ordinary `#[component]`s (contributed via the `dyn ControlAdvice` view); the
//! `#[control_advice]` stereotype (Stage 2) writes the impl.

use leaf_core::LeafError;

use crate::{Request, Response};

/// Maps a [`LeafError`] (raised by a handler / filter / extractor) to a
/// [`Response`] — Spring's `@ExceptionHandler`. Ordered (lower [`order`] = earlier);
/// the first advice returning `Some` wins.
///
/// Unlike the around-advice [`crate::WebFilter`], this is a SYNCHRONOUS, infallible
/// mapping (`&LeafError`, `&Request` → `Option<Response>`): error mapping inspects
/// already-collected data and produces a response or declines, so there is no
/// `.await` and no second failure to propagate. A `dyn`-dispatched, non-async trait
/// needs no `BoxFuture` seam.
///
/// [`order`]: ControlAdvice::order
pub trait ControlAdvice: Send + Sync {
    /// Map `err` (raised handling `req`) to a [`Response`], or return `None` to
    /// decline (letting a later advice — or the default mapping — handle it).
    fn handle(&self, err: &LeafError, req: &Request) -> Option<Response>;

    /// The relative order among advice (lower = consulted earlier). Ties keep the
    /// collection (registration) order — the sort is stable. Defaults to `0`.
    fn order(&self) -> i32 {
        0
    }
}

// Make `dyn ControlAdvice` an injectable VIEW (the by-trait-injection seam, emitted
// ONCE — orphan-rule-OK since `dyn ControlAdvice` is local to this crate). The
// `#[control_advice]` stereotype (Stage 2) publishes the `dyn ControlAdvice` view; the
// server collects EVERY provider as `Vec<Ref<dyn ControlAdvice>>` (collection injection)
// for its ordered error-mapping chain.
leaf_core::impl_resolve_view!(dyn ControlAdvice);
