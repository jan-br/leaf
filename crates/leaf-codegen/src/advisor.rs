//! The declarative-advice / AOP codegen the thin `#[advisable]`/`#[aspect]`/
//! `#[advice]`/`#[pointcut]` macros call (declarative-advice phase3/09;
//! proxy-interception phase3/08).
//!
//! A `#[component]` becomes proxyable by being marked `#[advisable]` (the
//! transparent-newtype seam the proxy substrate wraps); an `#[aspect]` is a
//! `#[component]` carrying advice methods; each `#[advice(...)]`/`#[pointcut(...)]`
//! method lowers to ONE flat const advisor IDENTITY row in the frozen `ADVISORS`
//! `linkme` slice (`AdvisorRow { contract, order }`) PLUS a public pairing const
//! the leaf-boot proxy-assembly pass binds to its live [`leaf_core::AdvisorDescriptor`]
//! (the runtime row holds a `&'static dyn Pointcut` + a `MakeInterceptor` that
//! resolves the aspect bean, neither of which is const-constructible at macro time —
//! so this unit emits the minimal anti-DCE identity row + the chain-order metadata,
//! and the live `AdvisorDescriptor` binding is the proxy-assembly pass's concern).
//!
//! Every emitted path is ABSOLUTE `::leaf_core::…` so a user crate's imports cannot
//! shadow the seam (the thin-macro rule, charter §2.10). A generic aspect is a
//! Tier-0 [`EmitError`] hinting `register_proxy!(Concrete)` (a generic type has no
//! single concrete `ContractId`, exactly like a generic bean).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

/// The built-in advice kinds, as DATA: each pins the advisor onto one of the
/// frozen `*_ORDER` chain-order consts (the chain-sort is `cmp_chain` over the
/// composite `ChainKey`, NEVER the slice order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdviceKind {
    /// `#[advice(before)]` — runs before the join point.
    Before,
    /// `#[advice(after)]` — runs after the join point returns or throws.
    After,
    /// `#[advice(after_returning)]` — runs after a NORMAL return.
    AfterReturning,
    /// `#[advice(after_throwing)]` — runs after an error.
    AfterThrowing,
    /// `#[advice(around)]` — the around-shaped interceptor (all kinds collapse to
    /// this at lowering; the chain sees ONE kind).
    Around,
}

impl AdviceKind {
    /// Parse the advice-kind keyword (`before`/`after`/…), defaulting to `around`.
    ///
    /// # Errors
    /// [`EmitError`] on an unrecognised keyword.
    pub fn parse(name: &str) -> Result<Self, EmitError> {
        match name {
            "before" => Ok(AdviceKind::Before),
            "after" => Ok(AdviceKind::After),
            "after_returning" => Ok(AdviceKind::AfterReturning),
            "after_throwing" => Ok(AdviceKind::AfterThrowing),
            "around" => Ok(AdviceKind::Around),
            other => Err(EmitError {
                message: format!(
                    "unknown advice kind `{other}` (expected \
                     before/after/after_returning/after_throwing/around)"
                ),
            }),
        }
    }

    /// The default chain-order [`leaf_core::OrderKey`] this kind pins to (the
    /// `DEFAULT_ORDER` floor with an `Implicit` source, since no explicit `order`
    /// was given). Around/before/after share the floor for user aspects; the
    /// built-in infrastructure advisors (tx/cache/validation) carry their own pinned
    /// `*_ORDER` consts emitted by `#[cacheable]`/leaf-tx/leaf-validation directly.
    #[must_use]
    pub fn order_tokens(self) -> TokenStream {
        quote! {
            ::leaf_core::OrderKey {
                value: ::leaf_core::DEFAULT_ORDER,
                source: ::leaf_core::OrderSource::Implicit,
            }
        }
    }
}

/// The parsed `#[advisable]` / `#[aspect]` attribute arguments (currently the
/// closed `order = <int>` axis; the pointcut is read off the method, not here).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvisorArgs {
    /// An explicit integer chain order (`order = 100`); `None` uses the kind floor.
    pub order: Option<i32>,
}

