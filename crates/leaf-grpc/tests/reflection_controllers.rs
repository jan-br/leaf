//! The reflection controllers + the shared index adapter. Three proofs:
//!  (1) the version-agnostic adapter over a real ReflectionIndex (list/symbol/not-found),
//!  (2) a conditioned controller gates its struct bean AND its route beans as one unit
//!      (the condition-propagation wiring proof) via leaf-boot's lazy assembly,
//!  (3) flipping `leaf.grpc.reflection.enabled` registers the route.

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{Injectable, Ref, ResolveCtx};
use leaf_grpc::reflection::{Answer, ReflectionIndex};
use leaf_grpc::GrpcRoute;

/// Build the index from the v1 reflection FDS (it self-describes
/// `grpc.reflection.v1.ServerReflection`) — a real, non-trivial descriptor set.
fn index() -> ReflectionIndex {
    mod gen_v1 {
        #![allow(dead_code, clippy::enum_variant_names)]
        leaf_grpc::include_proto!("grpc.reflection.v1");
    }
    ReflectionIndex::from_descriptor_sets(&[gen_v1::FILE_DESCRIPTOR_SET])
        .expect("the v1 reflection FDS decodes into an index")
}

#[test]
fn the_adapter_lists_the_reflection_service() {
    let idx = index();
    let services = idx.list_services();
    assert!(
        services.iter().any(|s| s == "grpc.reflection.v1.ServerReflection"),
        "list_services surfaces the reflection service FQN: {services:?}"
    );
}

#[test]
fn the_adapter_returns_a_file_and_its_closure_for_a_symbol() {
    let idx = index();
    let files = idx
        .file_containing_symbol("grpc.reflection.v1.ServerReflectionRequest")
        .expect("the defining file is found");
    assert!(!files.is_empty(), "file_containing_symbol returns the defining file(s)");
}

#[test]
fn an_unknown_symbol_is_a_not_found_answer() {
    // The adapter renders an unknown symbol as the NOT_FOUND marker (code 5), which the
    // controller wraps in a reflection ErrorResponse (NOT a transport Status).
    let idx = index();
    let answer = Answer::for_symbol(&idx, "does.not.Exist");
    match answer {
        Answer::NotFound { error_code, .. } => {
            assert_eq!(error_code, 5, "unknown symbol -> NOT_FOUND (code 5)");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ── the condition-propagation wiring proof (struct + routes gate as one unit) ──

/// Pin the link rows the boot needs: the hyper FALLBACK `dyn WebServer`, the JSON converter,
/// the leaf-grpc `GrpcDispatch` (the `dyn ProtocolDispatch` the Dispatcher collection-injects),
/// the `DefaultGrpcStatusMapper` FALLBACK, and the reflection controllers themselves (so their
/// COMPONENTS rows are present in the link-collected slice).
fn force_link() {
    let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
    let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
    let _ = std::any::TypeId::of::<leaf_grpc::GrpcDispatch>();
    let _ = std::any::TypeId::of::<leaf_grpc::DefaultGrpcStatusMapper>();
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1>();
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1alpha>();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Boot the whole `Application::run` pipeline (which runs the condition-routing + prune
/// pass) under the given args, then collection-inject the `Vec<Ref<dyn GrpcRoute>>` the
/// gated assembly admits, returning the registered route `path()`s.
async fn resolved_route_paths(args: &[String]) -> Vec<String> {
    force_link();
    leaf_tokio::install_ambient_store().ok();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
    let port = free_port();
    let mut all_args = vec![format!("--leaf.web.server.port={port}")];
    all_args.extend(args.iter().cloned());
    let running = Application::new()
        .with_name("reflection-gating")
        .with_spawner(spawner)
        .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
        .run(SealInputs::new().with_args(all_args), RunOverlay::none())
        .await
        .expect("the reflection app boots to Ready");

    let engine = running.context().engine();
    let cx = ResolveCtx::for_engine(engine);
    let routes: Vec<Ref<dyn GrpcRoute>> =
        <Vec<Ref<dyn GrpcRoute>> as Injectable>::inject(&cx)
            .await
            .expect("collection-injects the GrpcRoute beans");
    let paths = routes.iter().map(|r| r.path().to_string()).collect();
    running.shutdown().await;
    paths
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reflection_routes_are_gated_by_the_enabled_property_as_a_unit() {
    // OFF by default: the ReflectionV1 controller struct bean de-registers (its
    // #[conditional]) AND — via condition propagation — so do its GrpcRoute beans, so the
    // reflection path is absent from the collected routes.
    let off = resolved_route_paths(&[]).await;
    assert!(
        !off.iter().any(|p| p.contains("grpc.reflection.v1.ServerReflection")),
        "reflection is OFF by default — no reflection route registers: {off:?}"
    );

    // ON: flipping leaf.grpc.reflection.enabled=true registers the struct AND the routes.
    let on = resolved_route_paths(&["--leaf.grpc.reflection.enabled=true".to_string()]).await;
    assert!(
        on.iter().any(|p| p.contains("grpc.reflection.v1.ServerReflection")),
        "reflection ON registers the v1 ServerReflectionInfo route: {on:?}"
    );
}
