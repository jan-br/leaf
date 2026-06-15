//! Integration test `[boot-refresh]`: the fused container-lifecycle template
//! (phase3/13 R0..R8 refresh + the C1/C7 teardown drain) over a real graph and a
//! real tokio runtime (the dev-dep `leaf-tokio` `ExecutionFacility`).
//!
//! Proves the run-engine half of leaf-boot end-to-end:
//!
//! - `refresh()` brings up an `A → B` singleton graph EAGERLY (both built before
//!   `refresh()` returns) and ONCE-only (a second resolve reuses the published Arc),
//!   and publishes `Refreshed`+`Started` with `RunState=Running`/`Liveness=Correct`.
//! - a `Bootstrap::Background` bean is `Spawner::spawn`ed and JOINED at its wave
//!   boundary (the structured-concurrency join point).
//! - `shutdown()` drains the container `TeardownLedger` LIFO (reverse wave order)
//!   and walks `RunState` `Running → Stopping → Closing → Closed`, flipping
//!   readiness to `RefusingTraffic` first.

use std::any::{Any, TypeId};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use leaf_boot::{RunState, RunUnit};
use leaf_core::{
    AnnotationMetadata, Bean, Bootstrap, BoxFuture, CallbackError, ContractId, Cx, Descriptor,
    InjectionPlan, InjectionPoint, LifecycleFn, LifecyclePhase, LifecyclePlan, LifecycleStep,
    LivenessState, Origin, Provider, Published, ReadinessState, Ref, RegistryBuilder, ResolveCtx,
    Role, ScopeDef, StepId,
};

// ── the A → B singleton graph + a Background bean ────────────────────────────

#[derive(Debug)]
struct B {
    tag: &'static str,
}
impl Bean for B {}

#[derive(Debug)]
struct A {
    b: Ref<B>,
}
impl Bean for A {}

#[derive(Debug)]
struct Bg {
    #[allow(dead_code)]
    tag: &'static str,
}
impl Bean for Bg {}

fn descriptor(name: &'static str, contract: &str, self_type: TypeId, role: Role) -> Descriptor {
    Descriptor {
        contract: ContractId::of(contract),
        self_type,
        provides: &[],
        declared_name: Some(name),
        aliases: &[],
        scope: ScopeDef::SINGLETON,
        role,
        meta: &AnnotationMetadata::EMPTY,
        parent: None,
        origin: Origin::Native { crate_name: Some("leaf-boot::test") },
    }
}

struct BProvider {
    descriptor: Descriptor,
    builds: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<&'static str>>>,
}
impl Provider for BProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn provide<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<Published, leaf_core::LeafError>> {
        Box::pin(async move {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push("build:B");
            Ok(Published::shared_value(B { tag: "b" }))
        })
    }
}

struct AProvider {
    descriptor: Descriptor,
    builds: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<&'static str>>>,
}
impl Provider for AProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn provide<'a>(
        &'a self,
        cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<Published, leaf_core::LeafError>> {
        Box::pin(async move {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push("build:A");
            // Real nested resolution: A needs B, resolved THROUGH the engine.
            let engine = cx.engine().expect("engine back-reference threaded");
            let b = engine.get::<B>().await?;
            Ok(Published::shared_value(A { b }))
        })
    }
}

struct BgProvider {
    descriptor: Descriptor,
    builds: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<&'static str>>>,
}
impl Provider for BgProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn provide<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<Published, leaf_core::LeafError>> {
        Box::pin(async move {
            self.builds.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push("build:Bg");
            Ok(Published::shared_value(Bg { tag: "bg" }))
        })
    }
}

// ── per-type destroy callbacks recording into a shared static log ─────────────

static DESTROY_ORDER: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn destroy_a<'a>(
    _bean: &'a (dyn Any + Send + Sync),
    _cx: &'a Cx,
) -> BoxFuture<'a, Result<(), CallbackError>> {
    Box::pin(async move {
        DESTROY_ORDER.lock().unwrap().push("destroy:A");
        Ok(())
    })
}
fn destroy_b<'a>(
    _bean: &'a (dyn Any + Send + Sync),
    _cx: &'a Cx,
) -> BoxFuture<'a, Result<(), CallbackError>> {
    Box::pin(async move {
        DESTROY_ORDER.lock().unwrap().push("destroy:B");
        Ok(())
    })
}
fn destroy_bg<'a>(
    _bean: &'a (dyn Any + Send + Sync),
    _cx: &'a Cx,
) -> BoxFuture<'a, Result<(), CallbackError>> {
    Box::pin(async move {
        DESTROY_ORDER.lock().unwrap().push("destroy:Bg");
        Ok(())
    })
}

const A_DESTROY: &[LifecycleStep] =
    &[LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_a as LifecycleFn, id: StepId(1) }];
const B_DESTROY: &[LifecycleStep] =
    &[LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_b as LifecycleFn, id: StepId(2) }];
const BG_DESTROY: &[LifecycleStep] =
    &[LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_bg as LifecycleFn, id: StepId(3) }];

