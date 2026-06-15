//! Link-time discovery slices + the programmatic [`Registrar`] SPI.
//!
//! THE maximal-magic registration substrate (ADR-09 cross-crate-discovery,
//! discovery-codegen phase3/02): one `linkme` distributed-slice family, many
//! typed channels, collected at link time with **zero life-before-main and zero
//! runtime collection cost**. A crate's `#[component]`/`#[bean]`/`#[service]`/…
//! macros emit one const row per declaration into the matching slice via
//! absolute `::leaf_core` paths; the binary crate force-links every
//! participating crate (`use <crate> as _;`), and the cold `App<Define→Resolve>`
//! assembly pass lifts the rows through [`RegistryBuilder`](crate::RegistryBuilder)`::from_slices` (a later
//! unit) and seals the dense `BeanId` registry.
//!
//! ## Why `linkme`, and the three things that can silently delete a row
//!
//! `linkme` places each `#[distributed_slice(FOO)]` element as a `#[used]`
//! `#[link_section]` static; the linker concatenates the same-named sections
//! across every linked rlib and auto-defines the boundary symbols, so the slice
//! is materialized as a contiguous `&'static [T]` before `main` with no
//! constructor and no global lock (the anti-`inventory`, async-first choice).
//! The price is that the SAME row can vanish at three independent layers, and a
//! dropped row is a SILENT empty iterable, never a link error
//! (rust-cross-crate-composition §345–346):
//!
//! - **Layer 0 — linkage.** A crate the binary never path-references is not
//!   linked at all (`use <crate> as _;` force-link is the fix; owned by the
//!   binary crate's `#[leaf::main]`/`build.rs`).
//! - **Layer A — rustc reachability** (mostly fixed in rustc ≥ 1.62).
//! - **Layer B — `--gc-sections`/LLD `start-stop-gc`** (ELF default fixed in
//!   Rust 1.89 via `#[used(linker)]`/`SHF_GNU_RETAIN`; `-z nostart-stop-gc` is
//!   the pre-1.89 escape hatch).
//!
//! The headline defense is the expected-vs-found [`SourceTag`] self-check (a
//! later unit's `RegistryBuilder::self_check`): a crate that appears in the
//! `ExpectedManifest` but contributes zero rows becomes a LOUD
//! [`ErrorKind::AntiDce`](crate::ErrorKind::AntiDce), never a confusing
//! `NoSuchBean`. **Ordering is NEVER read from the slice** (link/section order is
//! unspecified and may be randomized); the freeze computes one canonical total
//! order from the stable [`ContractId`].
//!
//! ## One substrate, many typed channels
//!
//! Each channel is a separate `#[distributed_slice]` carrying one const row type
//! per kind. The bean channels ([`COMPONENTS`]/[`AUTO_CONFIGS`]) reuse the const
//! [`Descriptor`] verbatim. The run-participant + cross-cutting channels each
//! carry a minimal const **descriptor row** here; later units that own a kind
//! flesh out its row (every field is `&'static`/`Copy`, so the slices stay
//! const-constructible and additive). The macro never accumulates state — it
//! only emits one hand-writable row per call site, exactly as a downstream
//! author could write by hand.
//!
//! ## Macros reference `linkme` THROUGH leaf-core
//!
//! [`linkme`] is re-exported so emitted code writes
//! `::leaf_core::linkme::distributed_slice` (plus `#[linkme(crate =
//! ::leaf_core::linkme)]` so linkme's runtime types resolve there too), never a
//! bare `::linkme` (which would force every contributing crate to declare a
//! `linkme` dependency). This is the single pinned linkme path the whole workspace
//! shares.

// `#[linkme::distributed_slice]` expands to a `#[used]` `#[link_section = "…"]`
// static; the section override trips the crate-level `deny(unsafe_code)`. This
// is the ONE genuinely-required exception (link-time collection has no safe
// stable equivalent), so it is allowed HERE only, scoped to this module. No
// hand-written `unsafe` block exists in this file — only the macro-generated
// section attributes.
#![allow(unsafe_code)]

use crate::bind::ConfigBindThunk;
use crate::conditions::CondExpr;
use crate::definition::{Descriptor, Role};
use crate::error::{FailureAnalyzer, LeafError, Origin};
use crate::handle::ErasedBean;
use crate::identity::{ContractId, MarkerId};
use crate::injection::InjectionPlan;
use crate::order::OrderKey;
use crate::provider::ProviderSeed;
use crate::proxy::{BeanJoinPointsSpec, MakeInterceptor, MethodTable, Pointcut};

/// The pinned `linkme` re-export — emitted code references
/// `::leaf_core::linkme`, never a bare `::linkme`.
///
/// A `#[component]`/`#[bean]`/… macro hard-codes BOTH
/// `#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]` (the
/// attribute macro by its fully-qualified re-export path) AND
/// `#[linkme(crate = ::leaf_core::linkme)]` (linkme's supported `crate =`
/// override, so its runtime types — `DistributedSlice`, the `__private` module,
/// `Void` — also resolve through this re-export instead of a bare `::linkme`).
/// The override is load-bearing: without it the element expansion emits
/// `::linkme::…` and fails with `E0433: cannot find linkme in the crate root`.
/// With both, a contributing crate needs ONLY a `leaf-core` dependency, never its
/// own pin on `linkme`. There is exactly one `linkme` version in the workspace
/// (the BOM in `[workspace.dependencies]`), so all slices agree on the layout.
///
/// This re-export must stay `pub` (not `#[doc(hidden)] __rt`): the emitted
/// attribute paths name it directly, and a `#[doc(hidden)] pub use` would resolve
/// equally but is reserved for items users should never type — whereas this is
/// the documented `::leaf_core::linkme` path the macro emits. Re-exporting it is a
/// purely ADDITIVE, non-breaking change to the frozen leaf-core ABI.
pub use linkme;

