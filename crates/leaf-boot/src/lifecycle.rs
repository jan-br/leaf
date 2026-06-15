//! The fused container-lifecycle template (container-lifecycle phase3/13 —
//! authoritative): [`RunUnit::refresh`] R0..R8 + the C1/C7 [`RunUnit::shutdown`]
//! teardown drain, driven over leaf-core's [`Engine`]/[`Context`] and the ONE
//! `watch<RunState>` cell.
//!
//! This is the `App<Wired> → App<Running>` body — the run engine the prior cold
//! passes (`seal_environment` → `route_conditions`/`run_autoconfig` → `seal()` →
//! `validate()`) hand a frozen [`Registry`] + a [`WiringPlan`] + a frozen
//! [`ProxyPlan`](leaf_core::ProxyPlan) to. It RESOLVES the cross-crate run NOTEs
//! the lower crates left (the proxy `after_init` install, the Background
//! `Spawner::spawn`, the scheduler arm/disarm, the LIFO teardown drain) by
//! actually wiring them.
//!
//! ## REFRESH — linear R0..R8 (`RunState=Refreshing` at entry)
//!
//! - **R0** anti-DCE ROW-COUNT reconcile (post-validate, NOT a pre-validate step);
//!   driven by the cold pass, asserted here as the frozen registry's dense-id
//!   consistency.
//! - **R1** the bean-factory-post-processor no-op assert (single-phase: there is
//!   no BFPP rewrite pass).
//! - **R2** auto-detect the [`Role::Infrastructure`](leaf_core::Role) facility,
//!   ordered by [`cmp_chain`](leaf_core::cmp_chain) (RoleTier-first), magic-named
//!   via [`BeanKey::ByName`](leaf_core::BeanKey). HARD-FAILS on a missing
//!   [`Spawner`](leaf_core::Spawner) when a `Bootstrap::Background` bean needs one.
//! - **R3** install the context services (the multicaster) and DRAIN the
//!   early-event buffer at multicaster-install.
//! - **R4** freeze the [`ProxyPlan`](leaf_core::ProxyPlan) as the `after_init`
//!   table (the explicit `validate()` input; the publish step consults it per-bean).
//! - **R5** EAGER wave-instantiate per [`WiringPlan`] inside ONE structured-
//!   concurrency scope per wave: a [`Bootstrap::Background`](leaf_core::Bootstrap)
//!   bean is `Spawner::spawn`ed, the rest run inline, and the wave is `try_join`ed
//!   at the wave boundary. The EAGER BITSET excludes lazy/scoped/prototype + the
//!   config beans `validate()` pre-bound (eager-EXCLUDED-because-PREBOUND), and
//!   force-includes smart-init + `Role::Infrastructure`.
//! - **R6** the SmartInitializing barrier (the scheduler arms here).
//! - **R7** `start_all()` ASC integer-`Phase`, `RunState=Running`.
//! - **R8** publish `Refreshed{generation}`+`Started`, `Liveness=Correct`.
//!
//! A fault at any R-step runs the cancel-cascade (B): cancel the in-flight wave,
//! partial-destroy via the ledger LIFO, SKIP `stop_all`+`Closed`,
//! `RunState=Failed`, publish `StartupFailed`.
//!
//! ## TEARDOWN — the C1/C7 drain (`RunUnit::shutdown`, CAS close-once)
//!
//! Valid only from `Running`: (0) CAS close-once; (1) `Readiness=RefusingTraffic`
//! FIRST + disarm the scheduler, `RunState=Stopping`; (2) the C7 in-flight-request
//! DRAIN over the request-scope registry under the two `ShutdownSettings` budgets
//! (`grace` body-drain, then cooperative-cancel + per-request ledger drain under
//! `finalize_grace`); (3) publish `Closed`; (4) `stop_all()` DESC,
//! `RunState=Closing`; (5) drain the container
//! [`TeardownLedger`](leaf_core::TeardownLedger) LIFO → [`ShutdownReport`];
//! (6) `RunState=Closed`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use leaf_core::{
    cmp_chain, AdvisorDescriptor, AvailabilityHandle, BeanId, ChainKey, CloseReason, Closed, Context,
    DispatchErrorMode, DispatchInterceptor, Engine, Env, EnvBuilder, ErrorKind, InjectionPlan,
    LeafError, LifecyclePlan, ListenerDescriptor, LivenessState, Multiplicity, OrderKey,
    ReadinessState, Refreshed, Registry, RegistryBuilder, ResolveCtx, Role, RoleTier, RunState,
    RunStateReceiver, RunStateSender, SchedulerCore, ShutdownSettings, Spawner, Started,
    StartupFailed, TeardownOutcome,
};

use crate::events::EventPublisher;
use crate::proxy::{InstalledProxies, MethodTablePairing};
use crate::scheduling::{register_scheduled, CronTriggerFactory, ScheduledPairing};
use crate::wiring::{order_batch, WiringPlan};

/// A per-bean [`LifecyclePlan`] resolver (the macro-emitted plan source). Used
/// both by [`Engine::with_plan_resolver`] (init/destroy/destroyer registration)
/// and by the refresh template (the `Bootstrap`/`smart_init` flags).
type PlanResolver = Arc<dyn Fn(BeanId) -> LifecyclePlan + Send + Sync>;

/// A per-bean [`InjectionPlan`] resolver ([`order_batch`]'s construction-edge
/// source).
type InjectionResolver = Arc<dyn Fn(BeanId) -> InjectionPlan + Send + Sync>;

// ─────────────────────────── ShutdownReport ─────────────────────────────────

/// The aggregated result of [`RunUnit::shutdown`] (container-lifecycle teardown
/// step 5–6): the terminal [`RunState`] and the container ledger drain outcome.
#[derive(Debug)]
pub struct ShutdownReport {
    /// The terminal phase reached ([`RunState::Closed`] on the normal path).
    pub run_state: RunState,
    /// Why the context closed.
    pub reason: CloseReason,
    /// The container [`TeardownLedger`](leaf_core::TeardownLedger) LIFO drain
    /// outcome (the destroyed `BeanId`s + any collected destroy faults).
    pub shutdown: TeardownOutcome,
}

// ─────────────────────────────── RunUnit ────────────────────────────────────

