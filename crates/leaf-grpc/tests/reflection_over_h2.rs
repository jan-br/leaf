//! The gRPC SERVER REFLECTION integration proof (Stage 4): the shared hyper WebServer
//! boots in-process with H2; a tonic-generated reflection client (the reflection_v1.proto's
//! own client, dev-only, NO external grpcurl) drives ServerReflectionInfo over real H2.
//! Opt-in: OFF (default) -> Code::Unimplemented; ON -> list_services + file_containing_symbol.

// The tonic-generated CLIENT for the SHIPPED grpc.reflection.v1 reflection_v1.proto, compiled
// by tonic's own codegen into a SEPARATE $OUT_DIR/tonic/ dir (so it never collides with the
// leaf-grpc-build server trait). The polyglot reflection peer; leaf names no tonic above dev.
pub mod reflection_tonic {
    include!(concat!(env!("OUT_DIR"), "/tonic/grpc.reflection.v1.rs"));
}

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use futures::StreamExt;

use reflection_tonic::server_reflection_client::ServerReflectionClient;
use reflection_tonic::server_reflection_request::MessageRequest;
use reflection_tonic::server_reflection_response::MessageResponse;
use reflection_tonic::ServerReflectionRequest;

// The echo controller (the app service that becomes reflectable) — its build.rs FDS row
// is in the slice, and force_link must pin its bean rows too.
mod echo_controller;

#[test]
fn the_tonic_reflection_client_stub_is_generated() {
    // Compiling this file at all proves the include! resolved; name the client type so the
    // module is not dead-code-eliminated before the include is type-checked.
    let _ = std::any::type_name::<ServerReflectionClient<()>>();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Pin the link rows the boot needs: the hyper FALLBACK WebServer + JSON converter, the
/// leaf-grpc GrpcDispatch + DefaultGrpcStatusMapper FALLBACK, the echo controller (the app
/// service to reflect), AND the two reflection controllers (so their bean rows LINK; the
/// #[conditional] guard — not the link — is what gates them on/off).
fn force_link() {
    let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
    let _ = std::any::TypeId::of::<leaf_grpc::GrpcDispatch>();
    let _ = std::any::TypeId::of::<leaf_grpc::DefaultGrpcStatusMapper>();
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1>();
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1alpha>();
    // Pin the echo controller's bean rows so the app service it serves is reflectable.
    let _ = std::any::TypeId::of::<echo_controller::EchoController>();
}

async fn boot(args: Vec<String>) -> (u16, leaf_boot::RunningApp) {
    force_link();
    let port = free_port();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
    let mut all = vec![format!("--leaf.web.server.port={port}")];
    all.extend(args);
    let app = Application::new()
        .with_name("grpc-reflection")
        .with_spawner(spawner)
        .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
        .run(SealInputs::new().with_args(all), RunOverlay::none())
        .await
        .expect("the grpc-reflection app boots to Ready");
    (port, app)
}

async fn wait_until_up(port: u16) {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the grpc-reflection server never came up");
}

async fn refl_client(port: u16) -> ServerReflectionClient<tonic::transport::Channel> {
    let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .connect()
        .await
        .expect("tonic connects to the leaf reflection server");
    ServerReflectionClient::new(channel)
}

/// Drive ONE ServerReflectionInfo round-trip: send one request, read one response.
///
/// The echo_controller module's `ApiKeyFilter` web filter guards EVERY gRPC call (gRPC
/// metadata = H2 headers), so the reflection request carries `x-api-key: secret` to pass
/// the auth chain — the filter is part of this app. The OFF-by-default case then still
/// yields `Unimplemented` because the gate is the ABSENCE of a reflection ROUTE, not auth.
async fn one_round(
    c: &mut ServerReflectionClient<tonic::transport::Channel>,
    req: ServerReflectionRequest,
) -> Result<MessageResponse, tonic::Status> {
    let outbound = futures::stream::iter(vec![req]);
    let mut request = tonic::Request::new(outbound);
    request.metadata_mut().insert("x-api-key", "secret".parse().unwrap());
    let mut stream = c.server_reflection_info(request).await?.into_inner();
    let resp = stream
        .next()
        .await
        .expect("a reflection response frame")?;
    Ok(resp.message_response.expect("a message_response variant"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reflection_is_unimplemented_when_disabled_by_default() {
    // No leaf.grpc.reflection.enabled -> the #[conditional] controllers (struct + routes)
    // do not register -> the ServerReflectionInfo route is absent.
    let (port, running) = boot(vec![]).await;
    wait_until_up(port).await;
    let mut c = refl_client(port).await;

    let err = one_round(
        &mut c,
        ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        },
    )
    .await
    .expect_err("a reflection call with reflection disabled is rejected");
    assert_eq!(
        err.code(),
        tonic::Code::Unimplemented,
        "reflection OFF by default -> the unknown-method Unimplemented path, got {err:?}"
    );

    let _ = running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reflection_lists_services_and_returns_descriptors_when_enabled() {
    let (port, running) =
        boot(vec!["--leaf.grpc.reflection.enabled=true".to_string()]).await;
    wait_until_up(port).await;
    let mut c = refl_client(port).await;

    // 1. list_services -> the app's echo.Echo service appears (the gRPC wire FQN, sourced
    //    from the FDS the echo.proto's leaf-grpc-build run contributed to the slice).
    let resp = one_round(
        &mut c,
        ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        },
    )
    .await
    .expect("list_services succeeds with reflection enabled");
    let names: Vec<String> = match resp {
        MessageResponse::ListServicesResponse(r) => {
            r.service.into_iter().map(|s| s.name).collect()
        }
        other => panic!("expected ListServicesResponse, got {other:?}"),
    };
    assert!(
        names.iter().any(|n| n == "echo.Echo"),
        "list_services includes the app catalog service echo.Echo, got {names:?}"
    );

    // 2. file_containing_symbol("echo.EchoRequest") -> the defining echo.proto descriptor
    //    bytes (plus its dependency closure). The symbol is the FDS wire FQN, NOT a Rust name.
    let resp = one_round(
        &mut c,
        ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::FileContainingSymbol(
                "echo.EchoRequest".to_string(),
            )),
        },
    )
    .await
    .expect("file_containing_symbol succeeds");
    let fds_bytes: Vec<Vec<u8>> = match resp {
        MessageResponse::FileDescriptorResponse(r) => r.file_descriptor_proto,
        other => panic!("expected FileDescriptorResponse, got {other:?}"),
    };
    assert!(
        !fds_bytes.is_empty(),
        "the file + its dependency closure came back as descriptor bytes"
    );
    // Decode each returned descriptor and assert the defining file is among them: a file
    // whose message_type contains EchoRequest. (prost_types is reached via leaf_grpc's normal
    // dep; prost is named directly as a dev-dep for Message::decode.)
    let defines_echo_request = fds_bytes.iter().any(|bytes| {
        let file = <prost_types::FileDescriptorProto as prost::Message>::decode(&bytes[..])
            .expect("a returned descriptor decodes as a FileDescriptorProto");
        file.message_type.iter().any(|m| m.name() == "EchoRequest")
    });
    assert!(
        defines_echo_request,
        "the descriptor defining EchoRequest is in the returned closure"
    );

    let _ = running.shutdown().await;
}
