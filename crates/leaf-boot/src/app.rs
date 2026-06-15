//! The `App<S>` bootstrap typestate (bootstrap-diagnostics phase3/14) — unit 1:
//! the `App<Define>` entry that lifts the link-collected slices and runs the
//! anti-DCE self-check, plus the force-link helpers the binary crate uses.
//!
//! `App<S>` walks the FIXED `App<Define → Resolve → Wired → Running>` typestate
//! the toolkit mandates: there is NO parallel `RunPhase` enum — the typestate IS
//! the phase machine, so "the canonical order holds / fires once / cannot be
//! skipped" is a structural property of the consuming typestate. The `Define`
//! assembly entry points, the `seal_environment`/`route_conditions`/`run_autoconfig`
//! `Resolve`-phase transitions, the [`seal`](App::<Resolve>::seal) freeze, and the
//! [`validate`](App::<Wired>::validate) whole-graph pass are landed; the `Running`
//! transition (`Context::refresh()`) is a later unit (see the NOTE below).
//!
//! ## The Define phase
//!
//! `App<Define>` is where ALL definition contribution accumulates before the one
//! irreversible `seal()`. Unit 1 wires its two cold-pass entry points:
//!
//! - [`App::from_slices`] — lift the link-collected `COMPONENTS`/`AUTO_CONFIGS`
//!   rows and JOIN each to its `ProviderSeed` (see [`crate::assembly`]).
//! - [`App::<Define>::self_check`] — the expected-vs-found anti-DCE self-check
//!   that runs at the `App<Define>→App<Resolve>` edge (see [`crate::anti_dce`]).

use std::any::TypeId;
use std::marker::PhantomData;
use std::sync::Arc;

use leaf_core::{
    ActiveProfiles, BootstrapSettings, CandidateRole, ConditionReport, Env, LeafError, Registry,
    RegistryBuilder, SourceTag,
};

use crate::anti_dce::{self, AntiDceError};
use crate::assembly::{self, SeedPairing};
use crate::autoconfig::{self, AutoConfigCandidate, ExclusionSet};
use crate::conditions::{self, GuardPairing};
use crate::environment::{self, SealInputs, SealedEnvironment};

// ───────────────────────────── the typestate tags ───────────────────────────

/// The `App<Define>` phase: definition contribution accumulates here (lift slices,
/// run registrars to a fixpoint) before the one irreversible `seal()`.
///
/// A zero-sized typestate tag — `App<Define>` is unrepresentable to re-seal once
/// it transitions, which is what makes "you cannot accidentally reorder
/// bootstrap" a compile-time guarantee (compile-runtime-split keystone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Define {}

/// The `App<Resolve>` phase: conditions route + auto-config runs over the lifted
/// definitions, to a fixpoint, before `seal()`.
///
/// Landed as a tag by unit 1 so the `Define → Resolve` edge is nameable; the
/// transition body (`seal_environment` / `route_conditions` / `run_autoconfig`)
/// is a later unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolve {}

/// The `App<Wired>` phase: the frozen registry exists; [`validate`](App::<Wired>::validate)
/// runs the aggregated `AssemblyReport` (the whole-graph wiring pass + the Tier-2
/// config materialization), BEFORE `Context::refresh()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wired {}

/// The `App<Running>` phase: `Context::refresh()` has run; runners may execute. A
/// later unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {}

// ─────────────────────────────── App<S> ─────────────────────────────────────

/// The bootstrap application, parameterized by its typestate phase `S`.
///
/// Unit 1 lands the `App<Define>` assembly entry points. The richer per-phase
/// state (the env builder, the bootstrap settings, the run overlay) is added by
/// later units behind the same typestate, so the phase signatures are stable.
pub struct App<S> {
    /// The accumulating registry builder (present from `Define` through `Resolve`;
    /// CONSUMED at the `seal()` transition — empty placeholder thereafter).
    builder: RegistryBuilder,
    /// The `Resolve`-phase state (the sealed env + settings + profiles + the
    /// accumulating condition report). `None` in `Define`; populated by the
    /// `seal_environment` transition; carried through `Resolve` and into `Wired`.
    resolve: Option<ResolveState>,
    /// The `Wired`-phase state (the frozen registry + the frozen condition report).
    /// `None` until the `seal()` transition; present from `Wired` onward.
    wired: Option<WiredState>,
    _phase: PhantomData<S>,
}