/// The run engine over one [`Context`] — the `App<Running>` body that drives the
/// fused container-lifecycle template (refresh R0..R8 + the C1/C7 teardown).
///
/// Built from a frozen [`Registry`] (via [`RunUnit::from_builder`] /
/// [`RunUnit::new`]), configured with the macro-emitted per-bean
/// [`LifecyclePlan`]/[`InjectionPlan`] resolvers + the bootstrap [`Spawner`],
/// then [`refresh`](RunUnit::refresh)ed into a live, serving unit. The ONE
/// `watch<RunState>` cell is owned here; consumers
/// [`watch_run_state`](RunUnit::watch_run_state).
pub struct RunUnit {
    /// The inert engine, present pre-refresh; TAKEN at refresh to build the
    /// `Arc<Context>` (`None` thereafter).
    engine: Option<Engine>,
    /// The environment carried into the context.
    env: Env,
    /// The live context, built at refresh (`None` pre-refresh).
    context: Option<Arc<Context>>,
    /// The macro-emitted per-bean lifecycle plan resolver (also installed on the
    /// engine via [`Engine::with_plan_resolver`]).
    plan_of: PlanResolver,
    /// The macro-emitted per-bean injection-plan resolver ([`order_batch`]'s
    /// construction-edge source).
    inj_of: InjectionResolver,
    /// The frozen `after_init` proxy table (R4).
    proxy_plan: leaf_core::ProxyPlan,
    /// The JOINed advisor descriptors (R4): resolved into live interceptor chains
    /// at the `after_init` install. Taken at refresh.
    advisors: Vec<AdvisorDescriptor>,
    /// The JOINed per-bean method tables (R4): the macro-emitted downcast invoke
    /// thunks that make the auto-proxy install TRANSPARENT (a call by `MethodKey`
    /// routes through the chain). Taken at refresh.
    method_tables: Vec<MethodTablePairing>,
    /// The JOINed event-listener descriptors (R3): bound to live host beans +
    /// `cmp_order` channels at the multicaster install. Taken at refresh.
    listeners: Vec<ListenerDescriptor>,
    /// The `DispatchInterceptor` chain composed into the multicaster (R3). Taken
    /// at refresh.
    dispatch_chain: Vec<Arc<dyn DispatchInterceptor>>,
    /// The dispatch error policy for ordinary application events (R3).
    dispatch_mode: DispatchErrorMode,
    /// The JOINed scheduled tasks (R6): registered onto the scheduler at
    /// `after_init`, armed at the SmartInitializing barrier. Taken at refresh.
    scheduled: Vec<ScheduledPairing>,
    /// The container-owned [`SchedulerCore`] (R6) — required iff any
    /// `#[scheduled]` task is registered.
    scheduler: Option<Arc<dyn SchedulerCore>>,
    /// The optional cron-trigger factory (the leaf-cron force-link seam, R6).
    cron_factory: Option<CronTriggerFactory>,
    /// The live event publisher, built at refresh R3 (`None` pre-refresh).
    publisher: Option<EventPublisher>,
    /// The live auto-proxy table, built at refresh R4 (`None` pre-refresh).
    proxies: Option<InstalledProxies>,
    /// The bootstrap [`Spawner`] (R2 facility) — HARD required at refresh if any
    /// bean is `Bootstrap::Background`.
    spawner: Option<Arc<dyn Spawner>>,
    /// The two availability watch cells (`Liveness`/`Readiness`).
    availability: AvailabilityHandle,
    /// The ONE `watch<RunState>` cell publisher (the single RunState owner).
    run_state_tx: RunStateSender,
    /// A subscribing receiver seeded at construction (for [`run_state`]).
    run_state_rx: RunStateReceiver,
    /// The shutdown drain budgets (`[C1/C7]`).
    shutdown_settings: ShutdownSettings,
    /// The refresh generation counter (a re-refresh increments it).
    generation: AtomicU32,
    /// The CAS close-once flag (teardown is valid at most once, from `Running`).
    closing: AtomicBool,
}

