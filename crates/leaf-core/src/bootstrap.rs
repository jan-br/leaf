//! The bootstrap ABI: arguments, app-type, run-participant slices, settings.
//!
//! Realizes the leaf-core surface of bootstrap-diagnostics (phase3/14): the
//! ultra-stable types `leaf-boot`'s run pipeline (the THIRD orchestration layer
//! over `Context`) is built on. leaf-core owns the DATA + SEAMS; the run
//! pipeline (the `App<S>` typestate, `deduce`, `seal_environment`, `refresh`,
//! the validate orchestrator) lives in leaf-boot.
//!
//! - [`ApplicationArguments`] — the ONE owner of parsed argv (backing the
//!   command-line `PropertySource`, the `ApplicationArguments` singleton, and
//!   runner args). The `--opt[=value]` / non-option split is pure, here, and
//!   unit-tested.
//! - [`AppType`] (`AppType(ContractId)`) — an OPEN value (deduction is
//!   leaf-boot/integration); the const [`AppType::NONE`] + the `Servlet`/
//!   `Reactive`/`None` built-in vocabulary. [`CapabilitySet`] is the shared
//!   feature snapshot deduction + `OnWebApplication` both read.
//! - The run-participant linkme channels keyed/tie-broken on `ContractId` +
//!   [`OrderKey`]: [`APP_TYPE_DEDUCERS`] / [`CONTEXT_INITIALIZERS`] /
//!   [`EARLY_LISTENERS`] / [`FLAVOR_SEEDERS`] / [`EXIT_CODE_CONTRIBUTORS`], each
//!   with its const descriptor row + dedicated trait.
//! - [`ShutdownTrigger`] — the signal-source seam (leaf-tokio = `tokio::signal`).
//! - The unified strictness lever [`StartupValidation`] (`Strict`/`Lenient`/
//!   `Skip`, `Default = Strict`), the frozen [`BootstrapSettings`] self-binding
//!   record, and the [`ShutdownSettings`] drain budgets over [`Deadline`].
//! - The run-milestone vocabulary ([`RunMilestone`] + the named lifecycle
//!   facts).

// The run-participant `#[linkme::distributed_slice]` channels expand to `#[used]`
// `#[link_section]` statics; the section override trips `deny(unsafe_code)`. This
// is the same genuinely-required exception the `discovery` module documents,
// scoped HERE to this module only. No hand-written `unsafe` block exists.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use crate::discovery::linkme;
use crate::env::EnvBuilder;
use crate::error::LeafError;
use crate::identity::{contract_hash, ContractId};
use crate::order::OrderKey;

// ═══════════════════════════ application arguments ══════════════════════════

/// The ONE owner of parsed command-line arguments (bootstrap-diagnostics) —
/// `Arc`-shared, backing the command-line `PropertySource`, the
/// `ApplicationArguments` singleton, and runner args. Parsing is pure and lives
/// here.
///
/// The split is the GNU-ish `--name` / `--name=value` option vs non-option
/// convention: a `--name` with no `=` is a present option with no values (a
/// flag); `--name=value` adds `value` to `name`'s (multi-valued) list; a
/// bare `--` terminates option parsing (everything after is non-option);
/// anything else is a non-option positional argument.
#[derive(Clone, Debug, Default)]
pub struct ApplicationArguments {
    inner: Arc<ArgsInner>,
}

#[derive(Debug, Default)]
struct ArgsInner {
    source_args: Vec<String>,
    // Insertion-ordered option names with their accumulated values.
    options: Vec<(String, Vec<String>)>,
    non_option: Vec<String>,
}

