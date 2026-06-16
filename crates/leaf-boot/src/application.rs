//! The opinionated `run()` pipeline (bootstrap-diagnostics phase3/14) — the
//! `SpringApplication` analogue, the THIRD orchestration layer over `Context`
//! (ADR-05 line 47), the `::leaf_boot::Application` that `#[leaf::main]` targets.
//!
//! [`Application`] walks the FIXED `App<Define → Resolve → Wired → Running>`
//! typestate (there is NO parallel `RunPhase` enum — the typestate IS the state
//! machine), firing the named run-event sequence at each transition:
//!
//! ```text
//! Starting → EnvironmentPrepared → ContextInitialized → Prepared
//!          → Refreshed → Started → Liveness=Correct → [runners]
//!          → Ready → Readiness=AcceptingTraffic   |   Failed
//! ```
//!
//! The FIRST four facts fire on the upstream early-event buffer (here a buffer
//! drained at the multicaster install); `Refreshed`/`Started`/`Ready` fire on the
//! live [`EventPublisher`](crate::EventPublisher) after `Context::refresh()`. The
//! runners run in the precise readiness-gate window (after `Started`+Liveness,
//! BEFORE `Ready`+Readiness). On any fault [`handle_run_failure`](Application)
//! routes through the [`FAILURE_ANALYZERS`](leaf_core::FAILURE_ANALYZERS) chain +
//! the one [`Diagnostic`](leaf_core::Diagnostic) renderer + the
//! [`ErrorKind::exit_code`](leaf_core::ErrorKind) coordinator.
//!
//! This RESOLVES the cross-crate run NOTE the macros left
//! (`leaf-codegen/src/app.rs`: `#[leaf::main]` emits
//! `::leaf_boot::Application::new(Primary).run(...)` — the run ENGINE is here).

use std::process::ExitCode;
use std::sync::Arc;

use leaf_core::{
    analyze_first, cmp_order, AnalysisCtx, ApplicationArguments, BannerMode, BeanKey, CandidateRole,
    CreatorPolicy, Diagnostic, Env, EarlyListener, ErasedBean, FailureAnalyzer, FailureAnalysis,
    InjectionPlan, LeafError, LifecyclePlan, OrderKey, RenderStyle, Runner, RunMilestone,
    SchedulerCore, SourceTag, Spawner,
};

use crate::app::App;
use crate::assembly::{self, SeedPairing};
use crate::autoconfig::{AutoConfigCandidate, ExclusionSet};
use crate::conditions::GuardPairing;
use crate::environment::SealInputs;
use crate::lifecycle::RunUnit;
use crate::proxy::{build_join_points, AdvisorPairing, JoinPointPairing, MethodTablePairing};
use crate::scheduling::{CronTriggerFactory, ScheduledPairing};
use crate::validate::{ConfigBean, ConfigPairing, ValidationInputs};

type PlanResolver = Arc<dyn Fn(leaf_core::BeanId) -> LifecyclePlan + Send + Sync>;
type InjectionResolver = Arc<dyn Fn(leaf_core::BeanId) -> InjectionPlan + Send + Sync>;

/// A macro-emitted runner UPCAST thunk: downcast a resolved [`ErasedBean`] (the
/// origin-agnostic `Arc<dyn Any>` the registry's `dyn Runner` candidate view yields,
/// whose declared upcast is the identity re-erase) to the concrete runner type and
/// re-wrap it as `Arc<dyn Runner>`. `None` iff the bean is not that concrete type
/// (a different `dyn Runner` candidate matched the same view).
///
/// This is the "per-runner thunk the macro emits" the design names: an `ErasedBean`
/// cannot carry a `dyn Runner` vtable, so auto-collection JOINs each `dyn Runner`
/// candidate bean to this thunk by `ContractId` to recover a callable handle.
pub type RunnerUpcast = fn(ErasedBean) -> Option<Arc<dyn Runner>>;

// ──────────────────────────────── RunOverlay ────────────────────────────────

/// `withHook`/`Abandoned` carried as DATA (never an ambient thread-local read
/// across `.await`, the §2.3 hazard the toolkit dissolves by passing data).
#[derive(Default)]
pub struct RunOverlay {
    /// A single-run early listener (the `withHook` analogue, e.g. for tests).
    pub hook: Option<Box<dyn EarlyListener>>,
    /// `true` => an abandoned run: a fault rethrows WITHOUT analysis/close.
    pub abandoned: bool,
}

impl RunOverlay {
    /// The empty overlay (no hook, not abandoned) — the common path.
    #[must_use]
    pub fn none() -> Self {
        RunOverlay::default()
    }
}

impl std::fmt::Debug for RunOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunOverlay")
            .field("has_hook", &self.hook.is_some())
            .field("abandoned", &self.abandoned)
            .finish()
    }
}

// ──────────────────────────────── RunnerPairing ──────────────────────────────

/// The macro→runtime RUNNER JOIN row (the bootstrap analogue of
/// [`SeedPairing`](crate::SeedPairing)): pairs a `#[runner]` bean's IDENTITY (its
/// `ContractId`) with the macro-emitted [`RunnerUpcast`] thunk that recovers a
/// callable `Arc<dyn Runner>` from the resolved [`ErasedBean`].
///
/// The run pipeline AUTO-COLLECTS runner beans from the live `Context` by the
/// `dyn Runner` candidate view (see [`runner_candidate_ids`]), JOINs each to its
/// `RunnerPairing` by `ContractId`, resolves the erased bean, and upcasts it — so a
/// `#[runner]` bean runs automatically with NO explicit
/// [`with_runner`](Application::with_runner).
#[derive(Clone, Copy)]
pub struct RunnerPairing {
    /// The runner bean's stable identity (the JOIN key against the frozen registry).
    pub contract: leaf_core::ContractId,
    /// The macro-emitted upcast thunk (`ErasedBean` → `Arc<dyn Runner>`).
    pub upcast: RunnerUpcast,
    /// The runner's stream order (lower-value-first; the `cmp_order` sort key).
    pub order: OrderKey,
}

impl RunnerPairing {
    /// Build a runner pairing at the implicit (declaration) order.
    #[must_use]
    pub fn new(contract: leaf_core::ContractId, upcast: RunnerUpcast) -> Self {
        RunnerPairing { contract, upcast, order: OrderKey::implicit() }
    }

    /// Build a runner pairing at an explicit stream order.
    #[must_use]
    pub fn with_order(contract: leaf_core::ContractId, upcast: RunnerUpcast, order: OrderKey) -> Self {
        RunnerPairing { contract, upcast, order }
    }
}