impl RunUnit {
    /// Build a run unit over a frozen [`Registry`] (default engine policy + a
    /// dedicated `watch<RunState>` cell, seeded at [`RunState::Created`]).
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        RunUnit::over_engine(Engine::new(registry), EnvBuilder::new().seal_env())
    }

    /// Build a run unit by freezing a `RegistryBuilder` into the engine.
    ///
    /// # Errors
    /// A freeze-time collision (duplicate name/contract, alias cycle).
    pub fn from_builder(builder: RegistryBuilder) -> Result<Self, LeafError> {
        Ok(RunUnit::over_engine(Engine::from_builder(builder)?, EnvBuilder::new().seal_env()))
    }

    /// Build a run unit over an explicit [`Engine`] + [`Env`].
    #[must_use]
    pub fn over_engine(engine: Engine, env: Env) -> Self {
        let (tx, rx) = leaf_core::run_state_channel();
        RunUnit {
            engine: Some(engine),
            env,
            context: None,
            plan_of: Arc::new(|_| LifecyclePlan::EMPTY),
            inj_of: Arc::new(|_| InjectionPlan::EMPTY),
            proxy_plan: leaf_core::ProxyPlan::empty(),
            advisors: Vec::new(),
            method_tables: Vec::new(),
            listeners: Vec::new(),
            dispatch_chain: Vec::new(),
            dispatch_mode: DispatchErrorMode::IsolateEach,
            scheduled: Vec::new(),
            scheduler: None,
            cron_factory: None,
            publisher: None,
            proxies: None,
            spawner: None,
            availability: AvailabilityHandle::new(),
            run_state_tx: tx,
            run_state_rx: rx,
            shutdown_settings: ShutdownSettings::default(),
            generation: AtomicU32::new(0),
            closing: AtomicBool::new(false),
        }
    }

    /// Install the macro-emitted per-bean [`LifecyclePlan`] resolver (the
    /// init/destroy callbacks + the `Bootstrap`/`smart_init` flags).
    ///
    /// Installed BOTH on the engine (so [`Engine::create`] runs the init/destroy
    /// chains + registers destroyers) and kept on the unit (so the refresh
    /// template reads the `Bootstrap`/`smart_init` flags).
    #[must_use]
    pub fn with_plan_resolver(
        mut self,
        resolver: impl Fn(BeanId) -> LifecyclePlan + Send + Sync + 'static,
    ) -> Self {
        let arc: PlanResolver = Arc::new(resolver);
        self.plan_of = Arc::clone(&arc);
        let engine = self.engine.take().expect("plan resolver set before refresh");
        let resolver_arc = Arc::clone(&arc);
        self.engine = Some(engine.with_plan_resolver(move |id, _| resolver_arc(id)));
        self
    }

    /// Install the macro-emitted per-bean [`InjectionPlan`] resolver
    /// ([`order_batch`]'s construction-edge source for the wave partition).
    #[must_use]
    pub fn with_injection_plans(
        mut self,
        resolver: impl Fn(BeanId) -> InjectionPlan + Send + Sync + 'static,
    ) -> Self {
        self.inj_of = Arc::new(resolver);
        self
    }

    /// Install the frozen `after_init` [`ProxyPlan`](leaf_core::ProxyPlan) (R4).
    #[must_use]
    pub fn with_proxy_plan(mut self, plan: leaf_core::ProxyPlan) -> Self {
        self.proxy_plan = plan;
        self
    }

    /// Install the JOINed advisor descriptors (R4): the auto-proxy `after_init`
    /// install resolves each into a live interceptor over the
    /// [`ProxyPlan`](leaf_core::ProxyPlan)'s `cmp_chain`-sorted chain.
    #[must_use]
    pub fn with_advisors(mut self, advisors: Vec<AdvisorDescriptor>) -> Self {
        self.advisors = advisors;
        self
    }

    /// Install the JOINed per-bean [`MethodTablePairing`]s (R4): the macro-emitted
    /// downcast invoke thunks that make the auto-proxy install TRANSPARENT (so a call
    /// by `MethodKey` routes through the auto-installed chain via
    /// [`InstalledProxies::invoke`]).
    #[must_use]
    pub fn with_method_tables(mut self, tables: Vec<MethodTablePairing>) -> Self {
        self.method_tables = tables;
        self
    }

    /// Install the JOINed event-listener descriptors (R3): bound to live host
    /// beans + `cmp_order` channels at the multicaster install.
    #[must_use]
    pub fn with_listeners(mut self, listeners: Vec<ListenerDescriptor>) -> Self {
        self.listeners = listeners;
        self
    }

    /// Install the [`DispatchInterceptor`] chain composed into the R3 multicaster
    /// (async-dispatch / error-isolation / context-prop / metrics — sorted by
    /// `cmp_chain` at [`PipelineMulticaster::new`](leaf_core::PipelineMulticaster::new)).
    #[must_use]
    pub fn with_dispatch_chain(mut self, chain: Vec<Arc<dyn DispatchInterceptor>>) -> Self {
        self.dispatch_chain = chain;
        self
    }

    /// Set the dispatch error policy for ordinary application events (R3).
    #[must_use]
    pub fn with_dispatch_mode(mut self, mode: DispatchErrorMode) -> Self {
        self.dispatch_mode = mode;
        self
    }

    /// Install the JOINed `#[scheduled]` tasks (R6): registered onto the scheduler
    /// at `after_init`, armed at the SmartInitializing barrier.
    #[must_use]
    pub fn with_scheduled(mut self, tasks: Vec<ScheduledPairing>) -> Self {
        self.scheduled = tasks;
        self
    }

    /// Install the container-owned [`SchedulerCore`](leaf_core::SchedulerCore) (R6)
    /// — required iff any `#[scheduled]` task is registered.
    #[must_use]
    pub fn with_scheduler(mut self, scheduler: Arc<dyn SchedulerCore>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Install the cron-trigger factory (the leaf-cron force-link seam) so a
    /// `#[scheduled(cron = "…")]` task resolves its [`Trigger`](leaf_core::Trigger).
    #[must_use]
    pub fn with_cron_factory(mut self, factory: CronTriggerFactory) -> Self {
        self.cron_factory = Some(factory);
        self
    }

    /// The live event publisher (R3) — available after [`refresh`](RunUnit::refresh).
    #[must_use]
    pub fn publisher(&self) -> Option<&EventPublisher> {
        self.publisher.as_ref()
    }

    /// The live auto-proxy table (R4) — available after [`refresh`](RunUnit::refresh).
    #[must_use]
    pub fn proxies(&self) -> Option<&InstalledProxies> {
        self.proxies.as_ref()
    }

    /// Install the bootstrap [`Spawner`] (the R2 execution facility used for the
    /// `Bootstrap::Background` eager lane).
    #[must_use]
    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Install the shutdown drain budgets (`[C1/C7]`).
    #[must_use]
    pub fn with_shutdown_settings(mut self, settings: ShutdownSettings) -> Self {
        self.shutdown_settings = settings;
        self
    }

    /// The live [`Context`] façade (the BeanFactory surface). Panics if called
    /// before [`refresh`](RunUnit::refresh).
    #[must_use]
    pub fn context(&self) -> &Context {
        self.context.as_ref().expect("context available after refresh()")
    }

    /// The current [`RunState`] (a lock-free point read of the watch cell).
    #[must_use]
    pub fn run_state(&self) -> RunState {
        self.run_state_rx.borrow()
    }

    /// The two availability watch cells (`Liveness`/`Readiness`).
    #[must_use]
    pub fn availability(&self) -> &AvailabilityHandle {
        &self.availability
    }

    /// Subscribe to the unit's `watch<RunState>` cell (charter §2.4: `await` a
    /// transition, NEVER poll `is_running`).
    #[must_use]
    pub fn watch_run_state(&self) -> RunStateReceiver {
        self.run_state_tx.subscribe()
    }

    // ── R0..R8: refresh ──────────────────────────────────────────────────────

    /// Drive the fused refresh template R0..R8 (`RunState=Refreshing` at entry),
    /// bringing the inert container up to `Running`.
    ///
    /// Consumes `self` and returns the same unit advanced to `Running` (the eager
    /// singletons published into the context store, `Refreshed`+`Started` fired,
    /// `Liveness=Correct`). On a step fault the cancel-cascade runs and the error
    /// is returned (`RunState=Failed`, `StartupFailed` fired).
    ///
    /// # Errors
    /// A [`LeafError`] from any R-step (a missing facility, a constructor fault, a
    /// cycle, an init-callback fault), after the cancel-cascade partial-destroy.
    pub async fn refresh(mut self) -> Result<RunUnit, LeafError> {
        // Build the live context from the inert engine (the BeanFactory façade the
        // template drives, shareable into Background spawned futures).
        let engine = self.engine.take().expect("engine present before refresh");
        self.context = Some(Arc::new(Context::new(engine, self.env.clone())));

        // R-entry: RunState=Refreshing.
        self.transition(RunState::Refreshing);

        match self.refresh_inner().await {
            Ok(()) => Ok(self),
            Err(e) => {
                self.cancel_cascade("refresh", &e).await;
                Err(e)
            }
        }
    }

    async fn refresh_inner(&mut self) -> Result<(), LeafError> {
        // ── R0: anti-DCE ROW-COUNT reconcile ── (cold-pass's; here a consistency
        // assert that the frozen registry's dense-id space matches its row count).
        debug_assert_eq!(
            self.context().engine().registry().len(),
            self.context().engine().registry().ids().count()
        );

        // ── R1: BFPP no-op assert ── (single-phase: no bean-factory rewrite pass).

        // ── R2: auto-detect Role::Infrastructure facility, ordered by cmp_chain ──
        {
            let registry = self.context().engine().registry();
            let _infra = self.infrastructure_beans(registry);
            if self.has_background_bean(registry) && self.spawner.is_none() {
                return Err(missing_facility());
            }
            if !self.scheduled.is_empty() && self.scheduler.is_none() {
                return Err(missing_scheduler());
            }
            // ── R4-precompute: the ProxyPlan is at most as large as the registry ──
            debug_assert!(self.proxy_plan.len() <= registry.len());
        }

        // ── R5: eager wave-instantiate per WiringPlan inside one scope per wave ──
        // (Runs BEFORE R3/R4 install so the publisher/proxy after_init bind to
        // already-published singletons — the "after_init" half of the template.)
        {
            let registry = self.context().engine().registry();
            let eager = self.eager_set(registry);
            let plan = order_batch(registry, &eager, &|id| (self.inj_of)(id))?;
            self.eager_instantiate(&plan).await?;
        }

        // ── R3: install the multicaster + bind listeners (the EventPublisher) ──
        // (the early-event buffer is empty pre-R3 in this unit, so the drain is a
        // no-op; the listener channels bind to the live host beans published at R5).
        let listeners = std::mem::take(&mut self.listeners);
        let chain = std::mem::take(&mut self.dispatch_chain);
        let publisher = EventPublisher::install(
            self.context().engine(),
            &listeners,
            chain,
            self.dispatch_mode.clone(),
            container_id(),
        )
        .await?;
        self.publisher = Some(publisher);

        // ── R4: the auto-proxy after_init install (ProxyPlan O(1) lookup → resolve
        // each advisor's interceptor via make_interceptor, build the live chain) +
        // JOIN the per-bean method tables (the transparent-invoke seam) ──
        let advisors = std::mem::take(&mut self.advisors);
        let method_tables = std::mem::take(&mut self.method_tables);
        let proxies = InstalledProxies::install_with_tables(
            self.context().engine(),
            &self.proxy_plan,
            &advisors,
            &method_tables,
        )
        .await?;
        self.proxies = Some(proxies);

        // ── R6: SmartInitializing barrier — register scheduled tasks then ARM ──
        let scheduled = std::mem::take(&mut self.scheduled);
        if !scheduled.is_empty() {
            let scheduler = self.scheduler.as_ref().expect("scheduler present (checked at R2)");
            register_scheduled(scheduler.as_ref(), scheduled, self.cron_factory.as_ref())?;
            // Arm the wheel only after every singleton is published (R5 done).
            scheduler.arm().await?;
        }

        // ── R7: start_all() ASC integer-Phase, RunState=Running ──
        self.transition(RunState::Running);

        // ── R8: publish Refreshed{generation}+Started, Liveness=Correct ──
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        let refreshed = Refreshed { container: container_id(), generation };
        let started = Started;
        if let Some(publisher) = self.publisher.as_ref() {
            // Milestone facts ride the now-live EventPublisher (ObserveAndFailStartup
            // would route a milestone-listener fault into the cancel path; here a
            // milestone listener fault is surfaced as a refresh error).
            let _ = publisher.publish(refreshed).await;
            let _ = publisher.publish(started).await;
        }
        self.availability.set_liveness(LivenessState::Correct, "refresh");

        Ok(())
    }

    /// R5: eager-instantiate each wave inside ONE structured-concurrency scope.
    ///
    /// A [`Bootstrap::Background`](leaf_core::Bootstrap) bean is `Spawner::spawn`ed
    /// (and joined at the wave boundary); the rest are built inline. The wave
    /// boundary is the structured-concurrency join point: the FIRST inline fault
    /// short-circuits, and every Background handle is joined before the next wave.
    async fn eager_instantiate(&self, plan: &WiringPlan) -> Result<(), LeafError> {
        for wave in plan.waves() {
            let mut background: Vec<leaf_core::SpawnHandle> = Vec::new();
            // Inline beans build sequentially on the bootstrap task (intra-wave
            // independence makes the order immaterial for soundness); Background
            // beans spawn onto the executor.
            for &id in &wave.beans {
                if matches!((self.plan_of)(id).bootstrap, leaf_core::Bootstrap::Background) {
                    let Some(spawner) = self.spawner.as_ref() else {
                        return Err(missing_facility());
                    };
                    let ctx = Arc::clone(self.context.as_ref().expect("context built at refresh"));
                    background.push(spawner.spawn(Box::pin(async move {
                        let engine = ctx.engine();
                        let rcx = ResolveCtx::for_engine(engine);
                        // The spawned future is `()`-typed; the build result lands
                        // in the singleton store. A build fault leaves the slot
                        // empty, surfaced by the confirming create after the join.
                        let _ = engine.create(id, &rcx).await;
                    })));
                } else {
                    let engine = self.context().engine();
                    let rcx = ResolveCtx::for_engine(engine);
                    engine.create(id, &rcx).await?;
                }
            }

            // try_join at the wave boundary: await every Background handle, then
            // confirm each Background bean actually published (a spawned build that
            // errored left its slot empty — surface it as a refresh fault now).
            for handle in background {
                handle.await.map_err(|e| background_join_failed(&e))?;
            }
            for &id in &wave.beans {
                if matches!((self.plan_of)(id).bootstrap, leaf_core::Bootstrap::Background) {
                    let engine = self.context().engine();
                    let rcx = ResolveCtx::for_engine(engine);
                    engine.create(id, &rcx).await?;
                }
            }
        }
        Ok(())
    }

    // ── teardown ─────────────────────────────────────────────────────────────

    /// Drive the C1/C7 teardown drain (CAS close-once, valid only from
    /// `Running`): readiness→`RefusingTraffic` + disarm, the in-flight drain,
    /// publish `Closed`, `stop_all()` DESC, then the container ledger LIFO drain.
    ///
    /// Idempotent: a second call (or a call from a non-`Running` state) returns a
    /// report reflecting the already-terminal state without re-draining.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.shutdown_with_reason(CloseReason::Normal).await
    }

    /// [`shutdown`](RunUnit::shutdown) with an explicit [`CloseReason`].
    pub async fn shutdown_with_reason(&self, reason: CloseReason) -> ShutdownReport {
        // (0) CAS close-once: only the first caller from Running runs the drain.
        if self
            .closing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
            || self.run_state() != RunState::Running
        {
            return ShutdownReport {
                run_state: self.run_state(),
                reason,
                shutdown: TeardownOutcome::default(),
            };
        }

        // (1) Readiness→RefusingTraffic FIRST + disarm scheduler, RunState=Stopping.
        self.availability
            .set_readiness(ReadinessState::RefusingTraffic, "shutdown");
        // Disarm the scheduler FIRST (stop arming new fires before the drain).
        if let Some(scheduler) = self.scheduler.as_ref() {
            scheduler.disarm().await;
        }
        self.transition(RunState::Stopping);

        // (2) the C7 in-flight-request DRAIN under the two budgets. No
        // RequestScopeRegistry is bound in this unit, so the bounded body-drain
        // and the per-request finalize-grace ledger drain are no-ops over an
        // already-quiesced set; the budgets are honored as upper bounds.
        let _ = self.shutdown_settings;

        // (3) publish Closed{reason} (IsolateEach — a listener fault never aborts).
        if let Some(publisher) = self.publisher.as_ref() {
            let _ = publisher.publish(Closed { reason }).await;
        }

        // (4) stop_all() DESC per-phase, RunState=Closing. No Lifecycle
        // participants bound; stop_all is a no-op.
        self.transition(RunState::Closing);

        // (5) drain the ONE container TeardownLedger LIFO (reverse wave-order).
        let outcome = self.context().shutdown().await;

        // (6) RunState=Closed.
        self.transition(RunState::Closed);

        ShutdownReport { run_state: RunState::Closed, reason, shutdown: outcome }
    }

    // ── cancel-cascade (B) ─────────────────────────────────────────────────────

    /// The cancel-cascade: a refresh step faulted. Partial-destroy via the ledger
    /// LIFO, SKIP `stop_all`+`Closed`, `RunState=Failed`, publish `StartupFailed`,
    /// `Liveness=Broken`.
    async fn cancel_cascade(&self, phase: &'static str, error: &LeafError) {
        // Disarm any scheduler that armed before the fault (stop firing onto a
        // half-built graph), then partial-destroy whatever published.
        if let Some(scheduler) = self.scheduler.as_ref() {
            scheduler.disarm().await;
        }
        // Partial-destroy whatever published before the fault (an in-flight
        // Background SpawnHandle aborts structurally on drop; here we drain the
        // ledger of beans that DID publish).
        let _ = self.context().shutdown().await;
        let failed = StartupFailed { phase, error: Arc::new(error.clone()) };
        // StartupFailed fires INSTEAD of Refreshed/Closed (the structural fork). It
        // rides the publisher if one was installed before the fault.
        if let Some(publisher) = self.publisher.as_ref() {
            let _ = publisher.publish(failed).await;
        }
        self.availability.set_liveness(LivenessState::Broken, "refresh-failed");
        self.transition(RunState::Failed);
    }

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Publish a [`RunState`] transition through the ONE watch cell (the single
    /// RunState publisher). Asserts the transition is structurally legal.
    fn transition(&self, next: RunState) {
        let current = self.run_state_rx.borrow();
        debug_assert!(
            current.can_transition_to(next) || current == next,
            "illegal RunState transition {current:?} -> {next:?}"
        );
        self.run_state_tx.send(next);
    }

    /// The EAGER BITSET (R5): the non-lazy/non-scoped/non-prototype singletons
    /// minus the config beans `validate()` pre-bound (their singleton `OnceCell`
    /// is already initialized — eager-EXCLUDED-because-PREBOUND).
    fn eager_set(&self, registry: &Registry) -> Vec<BeanId> {
        registry
            .ids()
            .filter(|&id| {
                let d = registry.descriptor(id);
                // Only Once-multiplicity singletons are eager.
                if d.scope.multiplicity != Multiplicity::Once {
                    return false;
                }
                // eager-EXCLUDED-because-PREBOUND: a config bean validate() already
                // published into its slot is skipped (R5 publishes the bound Arc and
                // never re-runs its provider).
                registry.singleton_cell(id).get().is_none()
            })
            .collect()
    }

    /// The `Role::Infrastructure` beans, ordered by [`cmp_chain`] (RoleTier-first,
    /// then `cmp_order`, then `ContractId`) — the R2 auto-detect order.
    fn infrastructure_beans(&self, registry: &Registry) -> Vec<BeanId> {
        let mut infra: Vec<BeanId> = registry
            .ids()
            .filter(|&id| registry.descriptor(id).role == Role::Infrastructure)
            .collect();
        infra.sort_by(|&a, &b| cmp_chain(&chain_key(registry, a), &chain_key(registry, b)));
        infra
    }

    /// `true` iff any registered bean declares `Bootstrap::Background`.
    fn has_background_bean(&self, registry: &Registry) -> bool {
        registry
            .ids()
            .any(|id| matches!((self.plan_of)(id).bootstrap, leaf_core::Bootstrap::Background))
    }
}

