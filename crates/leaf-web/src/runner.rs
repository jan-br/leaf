//! The [`WebServerRunner`] — the leaf-web `#[runner]` that ASSEMBLES the server from the
//! container and serves.
//!
//! This is the DI payoff (the design's §4): the server does not wire itself from a central
//! registry — it injects every contribution as a `dyn`-view collection and builds the
//! [`Dispatcher`] from them:
//!
//! - `Ref<dyn WebServer>` — the pluggable backend (the FALLBACK `leaf-web-hyper` server, or
//!   a user backend that supersedes it via `OnMissingBean(dyn WebServer)`),
//! - `Vec<Ref<dyn Route>>` — every route any `#[controller]`/`#[rest_controller]`
//!   contributed (collection + by-trait injection),
//! - `Vec<Ref<dyn WebFilter>>` — every filter/interceptor any crate contributed,
//! - `Vec<Ref<dyn ControlAdvice>>` — every `#[control_advice]` mapping any crate contributed,
//! - `Ref<ServerProperties>` — the bound `leaf.web.server.*` address.
//!
//! It is an ordinary `#[runner]` bean (Spring's `ApplicationRunner` that starts the embedded
//! server): the run pipeline auto-collects it and fires it in the readiness-gate window. Its
//! [`Runner::run`](leaf_core::Runner::run) builds the dispatcher and calls
//! [`WebServer::serve`], which BLOCKS on the accept loop — the embedded server keeps the
//! process serving until shutdown (the Spring `WebServer` model; the runner thread is the
//! server thread). NOTHING here names a backend library — `serve` is the backend-free seam.

use std::sync::Arc;

use leaf_core::{LeafError, Ref};

use crate::advice::ControlAdvice;
use crate::filter::WebFilter;
use crate::handler::Route;
use crate::server::{Dispatcher, ServerProperties, WebServer};

/// The `#[runner]` that builds the [`Dispatcher`] from the container and serves it on the
/// injected [`WebServer`] backend.
///
/// Every field is injected: the backend (`Ref<dyn WebServer>`), the route/filter/advice
/// collections (`Vec<Ref<dyn _>>`, collection + by-trait injection), and the bound
/// [`ServerProperties`] (`Ref<ServerProperties>`). No `.with_runner`, no hand-built
/// dispatcher — the run pipeline auto-collects this runner and fires it.
#[leaf_macros::runner]
pub struct WebServerRunner {
    /// The pluggable embedded server (the FALLBACK hyper backend, or a user backend that
    /// superseded it via `OnMissingBean(dyn WebServer)`).
    server: Ref<dyn WebServer>,
    /// Every route any controller contributed — the routing table.
    routes: Vec<Ref<dyn Route>>,
    /// Every filter any crate contributed — the around-advice chain (ordered in the
    /// [`Dispatcher`]).
    filters: Vec<Ref<dyn WebFilter>>,
    /// Every control-advice any crate contributed — the error→response chain.
    advice: Vec<Ref<dyn ControlAdvice>>,
    /// The bound `leaf.web.server.*` address the backend binds to.
    props: Ref<ServerProperties>,
}

#[leaf_macros::async_impl]
impl leaf_core::Runner for WebServerRunner {
    /// Build the [`Dispatcher`] from the injected contributions and serve it.
    ///
    /// `serve` runs the accept loop (it BLOCKS until shutdown), so this runner is the
    /// embedded server's lifetime — the same shape Spring's embedded-server start takes.
    async fn run(&self, _args: &leaf_core::ApplicationArguments) -> Result<(), LeafError> {
        // COLLECTION + BY-TRAIT injection already gathered every contribution; build the
        // request engine from them (ordering of filters/advice is applied in `new`). The
        // `Ref<dyn _>` -> `Arc<dyn _>` conversion is a cheap clone-unwrap.
        let dispatcher = Arc::new(Dispatcher::new(
            self.routes.iter().map(|r| Ref::clone(r).into_arc()).collect(),
            self.filters.iter().map(|f| Ref::clone(f).into_arc()).collect(),
            self.advice.iter().map(|a| Ref::clone(a).into_arc()).collect(),
        ));
        // Serve on the injected backend with the bound address. This blocks on the accept
        // loop until shutdown — the embedded server is up for the life of the process.
        self.server.serve(dispatcher, &self.props).await
    }
}

// The `run` body is desugared into the `BoxFuture`-returning `Runner::run` by
// `#[async_impl]` (no hand-rolled `Box::pin`). The `&self`/`&props` borrows live across the
// single `serve(..).await`, which is the runner's whole body — no borrow escapes `run`.
