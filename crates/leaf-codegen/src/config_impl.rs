//! Impl-block-level lowering for `#[configuration] impl Cfg { #[bean] fn .. }` and
//! `#[aspect] impl A { #[advice(..)] fn .. }` (configuration-classes phase3/05;
//! aspect-model phase3/08+09).
//!
//! ## Why an IMPL-block macro (not an attr-on-method)
//!
//! In Spring `@Bean` methods live ON a `@Configuration` class and `@Around`/
//! `@Before` advice methods live ON an `@Aspect` class. A proc-macro ATTRIBUTE on a
//! single method cannot emit SIBLING const/`static` rows (a method-position attr
//! expands only the method, so it cannot contribute a `#[distributed_slice]` row),
//! which is why the bare `#[bean]`-on-a-method and `#[advice]`-on-a-method forms are
//! free-fn-only. The design's Rust-idiomatic answer is an IMPL-BLOCK-level macro:
//! `#[configuration]`/`#[aspect]` applied to the whole `impl` block CAN iterate the
//! impl's methods and emit ONE const row per `#[bean]`/`#[advice]` method, sidestepping
//! the attr-on-method constraint. This module is that iteration + lowering.
//!
//! ## What it emits
//!
//! - `#[configuration] impl AppConfig { #[bean] fn pool(&self, cfg: Ref<DbConfig>) ->
//!   Pool {..} #[bean] fn repo(&self, pool: Ref<Pool>) -> Repo {..} }` → one const
//!   `::leaf_core::Descriptor` per `#[bean]` method into `COMPONENTS`, each through
//!   the SAME [`crate::descriptor::emit`] currency (no second seed type). Each
//!   method's generated provider resolves the config bean (the receiver) + each param
//!   through the one `Engine::get` seam, then calls the METHOD on the config — so a
//!   `&self` method reads the MANAGED config singleton (singleton-correct: a param
//!   resolves once to the managed singleton, so the "second unmanaged instance" bug
//!   is impossible — configuration-classes phase3/05).
//! - `#[aspect] impl Audit { #[advice(around, order=N)] fn .. #[pointcut] fn .. }` →
//!   one const `::leaf_core::AdvisorRow` per advice/pointcut method into `ADVISORS`
//!   (the per-method advisor identity + chain-order pairing const), through the SAME
//!   [`crate::advisor::emit_advisor`] currency.
//!
//! ## The intra-config self-call lint
//!
//! The design mandates a `compile_error!` (with a rewrite hint) on an intra-config
//! `#[bean]`→`#[bean]` self-call (`self.repo()` inside a `#[bean]` body): under the
//! lite-only model a self-call returns a SECOND unmanaged instance. The macro CAN
//! see the body (Spring's enhancer treats it opaquely), so we turn the silent
//! lite-mode footgun into a loud diagnostic + the `take it as a parameter instead`
//! hint (phase3/05).
//!
//! ## Method-level DECLARATIVE concern annotations
//!
//! `#[advisable] impl Svc { #[transactional] fn .. #[cacheable(key="#0")] fn .. }` is
//! the SAME "iterate the impl's methods, emit one row per method" engine: the
//! [`emit_advisable_impl`] iterator (which emits the join-point spec + the
//! downcast-thunk method table) ALSO reads each natural concern annotation
//! (`#[transactional]`/`#[cacheable]`/`#[cache_put]`/`#[cache_evict]`/`#[validated]`/
//! `#[retryable]`/`#[concurrency_limit]`) on a `&self` method and emits, per concern,
//! the per-method metadata const + the `ADVISOR_PAIRINGS` row keyed by the bean's
//! `TypeId` + the return-classifier / arg-key fn — through the THIN [`crate::concern`]
//! emitters that reference the concern crates' interceptor builders. This is the
//! natural-annotation auto-wire path (no `#[aspect]`, no `.with_advisors`); the free-fn
//! `#[cacheable]`/`#[scheduled]` forms in [`crate::scheduling`] remain for non-impl
//! standalone use.

use proc_macro2::TokenStream;
use syn::{FnArg, ImplItem, ImplItemFn, ItemImpl, Pat, ReturnType, Type};

use crate::advisor::{self, AdviceKind};
use crate::concern;
use crate::descriptor::{self, BeanInput, Dependency, EmitError, Scope};
use crate::stereotype::Stereotype;

/// The attribute name a method must carry to be lowered as a `@bean` factory method.
const BEAN_ATTR: &str = "bean";

