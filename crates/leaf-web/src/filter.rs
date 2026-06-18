//! The around-advice seam: [`WebFilter`] + [`Next`] + the filter-chain runner.
//! Spring's servlet `Filter` + `HandlerInterceptor`, expressed in leaf's DI.
//!
//! A [`WebFilter`] wraps the rest of the request handling: it inspects/modifies
//! the [`Request`], then either calls [`Next::run`] to continue down the chain, or
//! short-circuits by returning a [`Response`] itself. Filters are ordinary beans
//! the server collects (`Vec<Ref<dyn WebFilter>>`, collection injection) and runs
//! ordered by [`WebFilter::order`] (lower = earlier). At the bottom of the chain
//! sits the [`Terminal`] — the route-dispatch step the server's `Dispatcher`
//! (Task 6) provides. [`FilterChain`] threads the ordered filters and the terminal
//! into a single runnable [`Next`].

use leaf_core::{BoxFuture, LeafError};

use crate::{Request, Response};

/// The bottom of the filter chain: the route-dispatch step that turns the
/// (filtered) [`Request`] into a [`Response`].
///
/// `Terminal` is dyn-dispatched and async, so it returns a [`BoxFuture`] at the
/// `dyn` seam (leaf's uniform answer to non-`dyn`-compatible `async fn`). The
/// server's `Dispatcher` (Task 6) implements it as "match a [`Route`] and invoke
/// its [`Handler`]"; the `#[cfg(test)]` fakes here are the lone Stage-1
/// hand-written impls.
///
/// [`Route`]: crate::Route
/// [`Handler`]: crate::Handler
pub trait Terminal: Send + Sync {
    /// Dispatch the fully-filtered request to its handler.
    fn dispatch<'a>(&'a self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>>;
}

/// An around-advice filter (Spring servlet `Filter` / `HandlerInterceptor`): it
/// runs before/after the rest of the chain, may modify the [`Request`], and either
/// continues via [`Next::run`] or short-circuits with its own [`Response`].
///
/// `filter` is dyn-dispatched and async → a [`BoxFuture`] at the `dyn` seam.
/// Filters are ordinary beans contributing the `dyn WebFilter` view; the server
/// collects them (`Vec<Ref<dyn WebFilter>>`) and runs them ordered by [`order`].
///
/// [`order`]: WebFilter::order
pub trait WebFilter: Send + Sync {
    /// Filter `req`: inspect/modify, then call `next.run(req)` to continue, or
    /// return a [`Response`] to short-circuit (skipping the remaining filters and
    /// the [`Terminal`]).
    fn filter<'a>(&'a self, req: Request, next: Next<'a>)
        -> BoxFuture<'a, Result<Response, LeafError>>;

    /// The relative order in the chain (lower = earlier). Ties keep the
    /// collection (registration) order — the sort is stable. Defaults to `0`.
    fn order(&self) -> i32 {
        0
    }
}

/// The continuation handed to a [`WebFilter`]: the remaining (ordered) filters
/// plus the [`Terminal`]. Calling [`run`] either invokes the next filter or — when
/// no filters remain — the terminal.
///
/// `Next` OWNS its remaining-filter list (each a `&'a dyn WebFilter`), so it lives
/// across every `.await` without borrowing the [`FilterChain`] that built it.
///
/// [`run`]: Next::run
pub struct Next<'a> {
    /// The not-yet-run filters, already ordered, in REVERSE so the next-to-run is
    /// the last element (a cheap `pop`, no front-shift).
    rev_remaining: Vec<&'a dyn WebFilter>,
    /// The route-dispatch step at the bottom of the chain.
    terminal: &'a dyn Terminal,
}

impl<'a> Next<'a> {
    /// Continue the chain: run the next filter, or — when none remain — the
    /// [`Terminal`].
    #[must_use]
    pub fn run(mut self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        match self.rev_remaining.pop() {
            Some(head) => {
                let next = Next { rev_remaining: self.rev_remaining, terminal: self.terminal };
                head.filter(req, next)
            }
            None => self.terminal.dispatch(req),
        }
    }
}

/// Builds a runnable chain from the container-collected filters + the terminal:
/// orders the filters by [`WebFilter::order`] (stable) and exposes [`run`].
///
/// [`run`]: FilterChain::run
pub struct FilterChain<'a> {
    /// The filters, sorted by `order()` (stable; ties keep input order).
    ordered: Vec<&'a dyn WebFilter>,
    terminal: &'a dyn Terminal,
}