// ─────────────────────── anti-DCE source identity ───────────────────────────

/// Stable per-crate identity stamped on every contributed row and emitted once
/// per participating crate into [`SOURCES`] (ADR-09 Defense MANIFEST).
///
/// `SourceTag(crate_name)` is the anchor the expected-vs-found self-check joins
/// on: a tag present in the binary's `ExpectedManifest` but absent from the
/// link-collected [`SOURCES`] means the crate was never linked
/// ([`ErrorKind::AntiDce`](crate::ErrorKind::AntiDce), `NotForceLinked`);
/// present-in-`SOURCES`-but-contributing-zero-rows means a section-GC drop. It is
/// the stable Cargo package name (an author-stable string), NOT a `TypeId`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SourceTag(pub &'static str);

/// `leaf_core::declare_source!("leaf-redis")` — submit exactly ONE per-crate
/// [`SourceTag`] into the link-collected [`SOURCES`] slice, so the binary's
/// expected-vs-found anti-DCE self-check ([`anti_dce::self_check`](crate)) can tell
/// "linked-but-zero-rows" from "never-linked" (ADR-09 Defense MANIFEST,
/// bootstrap-diagnostics phase3/14).
///
/// Every PARTICIPATING crate (each runtime integration / concern crate that
/// contributes `COMPONENTS`/`AUTO_CONFIGS` rows but is only reachable via a binary
/// force-link) calls this ONCE in its crate root with its author-stable Cargo
/// PACKAGE name (the same string a [`SourceTag`] / the binary's `ExpectedManifest`
/// carries — dashes, NOT the underscore crate ident):
///
/// ```ignore
/// // in leaf-redis/src/lib.rs
/// leaf_core::declare_source!("leaf-redis");
/// ```
///
/// The submission rides the SAME `#[distributed_slice]` mechanism every per-bean row
/// uses, so it survives — or vanishes WITH — that crate's rows: if `--gc-sections`
/// drops the crate's section the tag goes with it, and the self-check reports the
/// crate as [`SourceVanished`](crate). The emitted static is named `__LEAF_SOURCE`,
/// so a SECOND `declare_source!` in the same crate root is a duplicate-definition
/// COMPILE error — the once-per-crate contract is structural, not a convention.
///
/// The `$crate::linkme` path + `#[linkme(crate = $crate::linkme)]` override route
/// through the leaf-core re-export, so a participating crate names only `leaf_core`
/// (or the umbrella facade alias), never a bare `linkme` dependency.
#[macro_export]
macro_rules! declare_source {
    ($name:expr) => {
        // One per-crate anti-DCE anchor in SOURCES, keyed by the package name. The
        // fixed static ident makes a second invocation in the same crate a loud
        // duplicate-definition error (the once-per-crate contract, enforced).
        #[allow(non_upper_case_globals)]
        #[$crate::linkme::distributed_slice($crate::SOURCES)]
        #[linkme(crate = $crate::linkme)]
        static __LEAF_SOURCE: $crate::SourceTag = $crate::SourceTag($name);
    };
}

// ─────────────────────── programmatic registration ──────────────────────────

/// The minimal forward-compatible context handed to a [`Registrar`].
///
/// Scope note: a placeholder for the programmatic-registration unit, mirroring
/// the [`ResolveCtx`](crate::provider::ResolveCtx) pattern. The registry/boot
/// units flesh it out (`builder: &mut RegistryBuilder`, `env: &Env`,
/// `register_bean`/`register_alias`), all behind `#[non_exhaustive]` so adding
/// fields is not a breaking change. The lifetime is kept so adding borrowed
/// fields later is non-breaking; the `&mut` is sound because registrars run
/// STRICTLY in the cold pre-`seal()` assembly phase (extra-5), never
/// mid-instantiation.
#[non_exhaustive]
pub struct RegistrarCtx<'a> {
    // A private marker binds `'a` so the public signature is stable before the
    // builder/env borrows land. Zero-sized.
    _marker: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> RegistrarCtx<'a> {
    /// A root registrar context with no builder/env bound yet (tests + the bare
    /// assembly pass before the registry-builder unit lands).
    #[must_use]
    pub fn root() -> Self {
        RegistrarCtx { _marker: std::marker::PhantomData }
    }
}

impl Default for RegistrarCtx<'_> {
    fn default() -> Self {
        RegistrarCtx::root()
    }
}

impl std::fmt::Debug for RegistrarCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrarCtx").finish_non_exhaustive()
    }
}