/// Emit the full `#[configuration] impl Cfg { #[bean] fn .. }` artifact: one const
/// `::leaf_core::Descriptor` (+ its `ProviderSeed`/`InjectionPlan`) per `#[bean]`
/// method, all through the SAME descriptor currency. The impl block itself is kept
/// verbatim by the thin macro; this only appends the const rows.
///
/// The receiver type (the `Self` of the impl) is the config bean each method is
/// called on — resolved through the one `Engine::get` seam so a `&self` method reads
/// the managed config singleton.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic, a `#[bean]` method
/// has no return type / takes no `self` receiver, or a `#[bean]` body makes an
/// intra-config `#[bean]`→`#[bean]` self-call (the lite-mode footgun lint).
pub fn emit_configuration_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    let self_ty = self_ty_of(item)?;
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{}` is a generic `#[configuration]` impl: a generic config has no \
                 single concrete type. Make the config concrete (its `#[bean]` \
                 methods register concrete products).",
                type_ident(&self_ty)
            ),
        });
    }

    let bean_methods = bean_methods(item);
    let bean_idents: Vec<String> = bean_methods
        .iter()
        .map(|m| m.sig.ident.to_string())
        .collect();

    let mut rows = TokenStream::new();
    for method in &bean_methods {
        let input = config_method_input(&self_ty, method, &bean_idents)?;
        rows.extend(descriptor::emit(&input)?);
    }
    Ok(rows)
}

/// Emit the full `#[aspect] impl Aspect { #[advice(..)] fn .. }` artifact: one const
/// `::leaf_core::AdvisorRow` (+ its chain-order pairing const) per `#[advice]`/
/// `#[pointcut]` method into the frozen `ADVISORS` slice, through the SAME advisor
/// currency. The impl block is kept verbatim by the thin macro.
///
/// Each per-method advisor identity is keyed on `<AspectIdent>_<methodIdent>` so two
/// advice methods on one aspect (and two aspects in one module) never collide.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic or an `#[advice]`
/// attribute is malformed / names an unknown kind.
pub fn emit_aspect_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    let self_ty = self_ty_of(item)?;
    let aspect_ident = type_ident(&self_ty);
    let is_generic = !item.generics.params.is_empty();

    let mut rows = TokenStream::new();
    for method in &item.items {
        let ImplItem::Fn(func) = method else { continue };
        let Some((kind, order)) = advice_method_kind(func)? else {
            continue;
        };
        // The per-method advisor identity is `<Aspect>_<method>` so two advice
        // methods on one aspect emit distinct rows (the attr-on-method limitation
        // could not — it had no sibling-row channel).
        let advisor_ident = format!("{aspect_ident}_{}", func.sig.ident);
        rows.extend(advisor::emit_advisor(&advisor_ident, kind, &order, is_generic)?);
    }
    Ok(rows)
}

/// Emit the full `#[advisable] impl Svc { fn place(&self, a: A) -> R { .. } }`
/// artifact: the per-bean PROXY METADATA the auto-proxy pipeline JOINs — the
/// `__leaf_joinpoints_<Ident>` join-point spec (the `ProxyPlan` pointcut input) PLUS
/// the `__leaf_methods_<Ident>` method table (the transparent downcast-thunk index),
/// each carrying ONE row per advisable method (proxy-interception phase3/08).
///
/// This is the METHOD-AWARE form of `#[advisable]` (the struct form sees no impl, so
/// it emits an EMPTY method spec): an impl-position macro CAN iterate the impl's
/// methods, so it enumerates each `&self` method as an advised join point + a
/// transparently-invocable `MethodEntry`. The impl block itself is kept verbatim by
/// the thin macro; this only appends the const pairing artifacts.
///
/// Advisable methods are the `&self`/`&mut self` inherent methods (a method threading
/// the bean instance — an associated fn with no receiver is not an advisable call
/// seam); each lowers to a `MethodKey` of `Bean::method`, its NON-receiver arg types,
/// and its return type. `async fn` is detected so the thunk `.await`s the call.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic (a generic bean has no
/// single concrete `TypeId` — `register_proxy!(Concrete)` is the escape) or is a trait
/// impl (`#[advisable]` applies to the inherent impl, the call-seam carrier).
pub fn emit_advisable_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    let self_ty = self_ty_of(item)?;
    let bean_ident = type_ident(&self_ty);
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{bean_ident}` is a generic `#[advisable]` impl: a generic bean has no \
                 single concrete type to mint a proxy `MethodTable`/join-point spec. \
                 Register a concrete instantiation with `register_proxy!({bean_ident}<Concrete>)`."
            ),
        });
    }

    let methods = advisable_methods(item, &bean_ident);
    let join_points = advisor::emit_join_points(&bean_ident, &self_ty, &methods);
    let method_table = advisor::emit_method_table(&bean_ident, &self_ty, &methods);
    // The DECLARATIVE per-concern rows: each `&self` method carrying a natural concern
    // annotation (`#[transactional]`/`#[cacheable]`/…) emits its metadata const + the
    // `ADVISOR_PAIRINGS` row keyed by the bean's TypeId. The attr-on-method limitation
    // is sidestepped exactly like `#[bean]`/`#[advice]`: the impl-block macro iterates
    // the methods and emits the sibling rows (the natural-annotation auto-wire path).
    let concerns = emit_method_concerns(item, &bean_ident, &self_ty)?;
    Ok(quote::quote! { #join_points #method_table #concerns })
}

/// Emit the per-concern artifacts for every natural concern annotation on a `&self`
/// method of the `#[advisable] impl`. Each method may carry several concerns (e.g. a
/// `#[transactional] #[cacheable]` method) — each emits its own row.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) on a malformed concern attribute, a missing
/// required field, or a concern on a method that cannot carry it.
fn emit_method_concerns(
    item: &ItemImpl,
    bean_ident: &str,
    self_ty: &Type,
) -> Result<TokenStream, EmitError> {
    let mut rows = TokenStream::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        if !has_self_receiver(func) {
            continue;
        }
        let sig = concern::MethodSig {
            method_path: format!("{bean_ident}::{}", func.sig.ident),
            ret_type: match &func.sig.output {
                ReturnType::Type(_, ty) => (**ty).clone(),
                ReturnType::Default => syn::parse_str("()").expect("unit type parses"),
            },
            first_arg_type: non_receiver_arg_types(func).into_iter().next(),
        };
        for attr in &func.attrs {
            let Some(name) = attr.path().segments.last().map(|s| s.ident.to_string()) else {
                continue;
            };
            let Some(kind) = concern::Concern::from_keyword(&name) else {
                continue;
            };
            rows.extend(concern::emit_concern(kind, attr_tokens(attr), bean_ident, self_ty, &sig)?);
        }
    }
    Ok(rows)
}

