//! The [`EmbeddedWebServer`] — the leaf-web `#[keep_alive]` lifecycle bean that ASSEMBLES
//! the server from the container and SERVES for the life of the process.
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
//! ## Why a `#[keep_alive]`, not a `#[runner]`
//!
//! The embedded server is a LONG-RUNNING lifecycle component (Spring's `WebServer` over a
//! pluggable Tomcat/Netty): once started it keeps serving until shutdown. Modelling it as a
//! blocking `#[runner]` would STARVE the runner stream — a `Runner::run` that never returns
//! blocks every later runner. So it is a [`#[keep_alive]`](leaf_macros::keep_alive)
//! stereotype (a `#[component]` hardcoding `provides[] = dyn ::leaf_core::KeepAlive`, exactly
//! like `#[runner]` hardcodes `dyn Runner` and `#[web_filter]` hardcodes `dyn WebFilter`):
//! leaf-boot collects every `dyn KeepAlive` provider and SPAWNS its [`start`](KeepAlive::start)
//! onto the lifecycle machinery (off the runner stream by construction), latches readiness
//! via [`LifecycleCtx::on_ready`] when it is serving, parks it on the reactive shutdown
//! signal, and joins it (bounded by the grace budget) at teardown.
//!
//! Its [`KeepAlive::start`] builds the dispatcher and returns [`WebServer::serve`], handing
//! the [`LifecycleCtx`] straight through to the backend. NOTHING here names a backend
//! library — `serve` is the backend-free seam, and [`LifecycleCtx`] is a leaf-CORE type.

use std::sync::Arc;

use leaf_core::{BoxFuture, KeepAlive, LeafError, LifecycleCtx, Ref};

use crate::advice::ControlAdvice;
use crate::filter::WebFilter;
use crate::handler::Route;
use crate::server::{Dispatcher, ProtocolDispatch, ServerProperties, WebServer};

/// The `#[keep_alive]` bean that builds the [`Dispatcher`] from the container and serves it on
/// the injected [`WebServer`] backend for the life of the process.
///
/// Every field is injected: the backend (`Ref<dyn WebServer>`), the route/filter/advice
/// collections (`Vec<Ref<dyn _>>`, collection + by-trait injection), and the bound
/// [`ServerProperties`] (`Ref<ServerProperties>`). No `.with_runner`, no hand-built
/// dispatcher, no hand-rolled `dyn KeepAlive` registration — the `#[keep_alive]` stereotype
/// publishes the view and leaf-boot auto-collects + spawns this bean.
#[leaf_macros::keep_alive]
pub struct EmbeddedWebServer {
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
    /// Every protocol family any crate contributed (gRPC etc.) — checked by content-type
    /// before the HTTP route family. Collection + by-trait injection, like routes/filters.
    protocols: Vec<Ref<dyn ProtocolDispatch>>,
    /// The bound `leaf.web.server.*` address the backend binds to.
    props: Ref<ServerProperties>,
}

impl KeepAlive for EmbeddedWebServer {
    /// Build the [`Dispatcher`] from the injected contributions and serve it.
    ///
    /// The returned future is `'static` (it owns the assembled dispatcher, the cloned
    /// backend handle, the owned `Arc<ServerProperties>`, and the `ctx`), so leaf-boot can
    /// SPAWN it. We clone everything we need out of `&self` BEFORE delegating to
    /// [`WebServer::serve`], so the future borrows nothing of `self` across an await. The
    /// backend latches `ctx.on_ready` once serving, parks on `ctx.shutdown`, and drains.
    fn start(&self, ctx: LifecycleCtx) -> BoxFuture<'static, Result<(), LeafError>> {
        // COLLECTION + BY-TRAIT injection already gathered every contribution; build the
        // request engine from them (ordering of filters/advice is applied in `new`). The
        // `Ref<dyn _>` -> `Arc<dyn _>` conversion is a cheap clone-unwrap.
        let dispatcher = Arc::new(Dispatcher::new(
            self.routes.iter().map(|r| Ref::clone(r).into_arc()).collect(),
            self.filters.iter().map(|f| Ref::clone(f).into_arc()).collect(),
            self.advice.iter().map(|a| Ref::clone(a).into_arc()).collect(),
            self.protocols.iter().map(|p| Ref::clone(p).into_arc()).collect(),
        ));
        // Clone the backend handle + the bound address into OWNED Arcs so the serve future
        // is `'static` (it must outlive this `&self` borrow — leaf-boot spawns it).
        let server = Ref::clone(&self.server).into_arc();
        let props = Ref::clone(&self.props).into_arc();
        // Serve on the injected backend with the bound address + the lifecycle ctx. The
        // backend's `serve` future is `'static`; it binds, latches readiness, parks on the
        // shutdown signal, then drains — the embedded server's whole lifetime.
        server.serve(dispatcher, props, ctx)
    }
}
