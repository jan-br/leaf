//! The const `Descriptor` metamodel: the ONE bean-definition row shape.
//!
//! Realizes registry-core's `bean-definition` feature (phase3/01) and the scope
//! axes of `bean-lifecycle` (phase3/04), pinned by `TOOLKIT.md` and SEAMS C5.
//!
//! The thin `#[component]`/`#[bean]` macro emits exactly ONE const [`Descriptor`]
//! per bean via absolute `::leaf_core` paths. It is a **flat, closed-schema,
//! const** record — never an open `HashMap<TypeId, Box<dyn …>>` attribute bag
//! (that heap/`dyn` cost is explicitly rejected). Cross-cutting concerns attach
//! typed metadata through the const [`AnnotationMetadata`] tables, not a boxed
//! map. The parent→merged collapse is computed ONCE at `freeze()` (the freeze IS
//! the merge moment), so there is no runtime `MergedBeanDefinition` type and no
//! stale/recompute machinery.
//!
//! ## Scope is DATA on three orthogonal axes
//!
//! Scope is never a second handle type and never a `Box<dyn Scope>` SPI. It is
//! the [`ScopeDef`] triple — [`Multiplicity`] × [`StoreSource`] ×
//! [`TeardownPolicy`] — read by the one `Engine::create` driver:
//!
//! - [`Multiplicity::Once`] / [`Multiplicity::PerContextKey`] →
//!   [`Published::Shared`](crate::Published::Shared); [`Multiplicity::PerResolution`]
//!   → [`Published::Owned`](crate::Published::Owned) (a real owned move).
//! - [`StoreSource::ContainerStore`] → the registry's slot-indexed `OnceCell`
//!   singleton array; [`StoreSource::AmbientStore`] → a per-`Cx`-key
//!   `InstanceStore` reached through the async-context model (never a thread-local).
//!
//! The built-in `SINGLETON` / `PROTOTYPE` / `REQUEST` triples are const here; a
//! custom scope contributes its own const [`ScopeDef`] + a [`ScopeKind`]
//! registration and picks WHERE instances live — never a new mechanism.

use std::any::TypeId;

use crate::handle::ErasedBean;
use crate::identity::ContractId;
use crate::error::Origin;

// ─────────────────────────── Role ───────────────────────────────────────────

/// The framework-vs-application provenance of a bean (registry-core
/// `bean-definition`). Read as DATA (never a runtime `instanceof`): `refresh()`
/// auto-detects [`Role::Infrastructure`] providers to install them outermost.
///
/// This is the metamodel-side enum the const [`Descriptor`] carries; it maps
/// totally onto the ordering-side [`RoleTier`](crate::RoleTier) via
/// [`RoleTier::of`](crate::RoleTier::of) (SEAMS C6) so there is exactly one
/// role taxonomy in the kernel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Role {
    /// An ordinary application bean (the default) — innermost.
    #[default]
    Application,
    /// A support concern (middle tier).
    Support,
    /// Framework infrastructure — outermost (wraps everything else).
    Infrastructure,
}

// ─────────────────────── Candidate role (SEAMS C5) ──────────────────────────

/// The primacy axis of a [`CandidateRole`] (SEAMS C5).
///
/// Kept separate from `fallback` so a `@Fallback` bean CAN also be `@Primary`
/// (a starter shipping a default-primary among its own beans) — a case a flat
/// 3-way enum could not represent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Primacy {
    /// No `@Primary` declaration (the default).
    #[default]
    Plain,
    /// Declared `@Primary` — promoted by the Selector's primary_promote layer.
    Primary,
}

/// A candidate's selection role — the SINGLE source of truth read identically by
/// the Selector, the `DefinitionProbe`, and the condition report (SEAMS C5).
///
/// It is a TWO-axis value (`primacy` × `fallback`), not a 3-way enum, because a
/// `@Fallback` bean can also be `@Primary`. The familiar names are const
/// constructors over it: [`CandidateRole::NORMAL`], [`CandidateRole::PRIMARY`],
/// [`CandidateRole::FALLBACK`] (and `FALLBACK` composes with `.primary()`).
///
/// `fallback = true` is the DEFAULT soft override for auto-config beans (they
/// register, then lose to any user `Normal`); it is NOT the same as the opt-in
/// hard `#[on_missing_bean]` back-off (which mints no slot at all).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct CandidateRole {
    /// Whether the bean is `@Primary` (promoted when ambiguous).
    pub primacy: Primacy,
    /// Whether the bean is a soft `@Fallback` (demoted whenever a non-fallback
    /// of the same contract exists).
    pub fallback: bool,
}

