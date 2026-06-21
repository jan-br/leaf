//! The DI-assembly proof: leaf-boot lifts leaf-grpc's macro-emitted `#[bean]`
//! (`GrpcDispatch` providing `dyn ProtocolDispatch`) seed + a hand-written `#[bean]`
//! route bean (`App::from_slices` → freeze → resolve, the LAZY path leaf-web's
//! `tests/dispatch_through_mock.rs` uses — no eager validate, no transport), and runs
//! the `#[auto_config]` FALLBACK ladder (`run_autoconfig`) to register the default
//! `DefaultGrpcStatusMapper`. The test then resolves the gRPC protocol-dispatch + the
//! route collection + the FALLBACK mapper by collection + by-trait injection.
//!
//! Hand-writing ONE route bean is the Stage-2 stand-in for the Stage-4
//! `#[grpc_controller]` macro, exactly as leaf-web's assembly test hand-writes a Route
//! bean. It is published via the dogfooded `#[component]` holder + `#[configuration]`
//! `#[bean(provides = "dyn …")]` idiom (a struct stereotype takes no `provides`), the
//! SAME shape leaf-serde's `JsonConverterConfig` and leaf-grpc's own `GrpcDispatchConfig`
//! use — no hand-rolled `Provider`.
//!
//! (The full `Application::run` pipeline is NOT used here: leaf-web's `EmbeddedWebServer`
//! `#[component]` is linked transitively and field-injects a `dyn WebServer` no backend
//! provides in this crate, which the eager validate pass would reject. The lazy
//! `from_slices` → freeze → `Injectable::inject` path resolves only what the test asks
//! for, exactly as the leaf-web assembly test does.)

use futures::executor::block_on;
use http::{HeaderMap, Method};
use leaf_boot::{run_autoconfig, App, AutoConfigCandidate, ExclusionSet};
use leaf_core::{
    collect_slice, ActiveProfiles, BoxFuture, EnvBuilder, Injectable, Ref, ResolveCtx,
    AUTO_CONFIGS, GUARD_PAIRINGS, SEED_PAIRINGS,
};
use leaf_grpc::framing::encode_frame;
use leaf_grpc::{GrpcDispatch, GrpcHandler, GrpcRoute, GrpcStatusMapper};
use leaf_macros::{component, configuration};
use leaf_web::request::Request;
use leaf_web::ProtocolDispatch;
use leaf_web::{Body, Frame};

// ── the hand-written route bean (Stage-2 stand-in for #[grpc_controller]) ──

struct PingHandler;
impl GrpcHandler for PingHandler {
    fn call<'a>(&'a self, _req: Request) -> BoxFuture<'a, leaf_web::Response> {
        Box::pin(async move {
            let mut trailers = HeaderMap::new();
            trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
            let out =
                futures::stream::iter(vec![Ok::<_, leaf_core::LeafError>(Frame::Trailers(trailers))]);
            leaf_web::Response::ok()
                .with_header(leaf_web::http::header::CONTENT_TYPE, "application/grpc")
                .with_body_stream(Box::pin(out))
        })
    }
}

struct PingRoute {
    handler: PingHandler,
}

impl PingRoute {
    fn new() -> Self {
        PingRoute { handler: PingHandler }
    }
}

impl GrpcRoute for PingRoute {
    fn path(&self) -> &str {
        "/test.Ping/Ping"
    }
    fn handler(&self) -> &dyn GrpcHandler {
        &self.handler
    }
}

/// The `#[component]` holder publishing `PingRoute` as the `dyn GrpcRoute` view — the
/// dogfooded `#[configuration]` + `#[bean(provides = "dyn …")]` idiom (the Stage-2
/// stand-in for the Stage-4 `#[grpc_controller]` macro).
#[component]
struct PingRoutes;

impl PingRoutes {
    fn new() -> Self {
        PingRoutes
    }
}

impl Default for PingRoutes {
    fn default() -> Self {
        PingRoutes::new()
    }
}

#[configuration]
impl PingRoutes {
    #[bean(name = "pingRoute", provides = "dyn ::leaf_grpc::GrpcRoute")]
    fn ping_route(&self) -> PingRoute {
        PingRoute::new()
    }
}

fn grpc_req(path: &str) -> Request {
    let mut h = HeaderMap::new();
    h.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/grpc"));
    // `Request::new` wraps the given `Bytes` in a `Body::Full` — pass the framed bytes.
    Request::new(Method::POST, path.parse().expect("uri"), h, encode_frame(b""))
}

