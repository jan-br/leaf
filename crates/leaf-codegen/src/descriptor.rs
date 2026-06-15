//! The Descriptor const-row emitter — the heart of leaf-codegen (phase3/02).
//!
//! Given a parsed bean (a `struct`/`fn` + its annotations), this module emits the
//! ONE hand-writable const [`leaf_core::Descriptor`] row plus its const
//! [`leaf_core::ProviderSeed`] and per-bean [`leaf_core::InjectionPlan`], and the
//! `linkme` submission that contributes the row into the right distributed slice
//! (`COMPONENTS` for beans). Every emitted path is ABSOLUTE `::leaf_core::…` so a
//! user crate's `use` imports cannot shadow them (the thin-macro rule, ADR-03 /
//! charter §2.10): the macro emits already-flattened const DATA, all logic lives
//! HERE in a normal, unit-testable library.
//!
//! ## The four-layer pipeline, layer 1 (this unit)
//!
//! ADR-03's LAYER 1 is "proc-macro emits data only": exactly ONE const
//! `Descriptor` + one const `ProviderSeed` per bean, registered into a `linkme`
//! slice via absolute `::leaf_core` paths — literally what a user could hand-write
//! to `RegistryBuilder::register(d, p)`. This module is that emitter, driven by
//! the thin [`crate::descriptor::BeanInput`] model the macro parses with `syn`.
//!
//! The `Descriptor`/`ProviderSeed`/`InjectionPlan`/`AnnotationMetadata`/`Provider`
//! values are ALL absolute-`::leaf_core`-pathed const data — including the
//! distributed-slice attribute, which is named through leaf-core's `pub use linkme;`
//! re-export as `::leaf_core::linkme::distributed_slice` plus a `#[linkme(crate =
//! ::leaf_core::linkme)]` override so linkme's runtime types resolve there too.
//! The emitted artifact therefore contains ZERO non-`::leaf_core` paths, so a
//! contributing crate needs only a `leaf-core` dep (no direct `linkme`). The bean
//! is also opted into the engine-resolution seam via an emitted
//! `impl ::leaf_core::Bean for Ty {}`.
//!
//! ## The seams the emitted const row crosses
//!
//! - **`self_type`** — `TypeId::of` is not a plain `const fn`, but the
//!   inline-`const { … }` block IS const-evaluable on stable (≥ 1.91), so the row
//!   emits `self_type: const { ::core::any::TypeId::of::<Ty>() }` — a const seam,
//!   no `static`/`OnceLock`. Same shape backs each `TypeRow.view` and every
//!   `InjectionPoint.produced`.
//! - **`contract`** — the stable cross-build [`leaf_core::ContractId`] over the
//!   author-stable identity string (`crate::Module::Ident`), minted by the const
//!   `::leaf_core::ContractId::of(path)`.
//! - **the name** — explicit (the `name = "…"` attribute) or derived from the
//!   simple ident through Spring's `decapitalize`
//!   ([`leaf_core::derive_default_name`]) at MACRO time, so the const row carries a
//!   ready `&'static str`.
//! - **the meta** — the flattened const [`leaf_core::AnnotationMetadata`] the
//!   [`crate::annotation`] merge engine lowers (markers / qualifiers / depends-on /
//!   candidate role) — annotation-model OWNS `Descriptor.meta`.
//! - **the `ProviderSeed`** — a const `fn() -> Arc<dyn Provider>` that builds a
//!   generated `Provider` whose `provide` resolves each [`leaf_core::InjectionPoint`]
//!   (the [`leaf_core::InjectionPlan`]) through the `ResolveCtx` engine back-ref and
//!   invokes the bean's constructor.
//!
//! Generic beans hard-error here with a `register_component!(Concrete)` hint
//! ([`emit`] returns an [`EmitError`] the thin macro turns into `compile_error!`).

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::Type;

use crate::annotation::MergedAnnotation;

/// The framework-vs-application provenance axis emitted into `Descriptor.role`.
///
/// Mirrors the frozen `::leaf_core::Role` taxonomy; a stereotype differs from a
/// plain `@component` ONLY in this axis + its `meta.markers` (phase3/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    /// An ordinary application bean (the default).
    #[default]
    Application,
    /// A support concern.
    Support,
    /// Framework infrastructure (outermost).
    Infrastructure,
}

impl Role {
    /// The absolute `::leaf_core::Role` path expression for this role.
    fn tokens(self) -> TokenStream {
        match self {
            Role::Application => quote! { ::leaf_core::Role::Application },
            Role::Support => quote! { ::leaf_core::Role::Support },
            Role::Infrastructure => quote! { ::leaf_core::Role::Infrastructure },
        }
    }
}

/// The built-in scope an emitted bean targets (the const `ScopeDef` triple).
///
/// One of the three built-in `::leaf_core::ScopeDef` consts; a custom scope is a
/// later concern (it lowers to its own const triple, not a new mechanism).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    /// One shared, container-stored, managed-teardown instance (the default).
    #[default]
    Singleton,
    /// A fresh per-resolution owned move, no store, no teardown.
    Prototype,
    /// One shared instance per request `Cx`.
    Request,
}

impl Scope {
    /// The absolute `::leaf_core::ScopeDef` const path for this scope.
    fn tokens(self) -> TokenStream {
        match self {
            Scope::Singleton => quote! { ::leaf_core::ScopeDef::SINGLETON },
            Scope::Prototype => quote! { ::leaf_core::ScopeDef::PROTOTYPE },
            Scope::Request => quote! { ::leaf_core::ScopeDef::REQUEST },
        }
    }
}