impl CandidateRole {
    /// A plain candidate: not primary, not fallback (`{Plain, false}`).
    pub const NORMAL: CandidateRole = CandidateRole { primacy: Primacy::Plain, fallback: false };
    /// A `@Primary` candidate (`{Primary, false}`).
    pub const PRIMARY: CandidateRole = CandidateRole { primacy: Primacy::Primary, fallback: false };
    /// A `@Fallback` candidate (`{Plain, true}`) — the auto-config soft default.
    pub const FALLBACK: CandidateRole = CandidateRole { primacy: Primacy::Plain, fallback: true };

    /// `true` iff this candidate is `@Primary`.
    #[must_use]
    pub const fn is_primary(self) -> bool {
        matches!(self.primacy, Primacy::Primary)
    }

    /// `true` iff this candidate is a soft `@Fallback`.
    #[must_use]
    pub const fn is_fallback(self) -> bool {
        self.fallback
    }

    /// Derive the `@Primary` variant of this role, preserving `fallback` — so a
    /// `@Fallback @Primary` (`{Primary, true}`) starter bean is expressible.
    #[must_use]
    pub const fn primary(self) -> CandidateRole {
        CandidateRole { primacy: Primacy::Primary, fallback: self.fallback }
    }
}

// ─────────────────────── Scope: the three data axes ─────────────────────────

/// AXIS 1 — how many instances exist and where the ownership divergence lands.
///
/// The total two-arm publication map: `{Once, PerContextKey}` →
/// [`Published::Shared`](crate::Published::Shared), `PerResolution` →
/// [`Published::Owned`](crate::Published::Owned).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Multiplicity {
    /// Exactly one shared instance for the whole container (singleton): the
    /// per-slot `OnceCell` is the at-most-once creation guard.
    #[default]
    Once,
    /// A fresh owned instance per resolution (prototype): never stored, never
    /// refcounted, no teardown — a [`Published::Owned`](crate::Published::Owned)
    /// move.
    PerResolution,
    /// One shared instance per context key (request/session/custom scope),
    /// memoized in a per-`Cx` `InstanceStore` keyed by `BeanId`.
    PerContextKey,
}

/// An interned context-scope kind (request / session / a custom scope).
///
/// Used by [`StoreSource::AmbientStore`] to select WHICH ambient `InstanceStore`
/// (reached through the async-context `Cx` bundle) holds the per-key instances.
/// Keyed by the SAME stable [`contract_hash`](crate::contract_hash) so a custom
/// scope's kind survives across builds/crates; the built-ins are reserved
/// constants ([`ScopeKind::REQUEST`], [`ScopeKind::SESSION`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeKind(pub u64);

impl ScopeKind {
    /// Mint a `ScopeKind` from a canonical scope-name path, `const` so built-in
    /// and macro-emitted scope kinds intern at compile time.
    #[must_use]
    pub const fn of(canonical_path: &str) -> Self {
        ScopeKind(crate::identity::contract_hash(canonical_path))
    }

    /// The built-in request scope kind.
    pub const REQUEST: ScopeKind = ScopeKind::of("leaf::scope::request");
    /// The built-in session scope kind.
    pub const SESSION: ScopeKind = ScopeKind::of("leaf::scope::session");
}

impl std::fmt::Debug for ScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ScopeKind(0x{:016x})", self.0)
    }
}

/// AXIS 2 — WHERE the instance lives (which store backs it).
///
/// `ContainerStore` → the registry's slot-indexed `OnceCell` singleton array;
/// `AmbientStore(kind)` → a per-`Cx`-key `InstanceStore` reached through the
/// async-context model (NEVER a thread-local). This is data: it never selects a
/// `Box<dyn Scope>` strategy.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum StoreSource {
    /// The container-owned singleton store (the `OnceCell` array) — the default.
    #[default]
    ContainerStore,
    /// A per-`Cx`-key ambient `InstanceStore` selected by [`ScopeKind`].
    AmbientStore(ScopeKind),
}