/// Parse the `#[advice(kind, order = N)]` attribute body into the advice kind +
/// optional explicit order.
///
/// The FIRST bare ident (if any) is the advice kind (`before`/`around`/…),
/// defaulting to `around`; `order = <int>` sets the explicit chain order.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown kind, or a non-integer `order`.
pub fn parse_advice_args(attr: TokenStream) -> Result<(AdviceKind, AdvisorArgs), EmitError> {
    let mut kind = AdviceKind::Around;
    let mut args = AdvisorArgs::default();
    if attr.is_empty() {
        return Ok((kind, args));
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[advice] arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            // A bare ident => the advice kind keyword.
            syn::Expr::Path(p) => {
                let name = p
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .ok_or_else(|| EmitError {
                        message: "an #[advice] kind must be a bare identifier".into(),
                    })?;
                kind = AdviceKind::parse(&name)?;
            }
            // `order = <int>`.
            syn::Expr::Assign(assign) => {
                let key = assign_ident(&assign.left)?;
                if key != "order" {
                    return Err(EmitError {
                        message: format!("unknown #[advice] argument `{key}` (expected `order`)"),
                    });
                }
                args.order = Some(int_value(&assign.right)?);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unexpected #[advice] argument `{}` (expected a kind keyword or `order = N`)",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    Ok((kind, args))
}

/// Parse the `#[advisable]` / `#[aspect]` attribute body — only the optional
/// `order = <int>` axis (the headline marker carries no other config).
///
/// # Errors
/// [`EmitError`] on a malformed body or an unknown key.
pub fn parse_advisor_args(attr: TokenStream) -> Result<AdvisorArgs, EmitError> {
    let mut args = AdvisorArgs::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed advisor arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            syn::Expr::Assign(assign) => {
                let key = assign_ident(&assign.left)?;
                if key != "order" {
                    return Err(EmitError {
                        message: format!("unknown advisor argument `{key}` (expected `order`)"),
                    });
                }
                args.order = Some(int_value(&assign.right)?);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unexpected advisor argument `{}` (expected `order = N`)",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    Ok(args)
}

/// Emit the const advisor artifact for one advice/aspect element `ident`: the
/// minimal anti-DCE `::leaf_core::AdvisorRow { contract, order }` submitted into
/// the frozen `ADVISORS` slice, plus a PUBLIC pairing const carrying the chain
/// `::leaf_core::OrderKey` (named `__leaf_advisor_<Ident>` so the leaf-boot
/// proxy-assembly pass can bind the live `AdvisorDescriptor` to the row's identity).
///
/// `is_generic` makes the element a Tier-0 [`EmitError`] (a generic aspect has no
/// single concrete `ContractId`), hinting the `register_proxy!(Concrete)` escape.
///
/// # Errors
/// [`EmitError`] when the target is generic.
pub fn emit_advisor(
    ident: &str,
    kind: AdviceKind,
    args: &AdvisorArgs,
    is_generic: bool,
) -> Result<TokenStream, EmitError> {
    if is_generic {
        return Err(EmitError {
            message: format!(
                "`{ident}` is a generic aspect: a generic aspect has no single \
                 concrete type to register as a const advisor row. Register a \
                 concrete instantiation with `register_proxy!({ident}<Concrete>)`."
            ),
        });
    }
    let mangled = mangle(ident);
    let row_ident = format_ident!("__LEAF_ADVISOR_{}", mangled);
    let order_ident = format_ident!("__leaf_advisor_{}", mangled);
    let order = match args.order {
        // An explicit `order = N` is Annotation-sourced (it beats an Implicit floor
        // at an equal value, per the OrderSource tie-break).
        Some(n) => quote! {
            ::leaf_core::OrderKey {
                value: #n,
                source: ::leaf_core::OrderSource::Annotation,
            }
        },
        None => kind.order_tokens(),
    };
    // The advisor's stable identity is its module-qualified `module::Ident`
    // (a thin macro cannot resolve the module at expansion, so it is deferred to
    // the const initializer at the use site, exactly like a bean's contract).
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    Ok(quote! {
        // The PUBLIC chain-order pairing const: the leaf-boot proxy-assembly pass
        // reads this OrderKey beside the row to build the live AdvisorDescriptor
        // (whose &'static dyn Pointcut + MakeInterceptor are NOT const-constructible
        // at macro time — the bean bridge resolves the aspect at refresh).
        #[allow(non_upper_case_globals)]
        pub const #order_ident: ::leaf_core::OrderKey = #order;
        // The minimal anti-DCE advisor IDENTITY row in the frozen ADVISORS slice
        // (a dropped advisor is silently un-applied — the expected-vs-found
        // self-check catches it). The chain SORT is cmp_chain over the composite
        // ChainKey, never this slice's link order.
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorRow = ::leaf_core::AdvisorRow {
            contract: #contract,
            order: #order_ident,
        };
    })
}

/// The leading-ident name of a `register_proxy!(Concrete)` type (`Aspect<u32>` →
/// `Aspect`), used as the concrete advisor's identity base.
///
/// # Errors
/// [`EmitError`] if the type has no nameable leading ident.
pub fn proxy_ident(ty: &syn::Type) -> Result<String, EmitError> {
    match ty {
        syn::Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .ok_or_else(|| EmitError {
                message: "register_proxy! expects a concrete type with a nameable identifier".into(),
            }),
        _ => Err(EmitError {
            message: "register_proxy! expects a concrete type with a nameable identifier".into(),
        }),
    }
}

/// A spans-free, identifier-safe mangling of an ident for emitted helper names.
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, proc_macro2::Span::call_site())
}

/// The bare ident of an assignment left-hand side (`order = N` → `order`).
fn assign_ident(expr: &syn::Expr) -> Result<String, EmitError> {
    match expr {
        syn::Expr::Path(p) => p
            .path
            .get_ident()
            .map(ToString::to_string)
            .ok_or_else(|| EmitError {
                message: "a named argument must use a bare identifier key".into(),
            }),
        _ => Err(EmitError {
            message: "a named argument must use a bare identifier key".into(),
        }),
    }
}

/// The integer value of an `order = <int>` right-hand side (allowing a leading `-`).
fn int_value(expr: &syn::Expr) -> Result<i32, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => {
            i.base10_parse::<i32>().map_err(|e| EmitError {
                message: format!("`order` must be an i32 integer: {e}"),
            })
        }
        syn::Expr::Unary(syn::ExprUnary { op: syn::UnOp::Neg(_), expr, .. }) => {
            Ok(-int_value(expr)?)
        }
        other => Err(EmitError {
            message: format!("`order` must be an integer literal, got `{}`", quote! { #other }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn emits_an_absolute_core_advisor_row_into_the_advisors_slice() {
        // The headline: an aspect lowers to one const ::leaf_core::AdvisorRow
        // submitted into the frozen ADVISORS slice via the re-exported
        // ::leaf_core::linkme attr path + crate override, the SLICE absolute
        // ::leaf_core::ADVISORS.
        let ts = emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
            .expect("a concrete aspect emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]"),
            "got: {s}"
        );
        assert!(s.contains("#[linkme(crate=::leaf_core::linkme)]"), "got: {s}");
        assert!(s.contains("::leaf_core::AdvisorRow{"), "got: {s}");
    }

    #[test]
    fn advisor_contract_is_module_qualified_at_the_definition_site() {
        // A thin macro cannot resolve the aspect's module at expansion, so the
        // advisor identity is module-qualified at the definition site.
        let s = flat(
            &emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
                .expect("emits"),
        );
        assert!(
            s.contains(
                r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","TxAspect"))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn the_chain_order_rides_a_public_pairing_const() {
        // The chain OrderKey is a PUBLIC const under the deterministic pairing name
        // so the leaf-boot proxy-assembly pass can bind the live AdvisorDescriptor.
        let s = flat(
            &emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
                .expect("emits"),
        );
        assert!(
            s.contains("pubconst__leaf_advisor_TxAspect:::leaf_core::OrderKey"),
            "got: {s}"
        );
        // The default around order is the DEFAULT_ORDER floor with an Implicit source.
        assert!(s.contains("value:::leaf_core::DEFAULT_ORDER"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Implicit"), "got: {s}");
    }

    #[test]
    fn an_explicit_order_overrides_the_kind_floor() {
        let args = AdvisorArgs { order: Some(100) };
        let s = flat(&emit_advisor("A", AdviceKind::Around, &args, false).expect("emits"));
        assert!(s.contains("value:100i32"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Annotation"), "got: {s}");
        assert!(!s.contains("::leaf_core::DEFAULT_ORDER"), "explicit order wins: {s}");
    }

    #[test]
    fn a_negative_explicit_order_is_allowed() {
        let (_, args) = parse_advice_args(syn::parse_str("around, order = -50").expect("tokens"))
            .expect("parses");
        assert_eq!(args.order, Some(-50));
    }

    #[test]
    fn a_generic_aspect_is_a_hard_error_with_a_register_proxy_hint() {
        let err = emit_advisor("Generic", AdviceKind::Around, &AdvisorArgs::default(), true)
            .expect_err("a generic aspect must hard-error");
        assert!(err.message.contains("generic"), "got: {}", err.message);
        assert!(err.message.contains("register_proxy!"), "got: {}", err.message);
    }

    #[test]
    fn advice_kind_keyword_parses_and_defaults_to_around() {
        let (kind, _) = parse_advice_args(TokenStream::new()).expect("empty parses");
        assert_eq!(kind, AdviceKind::Around);
        let (kind, _) = parse_advice_args(syn::parse_str("before").expect("tokens")).expect("parses");
        assert_eq!(kind, AdviceKind::Before);
        let (kind, _) =
            parse_advice_args(syn::parse_str("after_throwing").expect("tokens")).expect("parses");
        assert_eq!(kind, AdviceKind::AfterThrowing);
    }

    #[test]
    fn an_unknown_advice_kind_is_a_hard_error() {
        let err = parse_advice_args(syn::parse_str("sideways").expect("tokens"))
            .expect_err("unknown kind errors");
        assert!(err.message.contains("unknown advice kind"), "got: {}", err.message);
    }

    #[test]
    fn an_unknown_advisor_arg_is_a_hard_error() {
        let err = parse_advisor_args(syn::parse_str(r#"bogus = 1"#).expect("tokens"))
            .expect_err("unknown arg errors");
        assert!(err.message.contains("unknown advisor argument"), "got: {}", err.message);
    }

    #[test]
    fn proxy_ident_reads_the_leading_ident_of_a_concrete_type() {
        let ty: syn::Type = syn::parse_str("Aspect<u32>").expect("a type");
        assert_eq!(proxy_ident(&ty).expect("nameable"), "Aspect");
    }
}
