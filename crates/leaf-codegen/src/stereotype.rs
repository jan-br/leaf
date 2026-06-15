//! The stereotype vocabulary + the `syn::ItemStruct` → [`BeanInput`] lowering the
//! THIN `#[component]`/`#[service]`/`#[repository]`/`#[controller]`/
//! `#[configuration]` macros call (component-stereotypes, phase3/02).
//!
//! A stereotype is structurally a plain `@component` that differs ONLY in its
//! `role` axis + its `meta.markers` transitive closure (the design's "differing
//! only in Role + meta markers"). This module owns that vocabulary as DATA and the
//! whole parse-to-[`BeanInput`] lowering, so the proc-macro stays thin: it parses
//! with `syn`, calls `component` (or the per-stereotype entry), and emits.
//!
//! The lowering reads the struct's FIELDS as the constructor's injection points
//! (each field's ident is the implicit string qualifier; its type is the
//! collaborator resolved through the one `Engine::get` seam). A generic target is a
//! Tier-0 hard error carrying the `register_component!(Concrete)` hint.

use syn::punctuated::Punctuated;
use syn::{Expr, ExprLit, Fields, FnArg, ItemFn, Lit, Meta, Pat, ReturnType, Token, Type};
use syn::ItemStruct;

use crate::annotation::{resolve, Annotation};
use crate::descriptor::{BeanInput, Dependency, EmitError, Role, Scope};

/// The parsed `#[stereotype(...)]` attribute arguments: an optional explicit
/// `name = "…"` (overriding the derived default) and an optional `scope = …`
/// (`singleton`/`prototype`/`request`, defaulting to `singleton`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StereotypeArgs {
    /// An explicit canonical name (`name = "userService"`), or `None` to derive.
    pub name: Option<String>,
    /// The scope triple (`scope = "prototype"`), defaulting to singleton.
    pub scope: Scope,
    /// An explicit provenance [`Role`] override (`role = "infrastructure"`), or
    /// `None` to keep the stereotype's own role (Application). The orthogonal
    /// framework-vs-application axis the stereotype NAME cannot carry — the one knob
    /// that lets a `#[component]` register a `Role::Infrastructure` bean (e.g. the
    /// primary `applicationTaskExecutor`).
    pub role: Option<Role>,
}

