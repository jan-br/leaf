use std::sync::atomic::{AtomicUsize, Ordering};

use leaf::prelude::*;

use crate::catalog::product_repository::ProductRepository;
use crate::order::service::OrderService;
use crate::platform::app_properties::AppProperties;

/// Set once when the runner fires (a process-global so the test can assert it ran).
pub static RUNNER_FIRED: AtomicUsize = AtomicUsize::new(0);

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

impl StartupRunner {
    fn new(orders: Ref<OrderService>, catalog: Ref<ProductRepository>, app: Ref<AppProperties>) -> Self {
        StartupRunner { orders, catalog, app }
    }
}

impl Runner for StartupRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf::core::ApplicationArguments,
    ) -> leaf::core::BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            let order = self.orders.place_order("COFFEE".into(), 2)?;
            let name = self.catalog.find(&order.sku).map_or("?", |p| p.name);
            println!(
                "[{}] placed order #{}: {}x {} ({}) = {}c",
                self.app.name, order.id, order.qty, order.sku, name, order.total_cents,
            );
            RUNNER_FIRED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