/// The advisable (`&self`/`&mut self`) inherent methods of an impl block, lowered to
/// [`advisor::MethodSpec`]s — one advised join point + method-table thunk each. An
/// associated fn with no receiver is skipped (it is not a per-instance call seam).
fn advisable_methods(item: &ItemImpl, bean_ident: &str) -> Vec<advisor::MethodSpec> {
    let mut specs = Vec::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        if !has_self_receiver(func) {
            continue;
        }
        let method_ident = func.sig.ident.to_string();
        let arg_types = non_receiver_arg_types(func);
        let ret_type = match &func.sig.output {
            ReturnType::Type(_, ty) => (**ty).clone(),
            // A `-> ()` method's return type is unit (the carrier round-trips `()`).
            ReturnType::Default => syn::parse_str("()").expect("unit type parses"),
        };
        specs.push(advisor::MethodSpec {
            method_path: format!("{bean_ident}::{method_ident}"),
            method_ident: Some(method_ident),
            is_async: func.sig.asyncness.is_some(),
            arg_types,
            ret_type,
        });
    }
    specs
}

/// `true` iff the method takes a `self`/`&self`/`&mut self` receiver.
fn has_self_receiver(func: &ImplItemFn) -> bool {
    func.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)))
}

/// The NON-receiver argument types of a method, in order (the positional tuple the
/// `ErasedArgs` carrier packs). The receiver is skipped; each typed param contributes
/// its declared type VERBATIM (no `Ref<…>` stripping — these are real call args, not
/// injection points).
fn non_receiver_arg_types(func: &ImplItemFn) -> Vec<Type> {
    func.sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Typed(pat_ty) => Some((*pat_ty.ty).clone()),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// The concrete `Self` type of an impl block (`impl AppConfig { .. }` → `AppConfig`).
///
/// # Errors
/// [`EmitError`] for a trait impl (`impl Trait for T`) — `#[configuration]`/
/// `#[aspect]` apply to an INHERENT impl, the bean/advice carrier.
fn self_ty_of(item: &ItemImpl) -> Result<Type, EmitError> {
    if item.trait_.is_some() {
        return Err(EmitError {
            message: "`#[configuration]`/`#[aspect]` apply to an inherent `impl Type \
                      { .. }` block (the bean/advice carrier), not a trait impl."
                .into(),
        });
    }
    Ok((*item.self_ty).clone())
}

/// The leading-ident name of a `Self` type (`AppConfig` / `Repo<u32>` → `Repo`),
/// used as the per-method identity base + diagnostics. Falls back to `Self` for an
/// unnameable type (a downstream type error catches the real shape).
fn type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "Self".into()),
        _ => "Self".into(),
    }
}

/// The `#[bean]`-attributed methods of an impl block, in declaration order.
fn bean_methods(item: &ItemImpl) -> Vec<&ImplItemFn> {
    item.items
        .iter()
        .filter_map(|i| match i {
            ImplItem::Fn(f) if has_attr(&f.attrs, BEAN_ATTR) => Some(f),
            _ => None,
        })
        .collect()
}