impl ApplicationArguments {
    /// Parse a raw argv (excluding the program name) into the owned arguments.
    ///
    /// # Errors
    /// Returns a [`LeafError`] only if an argument is not valid UTF-8 (the
    /// kernel's args model is `String`-based; lossy bytes are a
    /// [`ConfigIo`](crate::ErrorKind::ConfigIo) fault.
    pub fn parse(argv: Vec<std::ffi::OsString>) -> Result<Self, LeafError> {
        let mut source_args = Vec::with_capacity(argv.len());
        for os in argv {
            match os.into_string() {
                Ok(s) => source_args.push(s),
                Err(bad) => {
                    return Err(LeafError::new(crate::error::ErrorKind::ConfigIo).caused_by(
                        crate::error::Cause::plain(
                            "parsing command-line arguments",
                            format!("non-UTF-8 argument: {bad:?}"),
                        ),
                    ));
                }
            }
        }
        Ok(Self::from_strings(source_args))
    }

    /// Parse already-`String` args (the common in-process / test path).
    #[must_use]
    pub fn from_strings(source_args: Vec<String>) -> Self {
        let mut options: Vec<(String, Vec<String>)> = Vec::new();
        let mut non_option: Vec<String> = Vec::new();
        let mut options_done = false;

        for arg in &source_args {
            if options_done {
                non_option.push(arg.clone());
                continue;
            }
            if arg == "--" {
                options_done = true;
                continue;
            }
            if let Some(body) = arg.strip_prefix("--") {
                if body.is_empty() {
                    // Defensive: an empty option name is a positional.
                    non_option.push(arg.clone());
                    continue;
                }
                let (name, value) = match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.to_string(), None),
                };
                let entry = match options.iter_mut().find(|(n, _)| *n == name) {
                    Some(e) => e,
                    None => {
                        options.push((name, Vec::new()));
                        options.last_mut().unwrap()
                    }
                };
                if let Some(v) = value {
                    entry.1.push(v);
                }
            } else {
                non_option.push(arg.clone());
            }
        }

        ApplicationArguments {
            inner: Arc::new(ArgsInner { source_args, options, non_option }),
        }
    }

    /// The raw source argument list (as received).
    #[must_use]
    pub fn source_args(&self) -> &[String] {
        &self.inner.source_args
    }

    /// The names of all present options (insertion order).
    pub fn option_names(&self) -> impl Iterator<Item = &str> {
        self.inner.options.iter().map(|(n, _)| n.as_str())
    }

    /// The (possibly empty, possibly multi-) values for `name`.
    #[must_use]
    pub fn option_values(&self, name: &str) -> &[String] {
        self.inner
            .options
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[][..], |(_, v)| v.as_slice())
    }

    /// Whether `name` is a present option (a flag or valued).
    #[must_use]
    pub fn contains_option(&self, name: &str) -> bool {
        self.inner.options.iter().any(|(n, _)| n == name)
    }

    /// The non-option positional arguments.
    #[must_use]
    pub fn non_option_args(&self) -> &[String] {
        &self.inner.non_option
    }
}

// ═══════════════════════════ app-type deduction ═════════════════════════════

/// The application flavor (bootstrap-diagnostics) — an OPEN value keyed by a
/// stable [`ContractId`] so integrations contribute their own without an ABI
/// bump. Deduction (the cold-path fold over [`CapabilitySet`]) is leaf-boot's;
/// leaf-core owns the value + the built-in vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AppType(pub ContractId);

impl AppType {
    /// No web flavor (a plain CLI/worker application).
    pub const NONE: AppType = AppType(ContractId(contract_hash("leaf::app-type::none")));
    /// A servlet-style (blocking-IO web) flavor.
    pub const SERVLET: AppType = AppType(ContractId(contract_hash("leaf::app-type::servlet")));
    /// A reactive (non-blocking web) flavor.
    pub const REACTIVE: AppType = AppType(ContractId(contract_hash("leaf::app-type::reactive")));

    /// Build a custom flavor from a stable canonical id string.
    #[must_use]
    pub const fn of(canonical_path: &str) -> Self {
        AppType(ContractId::of(canonical_path))
    }
}

impl Default for AppType {
    fn default() -> Self {
        AppType::NONE
    }
}

