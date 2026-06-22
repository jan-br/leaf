//! The STOREFRONT REFLECTABLE PROOF (Stage 4): the umbrella-only storefront, with the
//! `grpc` capability + `--leaf.grpc.reflection.enabled=true`, is discoverable over real H2
//! by a tonic reflection client — `storefront.catalog.Catalog` appears in list_services and
//! its descriptors come back. ZERO storefront reflection code: the catalog #[grpc_controller]
//! contributes its FDS to leaf-grpc's discovery slice automatically; leaf-grpc serves
//! reflection via its dogfooded #[grpc_controller]s gated on the property.
#![cfg(feature = "grpc")]

extern crate leaf as leaf_grpc;
extern crate leaf as leaf_web;

use storefront as _;

use std::time::Duration;

// tonic's reflection CLIENT for leaf-grpc's grpc.reflection.v1 reflection_v1.proto, generated
// by the storefront build.rs into $OUT_DIR/tonic/grpc.reflection.v1.rs (dev/build-only).
pub mod reflection_tonic {
    include!(concat!(env!("OUT_DIR"), "/tonic/grpc.reflection.v1.rs"));
}
use futures::StreamExt;
use reflection_tonic::server_reflection_client::ServerReflectionClient;
use reflection_tonic::server_reflection_request::MessageRequest;
use reflection_tonic::server_reflection_response::MessageResponse;
use reflection_tonic::ServerReflectionRequest;

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
    panic!("the storefront grpc-reflection server never came up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_storefront_catalog_is_discoverable_via_reflection() {
    let port = free_port();
    let running = leaf::bootstrap("storefront")
        .run(
            leaf::RunInputs::new()
                .with_args([
                    format!("--leaf.web.server.port={port}"),
                    "--app.name=storefront".to_string(),
                    "--leaf.grpc.reflection.enabled=true".to_string(),
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
        .expect("tonic connects to the storefront reflection server");
    let mut c = ServerReflectionClient::new(channel);

    // list_services over the storefront -> storefront.catalog.Catalog appears (the wire FQN
    // from the catalog.proto FDS the #[grpc_controller] build contributed automatically).
    let outbound = futures::stream::iter(vec![ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    }]);
    let mut stream = c
        .server_reflection_info(tonic::Request::new(outbound))
        .await
        .expect("ServerReflectionInfo is served when reflection is enabled")
        .into_inner();
    let resp = stream
        .next()
        .await
        .expect("a reflection response frame")
        .expect("ok response")
        .message_response
        .expect("a message_response variant");
    let names: Vec<String> = match resp {
        MessageResponse::ListServicesResponse(r) => r.service.into_iter().map(|s| s.name).collect(),
        other => panic!("expected ListServicesResponse, got {other:?}"),
    };
    assert!(
        names.iter().any(|n| n == "storefront.catalog.Catalog"),
        "the storefront catalog service is reflectable, got {names:?}"
    );

    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf::core::RunState::Closed, "the storefront drained cleanly");
}
