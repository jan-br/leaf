//! The `grpc` capability module: the storefront's gRPC surface, built ENTIRELY from leaf
//! stereotypes — zero hand-written GrpcRoute/GrpcHandler/ProtocolDispatch impls.
//!
//! - `catalog_controller::CatalogGrpcController` — a #[grpc_controller] over the SAME
//!   CatalogService + ProductRepository the HTTP CatalogController serves, exposing
//!   `GetProduct` (unary) and `ListProducts` (server-stream).
//! - `catalog_controller::StorefrontGrpcErrors` — a GrpcStatusMapper mapping the
//!   unknown-SKU domain error (`unknown_sku_kind`) to Code::NotFound (the gRPC analogue
//!   of the StorefrontErrors #[control_advice]).
//!
//! Reached umbrella-only through `use leaf::prelude::*;`; the macro-emitted `::leaf_grpc::`
//! paths resolve through the `extern crate leaf as leaf_grpc;` facade alias in lib.rs.
pub mod catalog_controller;
