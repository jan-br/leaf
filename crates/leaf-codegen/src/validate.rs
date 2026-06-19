//! The `#[derive(Validate)]` constraint-derive codegen (validation, phase3/09
//! §validation).
//!
//! This is the heavy, unit-testable lowering the thin `#[derive(Validate)]` macro
//! calls. It reads a named-field struct's `#[validate(..)]` field attributes and
//! emits ONE `impl ::leaf_validation::ValidateInto` whose `validate_into` body drives
//! a per-field constraint check (or a nested `@Valid` cascade) into the
//! `leaf_validation::Cascade` — the SAME hand `impl ValidateInto` token pattern a
//! user would write by hand (cascade.rs), now derived.
//!
//! Per the thin-macro path rule (charter §2.10) every emitted path is ABSOLUTE
//! `::leaf_validation::` / `::leaf_validation::constraints::` — leaf-codegen depends
//! ONLY on leaf-core, never on leaf-validation, so the emitted code names the
//! validation crate the way `emit_validated` (concern.rs) already does.
//!
//! The per-field constraint lowering (keyed on the field's `#[validate(..)]`):
//!
//! - `#[validate(not_empty)]` → `c.check("<seg>", constraints::not_empty(&self.f))`
//! - `#[validate(min = N)]`   → `c.check("<seg>", constraints::min(self.f, N))`
//! - `#[validate(max = N)]`   → `c.check("<seg>", constraints::max(self.f, N))`
//! - `#[validate(range(min = A, max = B))]` → `… constraints::range(self.f, A, B)`
//! - `#[validate(email)]`     → `c.check("<seg>", constraints::email(&self.f))`
//! - `#[validate(pattern = "glob")]` → `c.check("<seg>", constraints::pattern(&self.f, "glob"))`
//! - `#[validate(nested)]`    → `c.enter("<seg>", addr_of(&self.f), &self.f)`; for a
//!   `Vec<T>` nested field the indexed `for (i, x) in self.f.iter().enumerate()`
//!   cascade (the cascade.rs `items[{i}]` pattern).
//!
//! The `<seg>` is the CANONICAL kebab name (`max_connections` → `"max-connections"`)
//! so a nested-config violation path maps back to the config KEY. A non-struct /
//! generic target is a Tier-0 `compile_error!` (the same guard as `struct_fields`).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Expr, ExprLit, Fields, Lit, Meta, Type};

use crate::descriptor::EmitError;

/// Derive the `leaf_validation::ValidateInto` impl for a named-field `struct`:
/// one `validate_into` body with one statement per `#[validate(..)]` field (in
/// declaration order). All emitted paths are absolute `::leaf_validation::`.
///
/// # Errors
/// [`EmitError`] when the target is not a named-field struct or is generic (a
/// generic validate target has no single concrete impl — mirror `struct_fields`).
pub fn emit_validate(input: &DeriveInput) -> Result<TokenStream, EmitError> {
    let ident = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{ident}` is a generic #[derive(Validate)]: a generic validate target has no \
                 single concrete `ValidateInto` impl. Derive on a concrete instantiation."
            ),
        });
    }
    let Data::Struct(data) = &input.data else {
        return Err(EmitError {
            message: format!(
                "`{ident}` is not a struct: #[derive(Validate)] targets a named-field struct \
                 (the JavaBean shape)."
            ),
        });
    };
    let Fields::Named(named) = &data.fields else {
        return Err(EmitError {
            message: format!("`{ident}` has no named fields: a validate target must be a named-field struct."),
        });
    };

    let mut stmts = Vec::new();
    for field in &named.named {
        let fid = field.ident.clone().expect("a named field has an ident");
        let seg = canonical_name(&fid.to_string());
        for attr in &field.attrs {
            if !attr.path().is_ident("validate") {
                continue;
            }
            // The `#[validate(..)]` body is a comma-list of constraint metas.
            let metas = attr
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|e| EmitError {
                    message: format!("malformed `#[validate(..)]` on field `{fid}`: {e}"),
                })?;
            for meta in &metas {
                stmts.push(lower_constraint(meta, &fid, &seg, &field.ty)?);
            }
        }
    }

    Ok(quote! {
        impl ::leaf_validation::ValidateInto for #ident {
            fn validate_into(&self, __c: &mut ::leaf_validation::Cascade<'_>) {
                #(#stmts)*
            }
        }
    })
}