/// Parse the comma-separated `#[stereotype(name = "…", scope = "…")]` argument
/// list (the token stream syn hands the proc-macro as the attribute body).
///
/// # Errors
/// [`EmitError`] on an unknown key, a non-string `name`/`scope`, or an unrecognised
/// scope value — surfaced verbatim by the thin macro as a `compile_error!`.
pub fn parse_args(attr: proc_macro2::TokenStream) -> Result<StereotypeArgs, EmitError> {
    let mut out = StereotypeArgs::default();
    if attr.is_empty() {
        return Ok(out);
    }
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let metas = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed stereotype attribute arguments: {e}"),
    })?;
    for meta in metas {
        let Meta::NameValue(nv) = meta else {
            return Err(EmitError {
                message: "stereotype arguments must be `key = \"value\"` pairs".into(),
            });
        };
        let key = nv
            .path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_default();
        let value = str_value(&nv.value).ok_or_else(|| EmitError {
            message: format!("`{key}` must be a string literal"),
        })?;
        match key.as_str() {
            "name" => out.name = Some(value),
            "scope" => out.scope = parse_scope(&value)?,
            "role" => out.role = Some(parse_role(&value)?),
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown stereotype argument `{other}` (expected `name`/`scope`/`role`)"
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// The string body of a `key = "literal"` value, if it is a string literal.
fn str_value(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

/// Map a `scope = "…"` value to the built-in [`Scope`] triple.
fn parse_scope(value: &str) -> Result<Scope, EmitError> {
    match value {
        "singleton" => Ok(Scope::Singleton),
        "prototype" => Ok(Scope::Prototype),
        "request" => Ok(Scope::Request),
        other => Err(EmitError {
            message: format!(
                "unknown scope `{other}` (expected `singleton`/`prototype`/`request`)"
            ),
        }),
    }
}

/// Map a `role = "…"` value to the provenance [`Role`] axis. The stereotype name
/// carries the marker closure; this orthogonal knob carries the
/// framework-vs-application provenance (`Context::refresh()` infrastructure
/// auto-detection), so a `#[component(role = "infrastructure")]` can register the
/// primary `applicationTaskExecutor` the same maximal-magic way as a user bean.
fn parse_role(value: &str) -> Result<Role, EmitError> {
    match value {
        "application" => Ok(Role::Application),
        "support" => Ok(Role::Support),
        "infrastructure" => Ok(Role::Infrastructure),
        other => Err(EmitError {
            message: format!(
                "unknown role `{other}` (expected `application`/`support`/`infrastructure`)"
            ),
        }),
    }
}

/// The five built-in stereotypes leaf ships, as DATA: each is a plain `@component`
/// differing only in its transitive marker closure + `role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stereotype {
    /// `@component` — the base marker every stereotype transitively implies.
    Component,
    /// `@service` — a business-logic bean (`[Service, Component]`).
    Service,
    /// `@repository` — a data-access bean; the marker the exception-translation
    /// advisor queries (`[Repository, Component]`).
    Repository,
    /// `@controller` — a web-layer bean (`[Controller, Component]`).
    Controller,
    /// `@configuration` — a `@bean`-factory holder (`[Configuration, Component]`).
    Configuration,
}

impl Stereotype {
    /// The canonical marker path this stereotype mints (its own `MarkerId` input).
    #[must_use]
    pub fn marker_path(self) -> &'static str {
        match self {
            Stereotype::Component => "leaf::Component",
            Stereotype::Service => "leaf::Service",
            Stereotype::Repository => "leaf::Repository",
            Stereotype::Controller => "leaf::Controller",
            Stereotype::Configuration => "leaf::Configuration",
        }
    }

    /// The framework-vs-application [`Role`] axis this stereotype carries. All five
    /// built-ins are `Application`; the orthogonal `Role` axis is reserved for
    /// `Context::refresh()` infrastructure auto-detection, not the stereotype name.
    #[must_use]
    pub fn role(self) -> Role {
        Role::Application
    }

    /// The composed [`Annotation`] (self transitively over `@component`) the merge
    /// engine flattens into `meta.markers`. `@component` is its own root; every
    /// other stereotype is a one-hop meta-edge over `@component` (so the flattened
    /// closure always contains `COMPONENT`, the default scan-include marker).
    #[must_use]
    pub fn annotation(self) -> Annotation {
        match self {
            Stereotype::Component => Annotation::new("leaf::Component"),
            other => Annotation::new(other.marker_path())
                .with_meta(Annotation::new("leaf::Component")),
        }
    }
}

/// Lower a parsed `#[stereotype] struct` to the [`BeanInput`] the descriptor
/// emitter consumes — the one place the syn AST is read.
///
/// Reads the struct's FIELDS as the constructor's injection points (named fields →
/// field-ident-qualified deps; tuple fields → positional `_<n>` deps). Builds the
/// stereotype's transitive marker closure + role, resolves the annotation, and sets
/// `module_qualified` so the contract is `module_path!()::Ident` at the definition
/// site. An explicit `name` overrides the derived default. A generic struct returns
/// an [`EmitError`] (→ `compile_error!`) with the `register_component!` hint.
///
/// # Errors
/// [`EmitError`] when the struct is generic (has type/const generic parameters) or
/// when an alias in the stereotype annotation is malformed.
pub fn struct_input(
    item: &ItemStruct,
    stereotype: Stereotype,
    explicit_name: Option<String>,
    explicit_role: Option<Role>,
    scope: Scope,
) -> Result<BeanInput, EmitError> {
    let ident = item.ident.to_string();
    let is_generic = !item.generics.params.is_empty();

    let self_ty: Type = syn::parse_str(&ident).map_err(|e| EmitError {
        message: format!("`{ident}` is not a valid self type: {e}"),
    })?;

    let deps = fields_to_deps(&item.fields);

    let meta = resolve(&stereotype.annotation()).map_err(|e| EmitError {
        message: e.to_string(),
    })?;

    let mut input = BeanInput::new(self_ty, ident.clone(), ident);
    input.module_qualified = true;
    // An explicit `role = "…"` arg overrides the stereotype's own role (the
    // orthogonal provenance axis); otherwise the stereotype's role (Application)
    // applies.
    input.role = explicit_role.unwrap_or_else(|| stereotype.role());
    input.scope = scope;
    input.explicit_name = explicit_name;
    input.meta = meta;
    input.deps = deps;
    // The struct field-default routes through `Injectable` (trait dispatch): each
    // field carries its VERBATIM type and the emitter derives the injection point +
    // resolution from `<FieldTy as ::leaf_core::Injectable>::{RESOLVABLE, inject}` —
    // never a name-stripped `Ref<…>` TypeId (the design's no-type-names rule).
    input.inject_via_trait = true;
    input.is_generic = is_generic;
    Ok(input)
}

/// The ONE entry point the thin stereotype macro calls: parse the attribute args,
/// lower the struct, emit the const registration artifact, and prepend the original
/// item so the macro output is `<item> <emitted const rows>`.
///
/// This is the whole macro body bar the `proc_macro` ↔ `proc_macro2` bridge — the
/// macro parses the item with `syn`, calls this, and returns the tokens (charter
/// §2.10: no logic in the macro).
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) on a malformed attribute, a generic struct
/// (with the `register_component!` hint), or a malformed stereotype annotation.
pub fn emit_struct(
    item: &ItemStruct,
    stereotype: Stereotype,
    attr: proc_macro2::TokenStream,
) -> Result<proc_macro2::TokenStream, EmitError> {
    let args = parse_args(attr)?;
    let input = struct_input(item, stereotype, args.name, args.role, args.scope)?;
    crate::descriptor::emit(&input)
}

