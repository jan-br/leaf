//! Integration tests driving the full `run` pipeline from the umbrella alone.

use std::any::TypeId;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use leaf::core::{ErasedArgs, MethodKey, ReadinessState, RunState};
use leaf::LeafError;

/// Serialize the app-booting tests: `StartupRunner` places an order at boot (bumping the
/// process-global `PRICE_LOOKUPS`), so two concurrent boots would race the cache-hit proof.
static APP_BOOT: Mutex<()> = Mutex::new(());

use crate::catalog::catalog_service::{CatalogService, PRICE_LOOKUPS};
use crate::order::repository::OrderRepository;
use crate::order::service::OrderService;
use crate::platform::app_properties::AppProperties;
use crate::platform::startup_runner::RUNNER_FIRED;
use crate::platform::transaction_manager::LocalTransactionManager;
use crate::pricing::discount_policy::DiscountPolicy;
use crate::pricing::promo_runner::{PromoRunner, PROMO_FIRED};

fn runtime() -> leaf::tokio::runtime::Runtime {
    leaf::tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a tokio runtime")
}

/// THE END-TO-END PROOF: the graph wires, config binds, the cache + tx concerns are
/// auto-advised, the runner fires, and shutdown drains cleanly.
#[test]
fn the_storefront_wires_advises_runs_and_shuts_down() {
    let _boot = APP_BOOT.lock().unwrap_or_else(|e| e.into_inner());
    runtime().block_on(async {
        let running = leaf::bootstrap("storefront")
            .run(
                leaf::RunInputs::new()
                    .with_args(["--app.name=Storefront", "--app.workers=4"])
                    .into(),
                leaf::boot::RunOverlay::none(),
            )
            .await
            .expect("the umbrella-only app runs to Ready");

        // (1) the graph wired: OrderService injected CatalogService + OrderRepository.
        running.context().get::<OrderService>().await.expect("OrderService resolves");
        running.context().get::<CatalogService>().await.expect("CatalogService injected");
        let repo = running.context().get::<OrderRepository>().await.expect("OrderRepository injected");

        // (2) AppProperties bound from the CLI args.
        let props = running.context().get::<AppProperties>().await.expect("AppProperties resolves");
        assert_eq!(props.name, "Storefront");
        assert_eq!(props.workers, 4);

        // (3) the #[runner] fired in the readiness window. The counter is process-global
        // and other tests boot the app concurrently, so we assert it fired (>= 1) rather
        // than an exact count.
        assert!(RUNNER_FIRED.load(Ordering::SeqCst) >= 1, "the #[runner] ran at startup");

        let registry = running.context().engine().registry();

        // (4a) place_order works end-to-end, driven through the auto-proxy so the
        // #[transactional] interceptor demarcates: a total from the catalog price, the
        // order saved, the tx committed (Ok path).
        let orders_id = *registry
            .candidates(TypeId::of::<OrderService>())
            .first()
            .expect("OrderService in registry");
        assert!(running.is_advised(orders_id), "the transactional bean is auto-advised");
        // StartupRunner already placed one order at boot; assert the delta from THIS call.
        let saved_before = repo.saved_count();
        let placed: Result<crate::order::Order, LeafError> = running
            .invoke_advised(
                orders_id,
                MethodKey::of("OrderService::place_order"),
                ErasedArgs::pack(("COFFEE".to_string(), 2u32)),
            )
            .await
            .expect("the advised call routes")
            .unpack()
            .unwrap();
        let order = placed.expect("the order places");
        assert_eq!(order.total_cents, 1299 * 2, "total = unit price * qty");
        assert_eq!(repo.saved_count(), saved_before + 1, "the order was saved");

        let tx = running.context().get::<LocalTransactionManager>().await.expect("tx manager");
        assert!(tx.begins() >= 1, "a tx was begun for the order");
        assert!(tx.commits() >= 1, "the Ok path committed");
        assert_eq!(tx.rollbacks(), 0, "no rollback on the Ok path");

        // (4b) the cached price_of: a repeat lookup for the SAME sku is a HIT — the
        // catalog body runs only once. Drive the advised method through the chain.
        let svc_id = *registry
            .candidates(TypeId::of::<CatalogService>())
            .first()
            .expect("CatalogService in registry");
        assert!(running.is_advised(svc_id), "the cacheable bean is auto-advised");

        let price_of = |sku: &str| {
            running.invoke_advised(
                svc_id,
                MethodKey::of("CatalogService::price_of"),
                ErasedArgs::pack((sku.to_string(),)),
            )
        };

        PRICE_LOOKUPS.store(0, Ordering::SeqCst);
        let p1: Result<i64, LeafError> =
            price_of("MUG").await.expect("the advised call routes").unpack().unwrap();
        assert_eq!(p1.expect("Ok"), 799, "the real lookup ran");
        assert_eq!(PRICE_LOOKUPS.load(Ordering::SeqCst), 1, "body ran on the cache MISS");

        let p2: Result<i64, LeafError> =
            price_of("MUG").await.expect("a cached call").unpack().unwrap();
        assert_eq!(p2.expect("Ok"), 799, "the CACHED value returns");
        assert_eq!(PRICE_LOOKUPS.load(Ordering::SeqCst), 1, "a cache HIT short-circuited the body");

        // (5) #[conditional] gating: discounts unset → the pricing feature (the
        // DiscountPolicy bean AND the PromoRunner that uses it) is entirely ABSENT.
        assert!(
            registry.candidates(TypeId::of::<DiscountPolicy>()).is_empty(),
            "DiscountPolicy is absent when pricing.discounts.enabled is unset"
        );
        assert!(
            registry.candidates(TypeId::of::<PromoRunner>()).is_empty(),
            "the conditional PromoRunner is absent (and never fires) when the flag is unset"
        );

        // (6) reached Running + AcceptingTraffic; shutdown drains cleanly.
        assert_eq!(running.unit().run_state(), RunState::Running);
        assert_eq!(running.unit().availability().readiness(), ReadinessState::AcceptingTraffic);

        let report = running.shutdown().await;
        assert_eq!(report.run_state, RunState::Closed, "the context closed");
        assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
    });
}

