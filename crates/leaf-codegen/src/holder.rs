//! The `#[holder]` attribute lowering — the thin sugar over a [`CxKey`] declaration.
//!
//! `#[holder]` is the declare-once sugar a concept-owning crate puts on a unit
//! struct to mint an ambient context key (locale in leaf-i18n, request attrs in a
//! web crate, tx in the tx crate — NEVER hardcoded in core). Unlike `#[catalog]`
//! / `#[component]`, a `CxKey` is PLAIN DATA, not a registered bean: there is NO
//! `linkme` row / NO `inventory` / NO `Descriptor` — the macro emits only the
//! trait impl + the `const`-constructed [`Holder`] accessor a user could hand-write.
//!
//! Given the parsed args `{name, policy, value, accessor}` and the user's unit
//! struct ident, [`emit_holder`] reproduces TOKEN-FOR-TOKEN the hand pattern:
//!
//! ```ignore
//! impl ::leaf_core::CxKey for LocaleKey {
//!     type Value = ::leaf_core::Locale;
//!     const NAME: &'static str = "locale";
//!     const POLICY: ::leaf_core::Propagation = ::leaf_core::Propagation::Inherit;
//! }
//! pub static LOCALE: ::leaf_core::Holder<LocaleKey> = ::leaf_core::Holder::new();
//! ```
//!
//! All emitted paths are ABSOLUTE `::leaf_core::…` (the thin-macro rule): a crate
//! using `#[holder]` needs only a `leaf-core` dep, no `use` can shadow the impl.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

/// The propagation policy a `#[holder]` declares, mirroring [`leaf_core::Propagation`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HolderPolicy {
    /// Auto-captured across a spawn hop (locale, request attrs, trace).
    Inherit,
    /// NEVER auto-captured across a hop (tx/connection resource) — the typed DEFAULT.
    #[default]
    Isolate,
}

/// The parsed `#[holder(name = "…", policy = …, value = …, accessor = …)]` arguments.
#[derive(Clone, Debug)]
pub struct HolderArgs {
    /// The stable `CxKey::NAME` (the diagnostic / bundle-schema name).
    pub name: String,
    /// The `CxKey::POLICY` propagation discipline (default [`HolderPolicy::Isolate`]).
    pub policy: HolderPolicy,
    /// The `CxKey::Value` carried in the bundle.
    pub value: syn::Type,
    /// An explicit accessor `static` ident; `None` => SCREAMING_SNAKE of the struct ident.
    pub accessor: Option<syn::Ident>,
}

/// Parse the `#[holder(name = "locale", policy = inherit, value = leaf_core::Locale)]` body.
///
/// `policy` is `inherit`/`isolate` (bare keyword, default `isolate`); `accessor` is an
/// optional bare ident overriding the derived SCREAMING_SNAKE accessor name.
///
/// # Errors
/// [`EmitError`] on a malformed body, a missing `name`/`value`, an unknown key, a
/// mistyped value, or a bad `policy` keyword.
pub fn parse_holder_args(attr: TokenStream) -> Result<HolderArgs, EmitError> {
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[holder] requires a `name` and a `value`".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[holder] arguments: {e}"),
    })?;

    let mut name: Option<String> = None;
    let mut policy = HolderPolicy::default();
    let mut value: Option<syn::Type> = None;
    let mut accessor: Option<syn::Ident> = None;

    for expr in &exprs {
        let syn::Expr::Assign(assign) = expr else {
            return Err(EmitError {
                message: format!(
                    "#[holder] arguments must be `key = value` pairs, got `{}`",
                    quote! { #expr }
                ),
            });
        };
        let key = assign_ident(&assign.left)?;
        match key.as_str() {
            "name" => {
                name = Some(str_value(&assign.right).ok_or_else(|| EmitError {
                    message: "`name` must be a string".into(),
                })?);
            }
            "policy" => policy = parse_policy(&assign.right)?,
            "value" => value = Some(type_value(&assign.right)?),
            "accessor" => accessor = Some(path_ident(&assign.right)?),
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown #[holder] argument `{other}` (expected \
                         `name`/`policy`/`value`/`accessor`)"
                    ),
                });
            }
        }
    }

    let name = name.ok_or_else(|| EmitError {
        message: "#[holder] requires a `name`".into(),
    })?;
    let value = value.ok_or_else(|| EmitError {
        message: "#[holder] requires a `value` type".into(),
    })?;
    Ok(HolderArgs {
        name,
        policy,
        value,
        accessor,
    })
}