/// AXIS 3 — HOW (and whether) the instance is torn down at scope end.
///
/// Drives whether `Engine::create` registers a destroyer in the `TeardownLedger`
/// (drained LIFO at shutdown — there is no async `Drop`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TeardownPolicy {
    /// Run the bean's destroy callbacks at scope/container shutdown (the default
    /// for shared beans).
    #[default]
    Managed,
    /// No teardown — the instance is handed off and forgotten (prototype: a
    /// `Published::Owned` move that the container never stores).
    None,
}

/// SCOPE — the const three-axis triple read by the one `Engine::create` driver.
///
/// `Multiplicity` × `StoreSource` × `TeardownPolicy`. Never a handle type, never
/// a `Box<dyn Scope>`. The built-in [`ScopeDef::SINGLETON`] / [`ScopeDef::PROTOTYPE`]
/// / [`ScopeDef::REQUEST`] are consts; a custom scope is just another const triple.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ScopeDef {
    /// AXIS 1: how many instances exist (and the publication shape).
    pub multiplicity: Multiplicity,
    /// AXIS 2: which store backs the instance.
    pub store: StoreSource,
    /// AXIS 3: how (and whether) the instance is torn down.
    pub teardown: TeardownPolicy,
}

impl ScopeDef {
    /// The default singleton scope: one container-stored, managed-teardown instance.
    pub const SINGLETON: ScopeDef = ScopeDef {
        multiplicity: Multiplicity::Once,
        store: StoreSource::ContainerStore,
        teardown: TeardownPolicy::Managed,
    };

    /// The prototype scope: a fresh per-resolution owned move, no store, no teardown.
    pub const PROTOTYPE: ScopeDef = ScopeDef {
        multiplicity: Multiplicity::PerResolution,
        store: StoreSource::ContainerStore,
        teardown: TeardownPolicy::None,
    };

    /// The built-in request scope: one shared instance per request `Cx`, torn
    /// down at request end.
    pub const REQUEST: ScopeDef = ScopeDef {
        multiplicity: Multiplicity::PerContextKey,
        store: StoreSource::AmbientStore(ScopeKind::REQUEST),
        teardown: TeardownPolicy::Managed,
    };

    /// `true` iff this scope publishes a [`Published::Shared`](crate::Published::Shared)
    /// (singleton or any context-scope instance), as opposed to an owned move.
    #[must_use]
    pub const fn is_shared(self) -> bool {
        matches!(self.multiplicity, Multiplicity::Once | Multiplicity::PerContextKey)
    }
}

impl Default for ScopeDef {
    fn default() -> Self {
        ScopeDef::SINGLETON
    }
}

// ─────────────────────── TypeRow (upcast row) ───────────────────────────────

/// An origin-agnostic upcast function: `Arc<dyn Any+Send+Sync>` (a concrete
/// erased bean) → the SAME `Arc` re-erased through a declared `dyn Svc` view.
///
/// One per declared injectable supertrait. The macro emits this as a const
/// fn-pointer doing the trait-upcast coercion (stable since 1.86).
pub type UpcastFn = fn(ErasedBean) -> ErasedBean;

/// One `provides[]` upcast row: a `dyn`-service view a bean is injectable as
/// (or a FactoryBean product type), plus the const fn that performs the upcast.
///
/// Concrete handles match an exact `TypeId`; a `dyn Svc` injection point matches
/// a `TypeRow` whose `view` is `TypeId::of::<dyn Svc>()`. Candidate resolution
/// reading the row finds a FactoryBean product pre-construction (the
/// getObjectType-without-getObject contract is just this row).
#[derive(Clone, Copy)]
pub struct TypeRow {
    /// The `TypeId` of the `dyn Svc` view (or product type) this bean provides.
    pub view: TypeId,
    /// The const fn that re-erases the bean's `Arc` through `view`.
    pub upcast: UpcastFn,
}

impl std::fmt::Debug for TypeRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The fn-pointer is not meaningfully printable; show the view TypeId.
        f.debug_struct("TypeRow").field("view", &self.view).finish_non_exhaustive()
    }
}