/// The ONE hand-writable programmatic-registration SPI: any
/// `fn(&mut RegistrarCtx)` run in the cold definition phase
/// (programmatic-registration, discovery-codegen phase3/02).
///
/// A registrar lowers each `register_bean` to one [`Descriptor`] + `Provider`
/// indistinguishable from a macro-emitted bean (the codegen-boundary escape
/// hatch — what every macro emits, by hand). It is reachable two ways:
/// `App::with_registrar(r)` (the Architecture-D escape hatch) OR a
/// `&'static dyn Registrar` row in the [`REGISTRARS`] slice via `#[import(..)]`.
///
/// `Send + Sync` because a `&'static dyn Registrar` rides the link-collected
/// slice and the cold assembly pass may run on the executor; `'static` because
/// the slice holds `&'static` rows. The error is the one [`LeafError`] chain (a
/// later unit aliases a `RegisterError` newtype over it).
pub trait Registrar: Send + Sync {
    /// Contribute definitions to the builder during the cold assembly phase.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if a definition is malformed, a name/contract
    /// collides, or (a later unit) registration is attempted after `seal()`.
    fn register(&self, cx: &mut RegistrarCtx<'_>) -> Result<(), LeafError>;
}

// ─────────────────── minimal const rows owned by later units ─────────────────
//
// Each row below is the minimal const ABI for a channel whose owning unit has
// not landed yet. Every field is `&'static`/`Copy` so the row is
// const-constructible into its slice; the owning unit ADDS fields later (the
// rows are deliberately NON-exhaustive-by-convention — downstream const sites
// build them through a `..Default` analogue once more fields exist). The
// `contract` (and, where ordered, `order`) fields are present from the start
// because every run-participant stream is `ContractId`-keyed and `cmp_order`-
// ordered (bootstrap-diagnostics phase3/14, ADR-09 line 117).

/// One `#[condition]`/`@Conditional` implementation row (conditions-autoconfig,
/// phase3/05). Minimal until the conditions unit lands the const `CondExpr`
/// tree; carries the stable identity the registry keys gating on.
#[derive(Clone, Copy, Debug)]
pub struct ConditionRow {
    /// Stable cross-build identity of the condition implementation.
    pub contract: ContractId,
    /// The condition's marker (the `ConditionId` analogue) for report grouping.
    pub marker: MarkerId,
}

/// One stereotype meta-edge row (component-stereotypes, phase3/02) — populated
/// ONLY for the optional runtime-closure fallback; the default path flattens the
/// transitive marker set at macro time into `Descriptor.meta.markers`.
#[derive(Clone, Copy, Debug)]
pub struct StereotypeRow {
    /// Stable cross-build identity of the stereotype.
    pub contract: ContractId,
    /// The stereotype's own marker.
    pub marker: MarkerId,
    /// The transitive markers this stereotype implies (e.g. `@RestController`
    /// ⇒ `[CONTROLLER, COMPONENT]`).
    pub implies: &'static [MarkerId],
}

/// One `@EventListener` row (events, phase3/12) — emitted per listener method,
/// collected exactly like [`COMPONENTS`]. Minimal until the events unit lands
/// the event-type/condition fields.
#[derive(Clone, Copy, Debug)]
pub struct EventListenerRow {
    /// Stable cross-build identity of the listener.
    pub contract: ContractId,
    /// Dispatch order (lower = earlier), read via the one
    /// [`cmp_order`](crate::cmp_order).
    pub order: OrderKey,
}

/// One declarative-advice / advisor row (declarative-advice, phase3/09;
/// proxy-interception, phase3/08). Minimal until the AOP unit lands the
/// pointcut/`RoleTier` fields the chain sort reads via
/// [`cmp_chain`](crate::cmp_chain).
#[derive(Clone, Copy, Debug)]
pub struct AdvisorRow {
    /// Stable cross-build identity of the advisor.
    pub contract: ContractId,
    /// Chain order (lower = outermost), read via the one ordering law.
    pub order: OrderKey,
}

/// One `@Scheduled` task row (scheduling). Minimal until the scheduling unit
/// lands the trigger/cron fields.
#[derive(Clone, Copy, Debug)]
pub struct ScheduledRow {
    /// Stable cross-build identity of the scheduled task.
    pub contract: ContractId,
}

/// One converter/formatter catalog row (binding-conversion). Minimal until the
/// conversion unit lands the source/target `TypeId` fields.
#[derive(Clone, Copy, Debug)]
pub struct CatalogRow {
    /// Stable cross-build identity of the catalog entry.
    pub contract: ContractId,
}

/// One static resource-bundle / classpath-resource row
/// (expr-i18n-resources, phase3/11). Minimal until the resources unit lands the
/// location/pattern fields.
#[derive(Clone, Copy, Debug)]
pub struct ResourceRow {
    /// Stable cross-build identity of the resource contribution.
    pub contract: ContractId,
    /// The resource location/pattern this row contributes.
    pub location: &'static str,
}

/// One `@ConfigurationProperties` metadata row (config-metadata) — the
/// build-time-emitted config key documentation/binding hints. Minimal until the
/// config unit lands the key/type fields.
#[derive(Clone, Copy, Debug)]
pub struct ConfigMetadataRow {
    /// Stable cross-build identity of the config-properties bean.
    pub contract: ContractId,
    /// The canonical config key prefix this row documents.
    pub prefix: &'static str,
}

// ───────────────── the per-bean WIRING-PAIRING channel rows ──────────────────
//
// THE per-bean wiring metadata is 100% macro-emitted: beside each bean's
// `Descriptor`, the `#[component]`/`#[advisable]`/`#[runner]`/`#[config_properties]`
// /`#[conditional]` macros emit `pub` pairing consts (`__leaf_seed_`/`__leaf_guard_`
// /`__leaf_joinpoints_`/`__leaf_methods_`/`__leaf_runner_upcast_`/`__leaf_config_bind_`
// + the per-bean `InjectionPlan`), each JOINed back to its bean by `ContractId`.
//
// These rows are the const-compatible TWINS of leaf-boot's `*Pairing` structs (the
// runtime contract leaf-boot's `from_slices`/route/proxy/validate passes consume):
// each carries the bean's `ContractId` + the const fn-ptr / `&'static` ref / data,
// so the macro can submit one row per declaration into the matching channel via the
// SAME `#[distributed_slice(::leaf_core::<SLICE>)]` + `#[linkme(crate =
// ::leaf_core::linkme)]` pattern as the `COMPONENTS` submission. The channel
// auto-collects them at link time exactly like `COMPONENTS`/`AUTO_CONFIGS`, so a
// normal annotated app wires itself with NO hand-assembled `.with_seeds`/… calls
// (those `.with_*` builders STAY as explicit escape hatches that ADD to the
// slice-collected set — charter §2.10 — but are not required). The rows live HERE
// (not in leaf-boot) so the macro references one stable `::leaf_core::<SLICE>` path
// and a contributing crate needs ONLY a `leaf-core` dependency.
//
// Every field is `Copy` (`ContractId`, a `fn` pointer, or a `&'static` ref / const
// `InjectionPlan`) so the row is const-constructible into its slice and the one
// `collect_slice` read idiom applies uniformly.

/// The runner-upcast thunk a `#[runner]` bean emits (`__leaf_runner_upcast_<Ident>`):
/// recovers a callable `Arc<dyn Runner>` from the resolved [`ErasedBean`]. Mirrors
/// leaf-boot's `RunnerUpcast` (the `fn` type is identical regardless of where the
/// alias lives), kept here so the [`RunnerPairingRow`] is a leaf-core type.
pub type RunnerUpcastFn =
    fn(ErasedBean) -> Option<std::sync::Arc<dyn crate::Runner>>;

/// One macro-emitted bean → [`ProviderSeed`] pairing (the const twin of leaf-boot's
/// `SeedPairing`), keyed by the bean's stable [`ContractId`]. Emitted by every
/// `#[component]`/`#[bean]`/`#[service]`/… beside its `Descriptor` and auto-collected
/// into [`SEED_PAIRINGS`]; leaf-boot's `from_slices` JOINs each `COMPONENTS` row to
/// its construction recipe by `contract`.
#[derive(Clone, Copy)]
pub struct SeedPairingRow {
    /// The stable cross-build identity of the bean this seed constructs.
    pub contract: ContractId,
    /// The const fn-pointer that BUILDS the bean's `Provider`
    /// (the macro-emitted `__leaf_seed_<Ident>`).
    pub seed: ProviderSeed,
    /// `true` iff this row is the recipe an `#[inject]` CONSTRUCTOR emits (the
    /// `#[advisable] impl { #[inject] fn new(..) }` path), as opposed to the struct
    /// stereotype's FIELD-DEFAULT recipe (`false`). The two can ride this slice for
    /// the SAME `contract` (a stateful/mixed bean wears both); leaf-boot's seed-index
    /// selects the constructor row over the field-default (the merge precedence).
    pub from_constructor: bool,
}

impl SeedPairingRow {
    /// The struct stereotype's FIELD-DEFAULT seed row (the recipe that injects every
    /// field). The `from_constructor` precedence flag is `false` — the constructor
    /// row, if present for this contract, wins the merge.
    #[must_use]
    pub const fn field_default(contract: ContractId, seed: ProviderSeed) -> Self {
        SeedPairingRow { contract, seed, from_constructor: false }
    }

