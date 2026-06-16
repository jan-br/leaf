//! A small order-management demo built on the `leaf` umbrella alone.
//!
//! Run:  `cargo run -p storefront`   Test: `cargo test -p storefront`.
//! Features-by-package (DDD): `platform` (infra beans + the startup runner), `catalog`,
//! `order`, `pricing`.
//!
//! Like a Spring `main`, this does NOTHING but hand off to the framework — all work runs
//! in beans. The demo (place an order, print a summary) lives in `platform::StartupRunner`
//! (leaf's `Runner` = Spring's `ApplicationRunner`), which fires in the readiness window.

mod catalog;
mod order;
mod platform;
mod pricing;

/// `#[leaf::main]` bootstraps + runs the app to Ready (the graph wires, config binds,
/// auto-configs participate, the runners fire), then drains a clean shutdown. The body is
/// empty: the application's behaviour lives in its beans, not in `main`.
#[leaf::main]
async fn main() -> Result<(), leaf::LeafError> {
    Ok(())
}