impl std::fmt::Debug for RunnerPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerPairing")
            .field("contract", &self.contract)
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

// ──────────────────────────── RunFailure / RunningApp ────────────────────────

/// A boot failure: the failing phase + the one [`LeafError`] (the failing phase
/// recorded in its chain) + the rendered [`FailureAnalysis`], if an analyzer
/// applied. Impls [`std::process::Termination`] for the explicit caller-owned
/// exit boundary — leaf NEVER calls `process::exit` internally.
#[derive(Debug)]
pub struct RunFailure {
    /// The milestone the run reached before the fault.
    pub phase: RunMilestone,
    /// The fault (its chain records the failing phase).
    pub error: LeafError,
    /// The teachable analysis, if a [`FailureAnalyzer`] applied.
    pub analysis: Option<FailureAnalysis>,
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "run failed at {}: {}", self.phase.slug(), self.error)
    }
}

impl std::error::Error for RunFailure {}

impl std::process::Termination for RunFailure {
    fn report(self) -> ExitCode {
        // Exit code = ErrorKind::exit_code() (ADR-12; no ExitCodeExceptionMapper).
        ExitCode::from(self.error.exit_code().clamp(0, 255) as u8)
    }
}

/// A successfully-running application: the live [`RunUnit`] over one `Context`.
/// Impls [`std::process::Termination`] (success = code 0 unless an exit-code
/// contributor / override raises it).
#[derive(Debug)]
pub struct RunningApp {
    /// The live run unit (the `Context` + the watch cells + the teardown drain).
    unit: RunUnit,
    /// The computed exit code (highest-magnitude over the success-path contributors).
    exit_code: i32,
}

impl RunningApp {
    /// The live run unit (the `Context` façade + availability + teardown).
    #[must_use]
    pub fn unit(&self) -> &RunUnit {
        &self.unit
    }

    /// The live [`Context`](leaf_core::Context) façade.
    #[must_use]
    pub fn context(&self) -> &leaf_core::Context {
        self.unit.context()
    }

    /// The exit code coordinator's computed code (the success-path fold).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// `true` iff `bean` was AUTOMATICALLY advised by the R4 auto-proxy install (its
    /// interceptor chain was built over the published singleton).
    #[must_use]
    pub fn is_advised(&self, bean: leaf_core::BeanId) -> bool {
        self.unit.proxies().is_some_and(|p| p.is_advised(bean))
    }

    /// TRANSPARENTLY invoke an advised method through the AUTO-INSTALLED interceptor
    /// chain (the R4 after_init proxy): route a call to `method` on the advised
    /// singleton `bean` through its chain, terminating in the macro-emitted
    /// [`MethodTable`](leaf_core::MethodTable) downcast thunk — so a `#[advisable]`
    /// bean's method is advised with NO hand-written `Call`/`Tail` in user code.
    ///
    /// # Errors
    /// An [`AdviceError`](leaf_core::AdviceError) if the bean is not advised / has no
    /// method table, the method is absent, the singleton is not published, or any
    /// interceptor / the real method faults.
    pub async fn invoke_advised(
        &self,
        bean: leaf_core::BeanId,
        method: leaf_core::MethodKey,
        args: leaf_core::ErasedArgs,
    ) -> Result<leaf_core::ErasedRet, leaf_core::AdviceError> {
        let proxies = self.unit.proxies().ok_or(leaf_core::AdviceError::DowncastMismatch { method })?;
        let engine = self.unit.context().engine();
        proxies.invoke(engine.registry(), engine, bean, method, args).await
    }

    /// The `App::exit()` analogue: drive `Context::shutdown()` (drain the ledger
    /// LIFO after `stop_all`), then return the computed [`ExitCode`].
    pub async fn exit(self) -> ExitCode {
        let _report = self.unit.shutdown().await;
        ExitCode::from(self.exit_code.clamp(0, 255) as u8)
    }

    /// Shut down the live context (the explicit awaited teardown drain), returning
    /// the [`ShutdownReport`](crate::ShutdownReport).
    pub async fn shutdown(&self) -> crate::ShutdownReport {
        self.unit.shutdown().await
    }
}

impl std::process::Termination for RunningApp {
    fn report(self) -> ExitCode {
        ExitCode::from(self.exit_code.clamp(0, 255) as u8)
    }
}

// ──────────────────────────────── Application ────────────────────────────────

/// The opinionated bootstrap application (the `SpringApplication` analogue).
///
/// A real annotated app needs NOTHING but `Application::new().run()`: [`run`](Application::run)
/// AUTO-COLLECTS every per-bean wiring channel from its macro-emitted `linkme`
/// distributed slice (seeds, guards, join-points, method-tables, runner-upcasts,
/// config-binds, injection-plans, advisors — plus the `COMPONENTS`/`AUTO_CONFIGS`
/// bean channels lifted by [`from_slices`](crate::from_slices)) and JOINs each by
/// `ContractId` (the `collect_from_slices` fold at the head of `run`).
///
/// The `.with_*` builder methods STAY as explicit ESCAPE HATCHES (charter §2.10) that
/// ADD TO / OVERRIDE the slice-collected set per `ContractId` — they are NOT required
/// for a normal annotated app. The defaults make a minimal app work with no tables.
pub struct Application {
    seeds: Vec<SeedPairing>,
    guards: Vec<GuardPairing>,
    autoconfig: Vec<AutoConfigCandidate>,
    exclusions: ExclusionSet,
    advisors: Vec<AdvisorPairing>,
    join_points: Vec<JoinPointPairing>,
    method_tables: Vec<MethodTablePairing>,
    creator_policy: CreatorPolicy,
    config_pairings: Vec<ConfigPairing>,
    listeners: Vec<leaf_core::ListenerDescriptor>,
    dispatch_chain: Vec<Arc<dyn leaf_core::DispatchInterceptor>>,
    scheduled: Vec<ScheduledPairing>,
    runners: Vec<Arc<dyn leaf_core::Runner>>,
    runner_beans: Vec<RunnerPairing>,
    exit_code_contributors: Vec<i32>,
    plan_of: PlanResolver,
    inj_of: InjectionResolver,
    spawner: Option<Arc<dyn Spawner>>,
    scheduler: Option<Arc<dyn SchedulerCore>>,
    cron_factory: Option<CronTriggerFactory>,
    analyzers: Vec<Box<dyn FailureAnalyzer>>,
    banner_mode_override: Option<BannerMode>,
    app_name: &'static str,
    inventory: Vec<(std::any::TypeId, CandidateRole)>,
    expected_sources: Vec<SourceTag>,
}

