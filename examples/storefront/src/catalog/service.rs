use leaf::prelude::*;

use crate::catalog::product::repository::ProductRepository;

/// A `@Component` injecting [`ProductRepository`] whose `price_of` is `#[cacheable]`:
/// a repeat lookup for the same SKU returns the cached price without re-running the body.
///
/// The cache manager is named by its VIEW — `manager = dyn CacheManager` — so it resolves
/// through the GENERAL by-trait injection path to WHATEVER bean provides `dyn CacheManager`:
/// the framework's auto-configured `InMemoryCacheManager` default by default, or a
/// Redis-backed `RedisCacheManager` when `--leaf.redis.enabled=true` makes redis the sole
/// provider — transparently, with no concrete pin and no app-written wrapper bean.
#[service]
#[derive(Debug)]
pub struct CatalogService {
    repo: Ref<ProductRepository>,
}

#[advisable]
impl CatalogService {
    /// The unit price (cents) for a SKU; `Err` if unknown. Cached per SKU.
    #[cacheable("prices", key = "#0", manager = dyn leaf::core::CacheManager)]
    pub fn price_of(&self, sku: String) -> Result<i64, LeafError> {
        self.repo.find(&sku).map(|p| p.price_cents).ok_or_else(|| {
            LeafError::new(leaf::core::ErrorKind::ConstructionFailed)
                .caused_by(leaf::core::Cause::plain("pricing a product", format!("unknown sku: {sku}")))
        })
    }
}
