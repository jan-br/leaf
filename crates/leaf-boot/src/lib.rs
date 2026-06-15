//! `leaf-boot` — Assembly engine + run pipeline atop leaf-core's Engine/Context.
//!
//! The THIRD orchestration layer over `Context` (ADR-05): `App<Define → Resolve →
//! Wired → Running>` walks the FIXED toolkit typestate, lifting the link-collected
//! `linkme` slices into a frozen registry and driving the run pipeline. This is
//! the charter's bootstrap layer — it RESOLVES the cross-crate NOTEs the lower
//! crates left (the `from_slices` lift, the anti-DCE self-check, the proxy
//! after-init install, the listener/scheduler binding, the run engine) and wires
//! the SPIs leaf-conditions/leaf-config/leaf-cron/leaf-tokio expose.
//!
//! ## Unit 1 — `App<Define>`: `from_slices` lift + anti-DCE self-check + force-link
//!
//! The cold assembly pass's entry points:
//!
//! - [`App::from_slices`] / [`from_slices`] — lift the link-collected
//!   [`leaf_core::COMPONENTS`] + [`leaf_core::AUTO_CONFIGS`] bean channels and
//!   JOIN each bare const `Descriptor` to its macro-emitted [`ProviderSeed`] via
//!   the [`SeedPairing`] table (the "macro-emitted mangled pairing consts"),
//!   building the [`leaf_core::RegistryBuilder`]. Ordering is NEVER read from the
//!   slice — `freeze()` computes the one canonical order from the stable
//!   [`leaf_core::ContractId`].
//! - [`App::<Define>::self_check`] / [`self_check`] — the expected-vs-found
//!   anti-DCE self-check at the `Define→Resolve` edge: a crate in the
//!   `ExpectedManifest` but absent from [`leaf_core::SOURCES`] is the LOUD
//!   [`AntiDceError::SourceVanished`] (lifting into the one [`leaf_core::LeafError`]
//!   spine with [`leaf_core::ErrorKind::AntiDce`]).
//! - [`force_link!`] + the default-feature `leaf-tokio` force-link — Layer-0
//!   anti-DCE: pin a participating crate's rlib onto the link graph.
//!
//! ## Unit 2 — `App<Resolve>`: `seal_environment` + `route_conditions` + `run_autoconfig`
//!
//! The cold resolution pass, the body of the `App<Define> → App<Resolve>` walk:
//!
//! - [`seal_environment`] — the 5f async fence: parse argv into
//!   [`leaf_core::ApplicationArguments`] + a highest-precedence command-line
//!   [`leaf_core::PropertySource`], drive leaf-config's `ConfigDataLoader`
//!   (`spring.config.import`-analogue), activate profiles via
//!   [`leaf_core::resolve_active`], bind `leaf.main.*` onto
//!   [`leaf_core::BootstrapSettings`] (`bindToApplication`), and snapshot the
//!   immutable [`leaf_core::Env`].
//! - [`route_conditions`] — evaluate the runtime-tier [`leaf_core::CondExpr`]
//!   leaves over the sealed `Env` in a Parse-then-Register sub-pass (resolving
//!   the [`GuardPairing`] JOIN), recording the [`leaf_core::ConditionReport`].
//! - [`run_autoconfig`] — the `exclude(ContractId) > user-bean back-off
//!   (OnMissingBean) > auto-config default` ladder over the [`AutoConfigCandidate`]
//!   set, registering survivors INCREMENTALLY at [`leaf_core::CandidateRole::FALLBACK`]
//!   (so a user `@Component` supersedes), gated by the
//!   `leaf.enable-autoconfiguration` kill-switch.
//!
//! These wire up the cross-crate NOTEs the lower crates left: leaf-config's
//! config-data load (driven in `seal_environment`), leaf-conditions'
//! `with_probe`/`with_active_profiles` ambient bridges (driven in the Register
//! sub-pass), and the macro-emitted `__leaf_guard_<Ident>` guard JOIN.
//!
//! ## Unit 3 — `seal()` + `App<Wired>::validate()` + the `WiringPlan`
//!
//! The cold freeze + the whole-graph validation pass, the `App<Resolve> →
//! App<Wired>` walk:
//!
//! - [`App::<Resolve>::seal`] — the ONE irreversible freeze: consume the mutable
//!   [`leaf_core::RegistryBuilder`] into the immutable dense-`BeanId`
//!   [`leaf_core::Registry`] (the slot-indexed `OnceCell` store) + freeze the
//!   accumulated condition records into the keyed [`leaf_core::ConditionReport`].
//! - [`order_batch`] / [`WiringPlan`] — the 3-pass sort (graph-build → layered
//!   topological [`Wave`] partition → cycle detect) folding each bean's MANDATORY
//!   construction edges + its `@DependsOn` targets into waves where every mandatory
//!   edge lands in a strictly-earlier wave (the R5 WAVE-PARTITION INVARIANT). A
//!   constructor cycle is [`leaf_core::ErrorKind::CircularDependency`] with the
//!   path + the convert-to-`LazyRef` hint; a `@DependsOn` cycle is
//!   [`leaf_core::ErrorKind::DependsOnCycle`].
//! - [`App::<Wired>::validate`] — the WHOLE-GRAPH validation pass: resolve every
//!   eager mandatory [`leaf_core::InjectionPoint`] via the
//!   [`Selector`](leaf_core::Selector), classify cycles, and AGGREGATE all
//!   `NoSuchBean`/`NoUniqueBean`/`ScopeMismatch`/`AdvisedConcreteInjection`/cycle
//!   faults into ONE [`leaf_core::AssemblyReport`] at Tier-2 (BEFORE refresh),
//!   enriching each `NoSuchBean` from the [`leaf_core::ConditionReport`].
//! - **C2** ([`ValidationInputs`] + [`ConfigBean`] + [`ValueDryRun`]) —
//!   PRE-MATERIALIZE the pure-projection `@ConfigurationProperties` beans (binder-
//!   backed bind + JSR, the bound `Arc` stored into the slot `OnceCell` so refresh
//!   R5 publishes it and never re-binds) + DRY-RUN every eager `@Value` coercion
//!   (value discarded), so malformed config surfaces in the ONE Tier-2 report.
//!   The [`leaf_core::StartupValidation`] lever (`Strict`/`Lenient`/`Skip`) is read
//!   ONCE at the pass's head.
//!
//! ## Unit 4 — the run engine: `Context::refresh()` R0..R8 + the C1/C7 teardown
//!
//! The `App<Wired> → App<Running>` transition body — the fused container-lifecycle
//! template (container-lifecycle phase3/13, authoritative). [`RunUnit`] drives one
//! [`leaf_core::Context`] through the linear refresh R0..R8 (anti-DCE row-count
//! reconcile, `Role::Infrastructure` auto-detect by [`leaf_core::cmp_chain`], the
//! multicaster install + early-event-buffer drain, the frozen
//! [`leaf_core::ProxyPlan`] `after_init` table, the EAGER wave-instantiation per
//! [`WiringPlan`] inside one structured-concurrency scope per wave — a
//! [`Bootstrap::Background`](leaf_core::Bootstrap) bean is
//! [`Spawner::spawn`](leaf_core::Spawner)ed and `try_join`ed at the wave boundary,
//! the rest inline; the SmartInitializing barrier; `start_all()` ASC; publish
//! `Refreshed`+`Started`, `Liveness=Correct`) and the cancel-cascade
//! (`RunState=Failed`, `StartupFailed`, `Liveness=Broken`).
//!
//! [`RunUnit::shutdown`] is the C1/C7 teardown (CAS close-once, valid only from
//! `Running`): `Readiness=RefusingTraffic` + disarm-scheduler FIRST, the in-flight
//! drain under the two [`leaf_core::ShutdownSettings`] budgets, publish `Closed`,
//! `stop_all()` DESC, then the container [`leaf_core::TeardownLedger`] LIFO drain
//! (reverse wave-order) into a [`ShutdownReport`]. The ONE `watch<RunState>` cell
//! ([`RunState`]/[`watch_run_state`]) is the phase axis the run engine publishes
//! through; the two availability cells (liveness/readiness) are the orthogonal
//! same-shape cells from the events subsystem.
//!
//! This RESOLVES the cross-crate run NOTEs the lower crates left (the proxy
//! `after_init` install, the listener/scheduler binding seams, the Background
//! `Spawner::spawn`, and the run engine itself) by wiring them over leaf-core's
//! `Engine::create`/ledger/watch-cell primitives + leaf-tokio's `Spawner`.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod anti_dce;
mod app;
mod application;
mod assembly;
mod autoconfig;
mod conditions;
mod environment;
mod events;
mod lifecycle;
mod proxy;
mod scheduling;
mod validate;
mod wiring;