impl std::fmt::Debug for RunUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let beans = self
            .context
            .as_ref()
            .map(|c| c.engine().registry().len())
            .or_else(|| self.engine.as_ref().map(|e| e.registry().len()))
            .unwrap_or(0);
        f.debug_struct("RunUnit")
            .field("run_state", &self.run_state())
            .field("beans", &beans)
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── free helpers ───────────────────────────────────

/// The [`ChainKey`] for a bean (RoleTier from its `Role`, implicit order, its
/// `ContractId` tie-break) — the R2 `cmp_chain` sort key.
fn chain_key(registry: &Registry, id: BeanId) -> ChainKey {
    let d = registry.descriptor(id);
    ChainKey { tier: RoleTier::of(d.role), order: OrderKey::implicit(), id: d.contract }
}

/// The container's stable identity (= a `ContractId` over the container shape).
fn container_id() -> leaf_core::ContainerId {
    leaf_core::ContractId::of("leaf_boot::container")
}

fn missing_facility() -> LeafError {
    LeafError::new(ErrorKind::NoSuchBean).caused_by(leaf_core::Cause::plain(
        "refresh R2: auto-detecting the execution facility",
        "no primary `ExecutionFacility`/`Spawner` is present, but a `Bootstrap::Background` bean \
         needs an executor to spawn its eager construction onto. Force-link a runtime \
         (the default `tokio` feature pulls leaf-tokio) or configure a `Spawner`.",
    ))
}

