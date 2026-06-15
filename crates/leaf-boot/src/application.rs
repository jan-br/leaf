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
    analyze_first, AnalysisCtx, ApplicationArguments, BannerMode, CandidateRole, Diagnostic,
    Env, EarlyListener, FailureAnalyzer, FailureAnalysis, InjectionPlan, LeafError, LifecyclePlan,
    RenderStyle, RunMilestone, SchedulerCore, Spawner,
};

use crate::app::App;
use crate::assembly::SeedPairing;
use crate::autoconfig::{AutoConfigCandidate, ExclusionSet};
use crate::conditions::GuardPairing;
use crate::environment::SealInputs;
use crate::lifecycle::RunUnit;
use crate::proxy::AdvisorPairing;
use crate::scheduling::{CronTriggerFactory, ScheduledPairing};
use crate::validate::ValidationInputs;

type PlanResolver = Arc<dyn Fn(leaf_core::BeanId) -> LifecyclePlan + Send + Sync>;
type InjectionResolver = Arc<dyn Fn(leaf_core::BeanId) -> InjectionPlan + Send + Sync>;

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
/// The binary (`#[leaf::main]`) builds an `Application` over the macro-emitted
/// JOIN tables (seeds, guards, auto-config candidates, advisors, listeners,
/// scheduled tasks, the per-bean injection/lifecycle plans) then awaits
/// [`run`](Application::run). The defaults make a minimal app work with no tables.
pub struct Application {
    seeds: Vec<SeedPairing>,
    guards: Vec<GuardPairing>,
    autoconfig: Vec<AutoConfigCandidate>,
    exclusions: ExclusionSet,
    advisors: Vec<AdvisorPairing>,
    listeners: Vec<leaf_core::ListenerDescriptor>,
    dispatch_chain: Vec<Arc<dyn leaf_core::DispatchInterceptor>>,
    scheduled: Vec<ScheduledPairing>,
    runners: Vec<Arc<dyn leaf_core::Runner>>,
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
            listeners: Vec::new(),
            dispatch_chain: Vec::new(),
            scheduled: Vec::new(),
            runners: Vec::new(),
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

    /// The macro→runtime bean seed JOIN table (the `COMPONENTS`/`AUTO_CONFIGS`
    /// `Descriptor`→`ProviderSeed` pairings `#[leaf::main]` emits).
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

    /// The advisor JOIN table (the R4 auto-proxy `after_init` install).
    #[must_use]
    pub fn with_advisors(mut self, advisors: Vec<AdvisorPairing>) -> Self {
        self.advisors = advisors;
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
        // self_check at the Define→Resolve edge (anti-DCE expected-vs-found).
        App::<Define>::self_check(&[]).map_err(LeafError::from)?;

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

        // (10) route_conditions + run_autoconfig → seal() → validate().
        app.route_conditions(&self.guards)?;
        app.run_autoconfig(&self.autoconfig, &self.exclusions)?;
        let app = app.seal()?;
        *phase = RunMilestone::Prepared;
        self.fire_early(overlay, RunMilestone::Prepared);

        // The Tier-2 aggregated validation pass (config materialization + dry-runs
        // live behind ValidationInputs; the minimal pipeline validates the wiring).
        let validation = ValidationInputs::new();
        app.validate(&validation)?;

        // (11) Context::refresh() — R0..R8. Refreshed/Started fire DURING via the
        // now-live EventPublisher; the runner window opens after.
        let unit = self.build_run_unit(app, movable)?;
        let unit = unit.refresh().await?;
        *phase = RunMilestone::Refreshed;

        // (12) Started + Liveness=Correct already fired inside refresh R8.
        *phase = RunMilestone::Started;

        // (13) call_runners() in the readiness-gate window (after Started+Liveness,
        // BEFORE Ready+Readiness) — sequentially, abort on the first Err.
        self.call_runners(&run_args).await?;
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
    fn build_run_unit(&self, app: App<Wired>, movable: MovableTables) -> Result<RunUnit, LeafError> {
        let proxy_plan = self.frozen_proxy_plan(app.registry())?;
        let (registry, env, settings) = app.into_run_parts();

        let plan_of = Arc::clone(&self.plan_of);
        let inj_of = Arc::clone(&self.inj_of);
        let advisors: Vec<leaf_core::AdvisorDescriptor> = self
            .advisors
            .iter()
            .map(|p| leaf_core::AdvisorDescriptor {
                id: p.contract,
                order: p.order,
                role: p.role,
                pointcut: p.pointcut,
                make_interceptor: p.make_interceptor,
            })
            .collect();

        let mut unit = RunUnit::over_engine(leaf_core::Engine::new(registry), env)
            .with_plan_resolver(move |id| plan_of(id))
            .with_injection_plans(move |id| inj_of(id))
            .with_proxy_plan(proxy_plan)
            .with_advisors(advisors)
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

    /// Compute the frozen [`ProxyPlan`](leaf_core::ProxyPlan) over the advised
    /// beans. The minimal pipeline has no join-point views to match yet (the macro
    /// emits `BeanJoinPoints` in a later codegen unit), so this is the empty plan
    /// unless a richer table is supplied; the advisor descriptors still ride into
    /// the unit so the install resolves any plan-referenced advisor.
    fn frozen_proxy_plan(
        &self,
        _registry: &leaf_core::Registry,
    ) -> Result<leaf_core::ProxyPlan, LeafError> {
        Ok(leaf_core::ProxyPlan::empty())
    }

    /// Run the merged runner stream sequentially in the readiness-gate window,
    /// over the shared [`ApplicationArguments`]. Abort on the first `Err`.
    async fn call_runners(&self, args: &ApplicationArguments) -> Result<(), LeafError> {
        for runner in &self.runners {
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

/// Highest-magnitude-wins exit-code aggregation (max-of-positives or
/// min-of-negatives, else 0) — Spring's contributor fold.
fn compute_exit_code(contributors: &[i32]) -> i32 {
    contributors
        .iter()
        .copied()
        .fold(0, |acc, c| if c.abs() > acc.abs() { c } else { acc })
}