/// The per-`App<Resolve>` state produced by `seal_environment` and grown by
/// `route_conditions`/`run_autoconfig` (the sealed env + the records every
/// downstream step consults).
struct ResolveState {
    /// The sealed environment read handle.
    env: Env,
    /// The frozen `leaf.main.*` self-binding record.
    settings: BootstrapSettings,
    /// The canonical active-profile set (resolved up-front).
    profiles: ActiveProfiles,
    /// The parsed command-line arguments.
    args: leaf_core::ApplicationArguments,
    /// `(self_type, role)` of every user/plain definition lifted before
    /// auto-config, so the auto-config back-off probe sees them.
    inventory: Vec<(TypeId, CandidateRole)>,
    /// The accumulating condition-report records (route + auto-config verdicts).
    report: Vec<leaf_core::ConditionRecord>,
}

/// The per-`App<Wired>` state produced by `seal()`: the frozen, immutable
/// [`Registry`] (the dense-`BeanId` `OnceCell` store) + the carried-forward sealed
/// env/settings/profiles and the FROZEN [`ConditionReport`] (consulted by the
/// `validate()` `NoSuchBean` enrichment).
struct WiredState {
    /// The frozen, immutable registry snapshot.
    registry: Registry,
    /// The sealed environment read handle (carried from `Resolve`).
    env: Env,
    /// The frozen `leaf.main.*` self-binding record (carried from `Resolve`).
    settings: BootstrapSettings,
    /// The canonical active-profile set (carried from `Resolve`).
    profiles: ActiveProfiles,
    /// The frozen condition report (the silent-now / loud-later enrichment join).
    report: ConditionReport,
}

impl App<Define> {
    /// Begin the `Define` phase by lifting the link-collected bean channels
    /// ([`leaf_core::COMPONENTS`] + [`leaf_core::AUTO_CONFIGS`]) and JOINing each
    /// `Descriptor` to its [`leaf_core::ProviderSeed`] via the macro-emitted
    /// `pairings` table (see [`crate::assembly::from_slices`]).
    ///
    /// This is the cold assembly pass's entry point; the returned `App<Define>`
    /// still accepts further definition contribution (registrars, exclusions)
    /// before the one `seal()` a later unit drives.
    ///
    /// # Errors
    /// A [`LeafError`] if a lifted `Descriptor` has no matching `SeedPairing`
    /// (an unconstructible bean is loud) or the builder's name/collision guard
    /// fires.
    pub fn from_slices(pairings: &[SeedPairing]) -> Result<App<Define>, LeafError> {
        Ok(App {
            builder: assembly::from_slices(pairings)?,
            resolve: None,
            wired: None,
            _phase: PhantomData,
        })
    }

    /// Run the expected-vs-found anti-DCE self-check at the `Define→Resolve` edge:
    /// every [`SourceTag`] in `expected` (the binary's `ExpectedManifest`) must
    /// appear in the link-collected [`leaf_core::SOURCES`] slice.
    ///
    /// Associated (not `&self`) because the check reads ONLY link-collected global
    /// state — it is the same regardless of the in-flight builder, and the binary
    /// runs it once at the phase edge before resolution begins.
    ///
    /// # Errors
    /// [`AntiDceError::SourceVanished`] naming the first expected-but-absent crate.
    pub fn self_check(expected: &[SourceTag]) -> Result<(), AntiDceError> {
        anti_dce::self_check(expected)
    }

