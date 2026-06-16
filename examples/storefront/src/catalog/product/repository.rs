use leaf::prelude::*;

use crate::catalog::r#mod::Product;

/// The seeded catalog (a small fixed inventory).
const CATALOG: &[Product] = &[
    Product { sku: "COFFEE", name: "Bag of Coffee", price_cents: 1299 },
    Product { sku: "MUG", name: "Ceramic Mug", price_cents: 799 },
    Product { sku: "FILTER", name: "Paper Filters", price_cents: 449 },
];

/// A `@Repository` holding the mod inventory.
#[derive(Debug)]
pub struct ProductRepository;
register_component!(ProductRepository);

impl ProductRepository {
    fn new() -> Self {
        ProductRepository
    }

    /// Look up a mod by SKU.
    pub fn find(&self, sku: &str) -> Option<Product> {
        CATALOG.iter().find(|p| p.sku == sku).cloned()
    }
}
