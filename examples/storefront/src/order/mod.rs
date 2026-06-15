pub mod repository;
pub mod service;

#[derive(Debug, Clone)]
pub struct Order {
    pub id: i64,
    pub sku: String,
    pub qty: u32,
    pub total_cents: i64,
}