/// Build the auto-config candidate for leaf-grpc's FALLBACK `DefaultGrpcStatusMapper`
/// from the PUBLIC linked slices (Descriptor by declared name; seed + guard JOINed by
/// the shared contract) — the same JOIN `leaf_boot`'s `collect_autoconfig_candidates`
/// performs, exactly as `leaf-boot`'s `auto_config_contributed_bean` test drives it.
fn default_mapper_candidate() -> AutoConfigCandidate {
    let descriptor = *AUTO_CONFIGS
        .iter()
        .find(|d| d.declared_name == Some("defaultGrpcStatusMapper"))
        .expect("the #[auto_config] default mapper reaches AUTO_CONFIGS");
    let seed = collect_slice(&SEED_PAIRINGS)
        .into_iter()
        .find(|r| r.contract == descriptor.contract)
        .map(|r| r.seed)
        .expect("the default mapper seed pairing keys on the same contract");
    let guard = collect_slice(&GUARD_PAIRINGS)
        .into_iter()
        .find(|r| r.contract == descriptor.contract)
        .map(|r| r.guard)
        .expect("the on_missing_bean guard pairing keys on the same contract");
    AutoConfigCandidate::new(descriptor, seed, Some(guard))
}

#[test]
fn grpc_dispatch_routes_and_mapper_resolve_by_injection() {
    // (1) Lift every macro-emitted #[component]/#[bean] row into a builder (the
    //     GrpcDispatchConfig + its grpc_dispatch bean, the GrpcStatusMapperAutoConfig
    //     holder, the hand-written PingRoutes + its ping_route bean).
    let mut builder = App::from_slices(&[])
        .expect("the auto-collected SEED_PAIRINGS base lifts every #[bean] row")
        .into_builder();

    // (2) Run the FALLBACK auto-config ladder for the default mapper over the SAME
    //     builder (no user mapper present → the OnMissingBean guard matches → it
    //     registers at FALLBACK). An empty env/profiles + empty seed-probe is the
    //     "no user bean / kill-switch unset" base.
    let env = EnvBuilder::new().seal_env();
    let registered = run_autoconfig(
        &[default_mapper_candidate()],
        &env,
        &mut builder,
        &ExclusionSet::new(),
        &ActiveProfiles::default(),
        &[],
    )
    .expect("the FALLBACK ladder runs")
    .registered;
    assert_eq!(registered, 1, "the default mapper registers (no user mapper supersedes it)");

    let registry = builder.freeze().expect("the grpc-beans registry freezes");
    let engine = leaf_core::Engine::new(registry);
    let cx = ResolveCtx::for_engine(&engine);

    // The route collection resolves (one provider — the hand-written PingRoute bean).
    let routes: Vec<Ref<dyn GrpcRoute>> =
        block_on(<Vec<Ref<dyn GrpcRoute>> as Injectable>::inject(&cx)).expect("routes resolve");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path(), "/test.Ping/Ping");

    // The FALLBACK default mapper resolves by trait with NO hand-written mapper bean.
    let mapper: Ref<dyn GrpcStatusMapper> =
        block_on(<Ref<dyn GrpcStatusMapper> as Injectable>::inject(&cx)).expect("the FALLBACK mapper resolves");
    let s = mapper
        .map(&leaf_core::LeafError::new(leaf_core::ErrorKind::NoSuchBean))
        .expect("default mapper claims it");
    assert_eq!(s.code, leaf_grpc::Code::Unimplemented);

    // The gRPC protocol-dispatch resolves by trait, with the route collection injected:
    // a request to the known method dispatches; an unknown method → Unimplemented.
    let dispatch: Ref<dyn ProtocolDispatch> =
        block_on(<Ref<dyn ProtocolDispatch> as Injectable>::inject(&cx)).expect("dyn ProtocolDispatch resolves");
    assert!(dispatch.handles(Some("application/grpc")));

    let resp = block_on(dispatch.dispatch(grpc_req("/test.Ping/Ping"))).expect("dispatch ok");
    let last_known = last_trailer(resp);
    assert_eq!(last_known.get("grpc-status").unwrap(), &http::HeaderValue::from_static("0"));

    let resp = block_on(dispatch.dispatch(grpc_req("/test.Ping/Missing"))).expect("dispatch ok");
    let last_unknown = last_trailer(resp);
    assert_eq!(
        last_unknown.get("grpc-status").unwrap(),
        &http::HeaderValue::from_static("12"),
        "an unknown method dispatches to Unimplemented trailers"
    );

    // GrpcDispatch::from_routes exists as the explicit ctor too (type-checks here).
    let _: GrpcDispatch =
        GrpcDispatch::from_routes(routes.iter().map(|r| Ref::clone(r).into_arc()).collect());
}

fn last_trailer(resp: leaf_web::Response) -> HeaderMap {
    use futures::StreamExt;
    match resp.into_body() {
        Body::Stream(s) => block_on(s.collect::<Vec<_>>())
            .into_iter()
            .filter_map(|r| match r.expect("frame") {
                Frame::Trailers(t) => Some(t),
                Frame::Data(_) => None,
            })
            .last()
            .expect("a trailers frame"),
        Body::Full(_) => panic!("grpc body is a stream"),
    }
}
