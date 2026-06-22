//! The storefront's `#[grpc_controller]` over the catalog domain — the SAME
//! `CatalogService` (the cacheable price) + `ProductRepository` (the name) the HTTP
//! `CatalogController` serves, now over gRPC on the SAME embedded server. Plus a
//! `GrpcStatusMapper` mapping the unknown-SKU domain error to `Code::NotFound` (the gRPC
//! analogue of the `StorefrontErrors` `#[control_advice]`). All stereotype beans,
//! umbrella-only (`use leaf::prelude::*;`).

use leaf::core::BoxStream;
use leaf::prelude::*;

use crate::catalog::product::repository::ProductRepository;
use crate::catalog::service::{unknown_sku_kind, CatalogService};

// The generated server trait (`Catalog`) + the prost messages (`GetProductRequest`,
// `Product`, `ListProductsRequest`) + the `catalog::*_DESCRIPTOR` path module for the
// `storefront.catalog` service. `include_proto!` splices the leaf-grpc-build output the
// storefront `build.rs` wrote into `$OUT_DIR/storefront.catalog.rs`.
leaf::grpc::leaf_grpc::include_proto!("storefront.catalog");

/// A #[grpc_controller] over the catalog domain — the SAME CatalogService (cacheable price)
/// + ProductRepository (the name) the HTTP CatalogController serves, now over gRPC. An
/// ordinary #[component]-family bean; its RPC methods lower to GrpcRoute beans (no
/// hand-written GrpcRoute/GrpcHandler). Field injection, exactly like the HTTP controller.
/// The `mappers` chain is the SAME ordered `dyn GrpcStatusMapper` collection `GrpcDispatch`
/// injects (the storefront's `StorefrontGrpcErrors` + the FALLBACK floor), so a domain
/// `LeafError` raised by the cacheable price lookup runs through the real GrpcStatusMapper
/// SPI to a Status — the gRPC twin of the HTTP `#[control_advice]`, not a spelled status.
#[grpc_controller]
pub struct CatalogGrpcController {
    catalog: Ref<CatalogService>,
    products: Ref<ProductRepository>,
    mappers: Vec<Ref<dyn leaf::grpc::leaf_grpc::GrpcStatusMapper>>,
}

impl CatalogGrpcController {
    /// Render a raised domain `LeafError` to a `Status` through the COLLECTION-INJECTED
    /// `GrpcStatusMapper` chain (user mappers first, the FALLBACK floor last) — the SAME
    /// path `GrpcDispatch` runs for a handler/filter error, so the unknown-SKU error maps
    /// to `NotFound` via `StorefrontGrpcErrors`.
    fn to_status(&self, err: &LeafError) -> Status {
        let refs: Vec<&dyn leaf::grpc::leaf_grpc::GrpcStatusMapper> =
            self.mappers.iter().map(|m| &**m).collect();
        leaf::grpc::leaf_grpc::map_first(&refs, err)
            .unwrap_or_else(|| Status::new(Code::Unknown, err.to_string()))
    }
}

#[grpc_controller]
impl Catalog for CatalogGrpcController {
    /// `GetProduct` (unary): the cacheable price lookup gates the unknown-SKU error (it
    /// raises the Integration{unknown_sku_kind} LeafError the GrpcStatusMapper maps to
    /// NotFound), then the name from the repository.
    async fn get_product(&self, req: GetProductRequest) -> Result<Product, Status> {
        let price_cents = self.catalog.price_of(req.sku.clone()).map_err(|e| self.to_status(&e))?;
        let name = self
            .products
            .find(&req.sku)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| req.sku.clone());
        Ok(Product { sku: req.sku, name, price_cents })
    }

    /// `ListProducts` (server-stream): one Product frame per catalog entry.
    async fn list_products(&self, _req: ListProductsRequest) -> Result<Streaming<Product>, Status> {
        let products: Vec<Product> = self
            .products
            .all()
            .into_iter()
            .map(|p| Product {
                sku: p.sku.to_string(),
                name: p.name.to_string(),
                price_cents: p.price_cents,
            })
            .collect();
        let stream: BoxStream<'static, Result<Product, Status>> =
            Box::pin(futures::stream::iter(products.into_iter().map(Ok)));
        Ok(Streaming::new(stream))
    }
}

/// A GrpcStatusMapper mapping the storefront's unknown-SKU domain error to Code::NotFound
/// (the gRPC analogue of the StorefrontErrors #[control_advice]). Published as the `dyn
/// GrpcStatusMapper` view via the dogfooded #[configuration] + #[bean(provides = "dyn …")]
/// holder idiom (a struct `#[component]` takes no `provides`) — the SAME collection-injection
/// DI the default FALLBACK mapper rides; first-Some wins, so this mapper supersedes the
/// FALLBACK for the unknown-SKU kind.
#[component]
#[derive(Debug, Default)]
pub struct StorefrontGrpcErrorsConfig;

#[configuration]
impl StorefrontGrpcErrorsConfig {
    #[bean(name = "storefrontGrpcErrors", provides = "dyn ::leaf_grpc::GrpcStatusMapper")]
    fn storefront_grpc_errors(&self) -> StorefrontGrpcErrors {
        StorefrontGrpcErrors
    }
}

/// The mapper value the bean publishes: the unknown-SKU Integration kind → Code::NotFound.
#[derive(Debug)]
pub struct StorefrontGrpcErrors;

impl leaf::grpc::leaf_grpc::GrpcStatusMapper for StorefrontGrpcErrors {
    fn map(&self, err: &LeafError) -> Option<Status> {
        match err.kind {
            leaf::core::ErrorKind::Integration { kind_id } if kind_id == unknown_sku_kind() => {
                Some(Status::not_found("unknown sku"))
            }
            _ => None,
        }
    }
}