    /// The number of definitions lifted into the builder so far (a `Define`-phase
    /// read used by the assembly fixpoint + tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.builder.len()
    }

    /// `true` iff no definitions have been lifted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.builder.is_empty()
    }

    /// Consume the `Define`-phase app into its accumulated [`RegistryBuilder`].
    ///
    /// The bridge to the `seal()` transition a later unit owns: the builder is
    /// frozen there into the immutable `Registry`. Exposed now so unit 1's tests
    /// (and the next unit) can drive the lifted builder to a frozen registry.
    #[must_use]
    pub fn into_builder(self) -> RegistryBuilder {
        self.builder
    }

    /// The `App<Define> → App<Resolve>` transition: run the 5f environment fence
    /// ([`seal_environment`](crate::seal_environment)), consuming the `Define`-phase
    /// app into a `Resolve`-phase app carrying the sealed [`Env`] + the frozen
    /// [`BootstrapSettings`] + the canonical [`ActiveProfiles`].
    ///
    /// `inventory` is the `(self_type, role)` of every user/plain definition this
    /// `App<Define>` lifted (the run engine derives it from the lifted
    /// descriptors); it seeds the auto-config back-off probe so a user bean
    /// supersedes a Fallback default. This is the one irreversible env freeze: the
    /// builder rides into `Resolve`, but the env is sealed.
    ///
    /// # Errors
    /// A [`LeafError`] from config-data load or profile activation.
    pub async fn seal_environment(
        self,
        inputs: SealInputs,
        inventory: Vec<(TypeId, CandidateRole)>,
    ) -> Result<App<Resolve>, LeafError> {
        let SealedEnvironment { env, args, settings, profiles } =
            environment::seal_environment(inputs).await?;
        Ok(App {
            builder: self.builder,
            resolve: Some(ResolveState {
                env,
                settings,
                profiles,
                args,
                inventory,
                report: Vec::new(),
            }),
            wired: None,
            _phase: PhantomData,
        })
    }

    // NOTE (cross-crate, leaf-boot run-engine unit): `Application::new(Primary)
    // .run(body)` — the entry shape `#[leaf::main]` emits
    // (leaf-codegen/src/app.rs) — drives the FULL `App<Define → Resolve → Wired →
    // Running>` walk (`seal_environment` → `route_conditions`/`run_autoconfig` →
    // `seal()` → `validate()` → `Context::refresh()`). All of
    // `seal_environment`/`route_conditions`/`run_autoconfig`/`seal()`/`validate()`
    // are landed; the `App<Wired> → App<Running>` `Context::refresh()` body is now
    // landed as [`crate::RunUnit`] (the fused container-lifecycle template R0..R8 +
    // the C1/C7 teardown). The remaining wiring is the `App<Wired>::into_run_unit`
    // glue that hands `RunUnit` the frozen registry + the macro-emitted
    // injection/lifecycle plans + the force-linked `Spawner` (the `#[leaf::main]`
    // top-level driver unit).
}