/// The resolved capability snapshot (bootstrap-diagnostics) that app-type
/// deduction AND `OnWebApplication`/`OnClass` cfg verdicts BOTH read — so a
/// condition and the deduction can never disagree.
///
/// A set of stable capability ids (each a [`ContractId`] over a feature name).
/// Populated by the binary's force-link/`ExpectedManifest` step; read-only at
/// run time.
#[derive(Clone, Debug, Default)]
pub struct CapabilitySet {
    capabilities: std::collections::HashSet<ContractId>,
}

impl CapabilitySet {
    /// An empty capability set.
    #[must_use]
    pub fn new() -> Self {
        CapabilitySet::default()
    }

    /// Build from an iterator of capability ids.
    #[must_use]
    pub fn from_ids(ids: impl IntoIterator<Item = ContractId>) -> Self {
        CapabilitySet { capabilities: ids.into_iter().collect() }
    }

    /// Add a capability by its stable name.
    pub fn insert_name(&mut self, name: &str) {
        self.capabilities.insert(ContractId::of(name));
    }

    /// Whether a capability id is present.
    #[must_use]
    pub fn contains(&self, id: ContractId) -> bool {
        self.capabilities.contains(&id)
    }

    /// Whether a capability NAME is present.
    #[must_use]
    pub fn contains_name(&self, name: &str) -> bool {
        self.capabilities.contains(&ContractId::of(name))
    }

    /// The number of capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

/// One app-type deducer row (bootstrap-diagnostics) — `cmp_order`-sorted, first
/// `applies` wins, else [`AppType::NONE`].
#[derive(Clone, Copy, Debug)]
pub struct DeducerDescriptor {
    /// The flavor this deducer votes for.
    pub app_type: AppType,
    /// The dispatch order (lower = consulted earlier).
    pub order: OrderKey,
    /// Whether this deducer applies given the resolved capabilities.
    pub applies: fn(&CapabilitySet) -> bool,
}

// ═══════════════════════════ context initializers ═══════════════════════════

/// The pre-refresh mutation context handed to a [`ContextInitializer`]
/// (bootstrap-diagnostics). Sync cold-path; exposes the `EnvBuilder` mutate
/// vocabulary.
///
/// Scope note: the full ctx (`register_post_processor`, `engine_policy`)
/// references types leaf-boot owns; this is the minimal forward-compatible
/// placeholder carrying the always-available `&mut EnvBuilder`.
/// `#[non_exhaustive]` so adding accessors is not a breaking change.
#[non_exhaustive]
pub struct PreRefreshCtx<'a> {
    /// The environment builder, mutable before the env is sealed.
    pub env: &'a mut EnvBuilder,
}

impl<'a> PreRefreshCtx<'a> {
    /// Build a pre-refresh context over an env builder.
    #[must_use]
    pub fn new(env: &'a mut EnvBuilder) -> Self {
        PreRefreshCtx { env }
    }

    /// The environment builder (mutable).
    pub fn env_mut(&mut self) -> &mut EnvBuilder {
        self.env
    }
}

impl std::fmt::Debug for PreRefreshCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreRefreshCtx").finish_non_exhaustive()
    }
}