impl Application {
    /// Begin building an application (empty tables, default resolvers).
    #[must_use]
    pub fn new() -> Self {
        Application {
            seeds: Vec::new(),
            guards: Vec::new(),
            autoconfig: Vec::new(),
            exclusions: ExclusionSet::new(),
            advisors: Vec::new(),
            join_points: Vec::new(),
            method_tables: Vec::new(),
            // Application aspects are admitted by default (the run() pipeline is the
            // binary/app-root where the full enabled-feature set is visible — the
            // @EnableAspectJAutoProxy analogue assembled here, never a racing bean).
            creator_policy: CreatorPolicy::ALL,
            config_pairings: Vec::new(),
            listeners: Vec::new(),
            dispatch_chain: Vec::new(),
            scheduled: Vec::new(),
            runners: Vec::new(),
            runner_beans: Vec::new(),
            exit_code_contributors: Vec::new(),
            plan_of: Arc::new(|_| LifecyclePlan::EMPTY),
            inj_of: Arc::new(|_| InjectionPlan::EMPTY),
            spawner: None,
            scheduler: None,
            cron_factory: None,
            analyzers: Vec::new(),
            banner_mode_override: None,
            app_name: "application",
            inventory: Vec::new(),
            expected_sources: Vec::new(),
        }
    }

    /// The `(self_type, role)` inventory of user/plain beans the auto-config
    /// back-off probe reads (so a user `@Component` supersedes a `Fallback`
    /// default). `#[leaf::main]` emits it from the lifted descriptors.
    #[must_use]
    pub fn with_inventory(mut self, inventory: Vec<(std::any::TypeId, CandidateRole)>) -> Self {
        self.inventory = inventory;
        self
    }

    /// ESCAPE HATCH — ADD seed pairings to the slice-collected `SEED_PAIRINGS` set
    /// (a seed for the SAME `ContractId` OVERRIDES the slice row). A normal annotated
    /// app needs none of these: `#[component]`/`#[bean]`/… auto-collect their seeds.
    #[must_use]
    pub fn with_seeds(mut self, seeds: Vec<SeedPairing>) -> Self {
        self.seeds = seeds;
        self
    }

    /// The condition guard JOIN table (the runtime-tier `CondExpr` leaves).
    #[must_use]
    pub fn with_guards(mut self, guards: Vec<GuardPairing>) -> Self {
        self.guards = guards;
        self
    }

    /// The auto-config candidate set (the `exclude > back-off > default` ladder).
    #[must_use]
    pub fn with_autoconfig(mut self, candidates: Vec<AutoConfigCandidate>) -> Self {
        self.autoconfig = candidates;
        self
    }

    /// The auto-config exclusion set (`leaf.autoconfigure.exclude`).
    #[must_use]
    pub fn with_exclusions(mut self, exclusions: ExclusionSet) -> Self {
        self.exclusions = exclusions;
        self
    }

    /// ESCAPE HATCH — ADD advisors to the slice-collected `ADVISOR_PAIRINGS` set (the
    /// R4 auto-proxy `after_init` install; an advisor for the SAME `ContractId`
    /// OVERRIDES the slice row). A normal app needs none: an `#[aspect]` auto-collects
    /// its live advisor (pointcut + make_interceptor). Use this for an advisor whose
    /// pointcut/interceptor is not const-emittable (a hand-built `&'static dyn Pointcut`).
    #[must_use]
    pub fn with_advisors(mut self, advisors: Vec<AdvisorPairing>) -> Self {
        self.advisors = advisors;
        self
    }

    /// The per-bean join-point JOIN table (the macro-emitted `__leaf_joinpoints_<Ident>`
    /// `BeanJoinPointsSpec` consts), keyed by `ContractId`. `frozen_proxy_plan` JOINs
    /// each to its frozen `BeanId` and runs the advisors' pointcuts over it — so the
    /// proxy plan is built from REAL macro-emitted per-bean data.
    #[must_use]
    pub fn with_join_points(mut self, join_points: Vec<JoinPointPairing>) -> Self {
        self.join_points = join_points;
        self
    }

    /// The per-bean method-table JOIN table (the macro-emitted `__leaf_methods_<Ident>`
    /// `&'static MethodTable` consts), keyed by `ContractId`. The run pipeline threads
    /// them through the R4 auto-proxy install so an advised call routes through the
    /// auto-installed interceptor chain TRANSPARENTLY (via
    /// [`RunningApp::invoke_advised`]) — no hand-written `Call`/`Tail` in user code.
    #[must_use]
    pub fn with_method_tables(mut self, tables: Vec<MethodTablePairing>) -> Self {
        self.method_tables = tables;
        self
    }

    /// Override the auto-proxy [`CreatorPolicy`] capability lattice (the
    /// `@EnableAspectJAutoProxy` analogue; defaults to admitting application aspects).
    #[must_use]
    pub fn with_creator_policy(mut self, policy: CreatorPolicy) -> Self {
        self.creator_policy = policy;
        self
    }

    /// The `@ConfigurationProperties` bind-thunk JOIN table (the macro-emitted
    /// `__leaf_config_bind_<Ident>` `ConfigBindThunk` consts), keyed by `ContractId`.
    /// `App<Wired>::validate` JOINs each to its frozen `BeanId` and threads it as the
    /// REAL C2 Tier-2 [`ConfigBean`](crate::ConfigBean) bind thunk (pre-materializing
    /// the config bean into its slot before refresh) — never a hand-mirrored thunk.
    #[must_use]
    pub fn with_config_properties(mut self, config_pairings: Vec<ConfigPairing>) -> Self {
        self.config_pairings = config_pairings;
        self
    }

    /// The event-listener descriptors (the R3 multicaster install).
    #[must_use]
    pub fn with_listeners(mut self, listeners: Vec<leaf_core::ListenerDescriptor>) -> Self {
        self.listeners = listeners;
        self
    }

    /// The dispatch-interceptor chain composed into the R3 multicaster.
    #[must_use]
    pub fn with_dispatch_chain(
        mut self,
        chain: Vec<Arc<dyn leaf_core::DispatchInterceptor>>,
    ) -> Self {
        self.dispatch_chain = chain;
        self
    }