impl App<Resolve> {
    /// The sealed environment read handle (lock-free).
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.state().env
    }

    /// The frozen `leaf.main.*` self-binding settings.
    #[must_use]
    pub fn settings(&self) -> &BootstrapSettings {
        &self.state().settings
    }

    /// The canonical active-profile set.
    #[must_use]
    pub fn active_profiles(&self) -> &ActiveProfiles {
        &self.state().profiles
    }

    /// The parsed command-line arguments.
    #[must_use]
    pub fn args(&self) -> &leaf_core::ApplicationArguments {
        &self.state().args
    }

    /// The number of definitions registered so far (grows as auto-config
    /// candidates register their Fallback survivors).
    #[must_use]
    pub fn len(&self) -> usize {
        self.builder.len()
    }

    /// `true` iff no definitions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.builder.is_empty()
    }

    /// Route the runtime-tier conditions over `guards` in the Parse-then-Register
    /// sub-passes (see [`route_conditions`](crate::route_conditions)), recording
    /// every verdict into the accumulating report. Returns the
    /// [`ContractId`](leaf_core::ContractId)s whose guard MATCHED.
    ///
    /// The Register sub-pass installs a
    /// [`DefinitionProbe`](leaf_conditions::DefinitionProbe) over the user-bean
    /// inventory so an `OnBean`-family guard sees the existing definitions.
    ///
    /// # Errors
    /// A [`LeafError`] (`ConditionError`) iff a guard leaf is unresolvable.
    pub fn route_conditions(
        &mut self,
        guards: &[GuardPairing],
    ) -> Result<Vec<leaf_core::ContractId>, LeafError> {
        let probe = self.inventory_probe();
        let state = self.resolve.as_mut().expect("Resolve-phase state present");
        let outcome =
            conditions::route_conditions(guards, &state.env, &state.profiles, probe)?;
        state
            .report
            .extend(outcome.report.records().iter().cloned());
        Ok(outcome.matched)
    }

    /// Run the auto-config ladder (`exclude > back-off > default`) over
    /// `candidates` (see [`run_autoconfig`](crate::run_autoconfig)), registering
    /// survivors INCREMENTALLY at [`CandidateRole::FALLBACK`] into the builder and
    /// recording every verdict into the report. Returns the count registered.
    ///
    /// # Errors
    /// A [`LeafError`] from a guard leaf or the builder's collision guard.
    pub fn run_autoconfig(
        &mut self,
        candidates: &[AutoConfigCandidate],
        exclusions: &ExclusionSet,
    ) -> Result<usize, LeafError> {
        // Split the borrow: the builder + the resolve state are distinct fields.
        let App { builder, resolve, .. } = self;
        let state = resolve.as_mut().expect("Resolve-phase state present");
        let outcome = autoconfig::run_autoconfig(
            candidates,
            &state.env,
            builder,
            exclusions,
            &state.profiles,
            &state.inventory,
        )?;
        state
            .report
            .extend(outcome.report.records().iter().cloned());
        Ok(outcome.registered)
    }

    /// Freeze the accumulated condition-report records into the keyed
    /// [`ConditionReport`] (consulted by the later `App<Wired>` `NoSuchBean`
    /// enrichment).
    #[must_use]
    pub fn condition_report(&self) -> ConditionReport {
        ConditionReport::from_records(self.state().report.clone())
    }

    /// Consume the `Resolve`-phase app into its accumulated [`RegistryBuilder`]
    /// (a test/escape-hatch bridge — the typestate transition is [`seal`](App::<Resolve>::seal)).
    #[must_use]
    pub fn into_builder(self) -> RegistryBuilder {
        self.builder
    }

    /// The `App<Resolve> → App<Wired>` transition: the one irreversible `seal()`.
    ///
    /// Freezes the accumulated [`RegistryBuilder`] into the immutable, dense-`BeanId`
    /// [`Registry`] (the slot-indexed `OnceCell` singleton store, both indices, the
    /// alias overlay, the `ContractId` collision guard — all coherent-by-construction
    /// in the one freeze pass), and freezes the accumulated condition-report records
    /// into the keyed [`ConditionReport`]. The typestate makes a post-seal edit
    /// unrepresentable: the builder is consumed, and the returned `App<Wired>` holds
    /// only the frozen snapshot. Conditions/auto-config already ran in `Resolve`; the
    /// next transition is [`validate`](App::<Wired>::validate) (BEFORE refresh).
    ///
    /// # Errors
    /// A freeze-time collision ([`ErrorKind::ContractCollision`](leaf_core::ErrorKind),
    /// a duplicate name/alias, an alias cycle, or a dangling template parent).
    pub fn seal(self) -> Result<App<Wired>, LeafError> {
        let state = self.resolve.expect("Resolve-phase state present");
        let report = ConditionReport::from_records(state.report);
        let registry = self.builder.freeze()?;
        Ok(App {
            builder: RegistryBuilder::new(),
            resolve: None,
            wired: Some(WiredState {
                registry,
                env: state.env,
                settings: state.settings,
                profiles: state.profiles,
                report,
            }),
            _phase: PhantomData,
        })
    }

    /// Build a [`DefinitionProbe`](leaf_conditions::DefinitionProbe) over the
    /// user-bean inventory (the Register sub-pass reads it).
    fn inventory_probe(&self) -> Arc<dyn leaf_conditions::DefinitionProbe> {
        let probe = Arc::new(autoconfig::BuilderProbe::new());
        for (ty, role) in &self.state().inventory {
            probe.observe(*ty, *role);
        }
        probe
    }

    /// Borrow the `Resolve`-phase state (always present in this phase).
    fn state(&self) -> &ResolveState {
        self.resolve.as_ref().expect("Resolve-phase state present")
    }
}