/// The context-initializer SPI (bootstrap-diagnostics) — sync cold-path,
/// dedicated trait (NOT the bean `Aware` setter).
pub trait ContextInitializer: Send + Sync {
    /// Mutate the pre-refresh context.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if initialization fails.
    fn initialize(&self, cx: &mut PreRefreshCtx<'_>) -> Result<(), LeafError>;
}

/// One context-initializer row (bootstrap-diagnostics). `make` builds the
/// initializer lazily so the const row stays a fn-pointer (never a live object).
#[derive(Clone, Copy, Debug)]
pub struct InitializerDescriptor {
    /// The cold-path order.
    pub order: OrderKey,
    /// The lazy constructor.
    pub make: fn() -> Box<dyn ContextInitializer>,
}

// ═══════════════════════════ early listeners + runners ══════════════════════

/// A pre-context run-event listener (bootstrap-diagnostics) — receives the
/// named run-milestone facts buffered on the early-event buffer before the
/// multicaster is installed.
pub trait EarlyListener: Send + Sync {
    /// React to a run milestone.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the listener fails (per the milestone's
    /// `DispatchErrorMode`, this may or may not abort startup).
    fn on_milestone(&self, milestone: RunMilestone) -> Result<(), LeafError>;
}

/// One early-listener row (bootstrap-diagnostics).
#[derive(Clone, Copy, Debug)]
pub struct EarlyListenerDescriptor {
    /// The dispatch order.
    pub order: OrderKey,
    /// The lazy constructor.
    pub make: fn() -> Box<dyn EarlyListener>,
}

/// The ONE runner SPI (bootstrap-diagnostics) — a single `cmp_order` stream,
/// run sequentially after the context is ready (the K8s readiness window).
pub trait Runner: Send + Sync {
    /// Run with the parsed application arguments.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the runner fails.
    fn run<'a>(
        &'a self,
        args: &'a ApplicationArguments,
    ) -> crate::future::BoxFuture<'a, Result<(), LeafError>>;
}

// ═══════════════════════════ flavor seeders ═════════════════════════════════

/// The flavor-seeder SPI (bootstrap-diagnostics, extra-15) — an origin-agnostic
/// seeder over the `EnvBuilder` mutate vocabulary, keyed by [`AppType`].
pub trait FlavorSeeder: Send + Sync {
    /// Seed flavor-specific defaults into the env builder.
    fn seed(&self, b: &mut EnvBuilder);
}

/// One flavor-seeder row (bootstrap-diagnostics).
#[derive(Clone, Copy, Debug)]
pub struct FlavorSeederDescriptor {
    /// The flavor this seeder applies to.
    pub app_type: AppType,
    /// The lazy constructor.
    pub make: fn() -> Box<dyn FlavorSeeder>,
}

// ═══════════════════════════ exit code + shutdown ═══════════════════════════

/// The exit-code contributor SPI (bootstrap-diagnostics) — the highest-magnitude
/// code wins (the exit-code fold lives in leaf-boot).
pub trait ExitCodeContributor: Send + Sync {
    /// This contributor's preferred exit code.
    fn exit_code(&self) -> i32;
}

/// One exit-code-contributor row (bootstrap-diagnostics).
#[derive(Clone, Copy, Debug)]
pub struct ContributorDescriptor {
    /// The lazy constructor.
    pub make: fn() -> Box<dyn ExitCodeContributor>,
}

/// An observe-only exit-code fact (bootstrap-diagnostics).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitCodeEvent {
    /// The computed exit code.
    pub code: i32,
}

/// The shutdown-trigger seam (bootstrap-diagnostics) — the signal source. The
/// concrete impl (leaf-tokio = `tokio::signal`) calls `fire` once when a
/// shutdown signal arrives; leaf never installs a global handler itself.
pub trait ShutdownTrigger: Send + Sync {
    /// Arm the trigger; `fire` is invoked once on a shutdown signal.
    fn arm(&self, fire: Box<dyn Fn() + Send + Sync>);
}

// ═══════════════════════════ settings ═══════════════════════════════════════

/// Where the startup banner is rendered (bootstrap-diagnostics). Degrade-and-warn
/// — never aborts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BannerMode {
    /// Render to the console (the default).
    #[default]
    Console,
    /// Render through the logging backend.
    Log,
    /// Do not render a banner.
    Off,
}