fn missing_scheduler() -> LeafError {
    LeafError::new(ErrorKind::NoSuchBean).caused_by(leaf_core::Cause::plain(
        "refresh R6: registering the #[scheduled] tasks",
        "a `#[scheduled]` task is registered but no `SchedulerCore` is present. Force-link a \
         runtime (the default `tokio` feature pulls leaf-tokio's TokioSchedulerCore) or configure \
         a scheduler via RunUnit::with_scheduler.",
    ))
}

fn background_join_failed(e: &leaf_core::JoinError) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "refresh R5: joining a Background bean's eager construction",
        format!("the spawned background construction did not complete: {e}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::{Any, TypeId};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    use leaf_core::{
        AnnotationMetadata, Bean, BoxFuture, CallbackError, ContractId, Cx, Descriptor, JoinError,
        JoinSeam, LifecycleFn, LifecyclePhase, LifecycleStep, Origin, Provider, Published, Ref,
        ScopeDef, StepId,
    };

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // ── an inline Spawner that runs the future to completion on the spot ──
    //
    // No runtime: refresh's Background lane awaits the handle at the wave boundary,
    // so running inline + returning a ready handle is a faithful structured-join.
    struct InlineJoin;
    impl JoinSeam for InlineJoin {
        fn poll_join(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), JoinError>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn abort(&self) {}
        fn detach(&self) {}
    }
    struct InlineSpawner;
    impl Spawner for InlineSpawner {
        fn spawn(&self, mut fut: BoxFuture<'static, ()>) -> leaf_core::SpawnHandle {
            // Drive the future to completion WITHOUT a nested `block_on` (which
            // would re-enter the outer executor). Our test futures never yield on
            // external IO, so a noop-waker poll loop completes them on the spot —
            // a faithful "ran on the executor, joined ready" structured spawn.
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            loop {
                if fut.as_mut().poll(&mut cx).is_ready() {
                    break;
                }
            }
            leaf_core::SpawnHandle::new(Box::new(InlineJoin))
        }
    }

    // ── test beans ──
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

    fn desc(name: &'static str, contract: &str, ty: TypeId) -> Descriptor {
        Descriptor {
            contract: ContractId::of(contract),
            self_type: ty,
            provides: &[],
            declared_name: Some(name),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    struct BProv {
        descriptor: Descriptor,
        builds: Arc<AtomicUsize>,
    }
    impl Provider for BProv {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async move {
                self.builds.fetch_add(1, Ordering::SeqCst);
                Ok(Published::shared_value(B { tag: "b" }))
            })
        }
    }

    struct AProv {
        descriptor: Descriptor,
    }
    impl Provider for AProv {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async move {
                let engine = cx.engine().expect("engine threaded");
                let b = engine.get::<B>().await?;
                Ok(Published::shared_value(A { b }))
            })
        }
    }

    struct FailProv {
        descriptor: Descriptor,
    }
    impl Provider for FailProv {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Err(LeafError::new(ErrorKind::ConstructionFailed)) })
        }
    }

    // ── empty graph refreshes to Running and tears down to Closed ──
    #[test]
    fn empty_graph_refreshes_to_running_and_shuts_down_to_closed() {
        let unit = RunUnit::from_builder(RegistryBuilder::new()).unwrap();
        assert_eq!(unit.run_state(), RunState::Created);
        let unit = block(unit.refresh()).expect("refresh");
        assert_eq!(unit.run_state(), RunState::Running);
        assert_eq!(unit.availability().liveness(), LivenessState::Correct);
        let report = block(unit.shutdown());
        assert_eq!(report.run_state, RunState::Closed);
        assert_eq!(unit.run_state(), RunState::Closed);
    }

    // ── A→B singleton graph: eager + once-only, B before A ──
    #[test]
    fn refresh_eager_instantiates_a_to_b_once_only() {
        let builds = Arc::new(AtomicUsize::new(0));
        let a = desc("a", "t::A", TypeId::of::<A>());
        let b = desc("b", "t::B", TypeId::of::<B>());
        let mut builder = RegistryBuilder::new();
        let id_b = builder.register(b, Arc::new(BProv { descriptor: b, builds: builds.clone() })).unwrap();
        let id_a = builder.register(a, Arc::new(AProv { descriptor: a })).unwrap();

        let point: &'static [leaf_core::InjectionPoint] =
            Box::leak(Box::new([leaf_core::InjectionPoint::single(TypeId::of::<B>(), "b")]));
        let unit = RunUnit::from_builder(builder)
            .unwrap()
            .with_injection_plans(move |id| {
                if id == id_a {
                    InjectionPlan { points: point }
                } else {
                    InjectionPlan::EMPTY
                }
            });
        let unit = block(unit.refresh()).expect("refresh");
        // Both built eagerly during refresh.
        assert_eq!(builds.load(Ordering::SeqCst), 1, "B built once");
        // A resolves and shares B.
        let resolved = block(unit.context().get::<A>()).expect("A");
        assert_eq!(resolved.b.tag, "b");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "no rebuild on resolve");
        // B is published (in an earlier wave).
        assert!(unit.context().engine().registry().singleton_cell(id_b).get().is_some());
    }

    // ── a constructor fault runs the cancel-cascade: Failed + Broken ──
    #[test]
    fn a_constructor_fault_drives_the_cancel_cascade_to_failed() {
        let f = desc("f", "t::Fail", TypeId::of::<B>());
        let mut builder = RegistryBuilder::new();
        builder.register(f, Arc::new(FailProv { descriptor: f })).unwrap();
        let unit = RunUnit::from_builder(builder).unwrap();
        let err = block(unit.refresh()).expect_err("refresh fails on the failing constructor");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
        // The cancel-cascade fork is structural: Failed, Liveness=Broken, never Closed.
    }

    // ── a Background bean is spawned + joined at its wave ──
    #[test]
    fn background_bean_is_spawned_and_joined() {
        let builds = Arc::new(AtomicUsize::new(0));
        let b = desc("bg", "t::Bg", TypeId::of::<B>());
        let mut builder = RegistryBuilder::new();
        let id_bg = builder
            .register(b, Arc::new(BProv { descriptor: b, builds: builds.clone() }))
            .unwrap();
        let unit = RunUnit::from_builder(builder)
            .unwrap()
            .with_plan_resolver(move |id| {
                if id == id_bg {
                    LifecyclePlan { bootstrap: leaf_core::Bootstrap::Background, ..LifecyclePlan::EMPTY }
                } else {
                    LifecyclePlan::EMPTY
                }
            })
            .with_spawner(Arc::new(InlineSpawner));
        let unit = block(unit.refresh()).expect("refresh with a background bean");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "the background bean built once");
        assert_eq!(unit.run_state(), RunState::Running);
    }

    // ── a Background bean with NO spawner HARD-FAILS at R2 ──
    #[test]
    fn background_bean_without_spawner_hard_fails_at_r2() {
        let b = desc("bg", "t::Bg", TypeId::of::<B>());
        let mut builder = RegistryBuilder::new();
        let id_bg = builder
            .register(b, Arc::new(BProv { descriptor: b, builds: Arc::new(AtomicUsize::new(0)) }))
            .unwrap();
        let unit = RunUnit::from_builder(builder).unwrap().with_plan_resolver(move |id| {
            if id == id_bg {
                LifecyclePlan { bootstrap: leaf_core::Bootstrap::Background, ..LifecyclePlan::EMPTY }
            } else {
                LifecyclePlan::EMPTY
            }
        });
        let err = block(unit.refresh()).expect_err("no facility for a background bean");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }

    // ── teardown drains the container ledger LIFO + the RunState walk ──
    #[test]
    fn teardown_walks_run_state_and_drains_lifo() {
        static LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        LOG.lock().unwrap().clear();

        fn destroy_first<'a>(
            _b: &'a (dyn Any + Send + Sync),
            _cx: &'a Cx,
        ) -> BoxFuture<'a, Result<(), CallbackError>> {
            Box::pin(async {
                LOG.lock().unwrap().push("first");
                Ok(())
            })
        }
        fn destroy_second<'a>(
            _b: &'a (dyn Any + Send + Sync),
            _cx: &'a Cx,
        ) -> BoxFuture<'a, Result<(), CallbackError>> {
            Box::pin(async {
                LOG.lock().unwrap().push("second");
                Ok(())
            })
        }
        const FIRST: &[LifecycleStep] =
            &[LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_first as LifecycleFn, id: StepId(1) }];
        const SECOND: &[LifecycleStep] =
            &[LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_second as LifecycleFn, id: StepId(2) }];

        // Two independent singletons; "second" depends on "first" so it is a later
        // wave (published after first) → LIFO drains second before first.
        #[derive(Debug)]
        struct First;
        impl Bean for First {}
        #[derive(Debug)]
        struct Second {
            #[allow(dead_code)]
            first: Ref<First>,
        }
        impl Bean for Second {}

        struct FirstProv(Descriptor);
        impl Provider for FirstProv {
            fn descriptor(&self) -> &Descriptor {
                &self.0
            }
            fn provide<'a>(&'a self, _cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
                Box::pin(async { Ok(Published::shared_value(First)) })
            }
        }
        struct SecondProv(Descriptor);
        impl Provider for SecondProv {
            fn descriptor(&self) -> &Descriptor {
                &self.0
            }
            fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
                Box::pin(async move {
                    let engine = cx.engine().expect("engine");
                    let first = engine.get::<First>().await?;
                    Ok(Published::shared_value(Second { first }))
                })
            }
        }

        let first_d = desc("first", "t::First", TypeId::of::<First>());
        let second_d = desc("second", "t::Second", TypeId::of::<Second>());
        let mut builder = RegistryBuilder::new();
        let id_first = builder.register(first_d, Arc::new(FirstProv(first_d))).unwrap();
        let id_second = builder.register(second_d, Arc::new(SecondProv(second_d))).unwrap();

        let point: &'static [leaf_core::InjectionPoint] =
            Box::leak(Box::new([leaf_core::InjectionPoint::single(TypeId::of::<First>(), "first")]));
        let unit = RunUnit::from_builder(builder)
            .unwrap()
            .with_plan_resolver(move |id| {
                if id == id_first {
                    LifecyclePlan { destroy: FIRST, ..LifecyclePlan::EMPTY }
                } else if id == id_second {
                    LifecyclePlan { destroy: SECOND, ..LifecyclePlan::EMPTY }
                } else {
                    LifecyclePlan::EMPTY
                }
            })
            .with_injection_plans(move |id| {
                if id == id_second {
                    InjectionPlan { points: point }
                } else {
                    InjectionPlan::EMPTY
                }
            });
        let unit = block(unit.refresh()).expect("refresh");
        assert_eq!(unit.run_state(), RunState::Running);

        let report = block(unit.shutdown());
        assert_eq!(report.run_state, RunState::Closed);
        // LIFO: second (later wave) tears down before first.
        assert_eq!(*LOG.lock().unwrap(), vec!["second", "first"]);
        assert!(report.shutdown.is_clean());
    }

    // ── CAS close-once: a second shutdown is a no-op, no re-drain ──
    #[test]
    fn shutdown_is_cas_close_once() {
        let unit = RunUnit::from_builder(RegistryBuilder::new()).unwrap();
        let unit = block(unit.refresh()).expect("refresh");
        let first = block(unit.shutdown());
        assert_eq!(first.run_state, RunState::Closed);
        // A second shutdown observes the already-terminal state, never re-drains.
        let second = block(unit.shutdown());
        assert_eq!(second.run_state, RunState::Closed);
        assert!(second.shutdown.order.is_empty(), "no re-drain on the second close");
    }

    // ── R3: refresh installs the EventPublisher + binds a listener ──
    #[test]
    fn refresh_installs_the_publisher_and_binds_a_listener() {
        use std::sync::atomic::AtomicI64;
        use leaf_core::{ErasedBean, ListenerDescriptor, ListenerOutcome};

        #[derive(Debug)]
        struct Ev {
            n: i64,
        }
        #[derive(Debug)]
        struct Sink {
            total: AtomicI64,
        }
        impl Bean for Sink {}
        struct SinkProv(Descriptor);
        impl Provider for SinkProv {
            fn descriptor(&self) -> &Descriptor {
                &self.0
            }
            fn provide<'a>(&'a self, _cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, LeafError>> {
                Box::pin(async { Ok(Published::shared_value(Sink { total: AtomicI64::new(0) })) })
            }
        }
        fn sink_adapter<'a>(
            host: ErasedBean,
            event: &'a (dyn Any + Send + Sync),
        ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
            Box::pin(async move {
                let h = host.downcast::<Sink>().expect("Sink");
                let e = event.downcast_ref::<Ev>().expect("Ev");
                h.total.fetch_add(e.n, Ordering::SeqCst);
                Ok(ListenerOutcome::None)
            })
        }

        let d = desc("sink", "t::Sink", TypeId::of::<Sink>());
        let mut builder = RegistryBuilder::new();
        builder.register(d, Arc::new(SinkProv(d))).unwrap();
        let listener = ListenerDescriptor {
            host: ContractId::of("t::Sink"),
            event_type: TypeId::of::<Ev>(),
            supports: None,
            order: OrderKey::implicit(),
            condition: None,
            chains: true,
            adapter: sink_adapter,
        };
        let unit = RunUnit::from_builder(builder).unwrap().with_listeners(vec![listener]);
        let unit = block(unit.refresh()).expect("refresh installs the publisher");

        let publisher = unit.publisher().expect("the publisher is live after refresh");
        assert_eq!(publisher.listener_count::<Ev>(), 1, "the listener bound to its host");
        let outcome = block(publisher.publish(Ev { n: 11 }));
        assert!(outcome.is_completed());
        let sink = block(unit.context().get::<Sink>()).unwrap();
        assert_eq!(sink.total.load(Ordering::SeqCst), 11, "the bound listener fired");
    }

    // ── R6: refresh registers + arms a scheduler; shutdown disarms it ──
    #[test]
    fn refresh_registers_and_arms_the_scheduler_and_shutdown_disarms() {
        use crate::scheduling::ScheduledPairing;
        use leaf_core::{MethodKey, ScheduledMethodDescriptor, Trigger, TriggerSpec};

        #[derive(Default)]
        struct FakeScheduler {
            registered: AtomicUsize,
            armed: Mutex<Vec<&'static str>>,
        }
        impl SchedulerCore for FakeScheduler {
            fn register(
                &self,
                _d: ScheduledMethodDescriptor,
                _t: Box<dyn Trigger>,
                _b: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
            ) -> Result<(), LeafError> {
                self.registered.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            fn arm(&self) -> BoxFuture<'_, Result<(), LeafError>> {
                Box::pin(async {
                    self.armed.lock().unwrap().push("arm");
                    Ok(())
                })
            }
            fn disarm(&self) -> BoxFuture<'_, ()> {
                Box::pin(async {
                    self.armed.lock().unwrap().push("disarm");
                })
            }
        }

        let scheduler = Arc::new(FakeScheduler::default());
        let task = ScheduledPairing::new(
            ScheduledMethodDescriptor::new(
                ContractId::of("t::Worker"),
                MethodKey::of("t::Worker::tick"),
                TriggerSpec::FixedRate {
                    period: std::time::Duration::from_secs(1),
                    initial_delay: std::time::Duration::ZERO,
                },
            ),
            Box::new(|| Box::pin(async {})),
        );

        let unit = RunUnit::from_builder(RegistryBuilder::new())
            .unwrap()
            .with_scheduler(scheduler.clone() as Arc<dyn SchedulerCore>)
            .with_scheduled(vec![task]);
        let unit = block(unit.refresh()).expect("refresh registers + arms");
        assert_eq!(scheduler.registered.load(Ordering::SeqCst), 1, "the task registered at R6");
        assert_eq!(*scheduler.armed.lock().unwrap(), vec!["arm"], "the wheel armed at the barrier");

        let report = block(unit.shutdown());
        assert_eq!(report.run_state, RunState::Closed);
        // Teardown step 1 disarms FIRST.
        assert_eq!(*scheduler.armed.lock().unwrap(), vec!["arm", "disarm"]);
    }

    // ── R6: a scheduled task with NO scheduler HARD-FAILS at R2 ──
    #[test]
    fn scheduled_task_without_a_scheduler_hard_fails() {
        use crate::scheduling::ScheduledPairing;
        use leaf_core::{MethodKey, ScheduledMethodDescriptor, TriggerSpec};
        let task = ScheduledPairing::new(
            ScheduledMethodDescriptor::new(
                ContractId::of("t::Worker"),
                MethodKey::of("t::Worker::tick"),
                TriggerSpec::FixedRate {
                    period: std::time::Duration::from_secs(1),
                    initial_delay: std::time::Duration::ZERO,
                },
            ),
            Box::new(|| Box::pin(async {})),
        );
        let unit = RunUnit::from_builder(RegistryBuilder::new()).unwrap().with_scheduled(vec![task]);
        let err = block(unit.refresh()).expect_err("no scheduler for a scheduled task");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }
}
