use std::sync::atomic::{AtomicUsize, Ordering};

use leaf::prelude::*;

use crate::catalog::product_repository::ProductRepository;
use crate::platform::cache_manager::InMemoryCache;

/// Body-run count of `price_of` — a process-global so a cache HIT (which short-circuits
/// the body) is observable without making the counter an injected field.
pub static PRICE_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// A `@Component` injecting [`ProductRepository`] whose `price_of` is `#[cacheable]`:
/// a repeat lookup for the same SKU returns the cached price without re-running the body.
#[component]
#[derive(Debug)]
pub struct CatalogService {
    repo: Ref<ProductRepository>,
}

#[advisable]
impl CatalogService {
    fn new(repo: Ref<ProductRepository>) -> Self {
        CatalogService { repo }
    }

    /// The unit price (cents) for a SKU; `Err` if unknown. Cached per SKU.
    #[cacheable("prices", key = "#0", manager = InMemoryCache)]
    pub fn price_of(&self, sku: String) -> Result<i64, LeafError> {
        PRICE_LOOKUPS.fetch_add(1, Ordering::SeqCst);
        self.repo.find(&sku).map(|p| p.price_cents).ok_or_else(|| {
            LeafError::new(leaf::core::ErrorKind::ConstructionFailed)
                .caused_by(leaf::core::Cause::plain("pricing a product", format!("unknown sku: {sku}")))
        })
    }
}
