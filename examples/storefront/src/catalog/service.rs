use leaf::prelude::*;
use leaf::InMemoryCacheManager;

use crate::catalog::product::repository::ProductRepository;

/// A `@Component` injecting [`ProductRepository`] whose `price_of` is `#[cacheable]`:
/// a repeat lookup for the same SKU returns the cached price without re-running the body.
///
/// The cache manager is the framework's auto-configured `InMemoryCacheManager` default
/// (`leaf_cache::CacheAutoConfig`) — no app-written wrapper bean. (Interim: by-trait
/// injection will let this name the `dyn CacheManager` view so a redis backend overrides
/// it transparently.)
#[component]
#[derive(Debug)]
pub struct CatalogService {
    repo: Ref<ProductRepository>,
}

#[advisable]
impl CatalogService {
    /// The unit price (cents) for a SKU; `Err` if unknown. Cached per SKU.
    #[cacheable("prices", key = "#0", manager = InMemoryCacheManager)]
    pub fn price_of(&self, sku: String) -> Result<i64, LeafError> {
        self.repo.find(&sku).map(|p| p.price_cents).ok_or_else(|| {
            LeafError::new(leaf::core::ErrorKind::ConstructionFailed)
                .caused_by(leaf::core::Cause::plain("pricing a product", format!("unknown sku: {sku}")))
        })
    }
}