/// Lower a concrete `register_component!(Concrete)` type to a [`BeanInput`] — the
/// escape hatch for a generic component (a `register_component!(Repo<u32>)`
/// monomorphized instantiation). The concrete type is registered as a plain
/// no-dependency `@component` constructed via `Concrete::new()`; its name + contract
/// derive from the leading path-segment ident.
///
/// Unlike the `#[component]` struct path, this does NOT read fields (a bare type has
/// none), so the user supplies an arity-0 `new()`. A still-generic argument
/// (`register_component!(Repo<T>)`, with `T` a generic param in scope) cannot be
/// detected at expansion — that is a downstream type error, not a macro one.
///
/// # Errors
/// [`EmitError`] if the type has no nameable leading ident, or its annotation is
/// malformed.
pub fn register_input(ty: &Type) -> Result<BeanInput, EmitError> {
    register_input_with(ty, None, None)
}

/// Lower a concrete `register_component!(Concrete, role = "…", name = "…")` type to a
/// [`BeanInput`], carrying an optional explicit [`Role`] provenance override + an
/// optional Spring bean-name override.
///
/// Same construct-via-`new()`, no-field-injection shape as [`register_input`] (a bare
/// type has no fields, so the user supplies an arity-0 `new()`); the extra knobs let a
/// FRAMEWORK bean register through this same maximal-magic channel — e.g. leaf-tokio's
/// `TokioExecutionFacility` as the `Role::Infrastructure` `applicationTaskExecutor`
/// (whose `gate` field is internal state, NOT an injection point, so the `#[component]`
/// struct-field path does not fit). `None`/`None` is the plain `register_component!`.
///
/// # Errors
/// [`EmitError`] if the type has no nameable leading ident, or its annotation is
/// malformed.
pub fn register_input_with(
    ty: &Type,
    explicit_name: Option<String>,
    explicit_role: Option<Role>,
) -> Result<BeanInput, EmitError> {
    let ident = leading_ident(ty).ok_or_else(|| EmitError {
        message: "register_component! expects a concrete type with a nameable identifier".into(),
    })?;
    let meta = resolve(&Stereotype::Component.annotation()).map_err(|e| EmitError {
        message: e.to_string(),
    })?;
    let mut input = BeanInput::new(ty.clone(), ident.clone(), ident);
    input.module_qualified = true;
    input.meta = meta;
    input.explicit_name = explicit_name;
    if let Some(role) = explicit_role {
        input.role = role;
    }
    Ok(input)
}

/// Emit the const registration artifact for a `register_component!(Concrete)`
/// invocation (the one thin function-like macro entry point).
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) on an unnameable type or a malformed
/// annotation.
pub fn emit_register(ty: &Type) -> Result<proc_macro2::TokenStream, EmitError> {
    crate::descriptor::emit(&register_input(ty)?)
}

/// Emit the const registration artifact for a
/// `register_component!(Concrete, role = "…", name = "…")` invocation — the
/// role/name-carrying form (see [`register_input_with`]).
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) on an unnameable type or a malformed
/// annotation.
pub fn emit_register_with(
    ty: &Type,
    explicit_name: Option<String>,
    explicit_role: Option<Role>,
) -> Result<proc_macro2::TokenStream, EmitError> {
    crate::descriptor::emit(&register_input_with(ty, explicit_name, explicit_role)?)
}