    /// The `#[scheduled]` task JOIN table (the R6 scheduler binding).
    #[must_use]
    pub fn with_scheduled(mut self, tasks: Vec<ScheduledPairing>) -> Self {
        self.scheduled = tasks;
        self
    }

    /// A runner (run sequentially in the readiness-gate window). Multiple calls
    /// accumulate; the run sorts them in registration order (the binary supplies
    /// the `cmp_order`-sorted stream).
    #[must_use]
    pub fn with_runner(mut self, runner: Arc<dyn leaf_core::Runner>) -> Self {
        self.runners.push(runner);
        self
    }

    /// The `#[runner]` bean JOIN table (the macro-emitted [`RunnerPairing`] upcast
    /// thunks), keyed by `ContractId`. The run pipeline AUTO-COLLECTS the live
    /// `dyn Runner` candidate beans from the refreshed `Context`, JOINs each to its
    /// pairing, upcasts the resolved bean, and runs them in the readiness-gate window
    /// (ordered by [`cmp_order`](leaf_core::cmp_order)) — so a `#[runner]` bean runs
    /// automatically with NO explicit [`with_runner`](Application::with_runner).
    #[must_use]
    pub fn with_runner_beans(mut self, runners: Vec<RunnerPairing>) -> Self {
        self.runner_beans = runners;
        self
    }

