//! The STOREFRONT gRPC PROOF — the gRPC analogue of `the_storefront_serves_its_domain_over_
//! real_http`: the umbrella-only storefront, with the `grpc` capability, serves its catalog
//! domain over REAL H2 through a #[grpc_controller], driven by a real tonic client — with
//! ZERO hand-written GrpcRoute/GrpcHandler/ProtocolDispatch. The same embedded server the
//! HTTP proof uses, now answering gRPC on the same lifecycle.
#![cfg(feature = "grpc")]

// The umbrella-only facade aliases the macros' `::leaf_grpc::`/`::leaf_web::`/`::prost::`
// paths resolve against (SOURCE aliases of the one `leaf` dep, like the HTTP proof's `as
// leaf_web`). A binary-crate root gets these auto-emitted; an integration test names them.
extern crate leaf as leaf_grpc;
extern crate leaf as leaf_web;

// Link the storefront LIBRARY's bean rows (the #[grpc_controller] + the mapper + the domain
// services) into this test binary.
use storefront as _;

use std::time::Duration;

// The tonic-generated CLIENT for catalog.proto, compiled from the SAME .proto by tonic's own
// codegen (the polyglot interop point: leaf's server trait + tonic's client, one wire). The
// storefront `build.rs` wrote it to $OUT_DIR/tonic/storefront.catalog.rs (a separate dir from
// leaf's $OUT_DIR/storefront.catalog.rs) so the two generators never collide.
pub mod catalog_tonic {
    include!(concat!(env!("OUT_DIR"), "/tonic/storefront.catalog.rs"));
}
use catalog_tonic::catalog_client::CatalogClient;
use futures::StreamExt;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn wait_until_up(port: u16) {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the storefront grpc server never came up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_storefront_serves_its_domain_over_real_grpc() {
    let port = free_port();

    // Boot the WHOLE storefront in-process (same entry the HTTP proof + #[leaf::main] drive);
    // the embedded server is a #[keep_alive] serving H2 on a spawned task, so run() returns
    // Ready and we hold the live app. The `grpc` capability's force-link pins the bundle.
    let running = leaf::bootstrap("storefront")
        .run(
            leaf::RunInputs::new()
                .with_args([
                    format!("--leaf.web.server.port={port}"),
                    "--app.name=storefront".to_string(),
                ])
                .into(),
            leaf::boot::RunOverlay::none(),
        )
        .await
        .expect("the storefront boots to Ready");

    wait_until_up(port).await;

    let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .connect()
        .await
        .expect("tonic connects to the storefront");
    let mut c = CatalogClient::new(channel);

    // 1. GetProduct(COFFEE) → the Product, resolved via CatalogService (the cached price) +
    //    ProductRepository (the name) — the SAME domain the HTTP /products/COFFEE serves.
    let product = c
        .get_product(tonic::Request::new(catalog_tonic::GetProductRequest { sku: "COFFEE".into() }))
        .await
        .expect("GetProduct COFFEE")
        .into_inner();
    assert_eq!(product.sku, "COFFEE");
    assert_eq!(product.name, "Bag of Coffee");
    assert_eq!(product.price_cents, 1299);

    // 2. GetProduct(NOPE) → Code::NotFound via the StorefrontGrpcErrors GrpcStatusMapper
    //    (the unknown-SKU domain channel — the gRPC analogue of the HTTP 404 advice).
    let err = c
        .get_product(tonic::Request::new(catalog_tonic::GetProductRequest { sku: "NOPE".into() }))
        .await
        .expect_err("an unknown SKU is a NotFound Status");
    assert_eq!(err.code(), tonic::Code::NotFound, "unknown SKU maps to NotFound via the mapper");

    // 3. ListProducts (server-stream) → the catalog, one Product per frame; COFFEE is present.
    let mut stream = c
        .list_products(tonic::Request::new(catalog_tonic::ListProductsRequest {}))
        .await
        .expect("ListProducts")
        .into_inner();
    let mut skus = Vec::new();
    while let Some(item) = stream.next().await {
        skus.push(item.expect("list item").sku);
    }
    assert!(skus.contains(&"COFFEE".to_string()), "the streamed catalog includes COFFEE, got {skus:?}");

    // Graceful shutdown → clean teardown to Closed (the same lifecycle the HTTP proof asserts).
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf::core::RunState::Closed, "the storefront drained cleanly");
}