// ─────────────────────── AnnotationMetadata (closed core) ───────────────────

/// The flat, const, closed-schema annotation table the [`Descriptor`] carries
/// (registry-core `bean-definition`).
///
/// Qualifiers/markers/`@DependsOn`/`@Primary`/`@Fallback`/`autowire_candidate`
/// are FIRST-CLASS core fields here (read on the hot-ish candidate path), NOT an
/// open `HashMap<TypeId, Box<dyn …>>` extension surface (that R3 heap/dyn cost
/// is rejected). All fields are `&'static` / `Copy` so the whole record is const.
///
/// Scope note: richer annotation-model tables (e.g. typed marker payloads) are
/// added as fields here by later units; downstream-construct via
/// [`AnnotationMetadata::EMPTY`] keeps existing const sites valid.
#[derive(Clone, Copy, Debug)]
pub struct AnnotationMetadata {
    /// Interned qualifier markers (single-marker qualifier keys).
    pub qualifiers: &'static [crate::identity::MarkerId],
    /// Interned generic markers (custom-qualifier marker types).
    pub markers: &'static [crate::identity::MarkerId],
    /// `@DependsOn` forced-ordering targets, by stable cross-build identity
    /// (resolved to `BeanId`s at freeze).
    pub depends_on: &'static [ContractId],
    /// The selection role (`@Primary`/`@Fallback`), SEAMS C5.
    pub candidate_role: CandidateRole,
    /// Whether the bean participates in plain by-type autowiring (Spring's
    /// `autowire-candidate`); `false` excludes it from the default candidate set.
    pub autowire_candidate: bool,
}

impl AnnotationMetadata {
    /// The empty/default metadata: no qualifiers, no markers, no depends-on, a
    /// plain candidate role, and an autowire candidate. The common case.
    pub const EMPTY: AnnotationMetadata = AnnotationMetadata {
        qualifiers: &[],
        markers: &[],
        depends_on: &[],
        candidate_role: CandidateRole::NORMAL,
        autowire_candidate: true,
    };
}

impl Default for AnnotationMetadata {
    fn default() -> Self {
        AnnotationMetadata::EMPTY
    }
}

// ─────────────────────── Descriptor (the const row) ─────────────────────────