/// The unified startup-validation strictness lever (SEAMS `[C2]`) — ONE enum,
/// `Default = Strict`. Read in EXACTLY ONE place (the head of
/// `App<Wired>::validate()`) and threaded downward as an explicit value (never
/// re-read from `Env`, never ambient).
///
/// - `Strict` (default): run the wiring pass, the strict-collect placeholder
///   sub-pass, and resolve every `@Value` with `Leniency::Strict`, aggregating
///   ALL failures into the one `AssemblyReport` at Tier-2.
/// - `Lenient`: still run the FULL wiring pass (`NoSuchBean`/`NoUniqueBean`/
///   `Cycle`/`ScopeMismatch` stay HARD — structural soundness is never tunable),
///   but DOWNGRADE the two value-shape facets (unresolved mandatory placeholders,
///   uncoercible `@Value`) to `Severity::Warn`.
/// - `Skip`: bypass the COLD-WALK COST ONLY, legal ONLY when a matching
///   `cargo leaf prepare` AOT plan is present (else rejected loudly); never
///   relaxes correctness, and the config-properties bind+JSR sub-pass STILL runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StartupValidation {
    /// Collect-all, fully strict (the default).
    #[default]
    Strict,
    /// Value-shape facets downgrade to `Warn`; wiring + config-JSR stay HARD.
    Lenient,
    /// Elide the cold walk iff a hash-matching AOT plan is present.
    Skip,
}

/// A shutdown drain budget (bootstrap-diagnostics, `[C1/C7]`) — a deadline as a
/// duration from the moment the drain begins, or `Indefinite` to wait forever.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Deadline {
    /// Wait at most this long.
    After(Duration),
    /// Wait indefinitely (no budget).
    #[default]
    Indefinite,
}

impl Deadline {
    /// A budget of `secs` seconds.
    #[must_use]
    pub const fn secs(secs: u64) -> Self {
        Deadline::After(Duration::from_secs(secs))
    }

    /// The duration budget, if bounded.
    #[must_use]
    pub const fn as_duration(self) -> Option<Duration> {
        match self {
            Deadline::After(d) => Some(d),
            Deadline::Indefinite => None,
        }
    }
}

/// The two shutdown drain budgets (bootstrap-diagnostics, `[C1/C7]`), consumed
/// by the container-lifecycle teardown step.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ShutdownSettings {
    /// The in-flight-request body-drain budget.
    pub grace: Deadline,
    /// The per-request tx-finalize budget AFTER the body grace elapses.
    pub finalize_grace: Deadline,
}

/// The frozen read-only `leaf.main.*` self-binding record (bootstrap-diagnostics,
/// extra-11) — bound once inside `seal_environment`, read thereafter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BootstrapSettings {
    /// Where the banner renders.
    pub banner_mode: BannerMode,
    /// An explicit web-application-type override (else deduced).
    pub web_application_type: Option<AppType>,
    /// Whether beans initialize lazily by default.
    pub lazy_initialization: bool,
    /// Whether bean-definition overriding is allowed.
    pub allow_bean_definition_overriding: bool,
    /// Whether constructor circular references are allowed.
    pub allow_circular_references: bool,
    /// Whether to register an OS shutdown hook.
    pub register_shutdown_hook: bool,
    /// Whether to keep the process alive with no non-daemon work (keep-alive).
    pub keep_alive: bool,
    /// The unified strictness lever (`[C2]`, 8th field; read ONCE at validate).
    pub startup_validation: StartupValidation,
    /// The shutdown drain budgets (`[C1/C7]`).
    pub shutdown: ShutdownSettings,
}

impl BootstrapSettings {
    /// The default settings: console banner, deduced flavor, eager init,
    /// no overriding/circular refs, shutdown hook on, strict validation.
    pub const DEFAULT: BootstrapSettings = BootstrapSettings {
        banner_mode: BannerMode::Console,
        web_application_type: None,
        lazy_initialization: false,
        allow_bean_definition_overriding: false,
        allow_circular_references: false,
        register_shutdown_hook: true,
        keep_alive: false,
        startup_validation: StartupValidation::Strict,
        shutdown: ShutdownSettings { grace: Deadline::Indefinite, finalize_grace: Deadline::Indefinite },
    };
}

impl Default for BootstrapSettings {
    fn default() -> Self {
        BootstrapSettings::DEFAULT
    }
}

// ═══════════════════════════ run milestones ═════════════════════════════════

