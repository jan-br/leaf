use leaf::prelude::*;

use crate::catalog::product::repository::ProductRepository;
use crate::order::service::OrderService;
use crate::platform::app_properties::AppProperties;

/// The application's startup driver — leaf's [`Runner`] is Spring's `ApplicationRunner`:
/// it runs once in the readiness-gate window, so `main` does nothing but hand off to the
/// framework. Collaborators are constructor-injected across features (`order`, `catalog`,
/// `platform`).
#[runner]
pub struct StartupRunner {
    orders: Ref<OrderService>,
    catalog: Ref<ProductRepository>,
    app: Ref<AppProperties>,
}

#[async_impl]
impl Runner for StartupRunner {
    async fn run(&self, _args: &leaf::core::ApplicationArguments) -> Result<(), LeafError> {
        let order = self.orders.place_order("COFFEE".into(), 2)?;
        let name = self.catalog.find(&order.sku).map_or("?", |p| p.name);
        println!(
            "[{}] placed order #{}: {}x {} ({}) = {}c",
            self.app.name, order.id, order.qty, order.sku, name, order.total_cents,
        );
        Ok(())
    }
}