/// Whether a method carries the named attribute (`#[bean]` / `#[advice]` / …),
/// matching on the attribute path's LAST segment so both `#[bean]` and a
/// `#[leaf::bean]`-qualified form are recognised.
fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == name)
    })
}

/// Lower one `#[bean]` METHOD of a `#[configuration]` impl to a [`BeanInput`]: the
/// product is the method's return type, the name/contract derive from the method
/// ident, the params are injection points, and the RECEIVER is the config type
/// (resolved as a bean so a `&self` method reads the managed config singleton).
///
/// # Errors
/// [`EmitError`] when the method has no return type, takes no `self` receiver (a
/// `@bean` method threads the config instance — an associated fn is a free-fn
/// `#[bean]` instead), or its body makes an intra-config self-call.
fn config_method_input(
    self_ty: &Type,
    method: &ImplItemFn,
    sibling_beans: &[String],
) -> Result<BeanInput, EmitError> {
    let method_ident = method.sig.ident.to_string();

    if !method.sig.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{method_ident}` is a generic `#[bean]` method: a generic factory \
                 has no single concrete product type. Register a concrete \
                 instantiation with `register_component!(Concrete)`."
            ),
        });
    }

    let ret_ty = match &method.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(EmitError {
                message: format!(
                    "`{method_ident}` is a `#[bean]` method but has no return type: a \
                     @bean factory must produce the bean it registers."
                ),
            });
        }
    };

    // A config-class @bean is a METHOD: it must take a `self` receiver (the config
    // instance the parameter-injection threads). A `#[bean]` associated fn with no
    // receiver belongs in the free-fn `#[bean]` form.
    let deps = method_deps(method, &method_ident)?;

    // The lite-mode footgun lint: an intra-config `#[bean]`→`#[bean]` self-call
    // (`self.other_bean(..)`) returns a SECOND unmanaged instance under lite-only,
    // so it is a loud compile_error! with the rewrite hint (phase3/05).
    lint_no_self_bean_call(method, &method_ident, sibling_beans)?;

    let meta = crate::annotation::resolve(&Stereotype::Component.annotation())
        .map_err(|e| EmitError { message: e.to_string() })?;

    let mut input = BeanInput::new(ret_ty, method_ident.clone(), method_ident.clone());
    input.module_qualified = true;
    input.scope = Scope::Singleton;
    input.meta = meta;
    input.deps = deps;
    input.ctor = Some(syn::parse_str(&method_ident).map_err(|e| EmitError {
        message: format!("`{method_ident}` is not a callable method ident: {e}"),
    })?);
    input.receiver_ty = Some(self_ty.clone());
    Ok(input)
}

/// Lower a `#[bean]` method's typed parameters to injection points, requiring a
/// `self` receiver (the config instance). Each typed param keys on its binding ident
/// (or `_<index>`), stripping a `Ref<…>` handle wrapper exactly like a struct field.
fn method_deps(method: &ImplItemFn, method_ident: &str) -> Result<Vec<Dependency>, EmitError> {
    let mut deps = Vec::new();
    let mut saw_receiver = false;
    for (i, arg) in method.sig.inputs.iter().enumerate() {
        match arg {
            FnArg::Receiver(_) => saw_receiver = true,
            FnArg::Typed(pat_ty) => {
                let name = match &*pat_ty.pat {
                    Pat::Ident(p) => p.ident.to_string(),
                    _ => format!("_{i}"),
                };
                deps.push(Dependency { name, ty: descriptor::produced_ty(&pat_ty.ty) });
            }
        }
    }
    if !saw_receiver {
        return Err(EmitError {
            message: format!(
                "`{method_ident}` is a `#[configuration]` `#[bean]` method but takes \
                 no `self` receiver: a config-class @bean method threads the config \
                 instance through `&self`. Use a free `fn` `#[bean]` factory for a \
                 standalone factory."
            ),
        });
    }
    Ok(deps)
}