/// The named run-milestone vocabulary (bootstrap-diagnostics) — the run-event
/// sequence ARE built-in lifecycle facts on the events subsystem.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RunMilestone {
    /// The run has begun.
    Starting,
    /// The environment has been prepared (`seal_environment` done).
    EnvironmentPrepared,
    /// The context has been initialized (pre-refresh initializers ran).
    ContextInitialized,
    /// The context is prepared (definitions loaded).
    Prepared,
    /// The context has been refreshed (beans eager-instantiated).
    Refreshed,
    /// The application has started.
    Started,
    /// All runners have been invoked.
    RunnersInvoked,
    /// The application is ready (accepting traffic).
    Ready,
}

impl RunMilestone {
    /// A short, stable slug for this milestone (rendering / tests).
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            RunMilestone::Starting => "starting",
            RunMilestone::EnvironmentPrepared => "environment-prepared",
            RunMilestone::ContextInitialized => "context-initialized",
            RunMilestone::Prepared => "prepared",
            RunMilestone::Refreshed => "refreshed",
            RunMilestone::Started => "started",
            RunMilestone::RunnersInvoked => "runners-invoked",
            RunMilestone::Ready => "ready",
        }
    }
}

// ═══════════════════════════ the linkme channels ════════════════════════════
//
// The run-participant streams: each a typed `#[distributed_slice]` joining the
// COMPONENTS/CONDITIONS/AUTO_CONFIGS family. Force-linked + self-checked via
// `ExpectedManifest`; the asymmetric quiet-DCE hazards (silent listener /
// deducer / seeder) are ALL one `AntiDceError::SourceVanished`. Ordering is
// computed from `OrderKey` (cmp_order), NEVER read from link/section order.

/// The app-type deducer channel (bootstrap-diagnostics).
#[linkme::distributed_slice]
pub static APP_TYPE_DEDUCERS: [DeducerDescriptor] = [..];

/// The context-initializer channel (bootstrap-diagnostics).
#[linkme::distributed_slice]
pub static CONTEXT_INITIALIZERS: [InitializerDescriptor] = [..];

/// The early (pre-context) run-event-listener channel (bootstrap-diagnostics).
#[linkme::distributed_slice]
pub static EARLY_LISTENERS: [EarlyListenerDescriptor] = [..];

/// The flavor-seeder channel (bootstrap-diagnostics, extra-15).
#[linkme::distributed_slice]
pub static FLAVOR_SEEDERS: [FlavorSeederDescriptor] = [..];

/// The exit-code-contributor channel (bootstrap-diagnostics).
#[linkme::distributed_slice]
pub static EXIT_CODE_CONTRIBUTORS: [ContributorDescriptor] = [..];

#[cfg(test)]
mod tests {
    use super::*;

    // ── ApplicationArguments parsing (the pure behavioral surface) ───────────