    /// The `#[inject]` CONSTRUCTOR's seed row (its parameters are the injection
    /// points; its body seeds state). `from_constructor` is `true`, so it WINS the
    /// merge over the struct field-default row for the same `contract`.
    #[must_use]
    pub const fn from_constructor(contract: ContractId, seed: ProviderSeed) -> Self {
        SeedPairingRow { contract, seed, from_constructor: true }
    }
}

/// One macro-emitted bean → [`InjectionPlan`] pairing (the per-bean construction-edge
/// plan), keyed by [`ContractId`] and auto-collected into [`INJECTION_PLAN_PAIRINGS`].
/// leaf-boot's wave planner consults it (defaulting to [`InjectionPlan::EMPTY`] — the
/// no-collaborator POJO case) to order construction.
#[derive(Clone, Copy)]
pub struct InjectionPlanPairingRow {
    /// The stable cross-build identity of the bean this plan constructs.
    pub contract: ContractId,
    /// The const per-bean injection plan (one `InjectionPoint` per dependency).
    pub plan: InjectionPlan,
    /// `true` iff this plan is the `#[inject]` CONSTRUCTOR's (its parameters are the
    /// points), as opposed to the struct stereotype's FIELD-DEFAULT plan (`false`).
    /// Mirrors [`SeedPairingRow::from_constructor`]: leaf-boot's injection-plan
    /// resolver selects the constructor plan over the field-default for one `contract`.
    pub from_constructor: bool,
}

impl InjectionPlanPairingRow {
    /// The struct stereotype's FIELD-DEFAULT injection plan (one point per field).
    #[must_use]
    pub const fn field_default(contract: ContractId, plan: InjectionPlan) -> Self {
        InjectionPlanPairingRow { contract, plan, from_constructor: false }
    }