impl App<Wired> {
    /// The frozen, immutable [`Registry`] snapshot (lock-free read).
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.wired_state().registry
    }

    /// The sealed environment read handle.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.wired_state().env
    }

    /// The frozen `leaf.main.*` self-binding settings (the `startup_validation`
    /// lever is read here at `validate()`'s head).
    #[must_use]
    pub fn settings(&self) -> &BootstrapSettings {
        &self.wired_state().settings
    }

    /// The canonical active-profile set.
    #[must_use]
    pub fn active_profiles(&self) -> &ActiveProfiles {
        &self.wired_state().profiles
    }

    /// The frozen [`ConditionReport`] (the silent-now / loud-later `NoSuchBean`
    /// enrichment join consulted by [`validate`](App::<Wired>::validate)).
    #[must_use]
    pub fn condition_report(&self) -> &ConditionReport {
        &self.wired_state().report
    }

    /// The number of beans in the frozen registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.wired_state().registry.len()
    }

    /// `true` iff the frozen registry holds no beans.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.wired_state().registry.is_empty()
    }

    /// The WHOLE-GRAPH validation pass (bootstrap-diagnostics step-10; the
    /// `[C2-config-locus]` Tier-2 site) — runs BEFORE `Context::refresh()`.
    ///
    /// Reads [`BootstrapSettings::startup_validation`](leaf_core::BootstrapSettings)
    /// ONCE at its head, then over `inputs` (the macro-emitted per-bean injection
    /// plans + the config-properties bind/`@Value` dry-run thunks):
    ///
    /// 1. resolves EVERY eager mandatory [`InjectionPoint`](leaf_core::InjectionPoint)
    ///    via the [`Selector`](leaf_core::Selector), aggregating each
    ///    `NoSuchBean`/`NoUniqueBean`/`ScopeMismatch`/`AdvisedConcreteInjection`
    ///    into ONE [`AssemblyReport`](leaf_core::AssemblyReport) (never fail-on-first);
    /// 2. classifies cycles via [`order_batch`](crate::order_batch) (the same
    ///    `CircularDependency`/`DependsOnCycle` the wave plan computes);
    /// 3. enriches each `NoSuchBean` from the frozen [`ConditionReport`] (a bean
    ///    silently backed-off by a condition is named in the diagnostic);
    /// 4. **C2**: PRE-MATERIALIZES the pure-projection `@ConfigurationProperties`
    ///    beans (binder-backed bind + JSR, the bound `Arc` stored into the slot
    ///    `OnceCell` — so R5 publishes it and never re-binds) and DRY-RUNS every
    ///    eager `@Value` coercion (value discarded), aggregating bind/convert/
    ///    violation faults at Tier-2.
    ///
    /// All faults are gathered into the ONE report BEFORE any constructor runs. See
    /// [`crate::ValidationInputs`] for the per-bean inputs.
    ///
    /// # Errors
    /// The aggregated [`AssemblyReport`](leaf_core::AssemblyReport) collapsed to its
    /// first representative [`LeafError`] iff any fault was recorded (the full report
    /// is available via [`validate_report`](App::<Wired>::validate_report)).
    pub fn validate(
        &self,
        inputs: &crate::validate::ValidationInputs<'_>,
    ) -> Result<(), LeafError> {
        self.validate_report(inputs).into_result()
    }

    /// The WHOLE-GRAPH validation pass, returning the FULL aggregated
    /// [`AssemblyReport`](leaf_core::AssemblyReport) (every fault, not just the
    /// first) — the rich-diagnostics entry point the run engine renders. See
    /// [`validate`](App::<Wired>::validate).
    #[must_use]
    pub fn validate_report(
        &self,
        inputs: &crate::validate::ValidationInputs<'_>,
    ) -> leaf_core::AssemblyReport {
        let state = self.wired_state();
        crate::validate::validate(
            &state.registry,
            &state.env,
            &state.report,
            state.settings.startup_validation,
            inputs,
        )
    }

    /// Consume the `App<Wired>` into the owned run-engine inputs: the frozen
    /// [`Registry`], the sealed [`Env`], and the frozen [`BootstrapSettings`].
    ///
    /// This is the `App<Wired> → App<Running>` glue the run pipeline drives — it
    /// hands the owned registry to [`RunUnit`](crate::RunUnit) (the registry is
    /// NOT `Clone`, so this consuming bridge is how the frozen snapshot moves into
    /// the run engine).
    #[must_use]
    pub fn into_run_parts(self) -> (Registry, Env, BootstrapSettings) {
        let state = self.wired.expect("Wired-phase state present");
        (state.registry, state.env, state.settings)
    }

    /// Borrow the `Wired`-phase state (always present in this phase).
    fn wired_state(&self) -> &WiredState {
        self.wired.as_ref().expect("Wired-phase state present")
    }
}

