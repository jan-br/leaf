//! `leaf-macros` — the THIN proc-macro crate (charter §2.10 / phase3/02).
//!
//! Every macro here parses its input with `syn`, delegates ALL logic to the normal,
//! unit-testable [`leaf_codegen`] library, and emits the resulting tokens. There is
//! NO logic in this crate beyond the `proc_macro` ↔ `proc_macro2` bridge and the
//! error→`compile_error!` lowering — the heavy lifting (annotation flatten, the
//! const `Descriptor`/`ProviderSeed`/`InjectionPlan` emission, the stereotype
//! vocabulary, the generic hard-error) all lives in `leaf-codegen`.
//!
//! ## The stereotype + bean surface
//!
//! - `#[component]` — the base stereotype; emits one const `::leaf_core::Descriptor`
//!   row into the `COMPONENTS` slice + its `ProviderSeed`/`InjectionPlan` + the
//!   engine-resolvability `Bean` impl, all via absolute `::leaf_core` paths.
//! - `#[service]` / `#[repository]` / `#[controller]` / `#[configuration]` — the
//!   same row differing ONLY in the transitive `meta.markers` closure (each is a
//!   `@component` one-hop meta-edge), per component-stereotypes.
//! - `#[bean]` — a factory-method bean inside a `#[configuration]`, lowering to the
//!   SAME const row shape (one shape, no second seed type).
//! - `register_component!(Concrete)` — the escape hatch for a generic component: a
//!   generic target is a Tier-0 `compile_error!` with this hint.
//!
//! Generic targets hard-error with a `register_component!(Concrete)` hint (a generic
//! type has no single concrete `TypeId`/`ContractId`, so it cannot be a const row).

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, ItemFn, ItemStruct, Type};

use leaf_codegen::advisor;
use leaf_codegen::app;
use leaf_codegen::conditional;
use leaf_codegen::config;
use leaf_codegen::descriptor::EmitError;
use leaf_codegen::listener;
use leaf_codegen::scheduling;
use leaf_codegen::stereotype::{self, Stereotype};

/// Turn an [`EmitError`] into a `compile_error!` token stream (the one
/// error→diagnostic lowering the thin macros share).
fn compile_error(err: &EmitError) -> proc_macro2::TokenStream {
    let message = &err.message;
    quote! { ::core::compile_error!(#message); }
}

/// `#[component]` — the base stereotype. Emits one const `::leaf_core::Descriptor`
/// row (+ `ProviderSeed`/`InjectionPlan`/`Bean` impl) for the annotated struct.
///
/// Attribute args (all optional): `name = "…"` (override the derived default name),
/// `scope = "singleton" | "prototype" | "request"`.
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Component)
}

/// `#[service]` — a business-logic stereotype (`meta.markers` = `[Service,
/// Component]`); otherwise identical to `#[component]`.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Service)
}

/// `#[repository]` — a data-access stereotype (`meta.markers` = `[Repository,
/// Component]`); the `Repository` marker is the data point the exception-translation
/// advisor queries (it carries ZERO behaviour here).
#[proc_macro_attribute]
pub fn repository(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Repository)
}

/// `#[controller]` — a web-layer stereotype (`meta.markers` = `[Controller,
/// Component]`); otherwise identical to `#[component]`.
#[proc_macro_attribute]
pub fn controller(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Controller)
}

/// `#[configuration]` — a `@bean`-factory holder stereotype (`meta.markers` =
/// `[Configuration, Component]`); otherwise identical to `#[component]`.
#[proc_macro_attribute]
pub fn configuration(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Configuration)
}