    /// A success-path exit-code contributor (the highest-magnitude fold).
    #[must_use]
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.exit_code_contributors.push(code);
        self
    }

    /// The macro-emitted per-bean [`LifecyclePlan`] resolver.
    #[must_use]
    pub fn with_plan_resolver(
        mut self,
        resolver: impl Fn(leaf_core::BeanId) -> LifecyclePlan + Send + Sync + 'static,
    ) -> Self {
        self.plan_of = Arc::new(resolver);
        self
    }

    /// The macro-emitted per-bean [`InjectionPlan`] resolver.
    #[must_use]
    pub fn with_injection_plans(
        mut self,
        resolver: impl Fn(leaf_core::BeanId) -> InjectionPlan + Send + Sync + 'static,
    ) -> Self {
        self.inj_of = Arc::new(resolver);
        self
    }

    /// The bootstrap [`Spawner`] (the Background eager lane + the scheduler body
    /// executor).
    #[must_use]
    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// The container-owned [`SchedulerCore`](leaf_core::SchedulerCore).
    #[must_use]
    pub fn with_scheduler(mut self, scheduler: Arc<dyn SchedulerCore>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// The cron-trigger factory (the leaf-cron force-link seam).
    #[must_use]
    pub fn with_cron_factory(mut self, factory: CronTriggerFactory) -> Self {
        self.cron_factory = Some(factory);
        self
    }

    /// A programmatic [`FailureAnalyzer`] (the escape hatch over the
    /// `FAILURE_ANALYZERS` slice).
    #[must_use]
    pub fn add_failure_analyzer(mut self, analyzer: Box<dyn FailureAnalyzer>) -> Self {
        self.analyzers.push(analyzer);
        self
    }

    /// Override the banner mode (else read from the bound `BootstrapSettings`).
    #[must_use]
    pub fn with_banner_mode(mut self, mode: BannerMode) -> Self {
        self.banner_mode_override = Some(mode);
        self
    }

    /// The application name (banner / diagnostics).
    #[must_use]
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.app_name = name;
        self
    }

    /// The binary's `ExpectedManifest` — the [`SourceTag`] set the run pipeline's
    /// anti-DCE self-check joins against the link-collected
    /// [`SOURCES`](leaf_core::SOURCES) slice at the `Define→Resolve` edge (ADR-09
    /// Defense MANIFEST, bootstrap-diagnostics phase3/14).
    ///
    /// Each expected source that contributed ZERO rows to `SOURCES` (a real DCE drop,
    /// or a misconfigured toolchain that never force-linked the crate) becomes a LOUD
    /// [`AntiDceError::SourceVanished`](crate::AntiDceError) naming the crate, instead
    /// of a confusing silent empty registry later. An empty manifest (the default)
    /// always passes — there is nothing to compare against.
    ///
    /// The umbrella feeds this from `leaf::expected_manifest()` (the force-linked
    /// participating set its enabled capability features know); each PARTICIPATING
    /// crate contributes its tag via [`leaf_core::declare_source!`]. The binary crate
    /// is NOT included — it IS the final link unit, so its own rows cannot vanish
    /// independently of the whole binary.
    #[must_use]
    pub fn with_expected_sources(mut self, sources: Vec<SourceTag>) -> Self {
        self.expected_sources = sources;
        self
    }

    /// AUTO-COLLECT every per-bean wiring pairing from its `linkme` distributed slice
    /// and FOLD it into the run tables (discovery-codegen phase3/02): the macro emits
    /// one pairing row per bean beside its `Descriptor`, `linkme` auto-collects them at
    /// link time exactly like `COMPONENTS`/`AUTO_CONFIGS`, and this folds each channel
    /// in so a normal annotated app wires itself with NO hand-assembled `.with_*`.
    ///
    /// Every channel: seeds (`SEED_PAIRINGS`), guards (`GUARD_PAIRINGS`), join-points
    /// (`JOINPOINT_PAIRINGS`), method-tables (`METHOD_TABLE_PAIRINGS`), runner-upcasts
    /// (`RUNNER_PAIRINGS`), config-binds (`CONFIG_BIND_PAIRINGS`), advisors
    /// (`ADVISOR_PAIRINGS`); the injection plans (`INJECTION_PLAN_PAIRINGS`) fold in
    /// through [`Application::auto_injection_resolver`] keyed by `ContractId`.
    /// (`COMPONENTS`/`AUTO_CONFIGS` themselves already auto-collect inside
    /// [`from_slices`](crate::from_slices).)
    ///
    /// The explicit `.with_*` builder calls STAY as escape hatches (charter §2.10) that
    /// ADD TO / OVERRIDE the slice-collected set: a slice row is the BASE, and an
    /// explicit pairing for the SAME `ContractId` overrides it (so a binary can still
    /// hand-tune a single bean's wiring). The fold is idempotent + order-independent.
    fn collect_from_slices(&mut self) {
        // seeds — explicit overrides slice on a ContractId collision. There is exactly
        // ONE seed row per contract (field-default, `#[bean]`, or referenced-constructor),
        // so the slice base folds straight in; a genuine double-emit re-surfaces as the
        // loud `slice_seed_index` build-seam error downstream.
        self.seeds = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS)
                .into_iter()
                .map(|r| SeedPairing::new(r.contract, r.seed)),
            std::mem::take(&mut self.seeds),
            |p| p.contract,
        );

        // condition guards (the gated-element CondExpr leaves). The slice row carries no
        // `self_type` (the report's secondary key); leaf-boot recovers it from the
        // frozen Descriptor, so `None` here is sound.
        self.guards = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::GUARD_PAIRINGS)
                .into_iter()
                .map(|r| GuardPairing::new(r.contract, None, r.guard)),
            std::mem::take(&mut self.guards),
            |p| p.contract,
        );

        // per-bean join-points (the ProxyPlan pointcut input).
        self.join_points = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::JOINPOINT_PAIRINGS)
                .into_iter()
                .map(|r| JoinPointPairing::new(r.contract, r.spec)),
            std::mem::take(&mut self.join_points),
            |p| p.contract,
        );

        // per-bean method tables (the transparent downcast-thunk index).
        self.method_tables = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::METHOD_TABLE_PAIRINGS)
                .into_iter()
                .map(|r| MethodTablePairing::new(r.contract, r.table)),
            std::mem::take(&mut self.method_tables),
            |p| p.contract,
        );

        // #[runner] beans (the upcast thunks, ordered by the row's OrderKey).
        self.runner_beans = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::RUNNER_PAIRINGS)
                .into_iter()
                .map(|r| RunnerPairing::with_order(r.contract, r.upcast, r.order)),
            std::mem::take(&mut self.runner_beans),
            |p| p.contract,
        );

        // @ConfigurationProperties bind thunks (the C2 Tier-2 pre-materialize recipe).
        self.config_pairings = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::CONFIG_BIND_PAIRINGS)
                .into_iter()
                .map(|r| ConfigPairing::new(r.contract, r.thunk)),
            std::mem::take(&mut self.config_pairings),
            |p| p.contract,
        );

        // advisors (the #[aspect] live advice: pointcut + make_interceptor bean bridge).
        self.advisors = merge_by_contract(
            leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS).into_iter().map(|r| {
                AdvisorPairing::new(r.contract, r.order, r.role, r.pointcut, r.make_interceptor)
            }),
            std::mem::take(&mut self.advisors),
            |p| p.contract,
        );

        // auto-config candidates (the AUTO_CONFIGS channel, held back from from_slices
        // so the `exclude > back-off > default` ladder gates them — the FIX for the
        // force-linked-auto-config-guard-ignored gap). The slice-built candidates JOIN
        // each AUTO_CONFIGS Descriptor to its seed (the now-merged `self.seeds`) + its
        // guard (GUARD_PAIRINGS); an explicit `.with_autoconfig` candidate OVERRIDES a
        // slice one on a ContractId collision (the same escape-hatch merge). A seed/guard
        // JOIN miss is the same loud anti-DCE error `from_slices` surfaces.
        //
        // NOTE: a degraded fallback to the unmerged explicit set on the (impossible-in-a
        // -healthy-app) duplicate-seed error keeps `collect_from_slices` infallible — the
        // loud error re-surfaces at the `from_slices` call in `run_inner`, where the run
        // pipeline routes it through failure analysis.
        let slice_candidates =
            assembly::collect_autoconfig_candidates(&self.seeds).unwrap_or_default();
        self.autoconfig = merge_by_contract(
            slice_candidates,
            std::mem::take(&mut self.autoconfig),
            |c| c.descriptor.contract,
        );

        // The auto-config back-off seed-probe: the (self_type, role) of every COMPONENTS
        // bean from_slices registers (so the first candidate's OnMissingBean sees them),
        // UNDER the explicit `.with_inventory` set (a `#[leaf::main]`-supplied inventory
        // is additive — duplicates are harmless to the count-based probe).
        let mut inventory = assembly::component_seed_probe();
        inventory.append(&mut self.inventory);
        self.inventory = inventory;
    }

    /// The slice-collected per-bean [`InjectionPlan`] resolver: JOIN each
    /// [`INJECTION_PLAN_PAIRINGS`](leaf_core::INJECTION_PLAN_PAIRINGS) row to its frozen
    /// [`BeanId`](leaf_core::BeanId) by `ContractId` (via the now-frozen registry),
    /// COMPOSED UNDER the explicit `.with_injection_plans` resolver: the explicit
    /// resolver runs first and a non-[`EMPTY`](InjectionPlan::EMPTY) plan WINS; otherwise
    /// the slice-collected plan applies. So `#[component]`-emitted plans auto-collect
    /// while an explicit override still takes precedence.
    fn auto_injection_resolver(&self, registry: &leaf_core::Registry) -> InjectionResolver {
        // One injection-plan row per contract (field-default, `#[bean]`, or
        // referenced-constructor), JOINed to its frozen BeanId. The first row for a
        // BeanId wins (a second would be the same double-emit `slice_seed_index` rejects).
        let mut by_id: std::collections::HashMap<leaf_core::BeanId, InjectionPlan> =
            std::collections::HashMap::new();
        for row in leaf_core::collect_slice(&leaf_core::INJECTION_PLAN_PAIRINGS) {
            if let Some(id) = registry.by_contract(row.contract) {
                by_id.entry(id).or_insert(row.plan);
            }
        }
        let explicit = Arc::clone(&self.inj_of);
        Arc::new(move |id: leaf_core::BeanId| -> InjectionPlan {
            let from_explicit = explicit(id);
            if !from_explicit.points.is_empty() {
                return from_explicit;
            }
            by_id.get(&id).copied().unwrap_or(InjectionPlan::EMPTY)
        })
    }

    /// Run the FULL pipeline: the typestate walk + the named run-event sequence +
    /// runners in the readiness-gate window + banner + failure analysis.
    ///
    /// # Errors
    /// A [`RunFailure`] (the failing milestone + the one [`LeafError`] + the
    /// rendered analysis) if any phase faults.
    pub async fn run(
        mut self,
        args: SealInputs,
        overlay: RunOverlay,
    ) -> Result<RunningApp, RunFailure> {
        // AUTO-COLLECT every per-bean wiring channel from its linkme slice + fold it
        // into the run tables (the explicit `.with_*` escape hatches add/override).
        self.collect_from_slices();
        // Move the non-Clone refresh tables out of `self` so `run_inner` can borrow
        // `&self` for the rest of the pipeline while still feeding the RunUnit.
        let movable = MovableTables {
            listeners: std::mem::take(&mut self.listeners),
            dispatch_chain: std::mem::take(&mut self.dispatch_chain),
            scheduled: std::mem::take(&mut self.scheduled),
        };
        let mut phase = RunMilestone::Starting;
        match self.run_inner(args, &overlay, movable, &mut phase).await {
            Ok(app) => Ok(app),
            Err(error) => Err(self.handle_run_failure(phase, error, &overlay)),
        }
    }

    async fn run_inner(
        &self,
        args: SealInputs,
        overlay: &RunOverlay,
        movable: MovableTables,
        phase: &mut RunMilestone,
    ) -> Result<RunningApp, LeafError> {
        // (4) buffer-fire `Starting` on the early-event buffer (here: the overlay
        // hook, the single-run early listener carried as data, never a thread-local).
        *phase = RunMilestone::Starting;
        self.fire_early(overlay, RunMilestone::Starting);

        // (6) seal_environment IS EnvironmentPrepared (the 5f async fence).
        let app = App::<Define>::from_slices(&self.seeds)?;
        // self_check at the Define→Resolve edge (anti-DCE expected-vs-found): every
        // SourceTag the binary's ExpectedManifest names must appear in the
        // link-collected SOURCES, or it is a LOUD SourceVanished naming the crate. The
        // manifest is the force-linked participating set (umbrella capability features
        // + binary crate); a healthy app's force-linked crates each `declare_source!`,
        // so the JOIN finds them. An empty manifest (the bare app) trivially passes.
        App::<Define>::self_check(&self.expected_sources).map_err(LeafError::from)?;

        let mut app = app.seal_environment(args, self.inventory.clone()).await?;
        *phase = RunMilestone::EnvironmentPrepared;
        self.fire_early(overlay, RunMilestone::EnvironmentPrepared);

        // (7) print the banner from the bound settings (degrade-and-warn).
        let banner_mode = self.banner_mode_override.unwrap_or(app.settings().banner_mode);
        print_banner(app.env(), banner_mode, self.app_name);

        // (9) ContextInitialized + Prepared (no programmatic initializers in this
        // pipeline; the early-buffer facts fire at the transition).
        *phase = RunMilestone::ContextInitialized;
        self.fire_early(overlay, RunMilestone::ContextInitialized);

        // The parsed ApplicationArguments (the shared runner arg).
        let run_args = app.args().clone();

        // (10) route_conditions + prune unmatched components + run_autoconfig →
        // seal() → validate(). `from_slices` registered EVERY component
        // unconditionally; route_conditions evaluates each component's
        // `#[conditional]`/`#[profile]` guard and returns the contracts that MATCHED,
        // so a guarded-but-unmatched COMPONENT is pruned from the builder BEFORE the
        // seal freeze (the registry-level analogue of holding an auto-config back).
        // Auto-config contracts aren't in the builder (held back here too), so the
        // prune is a no-op for them — they are gated by run_autoconfig below.
        let matched = app.route_conditions(&self.guards)?;
        app.prune_unmatched_components(&self.guards, &matched);
        app.run_autoconfig(&self.autoconfig, &self.exclusions)?;
        let app = app.seal()?;
        *phase = RunMilestone::Prepared;
        self.fire_early(overlay, RunMilestone::Prepared);

        // The Tier-2 aggregated validation pass. The C2 config-properties beans are
        // PRE-MATERIALIZED here from the REAL macro-emitted bind thunks: JOIN each
        // ConfigPairing to its frozen BeanId (by ContractId) and thread it as a
        // ConfigBean so validate() binds + pre-binds the bean into its slot (so refresh
        // R5 publishes the bound Arc and never re-binds). The per-bean injection plans
        // also flow in so the whole-graph wiring check resolves every mandatory edge.
        let config_beans: Vec<ConfigBean<'_>> = self
            .config_pairings
            .iter()
            .filter_map(|p| p.to_config_bean(app.registry()))
            .collect();
        // The injection-plan resolver: the slice-collected per-bean plans (JOINed to
        // their frozen BeanIds by ContractId) UNDER the explicit `.with_injection_plans`
        // override (the registry is now frozen, so the ContractId→BeanId JOIN resolves).
        let inj_of = self.auto_injection_resolver(app.registry());
        let plan_lookup = {
            let inj_of = Arc::clone(&inj_of);
            move |id: leaf_core::BeanId| -> InjectionPlan { inj_of(id) }
        };
        let validation = ValidationInputs::new()
            .with_plans(&plan_lookup)
            .with_config_beans(&config_beans);
        app.validate(&validation)?;
        drop(config_beans);
        drop(plan_lookup);

        // (11) Context::refresh() — R0..R8. Refreshed/Started fire DURING via the
        // now-live EventPublisher; the runner window opens after.
        let unit = self.build_run_unit(app, movable, inj_of)?;
        let unit = unit.refresh().await?;
        *phase = RunMilestone::Refreshed;

        // (12) Started + Liveness=Correct already fired inside refresh R8.
        *phase = RunMilestone::Started;

        // (13) call_runners() in the readiness-gate window (after Started+Liveness,
        // BEFORE Ready+Readiness) — sequentially, abort on the first Err. The
        // `#[runner]` beans are AUTO-COLLECTED from the live Context here.
        self.call_runners(unit.context(), &run_args).await?;
        *phase = RunMilestone::RunnersInvoked;

        // (14) Ready: flip Readiness=AcceptingTraffic (the K8s readiness gate — the
        // `Ready` fact IS the AvailabilityChanged(Readiness) the watch cell wakes).
        unit.availability()
            .set_readiness(leaf_core::ReadinessState::AcceptingTraffic, "ready");
        *phase = RunMilestone::Ready;

        let exit_code = compute_exit_code(&self.exit_code_contributors);
        Ok(RunningApp { unit, exit_code })
    }

    /// Build the [`RunUnit`] from the frozen `App<Wired>` + the macro-emitted plans
    /// + the JOINed advisors/listeners/scheduled tables (the run-engine glue).
    ///
    /// `inj_of` is the resolver built in `run_inner` AFTER seal (the slice-collected
    /// per-bean plans JOINed by `ContractId` under the explicit override), threaded
    /// here so the run unit + the validate pass see the SAME plans.
    fn build_run_unit(
        &self,
        app: App<Wired>,
        movable: MovableTables,
        inj_of: InjectionResolver,
    ) -> Result<RunUnit, LeafError> {
        let proxy_plan = self.frozen_proxy_plan(app.registry())?;
        let (registry, env, settings) = app.into_run_parts();

        let plan_of = Arc::clone(&self.plan_of);
        let advisors: Vec<leaf_core::AdvisorDescriptor> =
            self.advisors.iter().map(advisor_descriptor).collect();

        let mut unit = RunUnit::over_engine(leaf_core::Engine::new(registry), env)
            .with_plan_resolver(move |id| plan_of(id))
            .with_injection_plans(move |id| inj_of(id))
            .with_proxy_plan(proxy_plan)
            .with_advisors(advisors)
            .with_method_tables(self.method_tables.clone())
            .with_listeners(movable.listeners)
            .with_dispatch_chain(movable.dispatch_chain)
            .with_scheduled(movable.scheduled)
            .with_shutdown_settings(settings.shutdown);

        if let Some(spawner) = self.spawner.as_ref() {
            unit = unit.with_spawner(Arc::clone(spawner));
        }
        if let Some(scheduler) = self.scheduler.as_ref() {
            unit = unit.with_scheduler(Arc::clone(scheduler));
        }
        if let Some(factory) = self.cron_factory.as_ref() {
            unit = unit.with_cron_factory(Arc::clone(factory));
        }
        Ok(unit)
    }

    /// Compute the frozen [`ProxyPlan`](leaf_core::ProxyPlan) over the advised beans
    /// (proxy-interception phase3/08): JOIN the macro-emitted per-bean
    /// [`JoinPointPairing`]s to their frozen `BeanId`s, reify each into the runtime
    /// [`BeanJoinPoints`](leaf_core::BeanJoinPoints), and run every admitted advisor's
    /// pointcut over them via [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze) —
    /// the O(1) `advisors_for` decoration table the R4 `after_init` install consumes.
    ///
    /// With no join-point pairings (a minimal app with no `#[advisable]` beans) this
    /// is the empty plan; the advisor descriptors still ride into the unit so the
    /// install resolves any plan-referenced advisor.
    ///
    /// # Errors
    /// A [`LeafError`] from [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze) (the
    /// match is pure; the `Result` is kept so the seam can grow loud faults).
    fn frozen_proxy_plan(
        &self,
        registry: &leaf_core::Registry,
    ) -> Result<leaf_core::ProxyPlan, LeafError> {
        // JOIN the macro-emitted join-point specs to the frozen registry (reifying
        // each const spec into the owned runtime BeanJoinPoints the freeze borrows).
        let reified = build_join_points(&self.join_points, registry);
        if reified.is_empty() {
            return Ok(leaf_core::ProxyPlan::empty());
        }
        let advisors: Vec<leaf_core::AdvisorDescriptor> =
            self.advisors.iter().map(advisor_descriptor).collect();
        let join_points: std::collections::HashMap<leaf_core::BeanId, leaf_core::BeanJoinPoints<'_>> =
            reified.iter().map(|r| (r.id(), r.view())).collect();
        leaf_core::ProxyPlan::freeze(&advisors, registry, &self.creator_policy, &join_points)
            // AssemblyError wraps the one LeafError spine (the match is pure today,
            // so this never fires; kept so the seam can grow loud faults).
            .map_err(|e| e.0)
    }

    /// Run the merged runner stream sequentially in the readiness-gate window, over
    /// the shared [`ApplicationArguments`]. Abort on the first `Err`.
    ///
    /// The stream is the union of (a) the explicit [`with_runner`](Application::with_runner)
    /// handles and (b) the `#[runner]` beans AUTO-COLLECTED from the live `Context`:
    /// every `dyn Runner` candidate bean (see [`runner_candidate_ids`]) JOINed to its
    /// [`RunnerPairing`] by `ContractId`, resolved + upcast through the macro-emitted
    /// thunk. The auto-collected beans are ordered by [`cmp_order`](leaf_core::cmp_order)
    /// (the `RunnerPairing.order` key, then `ContractId`) and run after the explicit
    /// handles. A candidate with no matching pairing is skipped (it is enumerable but
    /// has no callable upcast thunk).
    async fn call_runners(
        &self,
        context: &leaf_core::Context,
        args: &ApplicationArguments,
    ) -> Result<(), LeafError> {
        // (a) the explicit handles (registration order).
        for runner in &self.runners {
            runner.run(args).await?;
        }

        // (b) the auto-collected #[runner] beans from the live Context.
        let engine = context.engine();
        let registry = engine.registry();
        let by_contract: std::collections::HashMap<leaf_core::ContractId, &RunnerPairing> =
            self.runner_beans.iter().map(|p| (p.contract, p)).collect();

        // The dyn Runner candidate beans, JOINed to their pairings (ordered by
        // cmp_order — the RunnerPairing.order key, then the stable ContractId).
        let mut collected: Vec<(OrderKey, leaf_core::ContractId, RunnerUpcast)> = Vec::new();
        for id in runner_candidate_ids(registry) {
            let contract = registry.descriptor(id).contract;
            if let Some(pairing) = by_contract.get(&contract) {
                collected.push((pairing.order, contract, pairing.upcast));
            }
        }
        collected.sort_by(|(oa, ca, _), (ob, cb, _)| cmp_order(oa, ob).then(ca.cmp(cb)));

        for (_order, contract, upcast) in collected {
            let bean = engine.get_erased(BeanKey::ByContract(contract)).await?;
            let runner = upcast(bean).ok_or_else(|| runner_upcast_failed(contract))?;
            runner.run(args).await?;
        }
        Ok(())
    }

    /// Fire the single-run overlay hook for a milestone (the `withHook` data path;
    /// the EarlyListener body is sync-fired — the minimal pipeline carries
    /// milestones as the listener's notification). A hook fault is ISOLATED here
    /// (an early-listener uses `IsolateEach`, never aborting the run).
    fn fire_early(&self, overlay: &RunOverlay, milestone: RunMilestone) {
        if let Some(hook) = overlay.hook.as_ref() {
            let _ = hook.on_milestone(milestone);
        }
    }

    /// Route a fault: if abandoned, rethrow (no analysis/close); else fire `Failed`
    /// (implicit in the cancel-cascade that already ran inside refresh), run the
    /// `FAILURE_ANALYZERS` chain + the programmatic analyzers, render via
    /// [`Diagnostic`](leaf_core::Diagnostic), and return the [`RunFailure`].
    fn handle_run_failure(
        &self,
        phase: RunMilestone,
        error: LeafError,
        overlay: &RunOverlay,
    ) -> RunFailure {
        if overlay.abandoned {
            return RunFailure { phase, error, analysis: None };
        }
        // The slice analyzers (force-linked) + the programmatic ones.
        let slice = leaf_core::collect_slice(&leaf_core::FAILURE_ANALYZERS);
        let mut all: Vec<&dyn FailureAnalyzer> = slice.to_vec();
        for a in &self.analyzers {
            all.push(a.as_ref());
        }
        let ctx = AnalysisCtx::empty();
        let analysis = analyze_first(&all, &error, &ctx);
        // Render to stderr (the default Human reporter). The structured analysis
        // also rides back in the RunFailure for programmatic consumers.
        eprint!("{}", error.render_to_string(RenderStyle::Human));
        if let Some(a) = analysis.as_ref() {
            eprintln!("\n{}\n{}", a.description, a.action);
        }
        RunFailure { phase, error, analysis }
    }
}