    /// The `#[inject]` CONSTRUCTOR's injection plan (one point per ctor parameter),
    /// which WINS the merge over the struct field-default for the same `contract`.
    #[must_use]
    pub const fn from_constructor(contract: ContractId, plan: InjectionPlan) -> Self {
        InjectionPlanPairingRow { contract, plan, from_constructor: true }
    }
}

/// One macro-emitted gated-element → [`CondExpr`] guard pairing (the const twin of
/// leaf-boot's `GuardPairing`), keyed by [`ContractId`] and auto-collected into
/// [`GUARD_PAIRINGS`]. Emitted by `#[conditional]`/`#[profile]` beside the gated
/// element's `Descriptor`; leaf-boot's condition routing JOINs each by `contract`.
///
/// NOTE the row carries the guard tree but NOT a `TypeId` (`TypeId::of` is fine in an
/// inline `const {}` block, but the row stays minimal — the `ContractId` is the JOIN
/// key and leaf-boot recovers the element `TypeId` from its frozen `Descriptor`).
#[derive(Clone, Copy)]
pub struct GuardPairingRow {
    /// The gated element's stable cross-build identity (the JOIN + report key).
    pub contract: ContractId,
    /// The const guard tree (the macro-emitted `__leaf_guard_<Ident>`).
    pub guard: &'static CondExpr,
}

/// One macro-emitted advisable-bean → [`BeanJoinPointsSpec`] pairing (the const twin
/// of leaf-boot's `JoinPointPairing`), keyed by [`ContractId`] and auto-collected into
/// [`JOINPOINT_PAIRINGS`]. Emitted by `#[advisable]`/`#[aspect]` beside the bean's
/// `Descriptor`; the proxy-assembly pass reifies it into the runtime join points the
/// `ProxyPlan` runs pointcuts over.
#[derive(Clone, Copy)]
pub struct JoinPointPairingRow {
    /// The advisable bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted const per-bean join-point spec (`__leaf_joinpoints_<Ident>`).
    pub spec: &'static BeanJoinPointsSpec,
}

/// One macro-emitted advised-bean → [`MethodTable`] pairing (the const twin of
/// leaf-boot's `MethodTablePairing`), keyed by [`ContractId`] and auto-collected into
/// [`METHOD_TABLE_PAIRINGS`]. Emitted by `#[advisable]`/`#[aspect]` beside the bean's
/// `Descriptor`; the auto-proxy install JOINs each by `contract` so an advised call
/// terminates in the matching downcast-and-invoke thunk.
#[derive(Clone, Copy)]
pub struct MethodTablePairingRow {
    /// The advised bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted const per-bean method table (`__leaf_methods_<Ident>`).
    pub table: &'static MethodTable,
}

/// One macro-emitted runner-bean → [`RunnerUpcastFn`] pairing (the const twin of
/// leaf-boot's `RunnerPairing`), keyed by [`ContractId`] and auto-collected into
/// [`RUNNER_PAIRINGS`]. Emitted by `#[runner]` beside the bean's `Descriptor`; the run
/// pipeline auto-collects the live `dyn Runner` candidates, JOINs each by `contract`,
/// and upcasts the resolved bean — so a `#[runner]` runs with NO explicit `with_runner`.
#[derive(Clone, Copy)]
pub struct RunnerPairingRow {
    /// The runner bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted upcast thunk (`__leaf_runner_upcast_<Ident>`).
    pub upcast: RunnerUpcastFn,
    /// The runner's stream order (lower-value-first; the `cmp_order` sort key).
    pub order: OrderKey,
}

/// One macro-emitted config-bean → [`ConfigBindThunk`] pairing (the const twin of
/// leaf-boot's `ConfigPairing`), keyed by [`ContractId`] and auto-collected into
/// [`CONFIG_BIND_PAIRINGS`]. Emitted by `#[config_properties]` beside the bean's
/// `Descriptor`; the C2 validate sub-pass JOINs each by `contract` and threads the
/// thunk as the real bind recipe (pre-materializing the bean into its slot).
#[derive(Clone, Copy)]
pub struct ConfigBindPairingRow {
    /// The config bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted pure-projection bind+JSR thunk (`__leaf_config_bind_<Ident>`).
    pub thunk: ConfigBindThunk,
}

/// One macro-emitted advisor → runtime-advice pairing (the const twin of leaf-boot's
/// `AdvisorPairing`), keyed by [`ContractId`] and auto-collected into
/// [`ADVISOR_PAIRINGS`]. Emitted by `#[aspect]` beside the aspect bean's `Descriptor`;
/// the run pipeline auto-collects it, reifies each into a live `AdvisorDescriptor`, and
/// installs the proxy at R4 — so an `#[aspect]` advises with NO hand-assembled
/// `.with_advisors`.
///
/// Unlike the `ADVISORS` anti-DCE IDENTITY row (which carries only `contract`+`order`),
/// THIS row also carries the `&'static dyn Pointcut` + the [`MakeInterceptor`] bean
/// bridge — both const-constructible at macro time: the pointcut is a const
/// combinator ([`Anything`](crate::Anything)/[`within`](crate::within)-style), and the
/// `make_interceptor` is a const `fn` that resolves the aspect bean by `ContractId` and
/// upcasts it to `Arc<dyn Interceptor>` (the aspect bean IS the interceptor).
#[derive(Clone, Copy)]
pub struct AdvisorPairingRow {
    /// The advisor's stable cross-build identity (the JOIN + chain tie-break key).
    pub contract: ContractId,
    /// The chain order (lower = outermost; the `cmp_chain` sort key).
    pub order: OrderKey,
    /// Framework-vs-application provenance (the `RoleTier` source).
    pub role: Role,
    /// The const typed-combinator pointcut predicate.
    pub pointcut: &'static dyn Pointcut,
    /// The const bean bridge that resolves this advisor's interceptor at refresh
    /// (the aspect bean, resolved by `ContractId` + upcast to `Arc<dyn Interceptor>`).
    pub make_interceptor: MakeInterceptor,
}

// ───────────────────────── the distributed slices ───────────────────────────
//
// THE channel family. `COMPONENTS`/`AUTO_CONFIGS` reuse the const `Descriptor`
// verbatim; the rest carry the minimal const rows above (or an existing trait
// object). The leading `#[allow(...)]` is not needed — `linkme` handles the
// section/`#[used]` machinery — but each slice is `pub` so the macro can
// `#[distributed_slice(::leaf_core::COMPONENTS)]` into it cross-crate.

/// THE bean channel: stereotyped components, scanned candidates, and `@bean`
/// methods all emit one const [`Descriptor`] here (discovery-codegen phase3/02).
#[linkme::distributed_slice]
pub static COMPONENTS: [Descriptor] = [..];

/// The auto-configuration channel: a SEPARATE slice so component-scanning over
/// [`COMPONENTS`] never picks auto-configs up (the AutoConfigurationExcludeFilter
/// boundary, made structural). Auto-config beans register at
/// `CandidateRole::FALLBACK` so a user bean transparently supersedes.
#[linkme::distributed_slice]
pub static AUTO_CONFIGS: [Descriptor] = [..];

/// The condition-implementation channel (conditions-autoconfig phase3/05).
#[linkme::distributed_slice]
pub static CONDITIONS: [ConditionRow] = [..];

/// The anti-DCE source-anchor channel: one [`SourceTag`] per crate that called
/// [`declare_source!`](crate::declare_source), so the self-check can tell
/// "linked-but-zero-rows" from "never-linked" (ADR-09 Defense MANIFEST).
#[linkme::distributed_slice]
pub static SOURCES: [SourceTag] = [..];

/// The stereotype meta-edge channel (component-stereotypes phase3/02) — the
/// optional runtime-closure fallback only.
#[linkme::distributed_slice]
pub static STEREOTYPES: [StereotypeRow] = [..];

/// The programmatic-registration channel: `#[import(MyRegistrar)]` emits one
/// `&'static dyn Registrar` row here (programmatic-registration phase3/02).
#[linkme::distributed_slice]
pub static REGISTRARS: [&'static dyn Registrar] = [..];

/// The `@EventListener` channel (events phase3/12).
#[linkme::distributed_slice]
pub static EVENT_LISTENERS: [EventListenerRow] = [..];

/// The declarative-advice / advisor channel (declarative-advice phase3/09).
#[linkme::distributed_slice]
pub static ADVISORS: [AdvisorRow] = [..];

/// The `@Scheduled` task channel (scheduling).
#[linkme::distributed_slice]
pub static SCHEDULED: [ScheduledRow] = [..];

/// The converter/formatter catalog channel (binding-conversion).
#[linkme::distributed_slice]
pub static CATALOGS: [CatalogRow] = [..];

/// The static-resource channel (expr-i18n-resources phase3/11).
#[linkme::distributed_slice]
pub static RESOURCES: [ResourceRow] = [..];

/// The `@ConfigurationProperties` metadata channel (config-metadata).
#[linkme::distributed_slice]
pub static CONFIG_METADATA: [ConfigMetadataRow] = [..];

// ── the per-bean WIRING-PAIRING channels (the COMPONENTS auto-collect substrate,
// extended to every per-bean wiring kind so a normal annotated app needs no
// hand-assembled `.with_*` calls; discovery-codegen phase3/02) ──

/// The bean → [`ProviderSeed`] pairing channel: a `#[component]`/`#[bean]`/… emits
/// one [`SeedPairingRow`] (`__leaf_seed_<Ident>`) here beside its `COMPONENTS` row.
#[linkme::distributed_slice]
pub static SEED_PAIRINGS: [SeedPairingRow] = [..];

/// The bean → [`InjectionPlan`] pairing channel: one [`InjectionPlanPairingRow`] per
/// bean (the per-bean construction-edge plan), beside the `COMPONENTS` row.
#[linkme::distributed_slice]
pub static INJECTION_PLAN_PAIRINGS: [InjectionPlanPairingRow] = [..];

/// The gated-element → [`CondExpr`] guard pairing channel: `#[conditional]`/`#[profile]`
/// emits one [`GuardPairingRow`] (`__leaf_guard_<Ident>`) per gated element.
#[linkme::distributed_slice]
pub static GUARD_PAIRINGS: [GuardPairingRow] = [..];

/// The advisable-bean → [`BeanJoinPointsSpec`] pairing channel: `#[advisable]`/`#[aspect]`
/// emits one [`JoinPointPairingRow`] (`__leaf_joinpoints_<Ident>`) per advisable bean.
#[linkme::distributed_slice]
pub static JOINPOINT_PAIRINGS: [JoinPointPairingRow] = [..];

/// The advised-bean → [`MethodTable`] pairing channel: `#[advisable]`/`#[aspect]` emits
/// one [`MethodTablePairingRow`] (`__leaf_methods_<Ident>`) per advised bean.
#[linkme::distributed_slice]
pub static METHOD_TABLE_PAIRINGS: [MethodTablePairingRow] = [..];

/// The runner-bean → [`RunnerUpcastFn`] pairing channel: `#[runner]` emits one
/// [`RunnerPairingRow`] (`__leaf_runner_upcast_<Ident>`) per runner bean.
#[linkme::distributed_slice]
pub static RUNNER_PAIRINGS: [RunnerPairingRow] = [..];

/// The config-bean → [`ConfigBindThunk`] pairing channel: `#[config_properties]` emits
/// one [`ConfigBindPairingRow`] (`__leaf_config_bind_<Ident>`) per config bean.
#[linkme::distributed_slice]
pub static CONFIG_BIND_PAIRINGS: [ConfigBindPairingRow] = [..];

/// The advisor → runtime-advice pairing channel: `#[aspect]` emits one
/// [`AdvisorPairingRow`] (carrying the const pointcut + `make_interceptor`) per aspect
/// bean, so the run pipeline auto-collects the live advisor with no hand-assembled
/// `.with_advisors`. (The separate [`ADVISORS`] slice stays the anti-DCE identity row.)
#[linkme::distributed_slice]
pub static ADVISOR_PAIRINGS: [AdvisorPairingRow] = [..];

/// The failure-analyzer channel (bootstrap-diagnostics phase3/14, ADR-12) — a
/// `&'static dyn FailureAnalyzer` per analyzer, `cmp_order`-sorted,
/// first-non-`None` wins. Reuses the [`FailureAnalyzer`] SPI from the error
/// model (never a second analyzer trait).
#[linkme::distributed_slice]
pub static FAILURE_ANALYZERS: [&'static dyn FailureAnalyzer] = [..];

// ─────────────────────────── iteration helper ───────────────────────────────

/// Lift a link-collected distributed slice into an owned `Vec<T>` (the cold
/// assembly-pass entry point; `RegistryBuilder::from_slices` calls this per
/// channel).
///
/// This is a thin `&'static [T] -> Vec<T>` copy. It exists as ONE named helper
/// so every channel is lifted identically and the slice is read in exactly one
/// idiom (never indexed by link position — ordering is computed at freeze from
/// the stable [`ContractId`], never the slice order). `T: Copy`
/// because every const row in this family is `Copy`; `T: 'static` because a
/// distributed-slice element type is always `'static`.
#[must_use]
pub fn collect_slice<T: Copy + 'static>(slice: &linkme::DistributedSlice<[T]>) -> Vec<T> {
    slice.iter().copied().collect()
}

/// Build a diagnostic [`Origin::Native`] tag from a [`SourceTag`] — the bridge
/// from a link-collected anchor to the error model's provenance (used when a
/// row's contributing crate must be named in a `LeafError` chain).
#[must_use]
pub fn origin_of(source: SourceTag) -> Origin {
    Origin::Native { crate_name: Some(source.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AnalysisCtx, Diagnostic, ErrorKind, FailureAnalysis, RenderStyle};

    // ── THE load-bearing linkme roundtrip ──────────────────────────────────────
    //
    // Submit one const element through the re-exported `::leaf_core::linkme` path
    // and iterate it back. This proves the whole substrate compiles AND links
    // in-crate: the `#[distributed_slice]` element is placed into the named
    // section with `#[used]`, and the slice is materialized from the linker
    // boundary symbols at runtime. If linkme were misconfigured (wrong section,
    // missing `#[used]`, wrong re-export path), this surfaces as an empty slice.
    //
    // We roundtrip the const-friendliest channel (SOURCES = `&'static str`) so
    // the proof does not fight `TypeId::of` not being a stable const fn (which is
    // exactly why the const `Descriptor`-emitting macro builds `self_type` at the
    // call site, not in a `static`). The mechanics are identical for every
    // channel.

    const TEST_SOURCE: SourceTag = SourceTag("leaf-core::discovery::test");

    #[linkme::distributed_slice(SOURCES)]
    static SUBMITTED_SOURCE: SourceTag = TEST_SOURCE;

    #[test]
    fn distributed_slice_roundtrips_a_submitted_element() {
        // The element submitted above must be visible when the slice is iterated.
        let found: Vec<SourceTag> = collect_slice(&SOURCES);
        assert!(
            found.contains(&TEST_SOURCE),
            "submitted SourceTag must roundtrip through the linkme slice; found {found:?}",
        );
    }

    // The `declare_source!` per-crate anchor: the once-per-crate SourceTag every
    // PARTICIPATING crate calls in its root so the binary's expected-vs-found
    // self-check can tell linked-but-zero-rows from never-linked. The static the
    // macro emits is scoped to this nested module so its fixed `__LEAF_SOURCE` ident
    // does not collide with the roundtrip submission above (and so a real crate's
    // single root-level call is the only `__LEAF_SOURCE` per crate).
    mod declared {
        crate::declare_source!("leaf-core::discovery::declared");
    }

    #[test]
    fn declare_source_submits_one_per_crate_tag_into_sources() {
        // The macro-submitted anchor is link-collected into SOURCES under the exact
        // package-name string handed to it (so the manifest JOIN finds it).
        let found: Vec<SourceTag> = collect_slice(&SOURCES);
        assert!(
            found.contains(&SourceTag("leaf-core::discovery::declared")),
            "declare_source! must submit its SourceTag into SOURCES; found {found:?}",
        );
    }

    #[test]
    fn collect_slice_lifts_into_an_owned_vec_via_the_one_helper() {
        // `collect_slice` is the single read idiom; it copies the static slice.
        let v = collect_slice(&SOURCES);
        // At minimum our own submission is present.
        assert!(!v.is_empty());
        assert!(v.iter().any(|s| s.0 == "leaf-core::discovery::test"));
    }

    // A const CONDITIONS row (fully const: ContractId + MarkerId) — proves a
    // second, struct-valued channel roundtrips too.
    #[linkme::distributed_slice(CONDITIONS)]
    static TEST_CONDITION: ConditionRow = ConditionRow {
        contract: ContractId::of("leaf_core::discovery::tests::OnTest"),
        marker: MarkerId::of("leaf::condition::OnTest"),
    };

    #[test]
    fn a_struct_row_channel_also_roundtrips() {
        let rows = collect_slice(&CONDITIONS);
        let mine = rows
            .iter()
            .find(|r| r.contract == ContractId::of("leaf_core::discovery::tests::OnTest"))
            .expect("submitted ConditionRow must roundtrip");
        assert_eq!(mine.marker, MarkerId::of("leaf::condition::OnTest"));
    }

    // ── REGISTRARS: a `&'static dyn Registrar` channel ─────────────────────────

    struct CountingRegistrar;
    impl Registrar for CountingRegistrar {
        fn register(&self, _cx: &mut RegistrarCtx<'_>) -> Result<(), LeafError> {
            Ok(())
        }
    }

    static COUNTING: CountingRegistrar = CountingRegistrar;

    #[linkme::distributed_slice(REGISTRARS)]
    static TEST_REGISTRAR: &dyn Registrar = &COUNTING;

    #[test]
    fn registrar_slice_collects_trait_objects_and_register_runs() {
        // The slice holds `&'static dyn Registrar`; iterate and drive one.
        let registrars: Vec<&'static dyn Registrar> = REGISTRARS.iter().copied().collect();
        assert!(!registrars.is_empty(), "our registrar must be link-collected");
        let mut cx = RegistrarCtx::root();
        for r in &registrars {
            r.register(&mut cx).expect("registrar runs in the cold phase");
        }
    }

    #[test]
    fn registrar_is_dyn_compatible_behind_a_static_ref() {
        // The whole point of the slice element type: Registrar is object-safe.
        let r: &'static dyn Registrar = &COUNTING;
        let mut cx = RegistrarCtx::default();
        assert!(r.register(&mut cx).is_ok());
    }

    // ── FAILURE_ANALYZERS: reuse the error-model SPI, not a second trait ────────

    struct NopAnalyzer;
    impl FailureAnalyzer for NopAnalyzer {
        fn analyze(&self, err: &LeafError, _ctx: &AnalysisCtx) -> Option<FailureAnalysis> {
            if err.kind == ErrorKind::AntiDce {
                Some(FailureAnalysis {
                    description: "anti-dce".into(),
                    action: "force-link the crate".into(),
                    cause: None,
                })
            } else {
                None
            }
        }
    }

    static NOP_ANALYZER: NopAnalyzer = NopAnalyzer;

    #[linkme::distributed_slice(FAILURE_ANALYZERS)]
    static TEST_ANALYZER: &dyn FailureAnalyzer = &NOP_ANALYZER;

    #[test]
    fn failure_analyzers_slice_carries_the_error_model_spi() {
        let analyzers: Vec<&'static dyn FailureAnalyzer> =
            FAILURE_ANALYZERS.iter().copied().collect();
        assert!(!analyzers.is_empty());
        let err = LeafError::new(ErrorKind::AntiDce);
        let hit = analyzers
            .iter()
            .find_map(|a| a.analyze(&err, &AnalysisCtx::empty()))
            .expect("the AntiDce analyzer matches");
        assert_eq!(hit.description, "anti-dce");
        // Sanity: the analysis renders through the one Diagnostic spine indirectly
        // (FailureAnalysis is plain data; the renderer lives on LeafError).
        let _ = err.render_to_string(RenderStyle::Human);
    }

    // ── const rows are const-constructible into every channel ───────────────────

    #[linkme::distributed_slice(STEREOTYPES)]
    static TEST_STEREOTYPE: StereotypeRow = StereotypeRow {
        contract: ContractId::of("leaf_core::discovery::tests::RestController"),
        marker: MarkerId::of("leaf::RestController"),
        implies: &[MarkerId::of("leaf::Controller"), MarkerId::of("leaf::Component")],
    };

    #[linkme::distributed_slice(EVENT_LISTENERS)]
    static TEST_LISTENER: EventListenerRow = EventListenerRow {
        contract: ContractId::of("leaf_core::discovery::tests::OnReady"),
        order: OrderKey::implicit(),
    };

    #[linkme::distributed_slice(ADVISORS)]
    static TEST_ADVISOR: AdvisorRow = AdvisorRow {
        contract: ContractId::of("leaf_core::discovery::tests::TxAdvisor"),
        order: OrderKey::implicit(),
    };

    #[linkme::distributed_slice(SCHEDULED)]
    static TEST_SCHEDULED: ScheduledRow =
        ScheduledRow { contract: ContractId::of("leaf_core::discovery::tests::Cleanup") };

    #[linkme::distributed_slice(CATALOGS)]
    static TEST_CATALOG: CatalogRow =
        CatalogRow { contract: ContractId::of("leaf_core::discovery::tests::DurationConverter") };

    #[linkme::distributed_slice(RESOURCES)]
    static TEST_RESOURCE: ResourceRow = ResourceRow {
        contract: ContractId::of("leaf_core::discovery::tests::Messages"),
        location: "classpath:/messages.properties",
    };

    #[linkme::distributed_slice(CONFIG_METADATA)]
    static TEST_CONFIG_META: ConfigMetadataRow = ConfigMetadataRow {
        contract: ContractId::of("leaf_core::discovery::tests::AppProps"),
        prefix: "app",
    };

    #[test]
    fn every_secondary_channel_collects_its_const_rows() {
        // Each minimal const row roundtrips through its slice — the proof that
        // the whole channel family is const-constructible cross-crate.
        assert!(collect_slice(&STEREOTYPES)
            .iter()
            .any(|r| r.implies.len() == 2 && r.marker == MarkerId::of("leaf::RestController")));
        assert!(collect_slice(&EVENT_LISTENERS)
            .iter()
            .any(|r| r.contract == ContractId::of("leaf_core::discovery::tests::OnReady")));
        assert!(collect_slice(&ADVISORS)
            .iter()
            .any(|r| r.contract == ContractId::of("leaf_core::discovery::tests::TxAdvisor")));
        assert!(collect_slice(&SCHEDULED)
            .iter()
            .any(|r| r.contract == ContractId::of("leaf_core::discovery::tests::Cleanup")));
        assert!(collect_slice(&CATALOGS)
            .iter()
            .any(|r| r.contract == ContractId::of("leaf_core::discovery::tests::DurationConverter")));
        assert!(collect_slice(&RESOURCES)
            .iter()
            .any(|r| r.location == "classpath:/messages.properties"));
        assert!(collect_slice(&CONFIG_METADATA).iter().any(|r| r.prefix == "app"));
    }

    // ── COMPONENTS / AUTO_CONFIGS exist and are iterable (Descriptor channel) ───
    //
    // We do NOT submit a const Descriptor here (TypeId::of is not a stable const
    // fn, so the macro builds Descriptor at the call site, not in a `static`).
    // The ABI guarantee this unit owns is that the channel EXISTS, has element
    // type `Descriptor`, and is iterable via the one helper; a real component
    // submission is exercised in leaf-macros' integration tests.

    #[test]
    fn descriptor_channels_exist_and_are_iterable() {
        let components: Vec<Descriptor> = collect_slice(&COMPONENTS);
        let auto_configs: Vec<Descriptor> = collect_slice(&AUTO_CONFIGS);
        // In a bare leaf-core test build nothing submits a Descriptor, so both
        // are legitimately empty — the assertion is that the read is total.
        assert_eq!(components.len(), COMPONENTS.len());
        assert_eq!(auto_configs.len(), AUTO_CONFIGS.len());
    }

    // ── SourceTag / origin bridge ──────────────────────────────────────────────

    #[test]
    fn source_tag_bridges_to_a_native_origin() {
        let tag = SourceTag("leaf-redis");
        match origin_of(tag) {
            Origin::Native { crate_name } => assert_eq!(crate_name, Some("leaf-redis")),
            other => panic!("expected Native origin, got {other:?}"),
        }
    }

    #[test]
    fn registrar_ctx_is_constructible_and_debug() {
        let _root = RegistrarCtx::root();
        let _default = RegistrarCtx::default();
        assert!(format!("{:?}", RegistrarCtx::root()).contains("RegistrarCtx"));
    }

    #[test]
    fn source_tag_equality_is_by_value() {
        assert_eq!(SourceTag("a"), SourceTag("a"));
        assert_ne!(SourceTag("a"), SourceTag("b"));
    }
}
