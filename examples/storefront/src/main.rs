//! A small order-management demo built on the `leaf` umbrella alone.
//!
//! Run:  `cargo run -p storefront`   Test: `cargo test -p storefront`.
//! Features-by-package (DDD): `platform` (infra beans + the startup runner), `catalog`,
//! `order`, `pricing`, and — with the `web` feature — `web` (the REST surface). The beans
//! live in the `storefront` LIBRARY crate (so the integration tests link them too); this
//! binary is the thin entrypoint that drives the framework.
//!
//! Like a Spring `main`, this does NOTHING but hand off to the framework — all work runs
//! in beans. The demo (place an order, print a summary) lives in `platform::StartupRunner`
//! (leaf's `Runner` = Spring's `ApplicationRunner`), which fires in the readiness window.
//! With `web`, the auto-configured `EmbeddedWebServer` `#[keep_alive]` then serves the REST
//! endpoints on a spawned lifecycle task while `#[leaf::main]` parks until shutdown.

// Link the storefront library's bean rows into this binary: a binary-only crate's
// `linkme` rows would not otherwise reach a sibling integration test's link graph, so the
// beans live in the lib and BOTH targets link them. Referencing the lib pins its rlib
// onto this binary's link graph (the bean `distributed_slice` rows ride along).
use storefront as _;

/// `#[leaf::main]` bootstraps + runs the app to Ready (the graph wires, config binds,
/// auto-configs participate, the runners fire). With the `web` feature the
/// `EmbeddedWebServer` `#[keep_alive]` serves the REST endpoints on a spawned lifecycle
/// task and `run_main` PARKS until shutdown; without it, `park_until_shutdown` is an
/// immediate no-op and the app drains a clean shutdown after the startup runner. The body
/// is empty: the application's behaviour lives in its beans, not in `main`.
#[leaf::main]
async fn main() -> Result<(), leaf::LeafError> {
    Ok(())
}
