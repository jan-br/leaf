//! Link-time discovery slices + the programmatic [`Registrar`] SPI.
//!
//! THE maximal-magic registration substrate (ADR-09 cross-crate-discovery,
//! discovery-codegen phase3/02): one `linkme` distributed-slice family, many
//! typed channels, collected at link time with **zero life-before-main and zero
//! runtime collection cost**. A crate's `#[component]`/`#[bean]`/`#[service]`/…
//! macros emit one const row per declaration into the matching slice via
//! absolute `::leaf_core` paths; the binary crate force-links every
//! participating crate (`use <crate> as _;`), and the cold `App<Define→Resolve>`
//! assembly pass lifts the rows through [`RegistryBuilder::from_slices`] (a later
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
//! order from the stable [`ContractId`](crate::ContractId).
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
//! [`linkme`](crate::linkme) is re-exported so emitted code writes
//! `::leaf_core::linkme::distributed_slice`, never a bare `::linkme` (which would
//! force every contributing crate to declare a `linkme` dependency). This is the
//! single pinned linkme path the whole workspace shares.

// `#[linkme::distributed_slice]` expands to a `#[used]` `#[link_section = "…"]`
// static; the section override trips the crate-level `deny(unsafe_code)`. This
// is the ONE genuinely-required exception (link-time collection has no safe
// stable equivalent), so it is allowed HERE only, scoped to this module. No
// hand-written `unsafe` block exists in this file — only the macro-generated
// section attributes.
#![allow(unsafe_code)]

use crate::definition::Descriptor;
use crate::error::{FailureAnalyzer, LeafError, Origin};
use crate::identity::{ContractId, MarkerId};
use crate::order::OrderKey;

/// The pinned `linkme` re-export — emitted code references
/// `::leaf_core::linkme`, never a bare `::linkme`.
///
/// A `#[component]`/`#[bean]`/… macro hard-codes
/// `#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]` so a
/// contributing crate needs ONLY a `leaf-core` dependency, never its own pin on
/// `linkme`. There is exactly one `linkme` version in the workspace (the BOM in
/// `[workspace.dependencies]`), so all slices agree on the element layout.
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
/// `provides_descriptors!()`, so the self-check can tell "linked-but-zero-rows"
/// from "never-linked" (ADR-09 Defense MANIFEST).
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
/// the stable [`ContractId`](crate::ContractId), never the slice order). `T: Copy`
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