impl Default for Application {
    fn default() -> Self {
        Application::new()
    }
}

impl std::fmt::Debug for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Application")
            .field("seeds", &self.seeds.len())
            .field("guards", &self.guards.len())
            .field("autoconfig", &self.autoconfig.len())
            .field("advisors", &self.advisors.len())
            .field("listeners", &self.listeners.len())
            .field("scheduled", &self.scheduled.len())
            .field("runners", &self.runners.len())
            .finish_non_exhaustive()
    }
}

// the typestate tags consumed in this module's signatures.
use crate::app::{Define, Wired};

/// The non-`Clone` refresh tables moved out of [`Application`] at the head of
/// [`Application::run`] so the rest of the pipeline can borrow `&self`.
struct MovableTables {
    listeners: Vec<leaf_core::ListenerDescriptor>,
    dispatch_chain: Vec<Arc<dyn leaf_core::DispatchInterceptor>>,
    scheduled: Vec<ScheduledPairing>,
}

// ────────────────────────────────── banner ──────────────────────────────────

/// Print the startup banner from the frozen [`Env`] + the bound [`BannerMode`].
///
/// The ONE deliberate fail-fast exception (charter §1.7): a banner failure NEVER
/// aborts a boot — it degrades to no-banner. leaf-figlet is a skeleton crate, so
/// this ships a self-contained default template; `${...}` placeholders
/// (`application.version` etc.) resolve against `env` best-effort.
pub fn print_banner(env: &Env, mode: BannerMode, app_name: &str) {
    match mode {
        BannerMode::Off => {}
        BannerMode::Console => {
            let version = leaf_core::PropertyResolver::get(env, "application.version")
                .map(|v| v.raw)
                .unwrap_or_else(|| "0.0.0".to_string());
            println!(":: {app_name} :: (v{version})");
        }
        BannerMode::Log => {
            // The log-mode banner reuses the diagnostics/tracing channel; here a
            // single info line to stderr (no logging backend is wired in core).
            eprintln!(":: {app_name} ::");
        }
    }
}

