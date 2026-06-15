use leaf::prelude::*;

use crate::catalog::catalog_service::CatalogService;
use crate::orders::order::Order;
use crate::orders::repository::OrderRepository;
use crate::platform::transaction_manager::LocalTransactionManager;

/// A `@Component` injecting [`CatalogService`] + [`OrderRepository`] whose `place_order`
/// is `#[transactional]` (commit on `Ok`, rollback on `Err`). It demonstrates the tx
/// concern, while `CatalogService` carries the cache concern — two concerns, two services.
#[component]
#[derive(Debug)]
pub struct OrderService {
    catalog: Ref<CatalogService>,
    orders: Ref<OrderRepository>,
}

#[advisable]
impl OrderService {
    fn new(catalog: Ref<CatalogService>, orders: Ref<OrderRepository>) -> Self {
        OrderService { catalog, orders }
    }

    /// Price the SKU (via the cached catalog lookup), total it, save, and return the
    /// order. `Ok` commits the surrounding tx; `Err` rolls it back.
    #[transactional(manager = LocalTransactionManager)]
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