    fn args(items: &[&str]) -> ApplicationArguments {
        ApplicationArguments::from_strings(items.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn parses_valued_options() {
        let a = args(&["--server.port=8080", "--name=leaf"]);
        assert_eq!(a.option_values("server.port"), &["8080".to_string()]);
        assert_eq!(a.option_values("name"), &["leaf".to_string()]);
        assert!(a.contains_option("server.port"));
    }

    #[test]
    fn parses_flag_options_with_no_value() {
        let a = args(&["--debug", "--verbose"]);
        assert!(a.contains_option("debug"));
        assert!(a.contains_option("verbose"));
        assert!(a.option_values("debug").is_empty(), "a flag has no values");
    }

    #[test]
    fn accumulates_multi_valued_options() {
        let a = args(&["--profile=prod", "--profile=eu"]);
        assert_eq!(a.option_values("profile"), &["prod".to_string(), "eu".to_string()]);
    }

    #[test]
    fn separates_non_option_positionals() {
        let a = args(&["run", "--flag", "input.txt"]);
        assert_eq!(a.non_option_args(), &["run".to_string(), "input.txt".to_string()]);
        assert!(a.contains_option("flag"));
    }

    #[test]
    fn double_dash_terminates_option_parsing() {
        let a = args(&["--opt=1", "--", "--not-an-option", "x"]);
        assert!(a.contains_option("opt"));
        assert_eq!(
            a.non_option_args(),
            &["--not-an-option".to_string(), "x".to_string()],
            "everything after -- is positional"
        );
    }

    #[test]
    fn option_names_are_insertion_ordered() {
        let a = args(&["--b=1", "--a=2", "--b=3"]);
        let names: Vec<&str> = a.option_names().collect();
        assert_eq!(names, vec!["b", "a"], "names dedup, first-seen order");
    }

    #[test]
    fn source_args_are_preserved_verbatim() {
        let a = args(&["--x=1", "pos"]);
        assert_eq!(a.source_args(), &["--x=1".to_string(), "pos".to_string()]);
    }

    #[test]
    fn parse_from_os_strings_round_trips_utf8() {
        let argv = vec![std::ffi::OsString::from("--k=v"), std::ffi::OsString::from("pos")];
        let a = ApplicationArguments::parse(argv).unwrap();
        assert_eq!(a.option_values("k"), &["v".to_string()]);
        assert_eq!(a.non_option_args(), &["pos".to_string()]);
    }

    #[test]
    fn unknown_option_has_empty_values() {
        let a = args(&["--present"]);
        assert!(a.option_values("absent").is_empty());
        assert!(!a.contains_option("absent"));
    }

    // ── AppType ──────────────────────────────────────────────────────────────

    #[test]
    fn app_type_built_ins_are_distinct_and_stable() {
        assert_ne!(AppType::NONE, AppType::SERVLET);
        assert_ne!(AppType::SERVLET, AppType::REACTIVE);
        assert_eq!(AppType::default(), AppType::NONE);
        // Stable + reproducible (rides contract_hash).
        assert_eq!(AppType::SERVLET, AppType::of("leaf::app-type::servlet"));
    }

    #[test]
    fn app_type_is_an_open_value() {
        let custom = AppType::of("acme::grpc");
        assert_ne!(custom, AppType::NONE);
        assert_eq!(custom, AppType::of("acme::grpc"));
    }

    // ── CapabilitySet ────────────────────────────────────────────────────────

    #[test]
    fn capability_set_membership_by_name_and_id() {
        let mut caps = CapabilitySet::new();
        caps.insert_name("web.servlet");
        assert!(caps.contains_name("web.servlet"));
        assert!(caps.contains(ContractId::of("web.servlet")));
        assert!(!caps.contains_name("web.reactive"));
        assert_eq!(caps.len(), 1);
    }

    #[test]
    fn capability_set_from_ids() {
        let caps = CapabilitySet::from_ids([ContractId::of("a"), ContractId::of("b")]);
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(ContractId::of("a")));
    }

    // ── deducer descriptor + applies fn ──────────────────────────────────────

    #[test]
    fn deducer_descriptor_applies_predicate_reads_capabilities() {
        const D: DeducerDescriptor = DeducerDescriptor {
            app_type: AppType::SERVLET,
            order: OrderKey::implicit(),
            applies: |caps| caps.contains(ContractId::of("web.servlet")),
        };
        let mut caps = CapabilitySet::new();
        assert!(!(D.applies)(&caps));
        caps.insert_name("web.servlet");
        assert!((D.applies)(&caps));
        assert_eq!(D.app_type, AppType::SERVLET);
    }

    // ── StartupValidation ────────────────────────────────────────────────────

    #[test]
    fn startup_validation_defaults_to_strict() {
        assert_eq!(StartupValidation::default(), StartupValidation::Strict);
    }

    // ── BootstrapSettings / ShutdownSettings / Deadline ──────────────────────

    #[test]
    fn bootstrap_settings_default_is_strict_console_with_shutdown_hook() {
        let s = BootstrapSettings::DEFAULT;
        assert_eq!(s.banner_mode, BannerMode::Console);
        assert_eq!(s.startup_validation, StartupValidation::Strict);
        assert!(s.register_shutdown_hook);
        assert!(!s.lazy_initialization);
        assert_eq!(s.shutdown.grace, Deadline::Indefinite);
    }

    #[test]
    fn deadline_secs_and_as_duration() {
        assert_eq!(Deadline::secs(30).as_duration(), Some(Duration::from_secs(30)));
        assert_eq!(Deadline::Indefinite.as_duration(), None);
    }

    #[test]
    fn shutdown_settings_carries_two_budgets() {
        let s = ShutdownSettings { grace: Deadline::secs(10), finalize_grace: Deadline::secs(5) };
        assert_eq!(s.grace.as_duration(), Some(Duration::from_secs(10)));
        assert_eq!(s.finalize_grace.as_duration(), Some(Duration::from_secs(5)));
    }

    // ── ShutdownTrigger seam ─────────────────────────────────────────────────

    #[test]
    fn shutdown_trigger_fires_the_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ImmediateTrigger;
        impl ShutdownTrigger for ImmediateTrigger {
            fn arm(&self, fire: Box<dyn Fn() + Send + Sync>) {
                // A test trigger fires immediately on arm.
                fire();
            }
        }

        static FIRED: AtomicBool = AtomicBool::new(false);
        let t: &dyn ShutdownTrigger = &ImmediateTrigger;
        t.arm(Box::new(|| FIRED.store(true, Ordering::SeqCst)));
        assert!(FIRED.load(Ordering::SeqCst));
    }