/// Enumerate the `dyn Runner` candidate beans in the frozen registry: every bean
/// whose `provides[]` declares the `dyn ::leaf_core::Runner` upcast view (what the
/// `#[runner]`/`#[component]`-implementing-`Runner` macro emits).
///
/// This is the design's "runners are ordinary beans resolved as a typed collection
/// from the frozen Context" (bootstrap-diagnostics phase3/14): the run pipeline
/// discovers runner beans by the `dyn Runner` candidate view (the cleanest
/// enumeration, per the design — no bespoke `RUNNERS` slice). The returned ids are
/// in the registry's deterministic candidate order.
///
/// [`Application::call_runners`](Application) consumes this for AUTO-COLLECTION: each
/// enumerated candidate is JOINed to its [`RunnerPairing`] by `ContractId`, the bean
/// is resolved as an [`ErasedBean`] (which is `Arc<dyn Any>` and cannot carry a
/// `dyn Runner` vtable), and the pairing's macro-emitted [`RunnerUpcast`] thunk
/// recovers the callable `Arc<dyn Runner>` — the concrete→trait upcast the design
/// names. Enumeration here is the discovery primitive; the pairing is the upcast.
#[must_use]
pub fn runner_candidate_ids(registry: &leaf_core::Registry) -> Vec<leaf_core::BeanId> {
    registry
        .candidates(std::any::TypeId::of::<dyn Runner>())
        .to_vec()
}