// The cold assembly pass + the Descriptor→ProviderSeed JOIN.
pub use assembly::{from_slices, SeedPairing};

// The expected-vs-found anti-DCE self-check.
pub use anti_dce::{self_check, AntiDceError};

// The App<S> bootstrap typestate + the Define-phase entry points + force-link.
pub use app::{App, Define, Resolve, Running, Wired};

// The 5f environment fence (seal_environment) + its inputs/product.
pub use environment::{
    command_line_source, seal_environment, seal_environment_with, ImportLocation, SealInputs,
    SealedEnvironment, COMMAND_LINE_SOURCE, PROGRAMMATIC_SOURCE,
};

// The App<Resolve> condition router (route_conditions) + the guard JOIN.
pub use conditions::{
    route_conditions, CollectingSink, GuardPairing, RouteOutcome, UNCONDITIONAL_GUARD,
};

// The App<Resolve> auto-config ladder (run_autoconfig) + its candidate/exclusion.
pub use autoconfig::{
    run_autoconfig, AutoConfigCandidate, AutoConfigOutcome, BuilderProbe, ExclusionSet,
};

// The App<Resolve> → App<Wired> seal + the eager-instantiation wave plan.
pub use wiring::{order_batch, PlanLookup, Wave, WiringPlan};

// The App<Wired> whole-graph validation pass inputs (the C2 Tier-2 site).
pub use validate::{
    validate, ConfigBean, ConfigBindResult, ValidationInputs, ValueDryRun,
};