/// Which distributed slice a bean's const `Descriptor` row is submitted into.
///
/// `COMPONENTS` is THE bean channel (stereotypes, scanned candidates, `@bean`);
/// `AUTO_CONFIGS` is the SEPARATE auto-configuration channel so component-scanning
/// over `COMPONENTS` never picks an auto-config up (the AutoConfigurationExcludeFilter
/// boundary, made structural). The SAME const `Descriptor` shape rides both — there
/// is no second seed type; an auto-config differs only in the channel + its
/// `CandidateRole::FALLBACK`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Slice {
    /// THE bean channel (the default).
    #[default]
    Components,
    /// The auto-configuration channel (`#[auto_config]`).
    AutoConfigs,
}

impl Slice {
    /// The absolute `::leaf_core::<SLICE>` path the `#[distributed_slice]` attribute
    /// targets.
    fn tokens(self) -> TokenStream {
        match self {
            Slice::Components => quote! { ::leaf_core::COMPONENTS },
            Slice::AutoConfigs => quote! { ::leaf_core::AUTO_CONFIGS },
        }
    }
}

/// One declared dependency of a bean's constructor — an injection point.
///
/// The emitter lowers each into one const `::leaf_core::InjectionPoint` row in the
/// bean's `InjectionPlan` and resolves it (through the `ResolveCtx` engine
/// back-ref) inside the generated `Provider`. `ty` is the collaborator's concrete
/// or `dyn Svc` type; `name` is the declared field/param ident (the implicit
/// string qualifier).
#[derive(Clone, Debug)]
pub struct Dependency {
    /// The declared field/param ident (the implicit string qualifier).
    pub name: String,
    /// The collaborator type to resolve.
    pub ty: Type,
}

/// One declared injectable `dyn Svc` view a bean is upcastable to (a `TypeRow`).
///
/// The emitter lowers each into one const `::leaf_core::TypeRow { view, upcast }`
/// row in `Descriptor.provides`, with the const fn-pointer performing the
/// trait-upcast coercion (`Arc<Concrete>` → `Arc<dyn Svc>`, stable since 1.86).
#[derive(Clone, Debug)]
pub struct ServiceView {
    /// The `dyn Svc` trait-object type this bean is injectable as.
    pub dyn_ty: Type,
}

/// The thin parsed model of a bean the macro feeds to the emitter.
///
/// This is the bridge between `syn` parsing (in the thin `leaf-macros`) and the
/// const-row emission (here). It carries the concrete type, the (already merged)
/// annotation view, the role/scope axes, the constructor's injection points, the
/// declared service-trait upcasts, and the optional explicit name.
#[derive(Clone, Debug)]
pub struct BeanInput {
    /// The bean's concrete type (the `self_type` and constructor receiver).
    pub self_ty: Type,
    /// The simple ident string used to derive the default name + the
    /// `crate::Module::Ident`-shaped contract identity path.
    pub ident: String,
    /// The author-stable identity path (`crate::module::Ident`) minting the
    /// `ContractId`. The macro builds it from `module_path!()` + the ident.
    pub contract_path: String,
    /// When `true`, the contract is module-qualified at the DEFINITION SITE via
    /// `concat!(module_path!(), "::", ident)` rather than emitted as the literal
    /// [`contract_path`](Self::contract_path) — the thin-macro path (a macro cannot
    /// resolve the bean's module at expansion, so the qualification is deferred to
    /// the const initializer at the use site). Defaults to `false` so hand-built
    /// inputs (and the emitter's own unit tests) keep emitting the literal.
    pub module_qualified: bool,
    /// An explicit `name = "…"` override; `None` derives from [`ident`](Self::ident).
    pub explicit_name: Option<String>,
    /// The role axis (stereotype-driven).
    pub role: Role,
    /// The scope triple to emit.
    pub scope: Scope,
    /// The constructor's declared dependencies (injection points).
    pub deps: Vec<Dependency>,
    /// The declared injectable `dyn Svc` views (`provides[]` upcast rows).
    pub provides: Vec<ServiceView>,
    /// The merged annotation metadata (markers / qualifiers / candidate role).
    pub meta: MergedAnnotation,
    /// The constructor CALL the generated `Provider` invokes after resolving the
    /// deps. `None` (the default) calls the inherent `#self_ty::new(args)`; `Some`
    /// is a free factory-function path (`#[bean] fn make(...) -> Svc`) called as
    /// `#path(args)` — the SAME const row shape, one seed type, just a different
    /// construction recipe.
    pub ctor: Option<syn::Path>,
    /// The CONFIGURATION-CLASS receiver type a `@bean` METHOD is called on. When
    /// `Some(Cfg)`, the generated provider resolves the config bean `Cfg` first
    /// (through the one `Engine::get` seam, so a `&self` method reads the managed
    /// config singleton) and invokes [`ctor`](Self::ctor) as a METHOD on it
    /// (`__recv.method(args)`) — the design's lite-only `#[configuration] impl Cfg {
    /// #[bean] fn .. }` shape (configuration-classes, phase3/05). `None` is the
    /// free-fn / inherent-`new` path. One Descriptor per method, no second seed type.
    pub receiver_ty: Option<Type>,
    /// `true` iff the target is generic (a hard error — see [`EmitError`]).
    pub is_generic: bool,
    /// Which distributed slice the const `Descriptor` row is submitted into
    /// (`COMPONENTS` by default; `AUTO_CONFIGS` for `#[auto_config]`).
    pub slice: Slice,
}

