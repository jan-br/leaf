//! A small order-management demo built on the `leaf` umbrella alone.
//!
//! Run:  `cargo run -p storefront`   Test: `cargo test -p storefront`.
//! Features-by-package (DDD): `platform` (infra beans), `catalog`, `orders`, `pricing`.

// The annotation macros emit absolute `::leaf_core::`/`::leaf_cache::`/`::leaf_tx::`
// paths; these source aliases of the single `leaf` dep make them resolve umbrella-only
// (NOT new Cargo deps — `Cargo.toml` still names only `leaf`).
extern crate leaf as leaf_core;
extern crate leaf as leaf_cache;
extern crate leaf as leaf_tx;

mod catalog;
mod orders;
mod platform;
mod pricing;

#[cfg(test)]
mod tests;

use std::any::TypeId;

use crate::catalog::product_repository::ProductRepository;
use crate::orders::order_service::OrderService;
use crate::platform::app_properties::AppProperties;
use crate::pricing::discount_policy::DiscountPolicy;

/// The umbrella-only entry: `#[leaf::main]` bootstraps + runs to Ready, then we resolve
/// `OrderService`, place a demo order, and print a one-line summary. The conditionally-
/// gated `DiscountPolicy` is applied only when it is present in the registry (i.e. when
/// run with `--pricing.discounts.enabled=true`).
#[leaf::main]
async fn main(app: &leaf::boot::RunningApp) -> Result<(), leaf::LeafError> {
    let props = app.context().get::<AppProperties>().await?;
    let catalog = app.context().get::<ProductRepository>().await?;
    let orders = app.context().get::<OrderService>().await?;

    let order = orders.place_order("COFFEE".into(), 2)?;
    let name = catalog.find(&order.sku).map_or("?", |p| p.name);

    let discount = match app.context().engine().registry().candidates(TypeId::of::<DiscountPolicy>()) {
        [] => 0,
        _ => app.context().get::<DiscountPolicy>().await?.discount_cents(order.total_cents),
    };

    println!(
        "[{}] placed order #{}: {}x {} ({}) = {}c (discount {}c)",
        props.name, order.id, order.qty, order.sku, name, order.total_cents, discount,
    );
    Ok(())
}
