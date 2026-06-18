use leaf::prelude::*;
use serde::Serialize;

use crate::catalog::product::repository::ProductRepository;
use crate::catalog::service::CatalogService;

/// The JSON product view a `GET /products/{sku}` returns — serialized to the body by the
/// `#[rest_controller]` `@ResponseBody` policy via the injected `HttpMessageConverter`.
#[derive(Serialize, PartialEq, Debug)]
pub struct ProductDto {
    /// The product SKU (the path capture).
    pub sku: String,
    /// The product display name (from the `ProductRepository`).
    pub name: String,
    /// The unit price in cents (from the cacheable `CatalogService::price_of`).
    pub price_cents: i64,
}

/// A `#[rest_controller]` (a `@Component`-family bean): an ORDINARY managed bean whose
/// collaborators are field-injected (`Ref<CatalogService>` for the cached price,
/// `Ref<ProductRepository>` for the name). Its request-mapping methods are lowered by the
/// controller-impl iterator into generated `Route` beans — NO hand-written
/// `Route`/`Handler`/`Provider`.
#[rest_controller]
#[derive(Debug)]
pub struct CatalogController {
    catalog: Ref<CatalogService>,
    products: Ref<ProductRepository>,
}

#[rest_controller]
impl CatalogController {
    /// `GET /products/{sku}` — the `Path<String>` SKU resolves via the `FromRequest(Parts)`
    /// extractor seam; the product's name comes from the repository and its price from the
    /// cacheable `CatalogService::price_of`. An unknown SKU is the `LeafError` that
    /// `price_of` raises (`StorefrontErrors` maps it to 404).
    #[get("/products/{sku}")]
    async fn get(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {
        let Path(sku) = sku;
        // The cacheable price lookup is the unknown-SKU gate: it raises the `LeafError`
        // the `#[control_advice]` maps to 404 (so we price BEFORE naming).
        let price_cents = self.catalog.price_of(sku.clone())?;
        let name = self
            .products
            .find(&sku)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| sku.clone());
        Ok(ProductDto { sku, name, price_cents })
    }

    /// `GET /_access_count` — a tiny probe exposing the access-log filter's request
    /// counter, so the integration test can prove the `WebFilter` around-advice ran on
    /// every request. Returns the count as a plain text body via `IntoResponse`-less
    /// serialization (the rest-controller policy serializes the `i64` as a JSON number,
    /// which parses fine as a count).
    #[get("/_access_count")]
    async fn access_count(&self) -> Result<i64, LeafError> {
        Ok(crate::web::access_log_filter::access_count())
    }
}