/// THE const bean-definition metamodel row — one per bean, macro-emitted.
///
/// A flat, closed-schema, const record (registry-core `bean-definition`). The
/// frozen `Descriptor` lives in the registry's `rows: Box<[Descriptor]>` indexed
/// by `BeanId` (no per-def `Arc`). At `freeze()` the asymmetric parent→merged
/// collapse runs ONCE (child keeps `scope`/`role`/`candidate_role`/`depends_on`
/// from itself, inherits qualifiers/init/destroy/meta from `parent`); the freeze
/// IS the merge moment, so there is no runtime `MergedBeanDefinition` type.
///
/// The construction recipe rides the opaque, fixed [`ProviderSeed`](crate::ProviderSeed)
/// (a const fn-pointer that BUILDS the `Provider`) — NOT editable arg metadata.
/// A BFPP-analogue may rewrite METADATA (scope/lazy/role/qualifiers) during
/// `App<Resolve>`, but the typed factory closure is fixed at the declaration site;
/// placeholder-resolved values become `ResolveCtx`-read inputs the `Provider`
/// reads, never definition edits.
#[derive(Clone, Copy, Debug)]
pub struct Descriptor {
    /// Stable cross-build identity (the durable key for merge / collision / DCE).
    pub contract: ContractId,
    /// The bean's own concrete `TypeId` (the in-process exact-match key).
    pub self_type: TypeId,
    /// Declared injectable `dyn Svc` views + FactoryBean product types.
    pub provides: &'static [TypeRow],
    /// The canonical declared name, if explicit (else derived at the macro site).
    pub declared_name: Option<&'static str>,
    /// Additional names this bean answers to (bean-naming overlay).
    pub aliases: &'static [&'static str],
    /// The three-axis scope triple read by `Engine::create`.
    pub scope: ScopeDef,
    /// Framework-vs-application provenance (auto-detected at `refresh()`).
    pub role: Role,
    /// The flat const annotation table (qualifiers/markers/depends_on/role).
    pub meta: &'static AnnotationMetadata,
    /// Template-merge parent (NOT hierarchy lookup): references a template
    /// definition merged into this row at `freeze()`.
    pub parent: Option<ContractId>,
    /// Diagnostic-only provenance; NEVER read on a resolution path.
    pub origin: Origin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{downcast_ref, Bean, Ref};
    use crate::identity::MarkerId;
    use std::sync::Arc;

    // ── Role ─────────────────────────────────────────────────────────────────

    #[test]
    fn role_default_is_application() {
        assert_eq!(Role::default(), Role::Application);
    }

    #[test]
    fn role_three_variants_are_distinct() {
        assert_ne!(Role::Application, Role::Support);
        assert_ne!(Role::Support, Role::Infrastructure);
        assert_ne!(Role::Application, Role::Infrastructure);
    }

    // ── CandidateRole (SEAMS C5: 2-axis) ─────────────────────────────────────

    #[test]
    fn candidate_role_const_constructors() {
        assert_eq!(CandidateRole::NORMAL, CandidateRole { primacy: Primacy::Plain, fallback: false });
        assert_eq!(CandidateRole::PRIMARY, CandidateRole { primacy: Primacy::Primary, fallback: false });
        assert_eq!(CandidateRole::FALLBACK, CandidateRole { primacy: Primacy::Plain, fallback: true });
        assert_eq!(CandidateRole::default(), CandidateRole::NORMAL);
    }

    #[test]
    fn candidate_role_fallback_can_also_be_primary() {
        // The exact case a flat 3-way enum could NOT represent (SEAMS C5).
        let fp = CandidateRole::FALLBACK.primary();
        assert_eq!(fp, CandidateRole { primacy: Primacy::Primary, fallback: true });
        assert!(fp.is_primary());
        assert!(fp.is_fallback());
    }

    #[test]
    fn candidate_role_predicates() {
        assert!(!CandidateRole::NORMAL.is_primary());
        assert!(!CandidateRole::NORMAL.is_fallback());
        assert!(CandidateRole::PRIMARY.is_primary());
        assert!(CandidateRole::FALLBACK.is_fallback());
    }

    // ── ScopeDef axes ─────────────────────────────────────────────────────────

    #[test]
    fn scopedef_axes_construct_independently() {
        // The three orthogonal axes can be combined freely (custom scope shape).
        let custom = ScopeDef {
            multiplicity: Multiplicity::PerContextKey,
            store: StoreSource::AmbientStore(ScopeKind::of("my::tenant::scope")),
            teardown: TeardownPolicy::Managed,
        };
        assert_eq!(custom.multiplicity, Multiplicity::PerContextKey);
        assert!(matches!(custom.store, StoreSource::AmbientStore(_)));
        assert_eq!(custom.teardown, TeardownPolicy::Managed);
    }

    #[test]
    fn builtin_singleton_scope_is_once_container_managed() {
        let s = ScopeDef::SINGLETON;
        assert_eq!(s.multiplicity, Multiplicity::Once);
        assert_eq!(s.store, StoreSource::ContainerStore);
        assert_eq!(s.teardown, TeardownPolicy::Managed);
        assert!(s.is_shared());
        // SINGLETON is the Default scope.
        assert_eq!(ScopeDef::default(), ScopeDef::SINGLETON);
    }

    #[test]
    fn builtin_prototype_scope_is_per_resolution_no_teardown() {
        let p = ScopeDef::PROTOTYPE;
        assert_eq!(p.multiplicity, Multiplicity::PerResolution);
        assert_eq!(p.teardown, TeardownPolicy::None);
        // Prototype is the only non-shared (owned-move) built-in.
        assert!(!p.is_shared());
    }

    #[test]
    fn builtin_request_scope_is_per_context_key_ambient() {
        let r = ScopeDef::REQUEST;
        assert_eq!(r.multiplicity, Multiplicity::PerContextKey);
        assert_eq!(r.store, StoreSource::AmbientStore(ScopeKind::REQUEST));
        assert!(r.is_shared());
    }

    #[test]
    fn scope_kind_is_interned_through_contract_hash_and_const() {
        // Same path => same kind, across builds; built-ins are reserved.
        const REQ: ScopeKind = ScopeKind::REQUEST;
        assert_eq!(REQ, ScopeKind::of("leaf::scope::request"));
        assert_ne!(ScopeKind::REQUEST, ScopeKind::SESSION);
        assert_eq!(ScopeKind::of("a::b"), ScopeKind::of("a::b"));
    }

    // ── TypeRow upcast ────────────────────────────────────────────────────────

    trait Greeter: Bean {
        fn greet(&self) -> String;
    }
    struct EnglishGreeter;
    impl Bean for EnglishGreeter {}
    impl Greeter for EnglishGreeter {
        fn greet(&self) -> String {
            "hi".into()
        }
    }

    /// A macro-shaped upcast fn that is the IDENTITY on the erased handle.
    ///
    /// `ErasedBean` is already `Arc<dyn Any + Send + Sync>`; a `dyn Svc` view is
    /// reached at the typed boundary via [`downcast_ref`] + [`Ref::from_arc`]
    /// (trait upcasting), so the row's upcast fn need only preserve the same
    /// `Arc` identity. The real macro emits exactly this kind of identity-or-
    /// coercion fn-pointer.
    fn upcast_identity(bean: ErasedBean) -> ErasedBean {
        bean
    }

    #[test]
    fn type_row_carries_a_view_typeid_and_an_upcast_fn() {
        // `TypeId::of` is not yet a stable `const fn`, so a real macro builds the
        // row's `view` in a `static`/`once`; here we build it at runtime. The
        // ABI shape (a `TypeId` + a const `UpcastFn`) is what this pins.
        let row = TypeRow { view: TypeId::of::<dyn Greeter>(), upcast: upcast_identity };
        assert_eq!(row.view, TypeId::of::<dyn Greeter>());

        // The upcast fn preserves the same underlying allocation (identity), so
        // the erased handle still downcasts to the concrete and upcasts to the view.
        let concrete = Arc::new(EnglishGreeter);
        let erased: ErasedBean = concrete.clone();
        let upcast = (row.upcast)(erased);
        let r: Ref<EnglishGreeter> = downcast_ref(upcast).expect("identity preserved");
        let view: Ref<dyn Greeter> = Ref::from_arc(r.into_arc());
        assert_eq!(view.greet(), "hi");
        // Debug shows the view but not the (unprintable) fn-pointer.
        assert!(format!("{row:?}").contains("TypeRow"));
    }

    // ── Descriptor const row ──────────────────────────────────────────────────

    static SVC_META: AnnotationMetadata = AnnotationMetadata {
        qualifiers: &[MarkerId::of("leaf::q::primary")],
        markers: &[],
        depends_on: &[],
        candidate_role: CandidateRole::PRIMARY,
        autowire_candidate: true,
    };

    #[test]
    fn descriptor_is_a_flat_const_row() {
        let d = Descriptor {
            contract: ContractId::of("crate::UserService"),
            self_type: TypeId::of::<EnglishGreeter>(),
            provides: &[],
            declared_name: Some("userService"),
            aliases: &["user"],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &SVC_META,
            parent: None,
            origin: Origin::Native { crate_name: Some("my-crate") },
        };
        assert_eq!(d.contract, ContractId::of("crate::UserService"));
        assert_eq!(d.declared_name, Some("userService"));
        assert_eq!(d.scope, ScopeDef::SINGLETON);
        assert_eq!(d.meta.candidate_role, CandidateRole::PRIMARY);
        assert_eq!(d.meta.qualifiers.len(), 1);
        assert!(d.parent.is_none());
        // Copy: descriptors are cheap dense rows.
        let d2 = d;
        assert_eq!(d2.declared_name, d.declared_name);
    }

    #[test]
    fn annotation_metadata_empty_is_the_common_default() {
        let m = AnnotationMetadata::EMPTY;
        assert!(m.qualifiers.is_empty());
        assert!(m.depends_on.is_empty());
        assert_eq!(m.candidate_role, CandidateRole::NORMAL);
        assert!(m.autowire_candidate);
        assert_eq!(AnnotationMetadata::default().candidate_role, CandidateRole::NORMAL);
    }
}