impl BeanInput {
    /// A minimal bean input over a concrete type + identity, with no deps, no
    /// service views, default role/scope, and empty annotation metadata.
    #[must_use]
    pub fn new(self_ty: Type, ident: impl Into<String>, contract_path: impl Into<String>) -> Self {
        BeanInput {
            self_ty,
            ident: ident.into(),
            contract_path: contract_path.into(),
            module_qualified: false,
            explicit_name: None,
            role: Role::default(),
            scope: Scope::default(),
            deps: Vec::new(),
            provides: Vec::new(),
            meta: MergedAnnotation::default(),
            ctor: None,
            receiver_ty: None,
            is_generic: false,
            slice: Slice::default(),
        }
    }
}

/// A bean the emitter refuses to lower — a Tier-0 `compile_error!` (phase3/02).
///
/// The headline case: a GENERIC bean. A generic type has no single concrete
/// `TypeId`/`ContractId`, so it cannot be a const registry row; the remediation is
/// to register a concrete instantiation via `register_component!(Concrete)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmitError {
    /// The human-readable explanation the thin macro emits verbatim.
    pub message: String,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EmitError {}

/// Emit the full const registration artifact for one bean: the per-bean
/// `InjectionPlan`, the `ProviderSeed`-built `Provider`, the const `Descriptor`
/// row, and the `linkme` `COMPONENTS` submission — all via absolute `::leaf_core`
/// paths.
///
/// # Errors
/// Returns an [`EmitError`] (→ `compile_error!`) when the bean cannot be lowered
/// to a const row — currently: a GENERIC target (hinting `register_component!`).
pub fn emit(input: &BeanInput) -> Result<TokenStream, EmitError> {
    if input.is_generic {
        return Err(EmitError {
            message: format!(
                "`{}` is generic: a generic bean has no single concrete type to \
                 register as a const Descriptor row. Register a concrete \
                 instantiation with `register_component!({}<Concrete>)`.",
                input.ident, input.ident
            ),
        });
    }

    let self_ty = &input.self_ty;
    // The const TypeId-of seam: `TypeId::of` is not a plain const fn, but the
    // inline-`const { … }` block is const-evaluable on stable (≥ 1.91), so the
    // whole row stays const — no `static`/`OnceLock`.
    let self_type = type_id_of(self_ty);

    // The bean name: explicit `name = "…"` or Spring's `decapitalize` of the
    // simple ident, derived HERE at macro time so the const row carries a ready
    // `&'static str`.
    let declared_name = match &input.explicit_name {
        Some(n) => n.clone(),
        None => leaf_core::derive_default_name(&input.ident).into_owned(),
    };

    let contract = emit_contract(input);
    let role = input.role.tokens();
    let scope = input.scope.tokens();
    let meta = input.meta.lower();
    let provides = emit_provides(&input.provides);

    // Unique, hygienic-ish identifiers for the emitted helper items. The macro
    // mangles on the bean ident so two beans in one module never collide.
    let mangled = mangle(&input.ident);
    let points_ident = format_ident!("__LEAF_POINTS_{}", mangled);
    let plan_ident = format_ident!("__LEAF_PLAN_{}", mangled);
    let meta_ident = format_ident!("__LEAF_META_{}", mangled);
    let provider_ident = format_ident!("__LeafProvider_{}", mangled);
    // The seed is PUBLIC under a deterministic `__leaf_seed_<Ident>` name (keyed on
    // the raw ident) so the assembly pass can pair the `COMPONENTS` descriptor with
    // its construction recipe; the rest of the helper items stay private-mangled.
    let seed_ident = format_ident!("__leaf_seed_{}", mangled);
    let desc_ident = format_ident!("__LEAF_DESCRIPTOR_{}", mangled);
    // The per-bean pairing-channel submission rows: the `__leaf_seed_<Ident>`
    // ProviderSeed and the per-bean `InjectionPlan` are auto-collected into the
    // `SEED_PAIRINGS`/`INJECTION_PLAN_PAIRINGS` slices (the COMPONENTS auto-collect
    // substrate, extended) so a normal annotated app needs no hand-assembled
    // `.with_seeds`/`.with_injection_plans` calls.
    let seed_row_ident = format_ident!("__LEAF_SEED_PAIRING_{}", mangled);
    let plan_row_ident = format_ident!("__LEAF_PLAN_PAIRING_{}", mangled);

    let points = emit_injection_points(&input.deps);
    let provider_impl = emit_provider(
        self_ty,
        input.ctor.as_ref(),
        input.receiver_ty.as_ref(),
        &input.deps,
        &provider_ident,
        &desc_ident,
    );
    let slice = input.slice.tokens();

    Ok(quote! {
        // ── the per-bean InjectionPlan (one const InjectionPoint per dependency) ──
        #[allow(non_upper_case_globals)]
        const #points_ident: &[::leaf_core::InjectionPoint] = &[ #(#points),* ];
        #[allow(non_upper_case_globals)]
        const #plan_ident: ::leaf_core::InjectionPlan =
            ::leaf_core::InjectionPlan { points: #points_ident };

        // ── the flattened const AnnotationMetadata (annotation-model owns this) ──
        #[allow(non_upper_case_globals)]
        static #meta_ident: ::leaf_core::AnnotationMetadata = #meta;

        // ── the engine-resolvability marker: `Engine::get::<T>` requires
        // `T: ::leaf_core::Bean` (NOT a blanket impl), so the bean is opted into the
        // one resolution seam here (the row would otherwise be unusable end-to-end).
        impl ::leaf_core::Bean for #self_ty {}

        // ── the generated Provider + the PUBLIC const ProviderSeed that BUILDS it ──
        // `#[doc(hidden)]`: the `__leaf_seed_<Ident>` const is framework-internal wiring
        // (the assembly pass's pairing key), not public API — so a contributing crate
        // under `#![warn(missing_docs)]` needs no doc on this generated const.
        #provider_impl
        #[allow(non_upper_case_globals)]
        #[doc(hidden)]
        pub const #seed_ident: ::leaf_core::ProviderSeed =
            || ::std::sync::Arc::new(#provider_ident);

        // ── the const Descriptor row, submitted into the COMPONENTS slice ──
        // CROSS-CRATE re-export (verified empirically against real leaf-core, NO
        // direct linkme dep in the contributing crate): the row reaches the slice
        // through leaf-core's `pub use linkme;` via TWO cooperating pieces —
        //   1. the attribute macro is named by its fully-qualified re-export path
        //      `#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]`
        //      (a proc-macro attribute DOES resolve by absolute path on stable), and
        //   2. `#[linkme(crate = ::leaf_core::linkme)]` overrides linkme's default
        //      `::linkme` so its runtime types (`DistributedSlice`, the `__private`
        //      module, `Void`) also resolve through the re-export.
        // Piece 2 is load-bearing: without it the element expansion emits a bare
        // `::linkme::…` runtime path → `E0433: cannot find linkme in the crate root`
        // (the exact failure that made a prior pass believe the re-export "does not
        // resolve"). With both, a contributing crate needs ONLY a `leaf-core` dep.
        // NOTE: the frozen `::leaf_core::COMPONENTS` slice carries a bare
        // `Descriptor` (no `seed`/`plan` link on the frozen row), so the
        // Descriptor→ProviderSeed/InjectionPlan pairing is completed by the
        // leaf-boot assembly pass; this unit emits the seed under the deterministic
        // public `__leaf_seed_<Ident>` name beside the row so that pass can pair them.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(#slice)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #desc_ident: ::leaf_core::Descriptor = ::leaf_core::Descriptor {
            contract: #contract,
            self_type: #self_type,
            provides: #provides,
            declared_name: ::core::option::Option::Some(#declared_name),
            aliases: &[],
            scope: #scope,
            role: #role,
            meta: &#meta_ident,
            parent: ::core::option::Option::None,
            origin: ::leaf_core::Origin::Native {
                crate_name: ::core::option::Option::Some(::core::env!("CARGO_PKG_NAME")),
            },
        };
        // ── the per-bean wiring-pairing submissions (auto-collect substrate) ──
        // Submit the `__leaf_seed_<Ident>` ProviderSeed and the per-bean
        // `InjectionPlan` into their `::leaf_core` distributed slices via the SAME
        // re-export pattern as the COMPONENTS row above, so leaf-boot's
        // `from_slices`/wave-planner auto-collect them by ContractId — no
        // hand-assembled `.with_seeds`/`.with_injection_plans` required. Binding the
        // plan const through the row also keeps it from being DCE'd before the
        // assembly pass can read it.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #seed_row_ident: ::leaf_core::SeedPairingRow = ::leaf_core::SeedPairingRow {
            contract: #contract,
            seed: #seed_ident,
        };
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::INJECTION_PLAN_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #plan_row_ident: ::leaf_core::InjectionPlanPairingRow =
            ::leaf_core::InjectionPlanPairingRow {
                contract: #contract,
                plan: #plan_ident,
            };
    })
}

/// Emit the bean's stable `::leaf_core::ContractId` const expression.
///
/// Two shapes: a [`module_qualified`](BeanInput::module_qualified) input defers the
/// module qualification to the DEFINITION SITE (`concat!(module_path!(), "::",
/// ident)`) because a thin macro cannot resolve the bean's module at expansion;
/// otherwise the already-qualified [`contract_path`](BeanInput::contract_path)
/// literal is emitted verbatim.
fn emit_contract(input: &BeanInput) -> TokenStream {
    if input.module_qualified {
        let ident = &input.ident;
        quote! {
            ::leaf_core::ContractId::of(
                ::core::concat!(::core::module_path!(), "::", #ident)
            )
        }
    } else {
        let contract_path = &input.contract_path;
        quote! { ::leaf_core::ContractId::of(#contract_path) }
    }
}

/// The const `TypeId`-of seam: `const { ::core::any::TypeId::of::<Ty>() }`.
///
/// `TypeId::of` is not a plain `const fn`, but the inline-`const { … }` block IS
/// const-evaluable on stable (≥ 1.91), so the emitted row needs no `static` /
/// `OnceLock` to mint a `TypeId` — it stays a const expression.
fn type_id_of(ty: &Type) -> TokenStream {
    quote! { const { ::core::any::TypeId::of::<#ty>() } }
}

/// The bean type a field/param of type `ty` injects: `Ref<T>` → `T` (the field
/// stores the shared handle; the provider resolves `T` and threads the `Ref<T>` in),
/// any other type → itself. Shared by the struct-field, free-fn-param, and
/// `#[configuration]`-method-param lowerings so the Ref-stripping rule is one place.
#[must_use]
pub fn produced_ty(ty: &Type) -> Type {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Ref"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return inner.clone();
    }
    ty.clone()
}

/// Lower the constructor's dependencies to const `::leaf_core::InjectionPoint`
/// rows (one per dependency — `single(produced, name)`, by-type by default).
fn emit_injection_points(deps: &[Dependency]) -> Vec<TokenStream> {
    deps.iter()
        .map(|dep| {
            let produced = type_id_of(&dep.ty);
            let name = &dep.name;
            quote! { ::leaf_core::InjectionPoint::single(#produced, #name) }
        })
        .collect()
}

/// Lower the declared `dyn Svc` views to a const `&[::leaf_core::TypeRow]`.
///
/// Each row pairs the view `TypeId` with a const fn-pointer that re-erases the
/// bean's `Arc` through the `dyn Svc` view (trait upcasting, stable since 1.86).
fn emit_provides(provides: &[ServiceView]) -> TokenStream {
    if provides.is_empty() {
        return quote! { &[] };
    }
    let rows = provides.iter().map(|sv| {
        let dyn_ty = &sv.dyn_ty;
        let view = type_id_of(dyn_ty);
        quote! {
            ::leaf_core::TypeRow {
                view: #view,
                upcast: |__bean: ::leaf_core::ErasedBean| -> ::leaf_core::ErasedBean { __bean },
            }
        }
    });
    quote! { &[ #(#rows),* ] }
}

/// Emit the generated `Provider` impl whose `provide` resolves each injection
/// point (through the `ResolveCtx` engine back-ref) and invokes the bean's
/// constructor, publishing the result as the origin-agnostic `Published`.
///
/// NOTE: the by-ref/by-value/owned adaptation of a resolved `Ref<T>` to the
/// constructor's declared parameter shape is the injection-resolution codegen
/// unit's concern; this emitter resolves each collaborator through the one
/// `Engine::get` seam and threads the handle into the constructor call. The
/// resulting tokens parse + reference only the frozen `::leaf_core` seam.
fn emit_provider(
    self_ty: &Type,
    ctor: Option<&syn::Path>,
    receiver_ty: Option<&Type>,
    deps: &[Dependency],
    provider_ident: &syn::Ident,
    desc_ident: &syn::Ident,
) -> TokenStream {
    // Resolve each dependency through the engine back-ref, then bind it to a
    // local the constructor call consumes.
    let resolves = deps.iter().map(|dep| {
        let local = format_ident!("__dep_{}", dep.name);
        let dep_ty = &dep.ty;
        quote! {
            let #local: ::leaf_core::Ref<#dep_ty> = __engine.get::<#dep_ty>().await?;
        }
    });
    let args = deps.iter().map(|dep| {
        let local = format_ident!("__dep_{}", dep.name);
        quote! { #local }
    });
    // The construction recipe, one of three shapes (all ONE Descriptor row / one
    // seed type — only the recipe differs):
    //   * a CONFIGURATION-CLASS `@bean` METHOD (`receiver_ty = Some(Cfg)`): resolve
    //     the config bean `Cfg` through the same `Engine::get` seam (so a `&self`
    //     method reads the MANAGED config singleton — singleton-correct by
    //     construction) and call `__recv.method(args)` (a `Ref<Cfg>` derefs to
    //     `Cfg`, so an `&self` method binds directly). This is the design's lite-only
    //     `#[configuration] impl Cfg { #[bean] fn .. }` lowering;
    //   * a free factory-fn path (`#[bean] fn`): `#path(args)`;
    //   * the inherent associated `new`, ANGLE-BRACKET-QUALIFIED `<Ty>::new(...)` so
    //     it is valid in expression position for ANY type — including a generic
    //     concrete `register_component!(Repo<u32>)`, where a bare `Repo<u32>::new()`
    //     would mis-parse as a chained comparison.
    let (receiver_resolve, construct) = match (receiver_ty, ctor) {
        (Some(recv), Some(method)) => (
            quote! {
                let __recv: ::leaf_core::Ref<#recv> = __engine.get::<#recv>().await?;
            },
            quote! { __recv.#method( #(#args),* ) },
        ),
        (_, Some(path)) => (TokenStream::new(), quote! { #path( #(#args),* ) }),
        (_, None) => (TokenStream::new(), quote! { <#self_ty>::new( #(#args),* ) }),
    };

    quote! {
        #[allow(non_camel_case_types)]
        struct #provider_ident;
        impl ::leaf_core::Provider for #provider_ident {
            fn descriptor(&self) -> &::leaf_core::Descriptor {
                &#desc_ident
            }
            fn provide<'__a>(
                &'__a self,
                __cx: &'__a ::leaf_core::ResolveCtx<'__a>,
            ) -> ::leaf_core::BoxFuture<
                '__a,
                ::core::result::Result<::leaf_core::Published, ::leaf_core::LeafError>,
            > {
                ::std::boxed::Box::pin(async move {
                    let __engine = __cx.engine().ok_or_else(|| {
                        ::leaf_core::LeafError::new(::leaf_core::ErrorKind::ConstructionFailed)
                    })?;
                    #receiver_resolve
                    #(#resolves)*
                    let __instance = #construct;
                    ::core::result::Result::Ok(::leaf_core::Published::shared_value(__instance))
                })
            }
        }
    }
}

/// A spans-free, identifier-safe mangling of a bean ident for the emitted helper
/// item names (so two beans in one module never collide).
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, Span::call_site())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, AttrValue};

    /// A `syn::Type` from a type-expression string (test ergonomics).
    fn ty(s: &str) -> Type {
        syn::parse_str::<Type>(s).expect("a valid type")
    }

    /// Render a `TokenStream` to a whitespace-collapsed string so assertions are
    /// robust against `quote!`'s token spacing (`:: leaf_core` vs `::leaf_core`).
    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    /// The headline bean: `struct Foo(Bar)` — one ctor dependency on `Bar`.
    fn foo_with_bar() -> BeanInput {
        let mut input = BeanInput::new(ty("Foo"), "Foo", "crate::Foo");
        input.deps.push(Dependency { name: "bar".into(), ty: ty("Bar") });
        input
    }

    #[test]
    fn emits_an_absolute_core_descriptor_const_row() {
        // The heart: a sample `struct Foo(Bar)` lowers to one const Descriptor row
        // using ABSOLUTE ::leaf_core paths (a user crate's imports cannot shadow
        // them). The whole emitted artifact must PARSE as a Rust item sequence.
        let ts = emit(&foo_with_bar()).expect("a concrete bean emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::Descriptor"), "got: {s}");
    }

    #[test]
    fn descriptor_carries_the_contract_id_and_derived_name() {
        // The row mints its stable cross-build identity from the author-stable
        // identity path, and derives the bean name via Spring's decapitalize at
        // MACRO time (so the const row carries a ready &'static str).
        let ts = emit(&foo_with_bar()).expect("emits");
        let s = flat(&ts);
        assert!(s.contains(r#"::leaf_core::ContractId::of("crate::Foo")"#), "got: {s}");
        // `Foo` decapitalizes to `foo`.
        assert!(s.contains(r#"declared_name:::core::option::Option::Some("foo")"#), "got: {s}");
    }

    #[test]
    fn module_qualified_contract_uses_module_path_at_the_definition_site() {
        // A macro cannot know the bean's module at expansion, so the contract
        // identity is module-qualified at the DEFINITION SITE via `module_path!()`
        // (concatenated with the ident) — the design's "package + module-path +
        // ident" identity. Opt-in so the plain (already-qualified) string path the
        // unit tests use is unaffected.
        let mut input = foo_with_bar();
        input.module_qualified = true;
        let s = flat(&emit(&input).expect("emits"));
        assert!(
            s.contains(
                r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","Foo"))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn explicit_name_overrides_the_derived_one() {
        // A `name = "fooBean"` explicit attribute wins over the derived default.
        let mut input = foo_with_bar();
        input.explicit_name = Some("fooBean".into());
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains(r#"Some("fooBean")"#), "got: {s}");
        assert!(!s.contains(r#"Some("foo")"#), "derived name must not also appear: {s}");
    }

    #[test]
    fn self_type_is_emitted_through_the_const_typeid_seam() {
        // TypeId::of is not a plain const fn, so the row emits the inline
        // `const { ::core::any::TypeId::of::<Foo>() }` seam (stable >= 1.91) — no
        // static/OnceLock — for the bean's own self_type.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(
            s.contains("self_type:const{::core::any::TypeId::of::<Foo>()}"),
            "got: {s}"
        );
    }

    #[test]
    fn injection_plan_has_one_point_per_constructor_dependency() {
        // `struct Foo(Bar)` => an InjectionPlan with exactly ONE InjectionPoint,
        // for `Bar`, carrying the param name as the implicit string qualifier.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("::leaf_core::InjectionPlan"), "got: {s}");
        assert!(
            s.contains(
                r#"::leaf_core::InjectionPoint::single(const{::core::any::TypeId::of::<Bar>()},"bar")"#
            ),
            "got: {s}"
        );
        // Exactly one point.
        assert_eq!(s.matches("InjectionPoint::single").count(), 1, "got: {s}");
    }

    #[test]
    fn a_bean_with_no_dependencies_has_an_empty_plan() {
        // A `struct Foo;` (no collaborators) emits a plan with zero points.
        let input = BeanInput::new(ty("Foo"), "Foo", "crate::Foo");
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains("::leaf_core::InjectionPlan"), "got: {s}");
        assert_eq!(s.matches("InjectionPoint::single").count(), 0, "got: {s}");
    }

    #[test]
    fn provider_seed_is_a_const_fn_pointer_that_builds_a_provider() {
        // The const row's construction recipe is a ::leaf_core::ProviderSeed — a
        // const fn-pointer building a generated Provider (not a live object).
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("::leaf_core::ProviderSeed"), "got: {s}");
        // The generated Provider impls the frozen seam.
        assert!(s.contains("impl::leaf_core::Providerfor"), "got: {s}");
        // The seed mints a fresh Arc<dyn Provider> (never a live object on the row).
        assert!(s.contains("::std::sync::Arc::new"), "got: {s}");
    }

    #[test]
    fn provider_resolves_each_dependency_and_invokes_the_constructor() {
        // The generated provider resolves each collaborator through the one
        // Engine::get seam (the ResolveCtx engine back-ref) and threads the handle
        // into the bean's constructor.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("__engine.get::<Bar>().await?"), "got: {s}");
        assert!(s.contains("<Foo>::new(__dep_bar)"), "got: {s}");
        assert!(s.contains("::leaf_core::Published::shared_value"), "got: {s}");
    }

    #[test]
    fn descriptor_is_submitted_into_the_components_linkme_slice() {
        // The row is contributed via the linkme distributed-slice attr so link-time
        // collection picks it up cross-crate with zero life-before-main. Both the
        // attribute macro AND linkme's runtime types are reached through leaf-core's
        // `pub use linkme;` re-export: the attr is named by its fully-qualified
        // `::leaf_core::linkme::distributed_slice` path, and `#[linkme(crate =
        // ::leaf_core::linkme)]` redirects linkme's own runtime path so a
        // contributing crate needs NO direct `linkme` dep (proven by the
        // leaf-macros integration tests, which have no linkme dev-dep).
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
        assert!(s.contains("#[linkme(crate=::leaf_core::linkme)]"), "got: {s}");
    }

    #[test]
    fn seed_and_injection_plan_are_submitted_into_their_pairing_slices() {
        // Beside the COMPONENTS row, the macro auto-collects the bean's
        // `__leaf_seed_<Ident>` ProviderSeed + per-bean InjectionPlan into the
        // SEED_PAIRINGS / INJECTION_PLAN_PAIRINGS channels (the COMPONENTS
        // auto-collect substrate, extended) — so a normal annotated app needs no
        // hand-assembled `.with_seeds`/`.with_injection_plans`. Same re-export
        // pattern as COMPONENTS (no direct linkme dep in the contributing crate).
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::SeedPairingRow{contract:"), "got: {s}");
        assert!(s.contains("seed:__leaf_seed_Foo"), "got: {s}");
        assert!(
            s.contains(
                "#[::leaf_core::linkme::distributed_slice(::leaf_core::INJECTION_PLAN_PAIRINGS)]"
            ),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::InjectionPlanPairingRow{contract:"), "got: {s}");
    }

    #[test]
    fn an_auto_config_bean_targets_the_auto_configs_slice() {
        // An auto-config differs structurally ONLY in the channel: the SAME const
        // Descriptor shape is submitted into the SEPARATE AUTO_CONFIGS slice (so
        // component-scanning over COMPONENTS never picks it up).
        let mut input = foo_with_bar();
        input.slice = Slice::AutoConfigs;
        let s = flat(&emit(&input).expect("emits"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]"),
            "got: {s}"
        );
        // It must NOT also land in COMPONENTS.
        assert!(
            !s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "an auto-config must not be in COMPONENTS: {s}"
        );
    }

    #[test]
    fn the_default_slice_is_components() {
        // A plain bean defaults to the COMPONENTS channel.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
    }

    #[test]
    fn the_bean_gets_a_leaf_core_bean_impl_so_it_is_engine_resolvable() {
        // `Engine::get::<T>` requires `T: ::leaf_core::Bean` (NOT a blanket impl).
        // The emitter therefore emits the marker impl beside the row so the bean is
        // resolvable through the one engine seam — without it the roundtrip cannot
        // produce the bean.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("impl::leaf_core::BeanforFoo{}"), "got: {s}");
    }

    #[test]
    fn the_provider_seed_is_a_public_const_under_a_deterministic_ident() {
        // The seed is exposed under a deterministic PUBLIC name (`__leaf_seed_<Ident>`)
        // so the assembly pass (leaf-boot — or a hand-written test standing in for
        // it) can PAIR the `COMPONENTS` descriptor with its construction recipe. The
        // ident keys on the raw bean ident, not the mangled screaming-case form.
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(
            s.contains("pubconst__leaf_seed_Foo:::leaf_core::ProviderSeed"),
            "got: {s}"
        );
    }

    #[test]
    fn role_axis_is_emitted_from_the_input() {
        // A stereotype differs from @component ONLY in role + meta.markers; the
        // role axis lowers to the absolute ::leaf_core::Role path.
        let mut input = foo_with_bar();
        input.role = Role::Infrastructure;
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains("role:::leaf_core::Role::Infrastructure"), "got: {s}");
    }

    #[test]
    fn default_role_is_application_and_default_scope_is_singleton() {
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("role:::leaf_core::Role::Application"), "got: {s}");
        assert!(s.contains("scope:::leaf_core::ScopeDef::SINGLETON"), "got: {s}");
    }

    #[test]
    fn prototype_and_request_scopes_lower_to_their_const_triples() {
        let mut proto = foo_with_bar();
        proto.scope = Scope::Prototype;
        assert!(
            flat(&emit(&proto).expect("emits")).contains("::leaf_core::ScopeDef::PROTOTYPE")
        );
        let mut req = foo_with_bar();
        req.scope = Scope::Request;
        assert!(
            flat(&emit(&req).expect("emits")).contains("::leaf_core::ScopeDef::REQUEST")
        );
    }

    #[test]
    fn candidate_role_rides_the_annotation_metadata() {
        // Primary/Fallback (the CandidateRole axis) is carried in meta, lowered by
        // the annotation-model merge engine the emitter calls.
        let mut input = foo_with_bar();
        input.meta = crate::annotation::resolve(
            &Annotation::new("leaf::Service").with_attr("primary", AttrValue::Bool(true)),
        )
        .expect("resolves");
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains("::leaf_core::CandidateRole::PRIMARY"), "got: {s}");
    }

    #[test]
    fn declared_service_views_emit_typerow_upcast_rows() {
        // A bean declaring it is injectable as `dyn Greeter` emits one
        // provides[] TypeRow: the view TypeId + a const upcast fn-pointer.
        let mut input = foo_with_bar();
        input.provides.push(ServiceView { dyn_ty: ty("dyn Greeter") });
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains("::leaf_core::TypeRow"), "got: {s}");
        assert!(
            s.contains("view:const{::core::any::TypeId::of::<dynGreeter>()}"),
            "got: {s}"
        );
        // The upcast row carries a const fn-pointer over ErasedBean.
        assert!(s.contains("upcast:|__bean:::leaf_core::ErasedBean|"), "got: {s}");
    }

    #[test]
    fn no_declared_views_emit_an_empty_provides_slice() {
        let s = flat(&emit(&foo_with_bar()).expect("emits"));
        assert!(s.contains("provides:&[]"), "got: {s}");
    }

    #[test]
    fn the_meta_markers_are_flattened_by_the_annotation_engine() {
        // The emitter calls the annotation-model merge so a stereotype's whole
        // transitive marker closure lands in meta (annotation-model owns this).
        let mut input = foo_with_bar();
        let rest = Annotation::new("leaf::RestController")
            .with_meta(Annotation::new("leaf::Controller").with_meta(Annotation::new("leaf::Component")));
        input.meta = crate::annotation::resolve(&rest).expect("resolves");
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::RestController")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::Component")"#), "got: {s}");
    }

    #[test]
    fn generic_bean_is_a_hard_error_with_a_register_component_hint() {
        // A generic bean has no single concrete TypeId/ContractId, so it cannot be
        // a const row: a Tier-0 compile_error! the macro emits, hinting the
        // concrete-instantiation escape hatch.
        let mut input = BeanInput::new(ty("Repo<T>"), "Repo", "crate::Repo");
        input.is_generic = true;
        let err = emit(&input).expect_err("a generic bean must hard-error");
        assert!(err.message.contains("generic"), "got: {}", err.message);
        assert!(err.message.contains("register_component!"), "got: {}", err.message);
    }

    #[test]
    fn factory_fn_bean_emits_through_the_same_const_row_shape() {
        // A bean minted from a constructor with several params (the @bean-fn
        // shape: `fn make(a: A, b: B) -> Svc`) lowers to the SAME const Descriptor
        // + two-point InjectionPlan — one shape, no second seed type.
        let mut input = BeanInput::new(ty("Svc"), "Svc", "crate::make");
        input.deps.push(Dependency { name: "a".into(), ty: ty("A") });
        input.deps.push(Dependency { name: "b".into(), ty: ty("B") });
        let ts = emit(&input).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert_eq!(s.matches("InjectionPoint::single").count(), 2, "got: {s}");
        assert!(s.contains("__engine.get::<A>().await?"), "got: {s}");
        assert!(s.contains("__engine.get::<B>().await?"), "got: {s}");
    }

    #[test]
    fn a_bean_factory_fn_calls_the_free_function_instead_of_new() {
        // A `#[bean] fn make_svc(dep: Ref<Dep>) -> Svc` lowers to the SAME const row,
        // but the generated provider invokes the FREE factory fn (`make_svc(args)`)
        // rather than the inherent `Svc::new(args)` — one shape, one seed type.
        let mut input = BeanInput::new(ty("Svc"), "Svc", "crate::make_svc");
        input.deps.push(Dependency { name: "dep".into(), ty: ty("Dep") });
        input.ctor = Some(syn::parse_str("make_svc").expect("a fn path"));
        let s = flat(&emit(&input).expect("emits"));
        assert!(s.contains("make_svc(__dep_dep)"), "got: {s}");
        assert!(!s.contains("Svc::new("), "the factory fn replaces ::new: {s}");
        // Still resolves the collaborator through the one engine seam.
        assert!(s.contains("__engine.get::<Dep>().await?"), "got: {s}");
    }

    #[test]
    fn a_configuration_class_bean_method_resolves_the_config_receiver_and_calls_the_method() {
        // The design's lite-only `#[configuration] impl AppConfig { #[bean] fn pool(&self,
        // cfg: Ref<DbConfig>) -> Pool }` lowering: ONE Descriptor per method whose
        // generated provider resolves the CONFIG bean (the receiver) AND each param
        // through the one Engine::get seam, then calls the bean METHOD on the config —
        // so a `&self` method reads the MANAGED config singleton (singleton-correct).
        let mut input = BeanInput::new(ty("Pool"), "pool", "crate::AppConfig::pool");
        input.deps.push(Dependency { name: "cfg".into(), ty: ty("DbConfig") });
        input.ctor = Some(syn::parse_str("pool").expect("a method ident"));
        input.receiver_ty = Some(ty("AppConfig"));
        let ts = emit(&input).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The config receiver is resolved as a bean through the SAME engine seam.
        assert!(s.contains("let__recv:::leaf_core::Ref<AppConfig>=__engine.get::<AppConfig>().await?"), "got: {s}");
        // The param is resolved too.
        assert!(s.contains("__engine.get::<DbConfig>().await?"), "got: {s}");
        // The construct calls the METHOD on the resolved config instance.
        assert!(s.contains("__recv.pool(__dep_cfg)"), "got: {s}");
        // It must NOT call an inherent `Pool::new` or a free `pool(..)`.
        assert!(!s.contains("<Pool>::new("), "a config-method bean is not a ::new ctor: {s}");
    }

    #[test]
    fn the_whole_emitted_artifact_is_a_valid_rust_file() {
        // Belt-and-braces: every axis combined still parses as a Rust item
        // sequence (the macro drops these items at the bean's definition site).
        let mut input = foo_with_bar();
        input.role = Role::Support;
        input.scope = Scope::Prototype;
        input.explicit_name = Some("theFoo".into());
        input.provides.push(ServiceView { dyn_ty: ty("dyn Greeter") });
        input.deps.push(Dependency { name: "baz".into(), ty: ty("Baz") });
        input.meta = crate::annotation::resolve(
            &Annotation::new("leaf::Service")
                .with_attr("qualifiers", AttrValue::List(vec![AttrValue::Str("leaf::q::Fast".into())])),
        )
        .expect("resolves");
        let ts = emit(&input).expect("emits");
        syn::parse2::<syn::File>(ts).expect("the full artifact is valid Rust");
    }
}