// The App<Wired> → App<Running> run engine: the fused container-lifecycle
// template (refresh R0..R8 + the C1/C7 teardown drain) + RunState/watch.
pub use lifecycle::{RunUnit, ShutdownReport};

// The R3 event-publisher install (multicaster + listener binding + dispatch chain).
pub use events::EventPublisher;

// The R4 auto-proxy after_init install (ProxyPlan → live AdviceChain table) + the
// macro→runtime advisor JOIN row + the Container-over-Engine adapter.
pub use proxy::{AdvisorPairing, EngineContainer, InstalledProxies};

// The R6 scheduler binding (descriptor → Trigger+body registration + arm) + the
// macro→runtime scheduled-task JOIN row + the cron-trigger seam.
pub use scheduling::{
    register_scheduled, resolve_trigger, CronTriggerFactory, ScheduledBody, ScheduledPairing,
};

// The opinionated run() pipeline — the SpringApplication analogue (the THIRD
// orchestration layer over Context, ADR-05) that #[leaf::main] targets.
pub use application::{
    print_banner, Application, RunFailure, RunOverlay, RunningApp,
};

// Re-export the one ProviderSeed type at the leaf-boot surface so a downstream
// pairing table can name it as `leaf_boot::ProviderSeed` (it is leaf-core's, not
// a second type).
pub use leaf_core::ProviderSeed;

// Re-export the ONE `watch<RunState>` cell ABI at the leaf-boot surface (the
// phase axis the run engine publishes through). These are leaf-core's types — the
// SAME cell, surfaced here so a downstream consumer reads `leaf_boot::RunState` /
// `leaf_boot::watch_run_state()` without naming leaf-core directly.
pub use leaf_core::{watch_run_state, RunState, RunStateReceiver, RunStateSender};
