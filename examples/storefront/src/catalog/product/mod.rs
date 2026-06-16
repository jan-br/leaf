/// A catalog product. A plain domain value, not a bean.
#[derive(Debug, Clone)]
pub struct Product {
    pub sku: &'static str,
    pub name: &'static str,
    pub price_cents: i64,
}