    // ── run participants are object-safe behind their descriptors ────────────

    struct NopInit;
    impl ContextInitializer for NopInit {
        fn initialize(&self, _cx: &mut PreRefreshCtx<'_>) -> Result<(), LeafError> {
            Ok(())
        }
    }

    #[test]
    fn context_initializer_runs_over_a_pre_refresh_ctx() {
        let mut builder = EnvBuilder::new();
        let mut cx = PreRefreshCtx::new(&mut builder);
        assert!(NopInit.initialize(&mut cx).is_ok());
        // The descriptor's make fn builds it lazily.
        const D: InitializerDescriptor =
            InitializerDescriptor { order: OrderKey::implicit(), make: || Box::new(NopInit) };
        let _boxed = (D.make)();
    }

    #[test]
    fn run_milestone_slugs_are_stable() {
        assert_eq!(RunMilestone::Ready.slug(), "ready");
        assert_eq!(RunMilestone::EnvironmentPrepared.slug(), "environment-prepared");
    }

    // ── linkme channels exist + are iterable (the ABI guarantee) ─────────────

    #[test]
    fn run_participant_channels_exist_and_are_iterable() {
        // In a bare leaf-core build nothing submits, so all are empty — the
        // assertion is that the read is total (the channel exists + types match).
        assert_eq!(APP_TYPE_DEDUCERS.len(), APP_TYPE_DEDUCERS.iter().count());
        assert_eq!(CONTEXT_INITIALIZERS.len(), CONTEXT_INITIALIZERS.iter().count());
        assert_eq!(EARLY_LISTENERS.len(), EARLY_LISTENERS.iter().count());
        assert_eq!(FLAVOR_SEEDERS.len(), FLAVOR_SEEDERS.iter().count());
        assert_eq!(EXIT_CODE_CONTRIBUTORS.len(), EXIT_CODE_CONTRIBUTORS.iter().count());
    }

    // A const deducer row submitted into the slice roundtrips (proving the
    // channel is const-constructible cross-crate).
    #[linkme::distributed_slice(APP_TYPE_DEDUCERS)]
    static TEST_DEDUCER: DeducerDescriptor = DeducerDescriptor {
        app_type: AppType::SERVLET,
        order: OrderKey::implicit(),
        applies: |_| false,
    };

    #[test]
    fn a_submitted_deducer_row_roundtrips() {
        let found = APP_TYPE_DEDUCERS.iter().any(|d| d.app_type == AppType::SERVLET);
        assert!(found, "the submitted deducer must be link-collected");
    }
}
