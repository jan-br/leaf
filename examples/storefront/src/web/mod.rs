//! The `web` feature module (Task 14): the storefront's REST surface, built ENTIRELY
//! from leaf web stereotypes — zero hand-written `Route`/`Provider`/`Handler` impls.
//!
//! - `catalog_controller::CatalogController` — a `#[rest_controller]` exposing
//!   `GET /products/{sku}` (the cacheable price lookup + the product name) and a small
//!   `GET /_access_count` probe for the filter proof.
//! - `order_controller::OrderController` — a `#[rest_controller]` exposing
//!   `POST /orders` (the `#[transactional]` place-order path) over a `Json<_>` body.
//! - `access_log_filter::AccessLogFilter` — a `WebFilter` (the around-advice seam),
//!   published as `dyn WebFilter`, that counts every request.
//! - `error_advice::StorefrontErrors` — a `#[control_advice]` mapping an unknown SKU
//!   to `404`.
//!
//! Reached umbrella-only through `use leaf::prelude::*;`; the macro-emitted
//! `::leaf_web::` paths resolve through the binary-crate facade alias `#[leaf::main]`
//! auto-emits.
pub mod access_log_filter;
pub mod catalog_controller;
pub mod error_advice;
pub mod order_controller;
