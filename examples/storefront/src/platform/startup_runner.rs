use leaf::prelude::*;

use crate::catalog::product::repository::ProductRepository;
use crate::order::service::OrderService;
use crate::platform::app_properties::AppProperties;
use crate::pricing::pricing_service::PricingService;

/// The application's startup driver — leaf's [`Runner`] is Spring's `ApplicationRunner`:
/// it runs once in the readiness-gate window, so `main` does nothing but hand off to the
/// framework. Collaborators are constructor-injected across features (`order`, `catalog`,
/// `platform`, `pricing`).
#[runner]
pub struct StartupRunner {
    orders: Ref<OrderService>,
    catalog: Ref<ProductRepository>,
    app: Ref<AppProperties>,
    pricing: Ref<PricingService>,
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
        // COLLECTION INJECTION: PricingService injected `Vec<Ref<dyn PricingRule>>` — ALL
        // beans providing the view. The runner reads the collected rules to prove a real
        // multi-provider collection injection wired at startup.
        println!(
            "[{}] {} pricing rules active {:?}: +{}c total surcharge",
            self.app.name,
            self.pricing.rule_count(),
            self.pricing.labels(),
            self.pricing.total_surcharge_cents(),
        );
        Ok(())
    }
}
