use leaf::prelude::*;
use serde::{Deserialize, Serialize};

use crate::order::service::OrderService;

/// The JSON request body a `POST /orders` carries — deserialized from the body by the
/// `Json<NewOrder>` extractor, which rides the injected `HttpMessageConverter` (the leaf-
/// serde JSON converter) through the converter-aware extraction seam.
#[derive(Deserialize, Debug)]
pub struct NewOrder {
    /// The product SKU to order.
    pub sku: String,
    /// The quantity.
    pub qty: u32,
}

/// The JSON order view a placed order returns — serialized by the `@ResponseBody` policy.
#[derive(Serialize, PartialEq, Debug)]
pub struct OrderDto {
    /// The allocated order id.
    pub id: i64,
    /// The ordered SKU.
    pub sku: String,
    /// The ordered quantity.
    pub qty: u32,
    /// The order total in cents.
    pub total_cents: i64,
}

/// A `#[rest_controller]` exposing the order-placement endpoint. It field-injects
/// `Ref<OrderService>` (the `#[transactional]` place-order path) — an ordinary managed
/// bean; its `#[post]` method lowers to a generated `Route` bean.
#[rest_controller]
#[derive(Debug)]
pub struct OrderController {
    orders: Ref<OrderService>,
}

#[rest_controller]
impl OrderController {
    /// `POST /orders` — the `Json<NewOrder>` body resolves through the injected converter
    /// (the converter-backed extraction); the order is placed via `OrderService`, and the
    /// created `OrderDto` is serialized back to JSON.
    #[post("/orders")]
    async fn create(&self, body: Json<NewOrder>) -> Result<OrderDto, LeafError> {
        let Json(NewOrder { sku, qty }) = body;
        let order = self.orders.place_order(sku, qty)?;
        Ok(OrderDto {
            id: order.id,
            sku: order.sku,
            qty: order.qty,
            total_cents: order.total_cents,
        })
    }
}