impl<'a> FilterChain<'a> {
    /// Build a chain over `filters` (sorted stably by [`WebFilter::order`]) ending
    /// in `terminal`.
    #[must_use]
    pub fn new(filters: &[&'a dyn WebFilter], terminal: &'a dyn Terminal) -> Self {
        let mut ordered: Vec<&'a dyn WebFilter> = filters.to_vec();
        // Stable sort: filters with equal `order()` keep their collection order.
        ordered.sort_by_key(|f| f.order());
        FilterChain { ordered, terminal }
    }

    /// Run the request through every filter (in order) then the terminal, unless a
    /// filter short-circuits.
    #[must_use]
    pub fn run(self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        // Reverse so `Next::run` can `pop` the next-to-run filter off the end. The
        // `Next` OWNS this list — no borrow of `self` escapes into the future.
        let mut rev_remaining = self.ordered;
        rev_remaining.reverse();
        Next { rev_remaining, terminal: self.terminal }.run(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Response};
    use bytes::Bytes;
    use futures::executor::block_on;
    use http::{Method, StatusCode};
    use leaf_core::{ErrorKind, LeafError};
    use std::sync::{Arc, Mutex};

    /// A `#[cfg(test)]` fake terminal: the route-dispatch end of the chain. In
    /// production the dispatcher (Task 6) provides this; here it just echoes a
    /// fixed `200 "terminal"` body so a test can prove the chain reached it.
    struct EchoTerminal;

    #[leaf_macros::async_impl]
    impl Terminal for EchoTerminal {
        async fn dispatch(&self, _req: Request) -> Result<Response, LeafError> {
            Ok(Response::ok().with_body(Bytes::from_static(b"terminal")))
        }
    }

    /// A logging filter (around-advice): records its tag in the shared log, then
    /// delegates to the rest of the chain. Hand-written async via `#[async_impl]`
    /// (an in-crate `#[cfg(test)]` impl — the lone Stage-1 exception).
    struct LogFilter {
        tag: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
        order: i32,
    }

    #[leaf_macros::async_impl]
    impl WebFilter for LogFilter {
        async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
            self.log.lock().expect("log lock").push(self.tag);
            next.run(req).await
        }
        fn order(&self) -> i32 {
            self.order
        }
    }

    /// A short-circuiting filter: if the request carries `x-block`, it returns a
    /// `403` WITHOUT calling `next` (the terminal must never run); otherwise it
    /// passes through.
    struct BlockFilter;

    #[leaf_macros::async_impl]
    impl WebFilter for BlockFilter {
        async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
            if req.header("x-block").is_some() {
                return Ok(Response::new(StatusCode::FORBIDDEN));
            }
            next.run(req).await
        }
    }

    fn request_with(block: bool) -> Request {
        let mut headers = http::HeaderMap::new();
        if block {
            headers.insert("x-block", http::HeaderValue::from_static("1"));
        }
        Request::new(Method::GET, "/x".parse().expect("uri"), headers, Bytes::new())
    }

    #[test]
    fn chain_runs_filters_in_order_then_reaches_terminal() {
        let log = Arc::new(Mutex::new(Vec::new()));
        // Declared out of order (order 10 before order 1) to prove ordering is by
        // `order()`, not declaration order.
        let late = LogFilter { tag: "late", log: log.clone(), order: 10 };
        let early = LogFilter { tag: "early", log: log.clone(), order: 1 };
        let filters: Vec<&dyn WebFilter> = vec![&late, &early];
        let terminal = EchoTerminal;

        let resp = block_on(
            FilterChain::new(&filters, &terminal).run(request_with(false)),
        )
        .expect("chain succeeds");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"terminal".as_slice());
        // Ordered by `order()`: early (1) before late (10), terminal reached.
        assert_eq!(*log.lock().expect("log"), vec!["early", "late"]);
    }

    #[test]
    fn short_circuit_skips_remaining_filters_and_terminal() {
        let log = Arc::new(Mutex::new(Vec::new()));
        // logger (order 0) runs first, then blocker (order 1) short-circuits — so
        // the logger ran but the terminal never does.
        let logger = LogFilter { tag: "log", log: log.clone(), order: 0 };
        let blocker = BlockFilter; // default order 0; placed after the logger.
        let filters: Vec<&dyn WebFilter> = vec![&logger, &blocker];
        let terminal = EchoTerminal;

        let resp = block_on(
            FilterChain::new(&filters, &terminal).run(request_with(true)),
        )
        .expect("chain succeeds");

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        // The logger ran; the blocker short-circuited so the terminal never ran
        // (the body is empty, not the terminal's "terminal").
        assert!(resp.body_bytes().is_empty());
        assert_eq!(*log.lock().expect("log"), vec!["log"]);
    }

    #[test]
    fn empty_chain_runs_the_terminal_directly() {
        let filters: Vec<&dyn WebFilter> = vec![];
        let terminal = EchoTerminal;
        let resp = block_on(
            FilterChain::new(&filters, &terminal).run(request_with(false)),
        )
        .expect("chain succeeds");
        assert_eq!(resp.body_bytes(), b"terminal".as_slice());
    }

    #[test]
    fn terminal_error_propagates_through_the_chain() {
        struct ErrTerminal;
        #[leaf_macros::async_impl]
        impl Terminal for ErrTerminal {
            async fn dispatch(&self, _req: Request) -> Result<Response, LeafError> {
                Err(LeafError::new(ErrorKind::ConstructionFailed))
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let logger = LogFilter { tag: "log", log: log.clone(), order: 1 };
        let filters: Vec<&dyn WebFilter> = vec![&logger];
        let terminal = ErrTerminal;

        let err = block_on(
            FilterChain::new(&filters, &terminal).run(request_with(false)),
        )
        .expect_err("terminal error propagates out");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
        assert_eq!(*log.lock().expect("log"), vec!["log"]);
    }
}