/// Validate the item a `#[holder]` is applied to: it MUST be a UNIT struct with no
/// generics (a `CxKey` is a zero-sized concrete type — a field-bearing or generic
/// struct has no single `TypeId`/value-shape to key the bundle by).
///
/// # Errors
/// [`EmitError`] (Tier-0 `compile_error`) when the struct is non-unit or generic.
pub fn validate_holder_struct(item: &syn::ItemStruct) -> Result<(), EmitError> {
    if !item.generics.params.is_empty() || item.generics.where_clause.is_some() {
        return Err(EmitError {
            message: format!(
                "#[holder] requires a non-generic unit struct, but `{}` is generic \
                 (a CxKey is a zero-sized concrete type)",
                item.ident
            ),
        });
    }
    match &item.fields {
        syn::Fields::Unit => Ok(()),
        _ => Err(EmitError {
            message: format!(
                "#[holder] requires a unit struct (`struct {};`), but `{}` has fields",
                item.ident, item.ident
            ),
        }),
    }
}

/// Emit the `CxKey` impl + the const-constructed [`Holder`] accessor `static` for a
/// `#[holder]` declaration on the unit struct `ident`.
///
/// Emits NO `linkme` row — a `CxKey` is plain data, not a registered bean.
#[must_use]
pub fn emit_holder(ident: &str, args: &HolderArgs) -> TokenStream {
    let key = format_ident!("{}", ident);
    let name = &args.name;
    let value = &args.value;
    let policy = match args.policy {
        HolderPolicy::Inherit => quote! { ::leaf_core::Propagation::Inherit },
        HolderPolicy::Isolate => quote! { ::leaf_core::Propagation::Isolate },
    };
    let accessor = args
        .accessor
        .clone()
        .unwrap_or_else(|| format_ident!("{}", accessor_name(ident)));
    quote! {
        impl ::leaf_core::CxKey for #key {
            type Value = #value;
            const NAME: &'static str = #name;
            const POLICY: ::leaf_core::Propagation = #policy;
        }
        // The accessor is macro-generated public API: it cannot carry a hand-doc, so
        // suppress `missing_docs` for crates that lint it (its meaning is the key's).
        #[allow(missing_docs)]
        pub static #accessor: ::leaf_core::Holder<#key> = ::leaf_core::Holder::new();
    }
}