/// The leading path-segment ident of a type (`Repo<u32>` → `Repo`), used as the
/// concrete bean's name + contract base.
fn leading_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Lower a `#[bean]` FACTORY FUNCTION to a [`BeanInput`] — the same const row shape
/// as a `#[component]` struct, but the construction recipe is the free function
/// itself (`#[bean] fn data_source(cfg: Ref<Config>) -> DataSource`), not a struct's
/// `::new`. One shape, one seed type, just a different ctor (the design's "no second
/// seed type").
///
/// The bean's TYPE is the function's return type; its NAME/contract derive from the
/// function ident (so `fn data_source(...)` → name `"dataSource"`); its injection
/// points are the function's typed parameters (a `Ref<T>` param injects bean `T`,
/// stripping the handle wrapper exactly like a struct field). An explicit `name`
/// overrides the derived default.
///
/// # Errors
/// [`EmitError`] if the function has no explicit return type (a `@bean` must produce
/// a value), takes a `self` receiver (the method-on-a-`@configuration` form, with
/// its config-instance threading, is deferred — see the NOTE in the macro), is
/// generic, or its annotation is malformed.
pub fn bean_input(
    func: &ItemFn,
    explicit_name: Option<String>,
    scope: Scope,
) -> Result<BeanInput, EmitError> {
    if !func.sig.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{}` is a generic @bean factory: a generic factory has no single \
                 concrete product type. Register a concrete instantiation with \
                 `register_component!(Concrete)`.",
                func.sig.ident
            ),
        });
    }

    let ret_ty = match &func.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(EmitError {
                message: format!(
                    "`{}` is a @bean but has no return type: a @bean factory must \
                     produce the bean it registers.",
                    func.sig.ident
                ),
            });
        }
    };

    let deps = sig_to_deps(func)?;

    let fn_ident = func.sig.ident.to_string();
    let meta = resolve(&Stereotype::Component.annotation()).map_err(|e| EmitError {
        message: e.to_string(),
    })?;

    // The bean ident (name/contract base) is the FUNCTION ident; the self type is
    // the RETURN type; the construction recipe is the function path.
    let mut input = BeanInput::new(ret_ty, fn_ident.clone(), fn_ident.clone());
    input.module_qualified = true;
    input.scope = scope;
    input.explicit_name = explicit_name;
    input.meta = meta;
    input.deps = deps;
    input.ctor = Some(syn::parse_str(&fn_ident).map_err(|e| EmitError {
        message: format!("`{fn_ident}` is not a callable factory path: {e}"),
    })?);
    Ok(input)
}

/// Emit the const registration artifact for a `#[bean]` factory function (the thin
/// macro entry point) — `<fn> <const rows>`.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) per [`bean_input`].
pub fn emit_bean(
    func: &ItemFn,
    explicit_name: Option<String>,
    scope: Scope,
) -> Result<proc_macro2::TokenStream, EmitError> {
    crate::descriptor::emit(&bean_input(func, explicit_name, scope)?)
}

/// Lower a factory function's typed parameters to constructor injection points. A
/// `self` receiver is rejected (the config-method form is deferred); each typed
/// parameter keys on its binding ident (or `_<index>`), stripping a `Ref<…>`
/// handle wrapper to the injected bean type exactly like a struct field.
fn sig_to_deps(func: &ItemFn) -> Result<Vec<Dependency>, EmitError> {
    let mut deps = Vec::new();
    for (i, arg) in func.sig.inputs.iter().enumerate() {
        match arg {
            FnArg::Receiver(_) => {
                return Err(EmitError {
                    message: format!(
                        "`{}` is a @bean with a `self` receiver: a config-class @bean \
                         METHOD (threading the config instance) is lowered by the \
                         IMPL-BLOCK macro, not the per-method attr. Put `#[bean]` on a \
                         method inside a `#[configuration] impl Cfg {{ .. }}` block \
                         (the impl-block macro iterates the methods and emits one \
                         Descriptor per `#[bean]`), or use a free `fn` `#[bean]` \
                         factory for a standalone factory.",
                        func.sig.ident
                    ),
                });
            }
            FnArg::Typed(pat_ty) => {
                let name = match &*pat_ty.pat {
                    Pat::Ident(p) => p.ident.to_string(),
                    _ => format!("_{i}"),
                };
                deps.push(Dependency { name, ty: strip_ref(&pat_ty.ty) });
            }
        }
    }
    Ok(deps)
}

/// The bean type a `#[bean]` free-fn PARAMETER of type `ty` injects: `Ref<T>` → `T`,
/// any other type → itself. The LEGACY name-stripped lowering for the `#[bean]`
/// free-fn path, which Task 6 migrates onto the [`Injectable`](leaf_core::Injectable)
/// trait (deleting this remaining `seg.ident == "Ref"` check). The struct
/// field-default path already routes through the trait (see [`fields_to_deps`]).
fn strip_ref(ty: &Type) -> Type {
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

/// Lower a struct's fields to constructor injection points. Named fields key on the
/// field ident (the implicit string qualifier); tuple fields key on `_<index>`.
///
/// Each dependency carries the field's VERBATIM type (e.g. `Ref<T>`) — NEVER a
/// name-stripped inner type. Resolution is the [`Injectable`](leaf_core::Injectable)
/// trait's job: the emitter (with `inject_via_trait`) derives the injection point from
/// `<FieldTy as Injectable>::RESOLVABLE` and resolves it via `<FieldTy as
/// Injectable>::inject`, so a re-exported/aliased `Ref` is irrelevant (trait dispatch,
/// not `seg.ident == "Ref"`). A field whose type is not `Injectable` (a bare bean type
/// or internal state) is a loud compile error at the field's injection site — the
/// design's no-bare-type, no-name-based-escape-hatch rule (use an `#[inject]`
/// constructor for a state-holding bean).
fn fields_to_deps(fields: &Fields) -> Vec<Dependency> {
    match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|f| Dependency {
                name: f.ident.as_ref().map(ToString::to_string).unwrap_or_default(),
                ty: f.ty.clone(),
            })
            .collect(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| Dependency { name: format!("_{i}"), ty: f.ty.clone() })
            .collect(),
        Fields::Unit => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::emit;

    fn item(src: &str) -> ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    fn flat(input: &BeanInput) -> String {
        emit(input)
            .expect("emits")
            .to_string()
            .split_whitespace()
            .collect()
    }

    #[test]
    fn component_marker_closure_is_just_component() {
        let merged = resolve(&Stereotype::Component.annotation()).expect("resolves");
        assert_eq!(merged.markers, vec!["leaf::Component".to_string()]);
    }

    #[test]
    fn service_transitively_implies_component() {
        // A @service is a @component (one-hop meta-edge) so the flattened closure
        // carries BOTH markers — and COMPONENT is what the default scan filter
        // matches transitively.
        let merged = resolve(&Stereotype::Service.annotation()).expect("resolves");
        assert_eq!(
            merged.markers,
            vec!["leaf::Service".to_string(), "leaf::Component".to_string()]
        );
    }

    #[test]
    fn every_stereotype_transitively_carries_component() {
        for st in [
            Stereotype::Service,
            Stereotype::Repository,
            Stereotype::Controller,
            Stereotype::Configuration,
        ] {
            let merged = resolve(&st.annotation()).expect("resolves");
            assert!(
                merged.markers.contains(&"leaf::Component".to_string()),
                "{st:?} must transitively imply @component"
            );
            assert_eq!(merged.markers[0], st.marker_path(), "self marker is first");
        }
    }

    #[test]
    fn unit_struct_lowers_to_a_no_dependency_bean() {
        let input = struct_input(
            &item("struct Greeter;"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("a unit struct lowers");
        assert!(input.deps.is_empty(), "a unit struct has no injection points");
        assert_eq!(input.ident, "Greeter");
        assert!(input.module_qualified, "the contract is module-qualified");
        // The derived name decapitalizes through the emitter.
        let s = flat(&input);
        assert!(s.contains(r#"Some("greeter")"#), "got: {s}");
    }

    #[test]
    fn named_fields_become_named_injection_points() {
        // Each named field is one injection point keyed on the field ident, carrying
        // the field's VERBATIM type. Resolution routes through `Injectable` (trait
        // dispatch), so the emitted point + resolve reference the field type as-is.
        let input = struct_input(
            &item("struct Loud { greeter: leaf_core::Ref<Greeter>, more: leaf_core::Ref<Other> }"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        assert_eq!(input.deps.len(), 2);
        assert_eq!(input.deps[0].name, "greeter");
        assert_eq!(input.deps[1].name, "more");
        let s = flat(&input);
        assert!(
            s.contains("<leaf_core::Ref<Greeter>as::leaf_core::Injectable>::inject(__cx).await?"),
            "got: {s}"
        );
    }

    #[test]
    fn a_ref_handle_field_resolves_through_the_injectable_trait_verbatim() {
        // A field stored as `Ref<Greeter>` injects through `<Ref<Greeter> as
        // Injectable>` (trait dispatch decides `Ref` semantics — never a name strip):
        // the dependency carries the FULL `Ref<Greeter>` type and the provider awaits
        // `<Ref<Greeter> as Injectable>::inject` (the resolved value IS the field the
        // ctor consumes — no re-wrapping, no `Ref<Ref<…>>`).
        let input = struct_input(
            &item("struct Loud { greeter: leaf_core::Ref<Greeter> }"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        assert_eq!(input.deps.len(), 1);
        let s = flat(&input);
        assert!(
            s.contains("<leaf_core::Ref<Greeter>as::leaf_core::Injectable>::inject(__cx).await?"),
            "got: {s}"
        );
        // The legacy name-stripped engine resolve is GONE.
        assert!(!s.contains("__engine.get::<Greeter>"), "got: {s}");
    }

    #[test]
    fn field_default_resolves_through_the_injectable_trait_not_a_name_strip() {
        // Task 4: the struct field-default path routes through `Injectable` (trait
        // dispatch), NOT a name-stripped `Bar` TypeId. For `#[component] struct Foo {
        // dep: Ref<Bar> }` (no `#[inject]` ctor) the field-default InjectionPoint is
        // built from `<Ref<Bar> as Injectable>::RESOLVABLE` and the provider resolves
        // via `<Ref<Bar> as Injectable>::inject` — the emitted tokens reference
        // `Injectable`, never the legacy `__engine.get::<Bar>()` name-strip.
        let input = struct_input(
            &item("struct Foo { dep: leaf_core::Ref<Bar> }"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        // The dep carries the VERBATIM field type (no `Ref<…>` strip).
        assert_eq!(input.deps.len(), 1);
        assert!(input.inject_via_trait, "the field-default routes through the trait");
        let s = flat(&input);
        // The point + the resolution both go through the Injectable trait, verbatim.
        assert!(
            s.contains(
                "<leaf_core::Ref<Bar>as::leaf_core::Injectable>::RESOLVABLE"
            ),
            "the injection point derives from `<FieldTy as Injectable>::RESOLVABLE`: {s}"
        );
        assert!(
            s.contains("<leaf_core::Ref<Bar>as::leaf_core::Injectable>::inject(__cx).await?"),
            "the provider resolves via `<FieldTy as Injectable>::inject`: {s}"
        );
        // The name-stripped legacy path is GONE: no `__engine.get::<Bar>()`, no
        // `InjectionPoint::single(... Bar ...)`.
        assert!(!s.contains("__engine.get::<Bar>"), "no name-stripped engine resolve: {s}");
        assert!(!s.contains("InjectionPoint::single"), "no name-stripped point: {s}");
    }

    #[test]
    fn tuple_fields_become_positional_injection_points() {
        let input = struct_input(
            &item("struct Pair(Greeter, usize);"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        assert_eq!(input.deps.len(), 2);
        assert_eq!(input.deps[0].name, "_0");
        assert_eq!(input.deps[1].name, "_1");
    }

    #[test]
    fn explicit_name_flows_through_to_the_input() {
        let input = struct_input(
            &item("struct Greeter;"),
            Stereotype::Component,
            Some("theGreeter".into()),
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        assert_eq!(input.explicit_name, Some("theGreeter".into()));
    }

    #[test]
    fn service_input_carries_the_service_marker() {
        let input = struct_input(
            &item("struct UserService;"),
            Stereotype::Service,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        let s = flat(&input);
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::Service")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::Component")"#), "got: {s}");
    }

    #[test]
    fn parse_args_empty_is_the_default() {
        let args = parse_args(proc_macro2::TokenStream::new()).expect("empty parses");
        assert_eq!(args, StereotypeArgs::default());
        assert_eq!(args.scope, Scope::Singleton);
    }

    #[test]
    fn parse_args_reads_name_and_scope() {
        let attr: proc_macro2::TokenStream =
            syn::parse_str(r#"name = "userService", scope = "prototype""#).expect("tokens");
        let args = parse_args(attr).expect("parses");
        assert_eq!(args.name, Some("userService".into()));
        assert_eq!(args.scope, Scope::Prototype);
    }

    #[test]
    fn parse_args_rejects_unknown_key() {
        let attr: proc_macro2::TokenStream = syn::parse_str(r#"bogus = "x""#).expect("tokens");
        let err = parse_args(attr).expect_err("unknown key errors");
        assert!(err.message.contains("unknown stereotype argument"), "got: {}", err.message);
    }

    #[test]
    fn parse_args_rejects_unknown_scope() {
        let attr: proc_macro2::TokenStream = syn::parse_str(r#"scope = "galaxy""#).expect("tokens");
        let err = parse_args(attr).expect_err("unknown scope errors");
        assert!(err.message.contains("unknown scope"), "got: {}", err.message);
    }

    #[test]
    fn parse_args_reads_an_infrastructure_role() {
        // The infra-role surface: `#[component(role = "infrastructure", name = "..")]`
        // requests `Role::Infrastructure` on the emitted Descriptor (the orthogonal
        // provenance axis the stereotype NAME cannot carry). The default is `None`
        // (the stereotype's own `role()` — Application — applies).
        let attr: proc_macro2::TokenStream =
            syn::parse_str(r#"role = "infrastructure", name = "applicationTaskExecutor""#)
                .expect("tokens");
        let args = parse_args(attr).expect("parses");
        assert_eq!(args.role, Some(Role::Infrastructure));
        assert_eq!(args.name, Some("applicationTaskExecutor".into()));
    }

    #[test]
    fn parse_args_rejects_an_unknown_role() {
        let attr: proc_macro2::TokenStream = syn::parse_str(r#"role = "wizard""#).expect("tokens");
        let err = parse_args(attr).expect_err("unknown role errors");
        assert!(err.message.contains("unknown role"), "got: {}", err.message);
    }

    #[test]
    fn an_explicit_role_arg_overrides_the_stereotype_role_in_the_emitted_descriptor() {
        // The whole point: a `role = "infrastructure"` arg makes the emitted const
        // Descriptor carry `Role::Infrastructure` (+ the custom declared_name),
        // replacing the hand-written infrastructure bean block with the macro.
        let ts = emit_struct(
            &item("struct TokioExecutionFacility;"),
            Stereotype::Component,
            syn::parse_str(r#"role = "infrastructure", name = "applicationTaskExecutor""#)
                .expect("tokens"),
        )
        .expect("emits");
        let s: String = ts.to_string().split_whitespace().collect();
        assert!(s.contains("role:::leaf_core::Role::Infrastructure"), "got: {s}");
        assert!(s.contains(r#"Some("applicationTaskExecutor")"#), "got: {s}");
    }

    #[test]
    fn no_role_arg_keeps_the_stereotype_role_application() {
        // Without a `role` arg, the stereotype's own role (Application) is emitted.
        let input = struct_input(
            &item("struct Greeter;"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowers");
        let s = flat(&input);
        assert!(s.contains("role:::leaf_core::Role::Application"), "got: {s}");
    }

    #[test]
    fn emit_struct_is_the_one_thin_macro_entry_point() {
        // The whole macro body: parse args + lower + emit. The output must be a
        // valid Rust item sequence carrying the COMPONENTS submission.
        let ts = emit_struct(
            &item("struct Greeter;"),
            Stereotype::Component,
            proc_macro2::TokenStream::new(),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s: String = ts.to_string().split_whitespace().collect();
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
        assert!(s.contains("impl::leaf_core::BeanforGreeter{}"), "got: {s}");
    }

    #[test]
    fn emit_struct_honours_an_explicit_name_arg() {
        let attr: proc_macro2::TokenStream =
            syn::parse_str(r#"name = "theGreeter""#).expect("tokens");
        let ts = emit_struct(&item("struct Greeter;"), Stereotype::Component, attr)
            .expect("emits");
        let s: String = ts.to_string().split_whitespace().collect();
        assert!(s.contains(r#"Some("theGreeter")"#), "got: {s}");
    }

    #[test]
    fn emit_struct_propagates_the_generic_hard_error() {
        let err = emit_struct(
            &item("struct Repo<T> { inner: T }"),
            Stereotype::Component,
            proc_macro2::TokenStream::new(),
        )
        .expect_err("generic struct hard-errors");
        assert!(err.message.contains("register_component!"), "got: {}", err.message);
    }

    #[test]
    fn register_component_lowers_a_concrete_instantiation() {
        // The escape hatch: `register_component!(Repo<u32>)` registers the concrete
        // monomorphization as a no-dep @component, naming on the leading ident.
        let ty: Type = syn::parse_str("Repo<u32>").expect("a concrete type");
        let input = register_input(&ty).expect("lowers");
        assert_eq!(input.ident, "Repo");
        assert!(input.deps.is_empty());
        assert!(!input.is_generic, "the concrete instantiation is not 'generic' here");
        let s = flat(&input);
        // `Repo` decapitalizes to `repo`; the self_type is the FULL concrete type.
        assert!(s.contains(r#"Some("repo")"#), "got: {s}");
        assert!(s.contains("::core::any::TypeId::of::<Repo<u32>>()"), "got: {s}");
        assert!(s.contains("<Repo<u32>>::new()"), "got: {s}");
    }

    #[test]
    fn emit_register_emits_a_components_row() {
        let ty: Type = syn::parse_str("Repo<u32>").expect("type");
        let ts = emit_register(&ty).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s: String = ts.to_string().split_whitespace().collect();
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
    }

    #[test]
    fn register_input_with_carries_an_infrastructure_role_and_custom_name() {
        // The infra-role `register_component!` form: a concrete construct-via-`new()`
        // bean (NO field injection — the facility shape) carrying the
        // `Role::Infrastructure` provenance + a custom Spring bean-name. This is how
        // `TokioExecutionFacility` registers the primary `applicationTaskExecutor`
        // through the macro instead of a hand-written const block.
        let ty: Type = syn::parse_str("TokioExecutionFacility").expect("a concrete type");
        let input = register_input_with(
            &ty,
            Some("applicationTaskExecutor".into()),
            Some(Role::Infrastructure),
        )
        .expect("lowers");
        assert!(input.deps.is_empty(), "no field injection on the register form");
        assert_eq!(input.role, Role::Infrastructure);
        assert_eq!(input.explicit_name, Some("applicationTaskExecutor".into()));
        let s = flat(&input);
        assert!(s.contains("role:::leaf_core::Role::Infrastructure"), "got: {s}");
        assert!(s.contains(r#"Some("applicationTaskExecutor")"#), "got: {s}");
        // The provider constructs via the arity-0 `::new()` (construct-and-publish).
        assert!(s.contains("<TokioExecutionFacility>::new()"), "got: {s}");
    }

    #[test]
    fn emit_register_with_emits_the_full_components_and_seed_row() {
        // The `register_component!`-with-args emit path: a COMPONENTS row AND the
        // SEED_PAIRINGS force-link (so the bean's seed auto-collects like any user
        // bean — the JOIN leaf-boot's from_slices completes from the slice).
        let ty: Type = syn::parse_str("TokioExecutionFacility").expect("type");
        let ts = emit_register_with(
            &ty,
            Some("applicationTaskExecutor".into()),
            Some(Role::Infrastructure),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s: String = ts.to_string().split_whitespace().collect();
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]"),
            "got: {s}"
        );
    }

    fn func(src: &str) -> ItemFn {
        syn::parse_str(src).expect("a valid fn item")
    }

    #[test]
    fn bean_factory_fn_lowers_to_the_same_row_with_a_fn_ctor() {
        // `#[bean] fn data_source(cfg: Ref<Config>) -> DataSource` — the product type
        // is the return type, the name derives from the fn ident (decapitalized),
        // the params are injection points, and the ctor is the fn itself.
        let input = bean_input(
            &func("fn data_source(cfg: leaf_core::Ref<Config>) -> DataSource { todo!() }"),
            None,
            Scope::Singleton,
        )
        .expect("a @bean fn lowers");
        assert_eq!(input.ident, "data_source");
        assert_eq!(input.deps.len(), 1);
        assert_eq!(input.deps[0].name, "cfg");
        let s = flat(&input);
        // The product type is DataSource; the name is the fn ident through
        // decapitalize (an already-lowercase snake_case ident is unchanged).
        assert!(s.contains("::core::any::TypeId::of::<DataSource>()"), "got: {s}");
        assert!(s.contains(r#"Some("data_source")"#), "got: {s}");
        // The ctor is the free factory fn; the collaborator strips its Ref wrapper.
        assert!(s.contains("data_source(__dep_cfg)"), "got: {s}");
        assert!(s.contains("__engine.get::<Config>().await?"), "got: {s}");
    }

    #[test]
    fn bean_factory_fn_with_no_deps_lowers() {
        let input = bean_input(&func("fn clock() -> Clock { todo!() }"), None, Scope::Singleton)
            .expect("lowers");
        assert!(input.deps.is_empty());
        let s = flat(&input);
        assert!(s.contains("clock()"), "got: {s}");
    }

    #[test]
    fn bean_with_no_return_type_is_an_error() {
        let err = bean_input(&func("fn nope() {}"), None, Scope::Singleton)
            .expect_err("a @bean must produce a value");
        assert!(err.message.contains("no return type"), "got: {}", err.message);
    }

    #[test]
    fn bean_with_a_self_receiver_is_a_deferred_error() {
        let err = bean_input(
            &func("fn data_source(&self) -> DataSource { todo!() }"),
            None,
            Scope::Singleton,
        )
        .expect_err("the config-method form is deferred");
        assert!(err.message.contains("self"), "got: {}", err.message);
    }

    #[test]
    fn generic_bean_factory_is_a_hard_error() {
        let err = bean_input(
            &func("fn make<T>() -> Wrap<T> { todo!() }"),
            None,
            Scope::Singleton,
        )
        .expect_err("a generic factory hard-errors");
        assert!(err.message.contains("register_component!"), "got: {}", err.message);
    }

    #[test]
    fn generic_struct_is_a_hard_error_with_the_register_component_hint() {
        // A generic struct has no single concrete TypeId/ContractId. The lowering
        // sets is_generic so the emitter hard-errors — surfaced verbatim by the
        // thin macro as a Tier-0 compile_error.
        let input = struct_input(
            &item("struct Repo<T> { inner: T }"),
            Stereotype::Component,
            None,
            None,
            Scope::Singleton,
        )
        .expect("lowering itself succeeds; the emitter rejects the generic");
        assert!(input.is_generic);
        let err = emit(&input).expect_err("a generic bean must hard-error");
        assert!(err.message.contains("generic"), "got: {}", err.message);
        assert!(
            err.message.contains("register_component!"),
            "got: {}",
            err.message
        );
    }
}
