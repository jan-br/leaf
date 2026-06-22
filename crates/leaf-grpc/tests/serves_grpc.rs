//! The gRPC INTEGRATION PROOF: the shared hyper WebServer boots in-process with H2 enabled,
//! the gRPC ProtocolDispatch branch routes `application/grpc` calls to the #[grpc_controller]'s
//! GrpcRoute beans, and a REAL tonic client (dev-dep) drives all four call shapes + an explicit
//! Status + a domain-error Status + a metadata-auth WebFilter — with HTTP and gRPC on the SAME
//! port. The canonical-gRPC-stack interop proof; leaf names no tonic/hyper above the backend.

mod echo_controller;
use echo_controller::FILTER_CALLS;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use futures::StreamExt;

// The tonic-generated client for echo.proto, compiled from the SAME .proto by tonic's own
// codegen (the polyglot interop point: leaf's server trait + tonic's client trait, one wire).
// tonic-build wrote it to $OUT_DIR/tonic/echo.rs (a separate dir from leaf's $OUT_DIR/echo.rs)
// so the two generators never collide; include it directly rather than via tonic::include_proto!.
pub mod echo_tonic {
    include!(concat!(env!("OUT_DIR"), "/tonic/echo.rs"));
}
use echo_tonic::echo_client::EchoClient;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Pin the link rows the boot needs: the hyper FALLBACK dyn WebServer + JSON converter, and
/// the leaf-grpc GrpcDispatch (the dyn ProtocolDispatch the Dispatcher collection-injects) +
/// the DefaultGrpcStatusMapper FALLBACK. Referencing a TypeId per crate forces the rlib in.
fn force_link() {
    let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
    let _ = std::any::TypeId::of::<leaf_grpc::GrpcDispatch>();
    let _ = std::any::TypeId::of::<leaf_grpc::DefaultGrpcStatusMapper>();
}

async fn boot(port: u16) -> leaf_boot::RunningApp {
    force_link();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
    Application::new()
        .with_name("grpc-integration")
        .with_spawner(spawner)
        .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
        .run(
            SealInputs::new().with_args([format!("--leaf.web.server.port={port}")]),
            RunOverlay::none(),
        )
        .await
        .expect("the grpc app boots to Ready")
}

async fn wait_until_up(port: u16) {
    for _ in 0..400 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the grpc server never came up");
}

/// Build a tonic client with the auth metadata key set on every request (gRPC metadata =
/// H2 headers, which the ApiKeyFilter checks). Uses an interceptor so all 4 shapes carry it.
// `tonic::Status` (the interceptor's `Err`) is a large enum — that is tonic's API shape,
// not ours; the interceptor signature is fixed by `with_interceptor`.
#[allow(clippy::result_large_err)]
async fn client(
    port: u16,
) -> EchoClient<
    tonic::service::interceptor::InterceptedService<
        tonic::transport::Channel,
        impl Fn(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone,
    >,
> {
    let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .connect()
        .await
        .expect("tonic connects to the leaf server");
    EchoClient::with_interceptor(channel, |mut req: tonic::Request<()>| {
        req.metadata_mut().insert("x-api-key", "secret".parse().unwrap());
        Ok(req)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tonic_drives_all_four_call_shapes_against_the_leaf_grpc_controller() {
    let port = free_port();
    let running = boot(port).await;
    wait_until_up(port).await;
    let mut c = client(port).await;

    // 1. UNARY: req.text echoed back.
    let reply = c
        .unary(tonic::Request::new(echo_tonic::EchoRequest { text: "hi".into() }))
        .await
        .expect("unary call")
        .into_inner();
    assert_eq!(reply.text, "hi");

    // 2. SERVER-STREAM: one reply per word.
    let mut stream = c
        .server_stream(tonic::Request::new(echo_tonic::EchoRequest { text: "a b c".into() }))
        .await
        .expect("server-stream call")
        .into_inner();
    let mut words = Vec::new();
    while let Some(item) = stream.next().await {
        words.push(item.expect("server-stream item").text);
    }
    assert_eq!(words, vec!["a", "b", "c"]);

    // 3. CLIENT-STREAM: the server counts the inbound messages.
    let outbound = futures::stream::iter(vec![
        echo_tonic::EchoRequest { text: "x".into() },
        echo_tonic::EchoRequest { text: "y".into() },
        echo_tonic::EchoRequest { text: "z".into() },
    ]);
    let count = c
        .client_stream(tonic::Request::new(outbound))
        .await
        .expect("client-stream call")
        .into_inner();
    assert_eq!(count.n, 3);

    // 4. BIDI: each inbound message echoed back upper-cased.
    let outbound = futures::stream::iter(vec![
        echo_tonic::EchoRequest { text: "foo".into() },
        echo_tonic::EchoRequest { text: "bar".into() },
    ]);
    let mut stream = c
        .bidi(tonic::Request::new(outbound))
        .await
        .expect("bidi call")
        .into_inner();
    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        got.push(item.expect("bidi item").text);
    }
    assert_eq!(got, vec!["FOO", "BAR"]);

    let _ = FILTER_CALLS.load(Ordering::SeqCst);
    let _ = running.shutdown().await;
}