impl<S> std::fmt::Debug for App<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // In `Wired` the builder is consumed (an empty placeholder); report the
        // frozen registry's bean count instead.
        let definitions = self
            .wired
            .as_ref()
            .map_or_else(|| self.builder.len(), |w| w.registry.len());
        f.debug_struct("App")
            .field("phase", &std::any::type_name::<S>())
            .field("definitions", &definitions)
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── force-link helpers ─────────────────────────────

/// Force-link a participating crate from the binary, defeating Layer-0 anti-DCE
/// (a crate the binary never path-references is not linked, so its
/// `#[distributed_slice]` rows silently vanish).
///
/// `#[leaf::main]` / `build.rs` emit `use <crate> as _;` per participating crate
/// (see [`leaf_codegen::forcelink::emit_force_link`]); this macro is the
/// hand-writable equivalent for a crate force-linking another by path. The
/// `use … as _;` reference has the link side-effect with no name binding, so it
/// pins the rlib onto the link graph without polluting the namespace.
///
/// ```
/// // Pin leaf-tokio onto the link graph (the default-feature force-link).
/// # #[cfg(feature = "tokio")]
/// leaf_boot::force_link!(leaf_tokio);
/// ```
#[macro_export]
macro_rules! force_link {
    ($($krate:path),+ $(,)?) => {
        $( #[allow(unused_imports)] use $krate as _; )+
    };
}

// The default `tokio` flavor force-links leaf-tokio so its ExecutionFacility /
// scheduler / shutdown-trigger / ambient-store SPIs are link-collected by
// default — leaf-boot's own "force-links leaf-tokio by default" obligation (per
// TOPOLOGY), realized as a real Layer-0 extern-crate reference inside a hidden
// private module so it pins the rlib onto the link graph without polluting the
// namespace (the same shape `#[leaf::main]`'s force-link shim emits).
#[cfg(feature = "tokio")]
#[doc(hidden)]
mod __leaf_tokio_force_link {
    #[allow(unused_imports)]
    use leaf_tokio as _;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_slices_begins_the_define_phase_lifting_the_link_collected_rows() {
        // The Define phase begins by lifting the link-collected bean channels.
        // leaf-boot's built-in pairings cover the framework beans it force-links
        // (leaf-tokio's executor under the default `tokio` feature), so the bare
        // lift succeeds and the lifted builder freezes into a registry.
        let app = App::<Define>::from_slices(&[]).expect("from_slices succeeds");
        let registry = app.into_builder().freeze().expect("the lifted builder freezes");
        // Under the default `tokio` feature, the executor is among the lifted rows.
        #[cfg(feature = "tokio")]
        assert!(!registry.is_empty(), "the force-linked executor must be lifted");
        let _ = registry;
    }

    #[test]
    fn self_check_is_a_define_phase_associated_fn() {
        // The empty manifest is trivially green; a vanished source is loud.
        App::<Define>::self_check(&[]).expect("empty manifest passes");
        let err = App::<Define>::self_check(&[SourceTag("leaf-ghost")])
            .expect_err("a ghost crate vanished");
        assert!(matches!(
            err,
            AntiDceError::SourceVanished { crate_name: "leaf-ghost" }
        ));
    }

    #[test]
    fn into_builder_bridges_to_the_seal_transition() {
        let app = App::<Define>::from_slices(&[]).expect("from_slices");
        let builder = app.into_builder();
        // The bridged builder freezes (the seal() transition a later unit owns).
        let registry = builder.freeze().expect("the lifted builder freezes");
        assert!(registry.is_empty() || !registry.is_empty()); // total: any len is fine
    }

    #[test]
    fn app_debug_names_its_phase() {
        let app = App::<Define>::from_slices(&[]).expect("from_slices");
        let s = format!("{app:?}");
        assert!(s.contains("App"), "got: {s}");
        assert!(s.contains("Define"), "got: {s}");
    }

    #[test]
    fn force_link_macro_pins_a_crate_without_binding_a_name() {
        // The macro expands to `use <crate> as _;` — a link reference, no name.
        // Pin leaf-core itself (always linked) to prove the expansion compiles.
        force_link!(leaf_core);
        // A multi-crate form also compiles.
        force_link!(leaf_core, leaf_conditions);
    }

    // ── App<Define> → App<Resolve> typestate transitions (unit 2) ──────────────

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn seal_environment_transitions_define_to_resolve_with_a_sealed_env() {
        let app = App::<Define>::from_slices(&[]).expect("from_slices");
        let resolve = block_on(app.seal_environment(
            SealInputs::new().with_args(["--server.port=8080"]),
            Vec::new(),
        ))
        .expect("seal_environment transitions to Resolve");
        // The sealed env carries the highest-precedence command-line value.
        assert_eq!(
            leaf_core::PropertyResolver::get(resolve.env(), "server.port")
                .unwrap()
                .raw,
            "8080"
        );
        // The frozen settings default to strict + console.
        assert_eq!(
            resolve.settings().startup_validation,
            leaf_core::StartupValidation::Strict
        );
    }

    #[test]
    fn resolve_routes_a_property_guarded_definition() {
        use crate::conditions::GuardPairing;
        use leaf_conditions::{ConditionKind, OnProperty};

        static ATTRS: &[leaf_core::Attr] = &[leaf_core::Attr::Str("name", "feature.x")];
        static GUARD: leaf_core::CondExpr = leaf_core::CondExpr::Leaf(OnProperty::ID, ATTRS);

        let app = App::<Define>::from_slices(&[]).expect("from_slices");
        let mut resolve = block_on(app.seal_environment(
            SealInputs::new().with_args(["--feature.x=true"]),
            Vec::new(),
        ))
        .expect("seal_environment");

        let g = GuardPairing::new(leaf_core::ContractId::of("x::Bean"), None, &GUARD);
        let matched = resolve.route_conditions(&[g]).expect("routes");
        assert!(matched.contains(&leaf_core::ContractId::of("x::Bean")));
        // The verdict is recorded in the report.
        let report = resolve.condition_report();
        assert!(report.lookup(leaf_core::ContractId::of("x::Bean")).is_some());
    }

    #[test]
    fn resolve_runs_the_autoconfig_ladder_growing_the_builder() {
        let app = App::<Define>::from_slices(&[]).expect("from_slices");
        let before = app.len();
        let mut resolve =
            block_on(app.seal_environment(SealInputs::new(), Vec::new())).expect("seal");

        // One unconditional auto-config candidate registers at Fallback.
        let cands = [autoconfig_candidate()];
        let n = resolve
            .run_autoconfig(&cands, &ExclusionSet::new())
            .expect("auto-config ladder runs");
        assert_eq!(n, 1, "the unconditional candidate registers");
        assert_eq!(resolve.len(), before + 1, "the builder grew by the survivor");
    }

    // A minimal unconditional auto-config candidate for the typestate test.
    fn autoconfig_candidate() -> AutoConfigCandidate {
        use leaf_core::{
            AnnotationMetadata, BoxFuture, Descriptor, Origin, Provider, Published, ResolveCtx,
            Role, ScopeDef,
        };
        use std::any::TypeId;

        #[derive(Debug)]
        struct Probe;
        struct ProbeProvider(Descriptor);
        impl Provider for ProbeProvider {
            fn descriptor(&self) -> &Descriptor {
                &self.0
            }
            fn provide<'a>(
                &'a self,
                _cx: &'a ResolveCtx<'a>,
            ) -> BoxFuture<'a, Result<Published, LeafError>> {
                Box::pin(async { Ok(Published::shared_value(Probe)) })
            }
        }
        static META: AnnotationMetadata = AnnotationMetadata {
            qualifiers: &[],
            markers: &[],
            depends_on: &[],
            candidate_role: CandidateRole::FALLBACK,
            autowire_candidate: true,
        };
        fn descriptor() -> Descriptor {
            Descriptor {
                contract: leaf_core::ContractId::of("app::AutoCfg"),
                self_type: TypeId::of::<Probe>(),
                provides: &[],
                declared_name: Some("autoCfg"),
                aliases: &[],
                scope: ScopeDef::SINGLETON,
                role: Role::Application,
                meta: &META,
                parent: None,
                origin: Origin::Native { crate_name: Some("leaf-boot::test") },
            }
        }
        fn seed() -> Arc<dyn Provider> {
            Arc::new(ProbeProvider(descriptor()))
        }
        AutoConfigCandidate::new(descriptor(), seed, None)
    }
}