/// The flip side of (5): WITH `--pricing.discounts.enabled=true`, the conditionally-gated
/// `DiscountPolicy` IS present and resolves.
#[test]
fn the_discount_policy_registers_when_enabled() {
    let _boot = APP_BOOT.lock().unwrap_or_else(|e| e.into_inner());
    runtime().block_on(async {
        let running = leaf::bootstrap("storefront")
            .run(
                leaf::RunInputs::new()
                    .with_args(["--pricing.discounts.enabled=true"])
                    .into(),
                leaf::boot::RunOverlay::none(),
            )
            .await
            .expect("the app runs to Ready");

        let registry = running.context().engine().registry();
        assert!(
            !registry.candidates(TypeId::of::<DiscountPolicy>()).is_empty(),
            "DiscountPolicy IS present when pricing.discounts.enabled=true"
        );
        let policy = running.context().get::<DiscountPolicy>().await.expect("DiscountPolicy resolves");
        assert_eq!(policy.discount_cents(1000), 100, "10% discount");

        // The conditional PromoRunner is ALSO present and fired during the readiness window.
        assert!(
            !registry.candidates(TypeId::of::<PromoRunner>()).is_empty(),
            "the conditional PromoRunner IS present when the flag is set"
        );
        assert!(PROMO_FIRED.load(Ordering::SeqCst) >= 1, "the conditional runner fired");

        let report = running.shutdown().await;
        assert_eq!(report.run_state, RunState::Closed);
    });
}

/// The Redis auto-config PARTICIPATES (force-linked via the umbrella's `redis` feature):
/// its `AUTO_CONFIGS` row + force-link references survive DCE, and the umbrella's
/// ExpectedManifest names leaf-redis. It coexists with the in-memory cache (FALLBACK
/// loses to our NORMAL `InMemoryCache`).
#[test]
fn the_redis_capability_participates_via_the_umbrella_force_link() {
    let crates = leaf::forcelink::participating_crates();
    assert!(crates.contains(&"leaf-redis"), "the redis capability participates: {crates:?}");
    assert!(crates.contains(&"leaf-tokio"), "its runtime peer participates: {crates:?}");
    let manifest = leaf::forcelink::expected_manifest();
    assert!(
        manifest.iter().any(|t| t.0 == "leaf-redis"),
        "the ExpectedManifest names leaf-redis: {manifest:?}"
    );
}

/// THE LIVE SELF-CHECK PROOF: the healthy app's `ExpectedManifest` is NON-EMPTY, every
/// crate it names actually contributed a `SourceTag` into the link-collected `SOURCES`,
/// and the live self-check passes over that real participating set (not a vacuous empty
/// manifest).
#[test]
fn the_live_anti_dce_self_check_runs_over_a_non_empty_contributing_manifest() {
    let manifest = leaf::forcelink::expected_manifest();
    assert!(!manifest.is_empty(), "the redis app has a NON-EMPTY (live) manifest");

    let found = leaf::core::collect_slice(&leaf::core::SOURCES);
    for tag in &manifest {
        assert!(
            found.contains(tag),
            "expected crate `{}` must have contributed a SourceTag into SOURCES; found {found:?}",
            tag.0
        );
    }

    leaf::boot::self_check(&manifest).expect("the healthy participating manifest passes");
}