/// The intra-config `#[bean]`→`#[bean]` self-call lint: walk the method body and
/// hard-error on a `self.<other_bean>()` call to a SIBLING `#[bean]` method (the
/// lite-mode footgun — it returns a second unmanaged instance). The remediation is
/// the design's hint: take the collaborator as a `Ref<T>` parameter instead.
///
/// # Errors
/// [`EmitError`] naming the self-called bean + the rewrite hint.
fn lint_no_self_bean_call(
    method: &ImplItemFn,
    method_ident: &str,
    sibling_beans: &[String],
) -> Result<(), EmitError> {
    struct SelfCallVisitor<'a> {
        siblings: &'a [String],
        offender: Option<String>,
    }
    impl<'a> syn::visit::Visit<'a> for SelfCallVisitor<'a> {
        fn visit_expr_method_call(&mut self, call: &'a syn::ExprMethodCall) {
            if self.offender.is_none()
                && matches!(&*call.receiver, syn::Expr::Path(p) if p.path.is_ident("self"))
            {
                let called = call.method.to_string();
                if self.siblings.iter().any(|s| s == &called) {
                    self.offender = Some(called);
                }
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }
    let mut visitor = SelfCallVisitor { siblings: sibling_beans, offender: None };
    syn::visit::Visit::visit_block(&mut visitor, &method.block);
    if let Some(other) = visitor.offender {
        return Err(EmitError {
            message: format!(
                "`{method_ident}` makes an intra-config `#[bean]`→`#[bean]` self-call \
                 to `self.{other}()`: under leaf's lite-only `#[configuration]` model \
                 this returns a SECOND unmanaged instance (not the managed singleton). \
                 Take `{other}: Ref<{}>` as a parameter instead, so the container \
                 injects the managed bean.",
                upper_camel(&other)
            ),
        });
    }
    Ok(())
}

/// A best-effort `snake_case`/ident → `UpperCamel` for the rewrite hint's type name
/// (the bean's PRODUCT type is unknown here — the hint shows the conventional
/// capitalised form so the user can substitute the real type).
fn upper_camel(ident: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for c in ident.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether an aspect-impl method is an advice/pointcut method and, if so, its kind +
/// parsed args. Returns `Ok(None)` for a plain (non-advice) method.
///
/// # Errors
/// [`EmitError`] on a malformed `#[advice]`/`#[pointcut]` attribute or unknown kind.
fn advice_method_kind(
    func: &ImplItemFn,
) -> Result<Option<(AdviceKind, advisor::AdvisorArgs)>, EmitError> {
    if let Some(attr) = find_attr(&func.attrs, "advice") {
        let tokens = attr_tokens(attr);
        let (kind, args) = advisor::parse_advice_args(tokens)?;
        return Ok(Some((kind, args)));
    }
    if let Some(attr) = find_attr(&func.attrs, "pointcut") {
        let tokens = attr_tokens(attr);
        let args = advisor::parse_advisor_args(tokens)?;
        return Ok(Some((AdviceKind::Around, args)));
    }
    Ok(None)
}

/// Find an attribute by its path's last segment (`#[advice]` / `#[pointcut]`).
fn find_attr<'a>(attrs: &'a [syn::Attribute], name: &str) -> Option<&'a syn::Attribute> {
    attrs.iter().find(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == name)
    })
}