/// Reify one [`AdvisorPairing`] into the live [`AdvisorDescriptor`](leaf_core::AdvisorDescriptor)
/// the [`ProxyPlan`](leaf_core::ProxyPlan) freezes / the R4 install resolves (shared
/// by `frozen_proxy_plan` and `build_run_unit`).
fn advisor_descriptor(p: &AdvisorPairing) -> leaf_core::AdvisorDescriptor {
    leaf_core::AdvisorDescriptor {
        id: p.contract,
        order: p.order,
        role: p.role,
        pointcut: p.pointcut,
        make_interceptor: p.make_interceptor,
    }
}

/// A runner bean's macro-emitted upcast thunk returned `None` (the resolved bean
/// was not the concrete runner type the pairing names — a JOIN-table mismatch).
fn runner_upcast_failed(contract: leaf_core::ContractId) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "auto-collecting #[runner] beans",
        format!(
            "the runner bean {contract:?} resolved, but its RunnerPairing upcast thunk did not \
             recover an `Arc<dyn Runner>` (the macro-emitted thunk names a different concrete type \
             than the resolved bean)"
        ),
    ))
}

/// Fold the slice-collected `base` pairings UNDER the explicit `over` pairings, keyed
/// by `ContractId` (the `key` projection): the `base` (slice) rows are the foundation,
/// and an explicit `over` row for the SAME `ContractId` OVERRIDES its `base` row (the
/// `.with_*` escape-hatch semantics — charter §2.10). The result is ordering-stable
/// (base rows first in their slice order, then any explicit-only rows) and total.
fn merge_by_contract<T, K>(
    base: impl IntoIterator<Item = T>,
    over: Vec<T>,
    key: K,
) -> Vec<T>
where
    K: Fn(&T) -> leaf_core::ContractId,
{
    let overridden: std::collections::HashSet<leaf_core::ContractId> =
        over.iter().map(&key).collect();
    let mut merged: Vec<T> = base.into_iter().filter(|b| !overridden.contains(&key(b))).collect();
    merged.extend(over);
    merged
}

/// Highest-magnitude-wins exit-code aggregation (max-of-positives or
/// min-of-negatives, else 0) — Spring's contributor fold.
fn compute_exit_code(contributors: &[i32]) -> i32 {
    contributors
        .iter()
        .copied()
        .fold(0, |acc, c| if c.abs() > acc.abs() { c } else { acc })
}