/// Lower ONE `#[validate(..)]` constraint meta over field `fid` (canonical segment
/// `seg`, type `ty`) to the matching `__c.check`/`__c.enter` statement.
fn lower_constraint(
    meta: &Meta,
    fid: &syn::Ident,
    seg: &str,
    ty: &Type,
) -> Result<TokenStream, EmitError> {
    match meta {
        // Bare-path constraints: `not_empty`, `email`, `nested`.
        Meta::Path(path) => {
            let key = path_key(path)?;
            match key.as_str() {
                "not_empty" => Ok(quote! {
                    __c.check(#seg, ::leaf_validation::constraints::not_empty(&self.#fid));
                }),
                "email" => Ok(quote! {
                    __c.check(#seg, ::leaf_validation::constraints::email(&self.#fid));
                }),
                "nested" => Ok(emit_nested(fid, seg, ty)),
                other => Err(unknown(other, fid)),
            }
        }
        // `key = value` constraints: `min = N`, `max = N`, `pattern = "glob"`.
        Meta::NameValue(nv) => {
            let key = path_key(&nv.path)?;
            match key.as_str() {
                "min" => {
                    let n = int_value(&nv.value, "min", fid)?;
                    Ok(quote! {
                        __c.check(#seg, ::leaf_validation::constraints::min(self.#fid, #n));
                    })
                }
                "max" => {
                    let n = int_value(&nv.value, "max", fid)?;
                    Ok(quote! {
                        __c.check(#seg, ::leaf_validation::constraints::max(self.#fid, #n));
                    })
                }
                "pattern" => {
                    let glob = str_value(&nv.value, "pattern", fid)?;
                    Ok(quote! {
                        __c.check(#seg, ::leaf_validation::constraints::pattern(&self.#fid, #glob));
                    })
                }
                other => Err(unknown(other, fid)),
            }
        }
        // List constraints: `range(min = A, max = B)`.
        Meta::List(list) => {
            let key = path_key(&list.path)?;
            match key.as_str() {
                "range" => {
                    let (lower, upper) = parse_range(list, fid)?;
                    Ok(quote! {
                        __c.check(#seg, ::leaf_validation::constraints::range(self.#fid, #lower, #upper));
                    })
                }
                other => Err(unknown(other, fid)),
            }
        }
    }
}

/// Emit the `#[validate(nested)]` cascade as ONE uniform, alias-safe call site.
///
/// The cardinal rule (charter §2.x): the codegen must NEVER decide the cascade SHAPE
/// (element-wise over a `Vec<T>` vs a single `enter`) from the field type's spelled name.
/// It emits `(&&CascadeTag(&self.field)).leaf_cascade(__c, "<seg>")`; the runtime autoref
/// ladder ([`leaf_validation::cascade`]) resolves the REAL field type — a `Vec<T>`
/// cascades element-wise under `<seg>[{i}]`, any other nested object `enter`s once. A
/// `type Items = Vec<Inner>;` alias resolves identically (no name-based Vec check).
fn emit_nested(fid: &syn::Ident, seg: &str, _ty: &Type) -> TokenStream {
    quote! {
        {
            #[allow(unused_imports)]
            use ::leaf_validation::{CascadeList as _, CascadeOne as _};
            (&&::leaf_validation::CascadeTag(&self.#fid)).leaf_cascade(__c, #seg);
        }
    }
}

/// The single-segment ident of a constraint key path (`min`, `range`, …).
fn path_key(path: &syn::Path) -> Result<String, EmitError> {
    path.get_ident().map(ToString::to_string).ok_or_else(|| EmitError {
        message: "a `#[validate(..)]` constraint must be a single identifier".into(),
    })
}

/// Read an integer literal as an `i64`-typed literal token (the constraint fns are
/// `i64`-only, so the emitted `N` carries an explicit `i64` suffix).
fn int_value(expr: &Expr, key: &str, fid: &syn::Ident) -> Result<syn::LitInt, EmitError> {
    if let Expr::Lit(ExprLit { lit: Lit::Int(i), .. }) = expr {
        Ok(syn::LitInt::new(&format!("{}i64", i.base10_digits()), i.span()))
    } else {
        Err(bad_int(key, fid))
    }
}

/// Read a string literal as a `&str` constraint argument.
fn str_value(expr: &Expr, key: &str, fid: &syn::Ident) -> Result<String, EmitError> {
    if let Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) = expr {
        Ok(s.value())
    } else {
        Err(EmitError {
            message: format!("`#[validate({key} = ...)]` on field `{fid}` requires a string literal"),
        })
    }
}

/// Parse `range(min = A, max = B)` into the two `i64`-suffixed bound literals.
fn parse_range(
    list: &syn::MetaList,
    fid: &syn::Ident,
) -> Result<(syn::LitInt, syn::LitInt), EmitError> {
    let inner = list
        .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
        .map_err(|e| EmitError {
            message: format!("malformed `#[validate(range(..))]` on field `{fid}`: {e}"),
        })?;
    let mut lower = None;
    let mut upper = None;
    for m in &inner {
        let Meta::NameValue(nv) = m else {
            return Err(EmitError {
                message: format!(
                    "`#[validate(range(..))]` on field `{fid}` expects `min = A, max = B`"
                ),
            });
        };
        match path_key(&nv.path)?.as_str() {
            "min" => lower = Some(int_value(&nv.value, "range.min", fid)?),
            "max" => upper = Some(int_value(&nv.value, "range.max", fid)?),
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown `#[validate(range(..))]` key `{other}` on field `{fid}` (expected `min`/`max`)"
                    ),
                });
            }
        }
    }
    match (lower, upper) {
        (Some(lo), Some(hi)) => Ok((lo, hi)),
        _ => Err(EmitError {
            message: format!(
                "`#[validate(range(..))]` on field `{fid}` requires both `min` and `max`"
            ),
        }),
    }
}

fn bad_int(key: &str, fid: &syn::Ident) -> EmitError {
    EmitError {
        message: format!("`#[validate({key} = ...)]` on field `{fid}` requires an integer literal"),
    }
}

fn unknown(key: &str, fid: &syn::Ident) -> EmitError {
    EmitError {
        message: format!(
            "unknown `#[validate({key})]` constraint on field `{fid}` (expected one of \
             `not_empty`/`email`/`nested`/`min`/`max`/`range`/`pattern`)"
        ),
    }
}

/// Map a snake_case field ident to its canonical kebab segment (the path the cascade
/// reports under, so a config violation maps to the canonical KEY). Mirrors the
/// `config.rs` `canonical_name`.
fn canonical_name(ident: &str) -> String {
    ident.replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn derive(src: &str) -> DeriveInput {
        syn::parse_str(src).expect("a valid derive input")
    }

    #[test]
    fn validate_emits_a_validate_into_impl_with_per_field_checks() {
        let ts = emit_validate(&derive(
            "struct CreateUser { #[validate(not_empty)] name: String, \
             #[validate(range(min = 0, max = 150))] age: i64 }",
        ))
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The impl is the cascade-aware ValidateInto face, absolute-pathed.
        assert!(s.contains("impl::leaf_validation::ValidateIntoforCreateUser"), "got: {s}");
        assert!(
            s.contains("fnvalidate_into(&self,__c:&mut::leaf_validation::Cascade<'_>)"),
            "got: {s}"
        );
        // not_empty borrows the field; the segment is the canonical kebab name.
        assert!(
            s.contains(
                r#"__c.check("name",::leaf_validation::constraints::not_empty(&self.name))"#
            ),
            "got: {s}"
        );
        // range is i64-by-value carrying both bounds.
        assert!(
            s.contains(
                r#"__c.check("age",::leaf_validation::constraints::range(self.age,0i64,150i64))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn validate_canonicalizes_field_names_to_kebab() {
        let s = flat(
            &emit_validate(&derive(
                "struct PoolProps { #[validate(min = 1)] max_connections: i64 }",
            ))
            .expect("emits"),
        );
        assert!(
            s.contains(
                r#"__c.check("max-connections",::leaf_validation::constraints::min(self.max_connections,1i64))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn validate_lowers_max_email_and_pattern() {
        let s = flat(
            &emit_validate(&derive(
                "struct M { #[validate(max = 10)] n: i64, \
                 #[validate(email)] e: String, \
                 #[validate(pattern = \"a*\")] p: String }",
            ))
            .expect("emits"),
        );
        assert!(
            s.contains(r#"__c.check("n",::leaf_validation::constraints::max(self.n,10i64))"#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"__c.check("e",::leaf_validation::constraints::email(&self.e))"#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"__c.check("p",::leaf_validation::constraints::pattern(&self.p,"a*"))"#),
            "got: {s}"
        );
    }

    #[test]
    fn validate_collects_multiple_attrs_on_one_field() {
        // Two #[validate(..)] on one field → two statements (collect-all).
        let s = flat(
            &emit_validate(&derive(
                "struct M { #[validate(not_empty)] #[validate(pattern = \"x*\")] s: String }",
            ))
            .expect("emits"),
        );
        assert!(
            s.contains(r#"::leaf_validation::constraints::not_empty(&self.s)"#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"::leaf_validation::constraints::pattern(&self.s,"x*")"#),
            "got: {s}"
        );
    }

    #[test]
    fn validate_lowers_a_scalar_nested_field_through_the_uniform_tag() {
        let s = flat(
            &emit_validate(&derive("struct Order { #[validate(nested)] customer: Customer }"))
                .expect("emits"),
        );
        // The cardinal rule: the nested cascade emits the ONE uniform type-driven tag
        // (NEVER a spelled `enter`/indexed-loop chosen from the field type's name). The
        // autoref ladder routes `Customer` to the single-object rung at runtime.
        assert!(
            s.contains(
                r#"(&&::leaf_validation::CascadeTag(&self.customer)).leaf_cascade(__c,"customer")"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn validate_lowers_a_vec_nested_field_through_the_same_uniform_tag() {
        // The cardinal rule, headline: a `Vec<Item>` nested field emits the EXACT SAME
        // uniform tag as a scalar nested field — the codegen never spells `Vec` or the
        // indexed loop (the name-based Vec violation is gone). The element-wise vs
        // single-object choice is the runtime autoref ladder's, keyed on the real type.
        let s = flat(
            &emit_validate(&derive("struct Order { #[validate(nested)] items: Vec<Item> }"))
                .expect("emits"),
        );
        assert!(
            s.contains(
                r#"(&&::leaf_validation::CascadeTag(&self.items)).leaf_cascade(__c,"items")"#
            ),
            "got: {s}"
        );
        // No spelled indexed loop survives in the emitted cascade.
        assert!(!s.contains(".iter().enumerate()"), "no spelled Vec loop survives: {s}");
    }

    #[test]
    fn validate_skips_unannotated_fields() {
        // A field with NO #[validate(..)] produces NO statement.
        let s = flat(
            &emit_validate(&derive(
                "struct M { #[validate(not_empty)] a: String, b: String }",
            ))
            .expect("emits"),
        );
        assert!(s.contains(r#"not_empty(&self.a)"#), "got: {s}");
        assert!(!s.contains("self.b"), "the unannotated field is not checked: {s}");
    }

    #[test]
    fn validate_rejects_a_non_struct() {
        let err = emit_validate(&derive("enum E { A, B }")).expect_err("an enum is rejected");
        assert!(err.message.contains("not a struct"), "got: {}", err.message);
    }

    #[test]
    fn validate_rejects_a_generic_target() {
        let err = emit_validate(&derive("struct P<T> { #[validate(nested)] inner: T }"))
            .expect_err("a generic target is rejected");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    #[test]
    fn validate_rejects_an_unknown_constraint() {
        let err = emit_validate(&derive("struct M { #[validate(bogus)] x: i64 }"))
            .expect_err("an unknown constraint key errors");
        assert!(err.message.contains("bogus") || err.message.contains("unknown"), "got: {}", err.message);
    }
}