/// The inner token stream of an attribute's parenthesised arguments (`#[advice(around,
/// order = 5)]` → `around, order = 5`); empty for a bare `#[advice]`/`#[pointcut]`.
fn attr_tokens(attr: &syn::Attribute) -> TokenStream {
    match &attr.meta {
        syn::Meta::List(list) => list.tokens.clone(),
        _ => TokenStream::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impl_item(src: &str) -> ItemImpl {
        syn::parse_str(src).expect("a valid impl block")
    }

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn a_configuration_impl_with_two_bean_methods_emits_two_descriptors() {
        // The headline: `#[configuration] impl AppConfig { #[bean] fn pool ..; #[bean]
        // fn repo .. }` emits ONE Descriptor per #[bean] method into COMPONENTS (two
        // here) — the design's per-method lowering, sidestepping the attr-on-method
        // limitation.
        let item = impl_item(
            "impl AppConfig {
                #[bean]
                fn pool(&self, cfg: leaf_core::Ref<DbConfig>) -> Pool { todo!() }
                #[bean]
                fn repo(&self, pool: leaf_core::Ref<Pool>) -> Repo { todo!() }
                fn not_a_bean(&self) -> u8 { 0 }
             }",
        );
        let ts = emit_configuration_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // Exactly two COMPONENTS Descriptor rows (one per #[bean] method).
        assert_eq!(
            s.matches("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]")
                .count(),
            2,
            "two #[bean] methods => two COMPONENTS rows: {s}"
        );
        // Each carries its method-derived name + product type.
        assert!(s.contains(r#"Some("pool")"#), "got: {s}");
        assert!(s.contains(r#"Some("repo")"#), "got: {s}");
        assert!(s.contains("::core::any::TypeId::of::<Pool>()"), "got: {s}");
        assert!(s.contains("::core::any::TypeId::of::<Repo>()"), "got: {s}");
        // The non-#[bean] method does NOT register.
        assert!(!s.contains(r#"Some("notABean")"#), "got: {s}");
    }

    #[test]
    fn each_bean_method_resolves_the_config_receiver_through_the_engine() {
        // Each method's provider resolves the config (the receiver) AND the param
        // through the one Engine::get seam, then calls the METHOD on the config — so a
        // `&self` method reads the MANAGED config singleton.
        let item = impl_item(
            "impl AppConfig {
                #[bean]
                fn pool(&self, cfg: leaf_core::Ref<DbConfig>) -> Pool { todo!() }
             }",
        );
        let s = flat(&emit_configuration_impl(&item).expect("emits"));
        assert!(
            s.contains("let__recv:::leaf_core::Ref<AppConfig>=__engine.get::<AppConfig>().await?"),
            "got: {s}"
        );
        assert!(s.contains("__engine.get::<DbConfig>().await?"), "got: {s}");
        assert!(s.contains("__recv.pool(__dep_cfg)"), "got: {s}");
    }

    #[test]
    fn an_intra_config_bean_self_call_is_a_loud_compile_error() {
        // The lite-mode footgun lint: `self.repo()` inside a #[bean] body (calling a
        // SIBLING #[bean]) returns a second unmanaged instance, so it is a loud
        // compile_error! with the `take it as a parameter instead` rewrite hint.
        let item = impl_item(
            "impl AppConfig {
                #[bean]
                fn repo(&self) -> Repo { todo!() }
                #[bean]
                fn service(&self) -> Service { Service::new(self.repo()) }
             }",
        );
        let err = emit_configuration_impl(&item).expect_err("the self-call must hard-error");
        assert!(err.message.contains("self.repo()"), "got: {}", err.message);
        assert!(err.message.contains("parameter instead"), "got: {}", err.message);
        // The hint names the conventional product type for substitution.
        assert!(err.message.contains("Ref<Repo>"), "got: {}", err.message);
    }

    #[test]
    fn a_self_call_to_a_non_bean_method_is_allowed() {
        // Only a self-call to a SIBLING #[bean] is the footgun. A self-call to an
        // ordinary helper method is fine (it is not a managed-bean dependency).
        let item = impl_item(
            "impl AppConfig {
                fn helper(&self) -> u8 { 7 }
                #[bean]
                fn svc(&self) -> Service { Service::new(self.helper()) }
             }",
        );
        emit_configuration_impl(&item).expect("a non-#[bean] self-call is allowed");
    }

    #[test]
    fn a_bean_method_with_no_return_type_is_an_error() {
        let item = impl_item(
            "impl AppConfig {
                #[bean]
                fn nope(&self) {}
             }",
        );
        let err = emit_configuration_impl(&item).expect_err("a @bean must produce a value");
        assert!(err.message.contains("no return type"), "got: {}", err.message);
    }

    #[test]
    fn a_bean_method_with_no_self_receiver_is_an_error() {
        // A config-class @bean is a METHOD threading the config instance: an
        // associated fn (no receiver) belongs in the free-fn #[bean] form.
        let item = impl_item(
            "impl AppConfig {
                #[bean]
                fn pool(cfg: leaf_core::Ref<DbConfig>) -> Pool { todo!() }
             }",
        );
        let err = emit_configuration_impl(&item).expect_err("a config @bean needs a receiver");
        assert!(err.message.contains("self"), "got: {}", err.message);
    }

    #[test]
    fn an_aspect_impl_with_two_advice_methods_emits_two_advisor_rows() {
        // The headline AOP closure: `#[aspect] impl Audit { #[advice(around)] fn time
        // ..; #[advice(before)] fn log .. }` emits ONE AdvisorRow per advice method
        // into ADVISORS (two here) — sidestepping the attr-on-method limitation.
        let item = impl_item(
            "impl Audit {
                #[advice(around, order = 100)]
                fn time(&self) {}
                #[advice(before)]
                fn log(&self) {}
                fn helper(&self) {}
             }",
        );
        let ts = emit_aspect_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert_eq!(
            s.matches("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]")
                .count(),
            2,
            "two #[advice] methods => two ADVISORS rows: {s}"
        );
        // The per-method advisor identity is `<Aspect>_<method>`.
        assert!(
            s.contains(r#"::core::concat!(::core::module_path!(),"::","Audit_time")"#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"::core::concat!(::core::module_path!(),"::","Audit_log")"#),
            "got: {s}"
        );
        // The explicit order rides the around method's pairing const.
        assert!(s.contains("value:100i32"), "got: {s}");
    }

    #[test]
    fn an_aspect_impl_advice_chain_order_pairing_const_is_per_method() {
        let item = impl_item(
            "impl Audit {
                #[advice(around, order = 50)]
                fn time(&self) {}
             }",
        );
        let s = flat(&emit_aspect_impl(&item).expect("emits"));
        assert!(
            s.contains("pubconst__leaf_advisor_Audit_time:::leaf_core::OrderKey"),
            "got: {s}"
        );
    }

    #[test]
    fn a_pointcut_method_in_an_aspect_impl_also_emits_an_advisor_row() {
        let item = impl_item(
            "impl Audit {
                #[pointcut]
                fn tx_methods(&self) {}
             }",
        );
        let s = flat(&emit_aspect_impl(&item).expect("emits"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]"),
            "got: {s}"
        );
        assert!(
            s.contains(r#"::core::concat!(::core::module_path!(),"::","Audit_tx_methods")"#),
            "got: {s}"
        );
    }

    #[test]
    fn an_aspect_impl_with_no_advice_methods_emits_nothing() {
        // An #[aspect] impl with only plain methods contributes no advisor rows (the
        // aspect bean's COMPONENTS registration is the struct macro's concern).
        let item = impl_item("impl Audit { fn helper(&self) {} }");
        let ts = emit_aspect_impl(&item).expect("emits");
        assert!(ts.is_empty(), "no advice methods => no rows: {}", flat(&ts));
    }

    #[test]
    fn a_trait_impl_is_rejected() {
        // #[configuration]/#[aspect] apply to an inherent impl, not a trait impl.
        let item = impl_item("impl SomeTrait for AppConfig { fn f(&self) {} }");
        let err = emit_configuration_impl(&item).expect_err("a trait impl is rejected");
        assert!(err.message.contains("inherent"), "got: {}", err.message);
    }

    #[test]
    fn a_generic_configuration_impl_is_a_hard_error() {
        let item = impl_item("impl<T> AppConfig<T> { #[bean] fn pool(&self) -> Pool { todo!() } }");
        let err = emit_configuration_impl(&item).expect_err("a generic config hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    // ── #[advisable] impl (the method-aware proxy-metadata form) ─────────────────

    #[test]
    fn an_advisable_impl_emits_join_points_and_a_method_table_for_each_self_method() {
        // The headline: `#[advisable] impl OrderService { fn place_order(&self, amount:
        // i64) -> i64 {..} }` emits BOTH the per-bean join-point spec (the ProxyPlan
        // pointcut input) AND the per-bean method table (the transparent downcast
        // thunks) — the two consts the auto-wire test previously hand-wrote.
        let item = impl_item(
            "impl OrderService {
                fn new(repo: Ref<Repository>) -> Self { todo!() }
                fn place_order(&self, amount: i64) -> i64 { todo!() }
             }",
        );
        let ts = emit_advisable_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The join-point spec pairing const (the ProxyPlan input).
        assert!(
            s.contains("pubconst__leaf_joinpoints_OrderService:::leaf_core::BeanJoinPointsSpec"),
            "got: {s}"
        );
        // The method table pairing static (the transparent downcast thunks).
        assert!(
            s.contains("pubstatic__leaf_methods_OrderService:&::leaf_core::MethodTable"),
            "got: {s}"
        );
        // The advised method's MethodKey + downcast thunk (the `&self` method only —
        // the associated `new` fn has no receiver, so it is NOT an advised call seam).
        assert!(
            s.contains(r#"::leaf_core::MethodKey::of("OrderService::place_order")"#),
            "got: {s}"
        );
        assert!(s.contains("__target.place_order(__a0)"), "got: {s}");
        assert!(!s.contains("OrderService::new"), "associated fn is not an advised seam: {s}");
    }

    #[test]
    fn an_advisable_impl_threads_arg_and_ret_types_into_both_consts() {
        let item = impl_item(
            "impl OrderService {
                fn place_order(&self, amount: i64) -> i64 { todo!() }
             }",
        );
        let s = flat(&emit_advisable_impl(&item).expect("emits"));
        // The join-point spec carries the method's arg/ret TypeIds.
        assert!(s.contains("ret_type:const{::core::any::TypeId::of::<i64>()}"), "got: {s}");
        assert!(s.contains("arg_types:&[const{::core::any::TypeId::of::<i64>()}]"), "got: {s}");
        // The thunk unpacks the positional tuple of the same arg type.
        assert!(s.contains("__args.unpack::<(i64,)>()"), "got: {s}");
    }

    #[test]
    fn an_advisable_impl_detects_an_async_method() {
        let item = impl_item(
            "impl Svc {
                async fn fetch(&self, id: u64) -> String { todo!() }
             }",
        );
        let s = flat(&emit_advisable_impl(&item).expect("emits"));
        assert!(s.contains("__target.fetch(__a0).await"), "got: {s}");
    }

    #[test]
    fn an_advisable_impl_with_no_self_methods_emits_empty_consts() {
        // Only associated fns => empty (no advisable call seam), but the consts still
        // emit (so the JOIN by ContractId always finds a row).
        let item = impl_item("impl Svc { fn make() -> Self { todo!() } }");
        let ts = emit_advisable_impl(&item).expect("emits");
        let s = flat(&ts);
        assert!(s.contains("methods:&[]"), "got: {s}");
        assert!(s.contains("::leaf_core::MethodTable(&[])"), "got: {s}");
    }

    #[test]
    fn a_generic_advisable_impl_is_a_hard_error_with_register_proxy_hint() {
        let item = impl_item("impl<T> Svc<T> { fn run(&self) {} }");
        let err = emit_advisable_impl(&item).expect_err("a generic advisable impl hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
        assert!(err.message.contains("register_proxy!"), "got: {}", err.message);
    }

    #[test]
    fn an_advisable_trait_impl_is_rejected() {
        let item = impl_item("impl SomeTrait for Svc { fn f(&self) {} }");
        let err = emit_advisable_impl(&item).expect_err("a trait impl is rejected");
        assert!(err.message.contains("inherent"), "got: {}", err.message);
    }

    // ── declarative per-concern annotations on #[advisable] impl methods ──────────

    #[test]
    fn an_advisable_impl_emits_a_tx_advisor_row_for_a_transactional_method() {
        // The headline natural-annotation auto-wire: a `#[transactional]` method on an
        // #[advisable] impl emits the tx ADVISOR_PAIRINGS row (keyed by the bean's
        // TypeId) ALONGSIDE the join-point spec + method table — no #[aspect], no row.
        let item = impl_item(
            "impl LedgerService {
                #[transactional(manager = LedgerTxManager)]
                fn record(&self, amount: i64) -> Result<i64, LeafError> { todo!() }
             }",
        );
        let ts = emit_advisable_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The method table + join points still emit.
        assert!(s.contains("pubstatic__leaf_methods_LedgerService"), "got: {s}");
        // The tx advisor row keyed by the bean TypeId, with the manager + return-T.
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]"),
            "got: {s}"
        );
        assert!(
            s.contains("::leaf_tx::make_transaction_interceptor_for::<LedgerTxManager,i64>"),
            "got: {s}"
        );
    }

    #[test]
    fn an_advisable_impl_emits_cache_rows_for_cacheable_and_evict_methods() {
        let item = impl_item(
            r#"impl UserService {
                #[cacheable("users", manager = MgrBean)]
                fn find(&self) -> i64 { todo!() }
                #[cache_evict("users", all_entries, manager = MgrBean)]
                fn evict(&self) -> i64 { todo!() }
             }"#,
        );
        let s = flat(&emit_advisable_impl(&item).expect("emits"));
        assert!(s.contains("::leaf_cache::CacheOp::Cacheable"), "got: {s}");
        assert!(s.contains("::leaf_cache::CacheOp::CacheEvict"), "got: {s}");
        assert!(s.contains("all_entries:true"), "got: {s}");
        // Two ADVISOR_PAIRINGS rows (one per cache method).
        assert_eq!(
            s.matches("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]")
                .count(),
            2,
            "two cache methods => two rows: {s}"
        );
    }

    #[test]
    fn an_advisable_impl_emits_validated_retryable_and_limit_rows() {
        let item = impl_item(
            "impl Svc {
                #[validated]
                fn create(&self, req: CreateUser) -> Result<String, LeafError> { todo!() }
                #[retryable(max = 3)]
                fn flaky(&self, base: i64) -> Result<i64, LeafError> { todo!() }
                #[concurrency_limit(2, gate = LimitGate)]
                async fn guarded(&self, x: i64) -> i64 { todo!() }
             }",
        );
        let ts = emit_advisable_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_validation::single_arg_make_interceptor::<CreateUser>()"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::RetryPolicy::new(3u32)"), "got: {s}");
        assert!(
            s.contains("::leaf_resilience::make_concurrency_interceptor::<LimitGate>()"),
            "got: {s}"
        );
    }

    #[test]
    fn a_malformed_concern_attribute_is_a_loud_compile_error() {
        // A `#[transactional]` with no manager is a loud error (no default manager type).
        let item = impl_item(
            "impl S {
                #[transactional]
                fn record(&self) -> Result<i64, E> { todo!() }
             }",
        );
        let err = emit_advisable_impl(&item).expect_err("a missing manager hard-errors");
        assert!(err.message.contains("manager"), "got: {}", err.message);
    }

    #[test]
    fn a_concern_on_an_associated_fn_is_ignored() {
        // Only `&self` methods are advisable call seams; a concern on an associated fn
        // (no receiver) is not an advised method, so it emits no row.
        let item = impl_item(
            "impl S {
                #[transactional(manager = M)]
                fn make() -> Self { todo!() }
             }",
        );
        let s = flat(&emit_advisable_impl(&item).expect("emits"));
        assert!(!s.contains("ADVISOR_PAIRINGS"), "an associated fn is not advised: {s}");
    }
}