const B_POINT: &[InjectionPoint] = &[InjectionPoint::single(TypeId::of::<B>(), "b")];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_brings_up_the_graph_eagerly_once_and_teardown_drains_lifo() {
    DESTROY_ORDER.lock().unwrap().clear();

    let builds_a = Arc::new(AtomicUsize::new(0));
    let builds_b = Arc::new(AtomicUsize::new(0));
    let builds_bg = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let a_desc = descriptor("a", "boot::test::A", TypeId::of::<A>(), Role::Application);
    let b_desc = descriptor("b", "boot::test::B", TypeId::of::<B>(), Role::Application);
    let bg_desc = descriptor("bg", "boot::test::Bg", TypeId::of::<Bg>(), Role::Application);

    let mut builder = RegistryBuilder::new();
    let id_b = builder
        .register(
            b_desc,
            Arc::new(BProvider { descriptor: b_desc, builds: builds_b.clone(), log: log.clone() }),
        )
        .unwrap();
    let id_a = builder
        .register(
            a_desc,
            Arc::new(AProvider { descriptor: a_desc, builds: builds_a.clone(), log: log.clone() }),
        )
        .unwrap();
    let id_bg = builder
        .register(
            bg_desc,
            Arc::new(BgProvider { descriptor: bg_desc, builds: builds_bg.clone(), log: log.clone() }),
        )
        .unwrap();

    let plan_of = move |id: leaf_core::BeanId| -> LifecyclePlan {
        if id == id_a {
            LifecyclePlan { destroy: A_DESTROY, ..LifecyclePlan::EMPTY }
        } else if id == id_b {
            LifecyclePlan { destroy: B_DESTROY, ..LifecyclePlan::EMPTY }
        } else if id == id_bg {
            LifecyclePlan {
                bootstrap: Bootstrap::Background,
                destroy: BG_DESTROY,
                ..LifecyclePlan::EMPTY
            }
        } else {
            LifecyclePlan::EMPTY
        }
    };

    // A's mandatory construction edge to B (so B is in a strictly-earlier wave).
    let inj_of = move |id: leaf_core::BeanId| -> InjectionPlan {
        if id == id_a {
            InjectionPlan { points: B_POINT }
        } else {
            InjectionPlan::EMPTY
        }
    };

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    let unit = RunUnit::from_builder(builder)
        .expect("engine from builder")
        .with_plan_resolver(plan_of)
        .with_injection_plans(inj_of)
        .with_spawner(spawner);

    let unit = unit.refresh().await.expect("refresh succeeds");

    // Eager + once-only.
    assert_eq!(builds_b.load(Ordering::SeqCst), 1, "B built once eagerly");
    assert_eq!(builds_a.load(Ordering::SeqCst), 1, "A built once eagerly");
    assert_eq!(builds_bg.load(Ordering::SeqCst), 1, "Bg built once (background, joined)");

    assert_eq!(unit.run_state(), RunState::Running);
    assert_eq!(unit.availability().liveness(), LivenessState::Correct);
    assert_eq!(unit.availability().readiness(), ReadinessState::AcceptingTraffic);

    // The graph resolves; a second resolve of B is the SAME Arc (once-only).
    let a = unit.context().get::<A>().await.expect("A resolves");
    assert_eq!(a.b.tag, "b");
    let b = unit.context().get::<B>().await.expect("B resolves");
    assert!(std::ptr::eq(a.b.as_arc().as_ref(), b.as_arc().as_ref()));
    assert_eq!(builds_b.load(Ordering::SeqCst), 1, "no rebuild on resolve");

    // Wave-partition invariant: B built before A.
    {
        let l = log.lock().unwrap();
        let pos_b = l.iter().position(|s| *s == "build:B").expect("B built");
        let pos_a = l.iter().position(|s| *s == "build:A").expect("A built");
        assert!(pos_b < pos_a, "B built before A (earlier wave): {l:?}");
    }

    // ── teardown ──
    let rx = unit.watch_run_state();
    let report = unit.shutdown().await;

    // LIFO container drain: A (later wave) tears down before B (earlier wave).
    let destroyed = DESTROY_ORDER.lock().unwrap().clone();
    let pos_destroy_a = destroyed.iter().position(|s| *s == "destroy:A").expect("A destroyed");
    let pos_destroy_b = destroyed.iter().position(|s| *s == "destroy:B").expect("B destroyed");
    assert!(
        pos_destroy_a < pos_destroy_b,
        "A (later wave) tears down before B (earlier wave): {destroyed:?}"
    );
    assert!(destroyed.contains(&"destroy:Bg"), "Bg destroyed: {destroyed:?}");

    assert_eq!(report.run_state, RunState::Closed, "ended Closed");
    assert!(report.shutdown.is_clean(), "clean drain: {:?}", report.shutdown.errors);

    assert_eq!(unit.availability().readiness(), ReadinessState::RefusingTraffic);

    let observed = rx.borrow();
    assert!(observed.is_rundown(), "watch cell saw rundown, got {observed:?}");
    let _ = id_b;
}
