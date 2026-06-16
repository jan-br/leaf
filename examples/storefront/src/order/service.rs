use leaf::prelude::*;
use leaf::InMemoryTransactionManager;

use crate::catalog::service::CatalogService;
use crate::order::Order;
use crate::order::repository::OrderRepository;

/// A `@Component` injecting [`CatalogService`] + [`OrderRepository`] whose `place_order`
/// is `#[transactional]` (commit on `Ok`, rollback on `Err`). It demonstrates the tx
/// concern, while `CatalogService` carries the cache concern — two concerns, two services.
#[service]
#[derive(Debug)]
pub struct OrderService {
    catalog: Ref<CatalogService>,
    orders: Ref<OrderRepository>,
}

#[advisable]
impl OrderService {
    /// Price the SKU (via the cached catalog lookup), total it, save, and return the
    /// order. `Ok` commits the surrounding tx; `Err` rolls it back.
    #[transactional(manager = InMemoryTransactionManager)]
    pub fn place_order(&self, sku: String, qty: u32) -> Result<Order, LeafError> {
        let unit = self.catalog.price_of(sku.clone())?;
        let order = Order {
            id: self.orders.next_id(),
            sku,
            qty,
            total_cents: unit * i64::from(qty),
        };
        self.orders.save(&order);
        Ok(order)
    }
}