/// Parse a `policy = inherit | isolate` bare-keyword right-hand side.
fn parse_policy(expr: &syn::Expr) -> Result<HolderPolicy, EmitError> {
    let ident = path_ident(expr).map_err(|_| EmitError {
        message: format!(
            "`policy` must be `inherit` or `isolate`, got `{}`",
            quote! { #expr }
        ),
    })?;
    match ident.to_string().as_str() {
        "inherit" => Ok(HolderPolicy::Inherit),
        "isolate" => Ok(HolderPolicy::Isolate),
        other => Err(EmitError {
            message: format!("`policy` must be `inherit` or `isolate`, got `{other}`"),
        }),
    }
}

/// The bare ident of an assignment left-hand side (the argument key).
fn assign_ident(expr: &syn::Expr) -> Result<String, EmitError> {
    path_ident(expr)
        .map(|i| i.to_string())
        .map_err(|_| EmitError {
            message: "a named argument must use a bare identifier key".into(),
        })
}

/// The single bare ident a path-expression names (e.g. `inherit`, `LOCALE`).
fn path_ident(expr: &syn::Expr) -> Result<syn::Ident, EmitError> {
    match expr {
        syn::Expr::Path(p) => p.path.get_ident().cloned().ok_or_else(|| EmitError {
            message: format!("expected a bare identifier, got `{}`", quote! { #expr }),
        }),
        _ => Err(EmitError {
            message: format!("expected a bare identifier, got `{}`", quote! { #expr }),
        }),
    }
}

/// The string value of a `key = "literal"` right-hand side.
fn str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

/// The type a `value = <Type>` right-hand side names. The arg is parsed as an
/// `Expr` (the punctuated parser), so a path like `leaf_core::Locale` arrives as an
/// `Expr::Path`; re-parse its tokens as a `syn::Type`.
fn type_value(expr: &syn::Expr) -> Result<syn::Type, EmitError> {
    syn::parse2::<syn::Type>(quote! { #expr }).map_err(|e| EmitError {
        message: format!("`value` must be a type, got `{}`: {e}", quote! { #expr }),
    })
}

/// The derived accessor name for a `…Key` struct: strip a trailing `Key` suffix,
/// then SCREAMING_SNAKE the remainder — `LocaleKey` -> `LOCALE`,
/// `RequestAttributesKey` -> `REQUEST_ATTRIBUTES`. A struct that does not end in
/// `Key` (or is exactly `Key`) keeps its full ident SCREAMING_SNAKE-d.
fn accessor_name(ident: &str) -> String {
    let base = match ident.strip_suffix("Key") {
        Some(stripped) if !stripped.is_empty() => stripped,
        _ => ident,
    };
    screaming_snake(base)
}

/// The SCREAMING_SNAKE_CASE of a (typically PascalCase) ident: `Locale` -> `LOCALE`,
/// `RequestAttributes` -> `REQUEST_ATTRIBUTES`.
fn screaming_snake(ident: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for ch in ident.chars() {
        if ch.is_uppercase() && prev_lower_or_digit {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
        prev_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(attr: TokenStream) -> HolderArgs {
        parse_holder_args(attr).expect("args should parse")
    }

    #[test]
    fn emits_cxkey_impl_and_inherit_holder_for_locale() {
        let args = parse(quote! { name = "locale", policy = inherit, value = leaf_core::Locale });
        let out = emit_holder("LocaleKey", &args).to_string();
        assert!(
            out.contains("impl :: leaf_core :: CxKey for LocaleKey"),
            "missing CxKey impl: {out}"
        );
        assert!(
            out.contains("const NAME : & 'static str = \"locale\""),
            "missing NAME const: {out}"
        );
        assert!(
            out.contains(":: leaf_core :: Propagation :: Inherit"),
            "missing Inherit policy: {out}"
        );
        assert!(
            out.contains("type Value = leaf_core :: Locale"),
            "missing Value type: {out}"
        );
        assert!(
            out.contains(
                "pub static LOCALE : :: leaf_core :: Holder < LocaleKey > = :: leaf_core :: Holder :: new ()"
            ),
            "missing LOCALE accessor: {out}"
        );
    }

    #[test]
    fn policy_defaults_to_isolate_when_absent() {
        let args = parse(quote! { name = "tx", value = TxBinding });
        assert_eq!(args.policy, HolderPolicy::Isolate);
        let out = emit_holder("TxKey", &args).to_string();
        assert!(
            out.contains(":: leaf_core :: Propagation :: Isolate"),
            "default policy must be Isolate: {out}"
        );
    }

    #[test]
    fn explicit_accessor_overrides_the_derived_name() {
        let args = parse(quote! { name = "locale", value = leaf_core::Locale, accessor = FOO });
        let out = emit_holder("LocaleKey", &args).to_string();
        assert!(
            out.contains("pub static FOO : :: leaf_core :: Holder < LocaleKey >"),
            "explicit accessor must win: {out}"
        );
        assert!(
            !out.contains("pub static LOCALE"),
            "derived accessor must not also appear: {out}"
        );
    }

    #[test]
    fn accessor_defaults_to_screaming_snake_with_key_suffix_stripped() {
        // `LocaleKey` -> `LOCALE` (the spec's example), so a `…Key` struct drops the
        // `Key` suffix before SCREAMING_SNAKE-ing.
        let locale = parse(quote! { name = "locale", value = leaf_core::Locale });
        assert!(emit_holder("LocaleKey", &locale)
            .to_string()
            .contains("pub static LOCALE : :: leaf_core :: Holder < LocaleKey >"));
        // A multi-word `…Key` struct: strip `Key`, then SCREAMING_SNAKE the rest.
        let ra = parse(quote! { name = "ra", value = ReqAttrs });
        let out = emit_holder("RequestAttributesKey", &ra).to_string();
        assert!(
            out.contains("pub static REQUEST_ATTRIBUTES : :: leaf_core :: Holder"),
            "accessor must strip `Key` then SCREAMING_SNAKE: {out}"
        );
    }

    #[test]
    fn rejects_a_non_unit_struct() {
        let item: syn::ItemStruct = syn::parse_quote! { struct K { field: u8 } };
        assert!(validate_holder_struct(&item).is_err());
    }

    #[test]
    fn rejects_a_generic_struct() {
        let item: syn::ItemStruct = syn::parse_quote! { struct K<T>; };
        assert!(validate_holder_struct(&item).is_err());
    }

    #[test]
    fn accepts_a_unit_struct() {
        let item: syn::ItemStruct = syn::parse_quote! { pub struct LocaleKey; };
        assert!(validate_holder_struct(&item).is_ok());
    }

    #[test]
    fn rejects_a_bad_policy_keyword() {
        let err = parse_holder_args(quote! { name = "x", value = V, policy = bogus });
        assert!(err.is_err(), "a bad policy keyword must be rejected");
    }

    #[test]
    fn rejects_a_missing_value() {
        assert!(parse_holder_args(quote! { name = "x" }).is_err());
    }

    #[test]
    fn rejects_an_unknown_argument() {
        assert!(parse_holder_args(quote! { name = "x", value = V, frob = 1 }).is_err());
    }
}