/// `#[bean]` — a FACTORY-FUNCTION bean. Lowers a `fn make(deps…) -> Product` to the
/// SAME const row shape as `#[component]`, but the construction recipe is the
/// function itself (one shape, no second seed type).
///
/// Attribute args (all optional): `name = "…"`, `scope = "…"`.
///
/// NOTE: the `#[bean]`-on-a-method form (a method of a `#[configuration]` struct,
/// which threads the config instance as the receiver) is deferred — a `self`
/// receiver is a `compile_error!` here. Use a free `fn` factory in v1.
#[proc_macro_attribute]
pub fn bean(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let args = match stereotype::parse_args(attr.into()) {
        Ok(a) => a,
        Err(EmitError { message }) => {
            return quote! { #parsed ::core::compile_error!(#message); }.into();
        }
    };
    match stereotype::emit_bean(&parsed, args.name, args.scope) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(EmitError { message }) => {
            quote! { #parsed ::core::compile_error!(#message); }.into()
        }
    }
}

/// `register_component!(Concrete)` — register a CONCRETE type as a `@component`
/// (the escape hatch for a generic component: `register_component!(Repo<u32>)`).
///
/// Emits the same const `::leaf_core::Descriptor` row as a no-dependency
/// `#[component]` constructed via `Concrete::new()`. This is the `compile_error!`
/// remediation a generic `#[component]`/`#[bean]` points at.
#[proc_macro]
pub fn register_component(item: TokenStream) -> TokenStream {
    let ty = parse_macro_input!(item as Type);
    match stereotype::emit_register(&ty) {
        Ok(rows) => rows.into(),
        Err(EmitError { message }) => quote! { ::core::compile_error!(#message); }.into(),
    }
}

/// The ONE shared thin body for the struct stereotypes: parse the struct with
/// `syn`, delegate to `leaf_codegen::stereotype::emit_struct`, and emit
/// `<item> <const rows>` (or a `compile_error!` on a hard error). No logic lives
/// here.
fn expand_stereotype(attr: TokenStream, item: TokenStream, stereotype: Stereotype) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    match stereotype::emit_struct(&parsed, stereotype, attr.into()) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        // The generic/malformed hard error: keep the original item so downstream
        // name-resolution errors do not pile on top of the real diagnostic.
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

// ═══════════════════ the config + binding surface ═══════════════════════════

/// `#[derive(BindTarget)]` — derive the self-describing config-binding seam for a
/// named-field struct: the const `::leaf_core::NodeSchema` + the cursor-calling
/// `::leaf_core::BindTarget::bind` fold (the JavaBean field descent). Requires
/// `Default` (the binder fills unset fields from the default).
///
/// Scalar fields bind via `FromConfigValue`; a `Vec<T>` binds as a list; a nested
/// `BindTarget` struct binds recursively. A generic or non-struct target is a
/// Tier-0 `compile_error!`.
#[proc_macro_derive(BindTarget)]
pub fn derive_bind_target(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match config::emit_bind_target(&input) {
        Ok(ts) => ts.into(),
        Err(err) => compile_error(&err).into(),
    }
}

/// `#[config_properties(prefix = "app")]` — bind a struct from the environment under
/// a canonical key prefix. Emits the `BindTarget` derive artifact PLUS one
/// `::leaf_core::ConfigMetadataRow` (the anti-DCE/config-doc anchor on the
/// `CONFIG_METADATA` slice) and a const `::leaf_core::ConfigGroup` documenting every
/// bound key (the `leaf metadata` rollup input).
///
/// The struct must derive `Default` (the JavaBean default-fill convention). The
/// original item is kept verbatim; the macro only appends the const artifacts.
#[proc_macro_attribute]
pub fn config_properties(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as DeriveInput);
    let args = match config::parse_config_args(attr.into()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    match config::emit_config_properties(&parsed, &args) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

/// `#[value("${app.port:8080}")]` — lower a `${…}`/`#{…}` value template to the const
/// `&[::leaf_core::ValueSegment]` the placeholder engine interprets, binding it to
/// the annotated `const`/`static` item.
///
/// Applied to a `const NAME: &[::leaf_core::ValueSegment]` (or `static`) declaration:
/// the macro replaces the placeholder initializer with the split segment array, so
/// `#[value("${k:def}")] const TEMPLATE: &[::leaf_core::ValueSegment];` carries the
/// parsed template. A malformed template (unbalanced `${`/`#{`) is a Tier-0
/// `compile_error!`.
#[proc_macro_attribute]
pub fn value(attr: TokenStream, item: TokenStream) -> TokenStream {
    let decl = parse_macro_input!(item as ValueConst);
    let segments = match config::emit_value(attr.into()) {
        Ok(ts) => ts,
        Err(err) => return compile_error(&err).into(),
    };
    // Bind the const's initializer to the parsed segment array (keeping the declared
    // ident/type/visibility), so the const carries the split template. The user may
    // write the declaration with NO initializer (`const T: &[..];`) — the template
    // in the attribute IS the initializer.
    let ValueConst { attrs, vis, ident, ty } = &decl;
    quote! {
        #(#attrs)*
        #vis const #ident: #ty = #segments;
    }
    .into()
}

/// A `const`/`static` declaration the `#[value]` attribute reads — the initializer
/// is OPTIONAL (the value template in the attribute supplies it), so this parses
/// the plain `[#attrs] [vis] const IDENT: TYPE [= _]? ;` shape `syn::ItemConst`
/// rejects when the initializer is absent.
struct ValueConst {
    attrs: Vec<syn::Attribute>,
    vis: syn::Visibility,
    ident: syn::Ident,
    ty: Box<syn::Type>,
}

impl syn::parse::Parse for ValueConst {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        let vis: syn::Visibility = input.parse()?;
        input.parse::<syn::Token![const]>()?;
        let ident: syn::Ident = input.parse()?;
        input.parse::<syn::Token![:]>()?;
        let ty: syn::Type = input.parse()?;
        // An optional `= <expr>` placeholder initializer is consumed and discarded
        // (the value template in the attribute replaces it).
        if input.peek(syn::Token![=]) {
            input.parse::<syn::Token![=]>()?;
            input.parse::<syn::Expr>()?;
        }
        input.parse::<syn::Token![;]>()?;
        Ok(ValueConst { attrs, vis, ident, ty: Box::new(ty) })
    }
}

/// `#[converter]` — register a `::leaf_core::Converter` impl into the converter
/// `CATALOGS` slice (one `::leaf_core::CatalogRow` anti-DCE anchor keyed on the
/// converter's stable identity). The user writes the `impl Converter`; this wires
/// its discovery. The annotated item is kept verbatim.
#[proc_macro_attribute]
pub fn converter(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let rows = config::emit_converter(&parsed.ident.to_string());
    quote! { #parsed #rows }.into()
}

// ═══════════════════ the conditional + autoconfig surface ════════════════════

/// `#[conditional(on_property("k", having_value = "v"), on_bean(Foo), …)]` — gate an
/// element's registration on a const condition tree. Lowers the DSL
/// (`on_property`/`on_bean`/`on_missing_bean`/`on_class` leaves + first-class
/// `all`/`any`/`not`) to ONE const `::leaf_core::CondExpr` (a public pairing const
/// the assembly pass joins to the element's `Descriptor`) plus one
/// `::leaf_core::ConditionRow` anti-DCE anchor per referenced kind.
///
/// Stack this beside `#[component]`/`#[auto_config]` on the same struct: it keeps
/// the item verbatim and only appends the guard artifact.
#[proc_macro_attribute]
pub fn conditional(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let expr = match conditional::parse_conditional(attr.into()) {
        Ok(e) => e,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let guard = conditional::emit_guard(&parsed.ident.to_string(), &expr);
    quote! { #parsed #guard }.into()
}

/// `#[profile("prod & (eu | us)")]` — gate an element on the active profile set.
/// Profiles are a PRESET: the whole `!`/`&`/`|` expression lowers to ONE
/// `::leaf_core::CondExpr::Leaf(ON_PROFILE, …)` (the same guard machinery as
/// `#[conditional]`). Mixed `&`/`|` without parentheses is a Tier-0 `compile_error!`.
#[proc_macro_attribute]
pub fn profile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let expr = match conditional::parse_profile_attr(attr.into()) {
        Ok(e) => e,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let guard = conditional::emit_guard(
        &parsed.ident.to_string(),
        &conditional::profile_to_cond(&expr),
    );
    quote! { #parsed #guard }.into()
}

/// `#[auto_config]` — register a struct as an AUTO-CONFIGURATION: the SAME const
/// `::leaf_core::Descriptor` shape, but submitted into the SEPARATE `AUTO_CONFIGS`
/// slice at `CandidateRole::FALLBACK` (so a user bean transparently supersedes it),
/// and so component-scanning over `COMPONENTS` never picks it up. A generic target
/// is a Tier-0 `compile_error!`.
///
/// Gate it by stacking `#[conditional(...)]`/`#[profile(...)]` on the same struct.
#[proc_macro_attribute]
pub fn auto_config(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    match conditional::emit_auto_config(&parsed, None) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

/// `#[import(RedisAutoConfig, CacheAutoConfig)]` — compose other elements into this
/// one. Emits one const `::leaf_core::ImportEdge` (the `from`→`to[]` composition
/// currency the assembly pass reads) plus an anti-DCE force-reference so the
/// importer path-references each importee (closing Layer-0 DCE for the edge).
#[proc_macro_attribute]
pub fn import(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let imports = match conditional::parse_import(attr.into()) {
        Ok(p) => p,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let rows = conditional::emit_import(&parsed.ident.to_string(), &imports);
    quote! { #parsed #rows }.into()
}

// ═══════════════════ the declarative-advice / AOP surface ════════════════════

/// `#[advisable]` — mark a `#[component]` struct as a PROXY TARGET (the transparent-
/// newtype seam the proxy substrate wraps). Structurally a `#[component]`: it emits
/// the same const `::leaf_core::Descriptor` row so the bean is registered and is
/// resolvable; the proxy plan is built at refresh from the matching `ADVISORS`. A
/// generic target hard-errors with the `register_proxy!(Concrete)` hint.
#[proc_macro_attribute]
pub fn advisable(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_stereotype(attr, item, Stereotype::Component)
}

/// `register_proxy!(Concrete)` — register a CONCRETE proxyable type (the escape
/// hatch for a generic aspect/advisable bean). Emits the same const
/// `::leaf_core::Descriptor` row as a no-dependency `#[advisable]`, the
/// `compile_error!` remediation a generic `#[advisable]`/`#[aspect]` points at.
#[proc_macro]
pub fn register_proxy(item: TokenStream) -> TokenStream {
    let ty = parse_macro_input!(item as Type);
    match stereotype::emit_register(&ty) {
        Ok(rows) => rows.into(),
        Err(err) => compile_error(&err).into(),
    }
}

/// `#[aspect(order = N)]` — an ASPECT bean carrying advice. Structurally a
/// `#[component]` (so the aspect bean is registered + resolvable) that ALSO emits
/// one const `::leaf_core::AdvisorRow` identity into the frozen `ADVISORS` slice +
/// the public chain-order pairing const the leaf-boot proxy-assembly pass binds to
/// the live `AdvisorDescriptor`. A generic aspect hard-errors with the
/// `register_proxy!(Concrete)` hint.
#[proc_macro_attribute]
pub fn aspect(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let attr2: proc_macro2::TokenStream = attr.into();
    let args = match advisor::parse_advisor_args(attr2.clone()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    // The aspect bean itself is a plain @component (no stereotype args beyond order,
    // which the advisor row consumes — the component row takes no name/scope here).
    let component = match stereotype::emit_struct(
        &parsed,
        Stereotype::Component,
        proc_macro2::TokenStream::new(),
    ) {
        Ok(rows) => rows,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let is_generic = !parsed.generics.params.is_empty();
    match advisor::emit_advisor(
        &parsed.ident.to_string(),
        advisor::AdviceKind::Around,
        &args,
        is_generic,
    ) {
        Ok(advisor_rows) => quote! { #parsed #component #advisor_rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

/// `#[advice(around, order = N)]` — one piece of ADVICE (a free fn body the proxy
/// chain wraps). Emits one const `::leaf_core::AdvisorRow` identity into `ADVISORS`
/// plus the public chain-order pairing const. The first bare ident is the advice
/// kind (`before`/`after`/`after_returning`/`after_throwing`/`around`, default
/// `around`).
#[proc_macro_attribute]
pub fn advice(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let (kind, args) = match advisor::parse_advice_args(attr.into()) {
        Ok(ka) => ka,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    match advisor::emit_advisor(&parsed.sig.ident.to_string(), kind, &args, false) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

/// `#[pointcut]` — a NAMED pointcut definition (a free fn whose ident names the
/// pointcut). Lowers to the SAME const `::leaf_core::AdvisorRow` identity shape as
/// `#[advice]` (the pointcut predicate itself is the proxy substrate's typed-
/// combinator model, bound at refresh); the row anchors its discovery + chain order.
#[proc_macro_attribute]
pub fn pointcut(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let args = match advisor::parse_advisor_args(attr.into()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    match advisor::emit_advisor(
        &parsed.sig.ident.to_string(),
        advisor::AdviceKind::Around,
        &args,
        false,
    ) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

// ═══════════════════════ the event-listener surface ══════════════════════════

/// `#[event_listener(order = N, condition = "…")]` — register a listener method
/// (a free fn) into the `EVENT_LISTENERS` channel. Emits one const
/// `::leaf_core::EventListenerRow` identity + the public dispatch-metadata pairing
/// consts (order + the inline defer + the condition-presence slot) the leaf-boot
/// events pass binds to a live `ListenerDescriptor`.
#[proc_macro_attribute]
pub fn event_listener(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_listener(attr, item, false)
}

/// `#[transactional_event_listener(phase = "after_commit", …)]` — a listener that
/// DEFERS to a transaction-synchronization phase (the transactional form). Same
/// `EVENT_LISTENERS` row shape as `#[event_listener]`, but the dispatch-metadata
/// pairing const carries the `::leaf_core::TxPhase` (default `AfterCommit`).
#[proc_macro_attribute]
pub fn transactional_event_listener(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_listener(attr, item, true)
}

/// The shared thin body for the two listener macros: parse the fn, lower to the
/// `EVENT_LISTENERS` row + dispatch-metadata pairing consts, emit `<fn> <rows>`.
fn expand_listener(attr: TokenStream, item: TokenStream, transactional: bool) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let args = match listener::parse_listener_args(attr.into(), transactional) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let rows = listener::emit_listener(&parsed.sig.ident.to_string(), &args);
    quote! { #parsed #rows }.into()
}

// ═══════════════ the scheduling / caching / resource / catalog surface ════════

/// `#[scheduled(cron = "…" | fixed_rate = N | fixed_delay = N, initial_delay = M)]`
/// — register a free-fn task into the `SCHEDULED` channel. Emits a const
/// `::leaf_core::ScheduledMethodDescriptor` (the trigger spec) + its `.to_row()`
/// identity into the frozen `SCHEDULED` slice. Exactly one trigger key is required.
#[proc_macro_attribute]
pub fn scheduled(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let spec = match scheduling::parse_schedule(attr.into()) {
        Ok(s) => s,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let rows = scheduling::emit_scheduled(&parsed.sig.ident.to_string(), "invoke", &spec);
    quote! { #parsed #rows }.into()
}

/// `#[cacheable("cacheName", sync = true, …)]` — cache a free-fn result. Emits a
/// const `::leaf_core::CacheOpMeta` + the cache advisor identity row in `ADVISORS`
/// (pinned to the `CACHE_ORDER` chain const). At least one cache name is required.
#[proc_macro_attribute]
pub fn cacheable(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemFn);
    let args = match scheduling::parse_cache_args(attr.into()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let rows = scheduling::emit_cacheable(&parsed.sig.ident.to_string(), "invoke", &args);
    quote! { #parsed #rows }.into()
}

/// `#[resource("config/app.yaml")]` — register a compiled-in classpath resource.
/// Emits a const `::leaf_core::ResourceEntry` (`include_bytes!`-backed) + the
/// `::leaf_core::ResourceRow` identity into the frozen `RESOURCES` slice. Applied to
/// a `const NAME: &[u8];`-style declaration whose ident names the resource.
#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let decl = parse_macro_input!(item as ResourceConst);
    let path = match resource_path(attr.into()) {
        Ok(p) => p,
        Err(err) => return compile_error(&err).into(),
    };
    let rows = scheduling::emit_resource(&decl.ident.to_string(), &path);
    // Bind the user's const to the compiled-in bytes (the resource IS the bytes);
    // the emitted ResourceEntry/ResourceRow ride alongside for discovery.
    let ResourceConst { attrs, vis, ident, ty } = &decl;
    quote! {
        #(#attrs)*
        #vis const #ident: #ty = ::core::include_bytes!(#path);
        #rows
    }
    .into()
}

/// `#[catalog(basename = "messages", locales = ["en", "de"])]` — register an i18n
/// message catalog. Emits a const `::leaf_core::CatalogDescriptor` + the
/// `::leaf_core::CatalogRow` identity into the frozen `CATALOGS` slice. Applied to a
/// unit struct whose ident names the catalog.
#[proc_macro_attribute]
pub fn catalog(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let args = match scheduling::parse_catalog_args(attr.into()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #parsed #error }.into();
        }
    };
    let rows = scheduling::emit_catalog(&parsed.ident.to_string(), &args);
    quote! { #parsed #rows }.into()
}

/// Parse the `#[resource("path")]` attribute body: a single string-literal path.
fn resource_path(attr: proc_macro2::TokenStream) -> Result<String, EmitError> {
    let lit: syn::LitStr = syn::parse2(attr).map_err(|e| EmitError {
        message: format!("#[resource] requires a single string path argument: {e}"),
    })?;
    Ok(lit.value())
}

/// A `const NAME: TYPE [= _]?;` declaration the `#[resource]` attribute reads — the
/// initializer is OPTIONAL (the emitted `ResourceEntry` accessor supplies the
/// bytes), mirroring the `#[value]` `ValueConst` parse.
struct ResourceConst {
    attrs: Vec<syn::Attribute>,
    vis: syn::Visibility,
    ident: syn::Ident,
    ty: Box<syn::Type>,
}

impl syn::parse::Parse for ResourceConst {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(syn::Attribute::parse_outer)?;
        let vis: syn::Visibility = input.parse()?;
        input.parse::<syn::Token![const]>()?;
        let ident: syn::Ident = input.parse()?;
        input.parse::<syn::Token![:]>()?;
        let ty: syn::Type = input.parse()?;
        if input.peek(syn::Token![=]) {
            input.parse::<syn::Token![=]>()?;
            input.parse::<syn::Expr>()?;
        }
        input.parse::<syn::Token![;]>()?;
        Ok(ResourceConst { attrs, vis, ident, ty: Box::new(ty) })
    }
}

// ═══════════════════════ the application-entry surface ═══════════════════════

/// `#[leaf::main]` (exported as `main`) — the BINARY-CRATE entrypoint. Splices in
/// the Layer-0 anti-DCE force-link shim + the const `ExpectedManifest` self-check
/// anchor (over the binary crate + the `scan(...)` list), and wraps the user's
/// `async fn main` body in a real `fn main()` that drives the run engine
/// (`::leaf_boot::Application::new(Primary).run()`).
///
/// Args (all optional): a leading primary application source TYPE, and
/// `scan("leaf-redis", …)` for the participating crates to force-link.
///
/// NOTE: the run ENGINE lives in leaf-boot (out of this unit's scope); the emitted
/// entry references the `::leaf_boot::Application` seam.
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let user_fn = parse_macro_input!(item as ItemFn);
    let args = match app::parse_main_args(attr.into()) {
        Ok(a) => a,
        Err(err) => {
            let error = compile_error(&err);
            return quote! { #user_fn #error }.into();
        }
    };
    // The binary crate's own package name (always force-linked). At expansion the
    // macro reads it from the contributing crate's `CARGO_PKG_NAME` env var.
    let binary_crate = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "crate".into());
    let rows = app::emit_main(&binary_crate, &args, &user_fn);
    rows.into()
}

/// `#[runner]` — a [`leaf_core::Runner`] bean. Structurally a `#[component]` that
/// ALSO declares it is injectable as `dyn ::leaf_core::Runner` (the `provides[]`
/// upcast the run pipeline collects the runner stream from). A generic runner
/// hard-errors with the `register_component!(Concrete)` hint.
#[proc_macro_attribute]
pub fn runner(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    match app::emit_runner(&parsed) {
        Ok(rows) => quote! { #parsed #rows }.into(),
        Err(err) => {
            let error = compile_error(&err);
            quote! { #parsed #error }.into()
        }
    }
}

/// `#[failure_analyzer]` — register a [`leaf_core::FailureAnalyzer`] impl into the
/// frozen `FAILURE_ANALYZERS` slice (the error-model SPI reused — never a second
/// analyzer trait). The user writes the `impl ::leaf_core::FailureAnalyzer for Ty`;
/// this emits a `static` instance + the `&'static dyn FailureAnalyzer` row. Applied
/// to a unit struct.
#[proc_macro_attribute]
pub fn failure_analyzer(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as ItemStruct);
    let rows = app::emit_failure_analyzer(&parsed.ident.to_string());
    quote! { #parsed #rows }.into()
}
