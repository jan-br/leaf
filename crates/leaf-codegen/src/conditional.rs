//! The `#[conditional(...)]` / `#[profile(...)]` / `#[auto_config]` / `#[import]`
//! codegen (conditions-autoconfig, phase3/05; import-composition; phase3/02).
//!
//! This is the heavy, unit-testable lowering the THIN conditional macros call. It
//! owns three pure jobs over a parsed `syn` model:
//!
//! 1. **the condition DSL → const [`leaf_core::CondExpr`] tree** — the
//!    `on_property(...)`/`on_bean(...)`/`on_missing_bean(...)`/`on_class(...)` leaf
//!    vocabulary plus the first-class `all(..)`/`any(..)`/`not(..)` boolean nodes,
//!    each lowered to one const `::leaf_core::CondExpr::Leaf(ConditionId, attrs)` (or
//!    the matching composite) via ABSOLUTE `::leaf_core` paths.
//! 2. **the profile micro-grammar → const [`leaf_core::ProfileExpr`]** — the
//!    `!`/`&`/`|` algebra parsed at MACRO time into a const `ProfileExpr`, lowered to
//!    a `Leaf(ON_PROFILE, attrs)` so a `#[profile("prod & eu")]` is structurally one
//!    `CondExpr` leaf (profiles are a PRESET, not a parallel engine). Mixed `&`/`|`
//!    without parens is a Tier-0 [`EmitError`].
//! 3. **the auto-config + import wiring** — `#[auto_config]` emits the same const
//!    `::leaf_core::Descriptor` into the SEPARATE `AUTO_CONFIGS` slice at
//!    `CandidateRole::FALLBACK` (`auto_config_role()`); `#[import]` emits the
//!    `ImportEdge` composition rows.
//!
//! Every emitted const ([`CondExpr`]/[`ProfileExpr`]/[`ConditionRow`](leaf_core::ConditionRow)/[`Descriptor`](leaf_core::Descriptor))
//! is absolute-`::leaf_core`-pathed, so the macro stays thin and a user crate's
//! imports cannot shadow the seam (the thin-macro rule, charter §2.10). The guard
//! tree is emitted as a PUBLIC const beside the element (the frozen
//! `Descriptor.meta` is the `AnnotationMetadata` shape and carries NO `CondExpr`
//! field, so the Descriptor→guard pairing is completed by the leaf-boot assembly
//! pass, exactly like the `ProviderSeed` pairing).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

// ─────────────────────────── the condition DSL ──────────────────────────────

/// One parsed condition expression — the codegen mirror of the frozen
/// [`leaf_core::CondExpr`] algebra (owned here; lowered to a const tree by
/// [`CondExpr::lower`]).
///
/// A `Leaf` carries the canonical condition-kind path (the `ConditionId` minting
/// input, hashed through the one [`leaf_core::contract_hash`]) plus its typed
/// attributes; `All`/`Any`/`Not` are the first-class boolean nodes; `Const` is the
/// build-folded constant a `true`/`false` literal lowers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CondExpr {
    /// A catalog-member leaf: a canonical kind path + its typed attributes.
    Leaf {
        /// The canonical condition-kind path (`leaf::condition::OnProperty`).
        kind: String,
        /// The leaf's const attributes.
        attrs: Vec<CondAttr>,
    },
    /// Conjunction: matches iff EVERY child matches (vacuously `true`).
    All(Vec<CondExpr>),
    /// Disjunction: matches iff ANY child matches (vacuously `false`).
    Any(Vec<CondExpr>),
    /// Negation of its single child.
    Not(Box<CondExpr>),
    /// A build-folded constant.
    Const(bool),
}

/// One typed attribute on a [`CondExpr::Leaf`] — the codegen mirror of the frozen
/// [`leaf_core::Attr`] carriage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CondAttr {
    /// A string-valued attribute (`having_value = "true"`).
    Str(String, String),
    /// A boolean-valued attribute (`match_if_missing = false`).
    Bool(String, bool),
    /// A type-valued attribute (`on_bean(Foo)` carries the `Foo` type).
    Type(String, syn::Type),
}

impl CondExpr {
    /// Lower this expression to a const `::leaf_core::CondExpr` token expression
    /// (absolute paths). A `Leaf`'s `ConditionId` is minted from the canonical kind
    /// path through `::leaf_core::ConditionId(::leaf_core::contract_hash(path) as
    /// u32)` so it agrees byte-for-byte with the runtime catalog's id minting.
    #[must_use]
    pub fn lower(&self) -> TokenStream {
        match self {
            CondExpr::Const(b) => quote! { ::leaf_core::CondExpr::Const(#b) },
            CondExpr::Leaf { kind, attrs } => {
                let id = lower_condition_id(kind);
                let attr_rows = attrs.iter().map(CondAttr::lower);
                quote! {
                    ::leaf_core::CondExpr::Leaf(#id, &[ #(#attr_rows),* ])
                }
            }
            CondExpr::All(children) => {
                let rows = children.iter().map(CondExpr::lower);
                quote! { ::leaf_core::CondExpr::All(&[ #(#rows),* ]) }
            }
            CondExpr::Any(children) => {
                let rows = children.iter().map(CondExpr::lower);
                quote! { ::leaf_core::CondExpr::Any(&[ #(#rows),* ]) }
            }
            CondExpr::Not(inner) => {
                let row = inner.lower();
                quote! { ::leaf_core::CondExpr::Not(&#row) }
            }
        }
    }

    /// The set of canonical condition-kind paths referenced by every `Leaf` in this
    /// tree (used to emit one `ConditionRow` anti-DCE anchor per referenced kind).
    #[must_use]
    pub fn leaf_kinds(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_kinds(&mut out);
        out
    }

    fn collect_kinds(&self, out: &mut Vec<String>) {
        match self {
            CondExpr::Leaf { kind, .. } => {
                if !out.contains(kind) {
                    out.push(kind.clone());
                }
            }
            CondExpr::All(children) | CondExpr::Any(children) => {
                for c in children {
                    c.collect_kinds(out);
                }
            }
            CondExpr::Not(inner) => inner.collect_kinds(out),
            CondExpr::Const(_) => {}
        }
    }
}

impl CondAttr {
    /// Lower one attribute to a const `::leaf_core::Attr` token expression.
    #[must_use]
    pub fn lower(&self) -> TokenStream {
        match self {
            CondAttr::Str(k, v) => quote! { ::leaf_core::Attr::Str(#k, #v) },
            CondAttr::Bool(k, v) => quote! { ::leaf_core::Attr::Bool(#k, #v) },
            CondAttr::Type(k, ty) => quote! {
                ::leaf_core::Attr::Type(#k, const { ::core::any::TypeId::of::<#ty>() })
            },
        }
    }
}

/// The const `::leaf_core::ConditionId` expression minted from a canonical kind
/// path — `ConditionId(contract_hash(path) as u32)`, exactly the runtime catalog's
/// id minting (and the same shape `ON_PROFILE` uses), so a macro-emitted leaf
/// resolves against the link-collected `CONDITIONS` row for that kind.
#[must_use]
fn lower_condition_id(kind: &str) -> TokenStream {
    quote! {
        ::leaf_core::ConditionId(::leaf_core::contract_hash(#kind) as u32)
    }
}

/// Parse the `#[conditional(...)]` attribute body into a [`CondExpr`].
///
/// The grammar is the documented leaf condition DSL:
/// - `on_property("k", having_value = "v", match_if_missing = false)` →
///   `Leaf(OnProperty, ..)`; the first positional string is the property key.
/// - `on_bean(Ty)` / `on_missing_bean(Ty)` / `on_single_candidate(Ty)` →
///   `Leaf(OnBean*, [type = Ty])`.
/// - `on_class("crate")` / `on_missing_class("crate")` → `Leaf(OnClass*, ..)`.
/// - `all(..)` / `any(..)` / `not(..)` → the first-class boolean nodes.
/// - `true` / `false` → `Const`.
///
/// A bare `#[conditional]` (empty body) is the vacuously-true `All([])`
/// (`UNCONDITIONAL`).
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown leaf kind, a missing/mistyped
/// argument, or `not(..)` with anything but exactly one child.
pub fn parse_conditional(attr: TokenStream) -> Result<CondExpr, EmitError> {
    if attr.is_empty() {
        return Ok(CondExpr::All(Vec::new()));
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[conditional] arguments: {e}"),
    })?;
    let mut nodes = Vec::new();
    for expr in &exprs {
        nodes.push(parse_cond_expr(expr)?);
    }
    // Multiple comma-separated top-level conditions are implicitly ANDed (Spring's
    // multiple-@Conditional stacking semantics).
    match nodes.len() {
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => Ok(CondExpr::All(nodes)),
    }
}

/// Parse one `syn::Expr` into a [`CondExpr`] node.
fn parse_cond_expr(expr: &syn::Expr) -> Result<CondExpr, EmitError> {
    match expr {
        // `true` / `false` literal → a build-folded constant.
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) => Ok(CondExpr::Const(b.value)),
        // A call expression: `kind(args...)`.
        syn::Expr::Call(call) => parse_cond_call(call),
        // A bare ident path used as a no-arg marker (rare; treated as a leaf kind).
        syn::Expr::Path(p) => {
            let name = path_ident(&p.path).ok_or_else(|| EmitError {
                message: "expected a condition kind like `on_property(...)`".into(),
            })?;
            Ok(CondExpr::Leaf { kind: canonical_kind(&name)?, attrs: Vec::new() })
        }
        other => Err(EmitError {
            message: format!(
                "unexpected #[conditional] expression `{}` (expected `on_*(...)`, \
                 `all/any/not(...)`, or a bool literal)",
                quote! { #other }
            ),
        }),
    }
}

/// Parse a `kind(args...)` call expression into a [`CondExpr`].
fn parse_cond_call(call: &syn::ExprCall) -> Result<CondExpr, EmitError> {
    let syn::Expr::Path(p) = &*call.func else {
        return Err(EmitError {
            message: "a #[conditional] node must be a simple `kind(...)` call".into(),
        });
    };
    let name = path_ident(&p.path).ok_or_else(|| EmitError {
        message: "a #[conditional] node must be a simple `kind(...)` call".into(),
    })?;

    match name.as_str() {
        "all" | "any" => {
            let mut children = Vec::new();
            for arg in &call.args {
                children.push(parse_cond_expr(arg)?);
            }
            Ok(if name == "all" {
                CondExpr::All(children)
            } else {
                CondExpr::Any(children)
            })
        }
        "not" => {
            if call.args.len() != 1 {
                return Err(EmitError {
                    message: format!("`not(..)` takes exactly one child, got {}", call.args.len()),
                });
            }
            Ok(CondExpr::Not(Box::new(parse_cond_expr(&call.args[0])?)))
        }
        // The property family: first positional string is the key.
        "on_property" | "on_missing_property" => parse_property_leaf(&name, call),
        // The bean family: a single type argument.
        "on_bean" | "on_missing_bean" | "on_single_candidate" => parse_bean_leaf(&name, call),
        // The class family: a single string argument (a crate/feature name).
        "on_class" | "on_missing_class" => parse_class_leaf(&name, call),
        other => Err(EmitError {
            message: format!(
                "unknown #[conditional] kind `{other}` (expected on_property/on_bean/\
                 on_missing_bean/on_single_candidate/on_class/on_missing_class/all/any/not)"
            ),
        }),
    }
}

/// Parse an `on_property("k", having_value = "v", match_if_missing = b)` leaf.
fn parse_property_leaf(name: &str, call: &syn::ExprCall) -> Result<CondExpr, EmitError> {
    let mut attrs = Vec::new();
    let mut saw_key = false;
    for (i, arg) in call.args.iter().enumerate() {
        match arg {
            // The first positional string literal is the property name. The runtime
            // `OnProperty`/`OnMissingProperty` reads it from the `"name"` attr (the
            // multi-name form ANDs them), so the macro emits it under `"name"` — NOT
            // `"key"`, which the runtime ignores (a vacuously-true guard that never gates).
            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) if i == 0 => {
                attrs.push(CondAttr::Str("name".into(), s.value()));
                saw_key = true;
            }
            // `having_value = "..."` / `prefix = "..."` etc.
            syn::Expr::Assign(assign) => {
                let key = assign_key(assign)?;
                attrs.push(parse_named_attr(&key, &assign.right)?);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "`{name}` expects a property key string then `name = value` pairs, \
                         got `{}`",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    if !saw_key {
        return Err(EmitError {
            message: format!("`{name}` requires a property key string as its first argument"),
        });
    }
    Ok(CondExpr::Leaf { kind: canonical_kind(name)?, attrs })
}

/// Parse an `on_bean(Ty)` / `on_missing_bean(Ty)` / `on_single_candidate(Ty)` leaf.
fn parse_bean_leaf(name: &str, call: &syn::ExprCall) -> Result<CondExpr, EmitError> {
    if call.args.len() != 1 {
        return Err(EmitError {
            message: format!("`{name}` takes exactly one type argument, got {}", call.args.len()),
        });
    }
    let ty = expr_to_type(&call.args[0]).ok_or_else(|| EmitError {
        message: format!("`{name}` expects a bean TYPE argument (e.g. `{name}(MyService)`)"),
    })?;
    Ok(CondExpr::Leaf {
        kind: canonical_kind(name)?,
        attrs: vec![CondAttr::Type("type".into(), ty)],
    })
}

/// Parse an `on_class("crate")` / `on_missing_class("crate")` leaf.
fn parse_class_leaf(name: &str, call: &syn::ExprCall) -> Result<CondExpr, EmitError> {
    if call.args.len() != 1 {
        return Err(EmitError {
            message: format!("`{name}` takes exactly one string argument, got {}", call.args.len()),
        });
    }
    let s = match &call.args[0] {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => s.value(),
        other => {
            return Err(EmitError {
                message: format!("`{name}` expects a string argument, got `{}`", quote! { #other }),
            });
        }
    };
    Ok(CondExpr::Leaf {
        kind: canonical_kind(name)?,
        attrs: vec![CondAttr::Str("class".into(), s)],
    })
}

/// Lower a `name = value` named attribute to a [`CondAttr`].
fn parse_named_attr(key: &str, value: &syn::Expr) -> Result<CondAttr, EmitError> {
    match value {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => {
            Ok(CondAttr::Str(key.to_string(), s.value()))
        }
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) => {
            Ok(CondAttr::Bool(key.to_string(), b.value))
        }
        other => Err(EmitError {
            message: format!("attribute `{key}` must be a string or bool, got `{}`", quote! { #other }),
        }),
    }
}

/// The key of a `name = value` assignment (the left side must be a bare ident).
fn assign_key(assign: &syn::ExprAssign) -> Result<String, EmitError> {
    match &*assign.left {
        syn::Expr::Path(p) => path_ident(&p.path).ok_or_else(|| EmitError {
            message: "a named condition attribute must use a bare identifier key".into(),
        }),
        _ => Err(EmitError {
            message: "a named condition attribute must use a bare identifier key".into(),
        }),
    }
}

/// Map a leaf-DSL kind name (`on_property`) to its canonical condition-kind path
/// (`leaf::condition::OnProperty`) — the `ConditionId` minting input shared with
/// the runtime catalog.
fn canonical_kind(name: &str) -> Result<String, EmitError> {
    let camel = match name {
        "on_property" => "OnProperty",
        "on_missing_property" => "OnMissingProperty",
        "on_bean" => "OnBean",
        "on_missing_bean" => "OnMissingBean",
        "on_single_candidate" => "OnSingleCandidate",
        "on_class" => "OnClass",
        "on_missing_class" => "OnMissingClass",
        other => {
            return Err(EmitError {
                message: format!("unknown condition kind `{other}`"),
            });
        }
    };
    Ok(format!("leaf::condition::{camel}"))
}

/// The leading ident of a path (a single-segment path's ident), or `None`.
fn path_ident(path: &syn::Path) -> Option<String> {
    path.get_ident().map(ToString::to_string).or_else(|| {
        path.segments.last().map(|s| s.ident.to_string())
    })
}

/// Best-effort conversion of an expression used in argument position to a type
/// (so `on_bean(Foo)` and `on_bean(crate::Foo)` both work).
fn expr_to_type(expr: &syn::Expr) -> Option<syn::Type> {
    match expr {
        syn::Expr::Path(p) => Some(syn::Type::Path(syn::TypePath {
            qself: p.qself.clone(),
            path: p.path.clone(),
        })),
        _ => None,
    }
}

// ─────────────────────────── the profile grammar ────────────────────────────

/// One parsed profile expression — the codegen mirror of the frozen
/// [`leaf_core::ProfileExpr`] (owned here; lowered to a const tree by [`ProfileExpr::lower`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileExpr {
    /// A bare profile name; matches iff active.
    Name(String),
    /// Negation; matches iff the name is ABSENT.
    Not(Box<ProfileExpr>),
    /// Conjunction (`&`); matches iff every child matches.
    And(Vec<ProfileExpr>),
    /// Disjunction (`|`); matches iff any child matches.
    Or(Vec<ProfileExpr>),
}

impl ProfileExpr {
    /// Lower this profile expression to a const `::leaf_core::ProfileExpr` token
    /// expression (absolute paths). Recursive nodes use `&` references into nested
    /// const arrays/values, matching the frozen `&'static`-tree shape.
    #[must_use]
    pub fn lower(&self) -> TokenStream {
        match self {
            ProfileExpr::Name(n) => quote! { ::leaf_core::ProfileExpr::Name(#n) },
            ProfileExpr::Not(inner) => {
                let row = inner.lower();
                quote! { ::leaf_core::ProfileExpr::Not(&#row) }
            }
            ProfileExpr::And(children) => {
                let rows = children.iter().map(ProfileExpr::lower);
                quote! { ::leaf_core::ProfileExpr::And(&[ #(#rows),* ]) }
            }
            ProfileExpr::Or(children) => {
                let rows = children.iter().map(ProfileExpr::lower);
                quote! { ::leaf_core::ProfileExpr::Or(&[ #(#rows),* ]) }
            }
        }
    }
}

/// Parse a profile-expression string (`"prod & (eu | us)"`) into a [`ProfileExpr`]
/// over the frozen 3-operator algebra. Mixing `&` and `|` at the SAME level
/// without parentheses is a Tier-0 [`EmitError`] (the fail-fast-on-ambiguity rule).
///
/// # Errors
/// [`EmitError`] on a syntactically malformed expression (mismatched parens, mixed
/// `&`/`|` without parens, empty operand, illegal character).
pub fn parse_profile(s: &str) -> Result<ProfileExpr, EmitError> {
    let tokens = tokenize_profile(s)?;
    let mut pos = 0;
    let expr = parse_profile_inner(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(EmitError {
            message: format!("trailing tokens in profile expression `{s}`"),
        });
    }
    Ok(expr)
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum PTok {
    Name(String),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize_profile(s: &str) -> Result<Vec<PTok>, EmitError> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '!' => {
                toks.push(PTok::Not);
                chars.next();
            }
            '&' => {
                toks.push(PTok::And);
                chars.next();
            }
            '|' => {
                toks.push(PTok::Or);
                chars.next();
            }
            '(' => {
                toks.push(PTok::LParen);
                chars.next();
            }
            ')' => {
                toks.push(PTok::RParen);
                chars.next();
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(PTok::Name(name));
            }
            other => {
                return Err(EmitError {
                    message: format!("illegal character `{other}` in profile expression"),
                });
            }
        }
    }
    if toks.is_empty() {
        return Err(EmitError {
            message: "empty profile expression".into(),
        });
    }
    Ok(toks)
}

fn parse_profile_inner(tokens: &[PTok], pos: &mut usize) -> Result<ProfileExpr, EmitError> {
    let first = parse_profile_primary(tokens, pos)?;
    let op = match tokens.get(*pos) {
        Some(PTok::And) => Some(PTok::And),
        Some(PTok::Or) => Some(PTok::Or),
        _ => None,
    };
    let Some(op) = op else {
        return Ok(first);
    };
    let mut operands = vec![first];
    while let Some(tok) = tokens.get(*pos) {
        match tok {
            PTok::And | PTok::Or if *tok == op => {
                *pos += 1;
                operands.push(parse_profile_primary(tokens, pos)?);
            }
            PTok::And | PTok::Or => {
                return Err(EmitError {
                    message: "mixed `&` and `|` without parentheses in profile expression".into(),
                });
            }
            _ => break,
        }
    }
    Ok(match op {
        PTok::And => ProfileExpr::And(operands),
        _ => ProfileExpr::Or(operands),
    })
}

fn parse_profile_primary(tokens: &[PTok], pos: &mut usize) -> Result<ProfileExpr, EmitError> {
    match tokens.get(*pos) {
        Some(PTok::Not) => {
            *pos += 1;
            let inner = parse_profile_primary(tokens, pos)?;
            Ok(ProfileExpr::Not(Box::new(inner)))
        }
        Some(PTok::LParen) => {
            *pos += 1;
            let inner = parse_profile_inner(tokens, pos)?;
            match tokens.get(*pos) {
                Some(PTok::RParen) => {
                    *pos += 1;
                    Ok(inner)
                }
                _ => Err(EmitError {
                    message: "missing closing parenthesis in profile expression".into(),
                }),
            }
        }
        Some(PTok::Name(n)) => {
            *pos += 1;
            Ok(ProfileExpr::Name(n.clone()))
        }
        Some(other) => Err(EmitError {
            message: format!("expected a profile name, `!`, or `(`, got `{other:?}`"),
        }),
        None => Err(EmitError {
            message: "unexpected end of profile expression".into(),
        }),
    }
}

/// Parse the `#[profile("...")]` attribute body (a single string literal, or a
/// comma-separated list of names treated as an OR per the array form) into a
/// [`ProfileExpr`].
///
/// # Errors
/// [`EmitError`] when the body is not a string literal / name list, or the
/// expression is malformed.
pub fn parse_profile_attr(attr: TokenStream) -> Result<ProfileExpr, EmitError> {
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[profile(...)] requires a profile expression".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[profile] arguments: {e}"),
    })?;
    let mut names = Vec::new();
    for expr in &exprs {
        match expr {
            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => {
                let parsed = parse_profile(&s.value())?;
                if exprs.len() == 1 {
                    return Ok(parsed);
                }
                names.push(parsed);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "#[profile] expects string profile expressions, got `{}`",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    // Multiple string args = the array OR form.
    Ok(ProfileExpr::Or(names))
}

/// Lower a [`ProfileExpr`] to the equivalent profile [`CondExpr`] leaf — a
/// `Leaf(ON_PROFILE, [expr = "<rendered>"])`. Profiles are a PRESET: the whole
/// expression rides ONE `ON_PROFILE` leaf whose attr carries the rendered
/// expression string the runtime `OnProfile` condition re-parses via
/// `accepts_profiles`. The runtime reads it from the `"expr"` attr
/// (`leaf_conditions::profile`), so the macro emits it under `"expr"` — NOT
/// `"profiles"` (the same align-codegen-to-the-runtime-attr rule `on_property`
/// follows for `"name"`; a mismatched key made the guard vacuously active).
#[must_use]
pub fn profile_to_cond(expr: &ProfileExpr) -> CondExpr {
    CondExpr::Leaf {
        kind: "leaf::condition::OnProfile".into(),
        attrs: vec![CondAttr::Str("expr".into(), render_profile(expr))],
    }
}

/// Render a [`ProfileExpr`] back to its canonical string form (so the runtime
/// `OnProfile` leaf can re-parse it through the same 3-operator grammar).
fn render_profile(expr: &ProfileExpr) -> String {
    match expr {
        ProfileExpr::Name(n) => n.clone(),
        ProfileExpr::Not(inner) => format!("!{}", render_profile(inner)),
        ProfileExpr::And(children) => {
            let parts: Vec<String> = children.iter().map(render_profile_grouped).collect();
            parts.join(" & ")
        }
        ProfileExpr::Or(children) => {
            let parts: Vec<String> = children.iter().map(render_profile_grouped).collect();
            parts.join(" | ")
        }
    }
}

/// Render a child, parenthesizing a compound so the re-parse keeps precedence.
fn render_profile_grouped(expr: &ProfileExpr) -> String {
    match expr {
        ProfileExpr::Name(_) | ProfileExpr::Not(_) => render_profile(expr),
        ProfileExpr::And(_) | ProfileExpr::Or(_) => format!("({})", render_profile(expr)),
    }
}

// ─────────────────── the conditional guard emission helpers ──────────────────

/// Emit the const guard artifact for one gated element: the PUBLIC const
/// `::leaf_core::CondExpr` tree (under a deterministic `__leaf_guard_<Ident>` name
/// so the leaf-boot assembly pass can pair it with the element's `Descriptor`),
/// plus one `::leaf_core::ConditionRow` anti-DCE anchor per referenced condition
/// kind submitted into the frozen `CONDITIONS` slice.
///
/// `ident` is the gated element's bean ident (the pairing key); the guard const is
/// named deterministically off it, exactly like the `ProviderSeed` pairing.
#[must_use]
pub fn emit_guard(ident: &str, expr: &CondExpr) -> TokenStream {
    let mangled = mangle(ident);
    let guard_ident = format_ident!("__leaf_guard_{}", mangled);
    let guard_row_ident = format_ident!("__LEAF_GUARD_PAIRING_{}", mangled);
    let tree = expr.lower();
    // The gated element's module-qualified contract (the GUARD_PAIRINGS JOIN key),
    // built at the use site exactly like a bean's contract.
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };

    // One CONDITIONS anti-DCE anchor per distinct referenced kind, named off both
    // the gated element and the kind so two kinds (or two elements) never collide.
    let mut anchors = Vec::new();
    for (i, kind) in expr.leaf_kinds().iter().enumerate() {
        let row_ident = format_ident!("__LEAF_COND_{}_{}", mangled, i);
        let id = lower_condition_id(kind);
        anchors.push(quote! {
            #[allow(non_upper_case_globals)]
            #[::leaf_core::linkme::distributed_slice(::leaf_core::CONDITIONS)]
            #[linkme(crate = ::leaf_core::linkme)]
            static #row_ident: ::leaf_core::ConditionRow = ::leaf_core::ConditionRow {
                contract: ::leaf_core::ContractId::of(#kind),
                marker: ::leaf_core::MarkerId::of(#kind),
            };
            // Reference the id const so the kind-path constant is not folded away
            // before the anchor row is built (keeps the leaf id legible).
            const _: ::leaf_core::ConditionId = #id;
        });
    }

    quote! {
        // `#[doc(hidden)]`: the `__leaf_guard_<Ident>` const is framework-internal
        // wiring (the assembly pass's guard-pairing key), not public API — so a
        // contributing crate under `#![warn(missing_docs)]` needs no doc on it.
        #[allow(non_upper_case_globals)]
        #[doc(hidden)]
        pub const #guard_ident: ::leaf_core::CondExpr = #tree;
        // Submit the guard into GUARD_PAIRINGS (the auto-collect substrate) keyed by
        // the gated element's ContractId — so leaf-boot's condition routing finds it
        // with no hand-assembled `.with_guards`. Same re-export pattern as COMPONENTS.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::GUARD_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #guard_row_ident: ::leaf_core::GuardPairingRow = ::leaf_core::GuardPairingRow {
            contract: #contract,
            guard: &#guard_ident,
        };
        #(#anchors)*
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

// ─────────────────────────────── #[auto_config] ─────────────────────────────

/// Lower a `#[auto_config]` struct to the [`crate::descriptor::BeanInput`] the
/// emitter consumes, but TARGETING the `AUTO_CONFIGS` slice at
/// `CandidateRole::FALLBACK` (`auto_config_role()`) — the structural flag
/// distinguishing the auto-config channel from `COMPONENTS` without a second seed
/// type. An auto-config IS a `Descriptor`; it differs only in the channel + its
/// fallback candidate role (so a user bean transparently supersedes it).
///
/// # Errors
/// [`EmitError`] when the struct is generic (no single concrete type) or its
/// stereotype annotation is malformed.
pub fn auto_config_input(item: &syn::ItemStruct) -> Result<crate::descriptor::BeanInput, EmitError> {
    // Reuse the stereotype struct lowering (fields → deps, contract, meta), then
    // flag it onto the auto-config channel at the fallback candidate role.
    let mut input = crate::stereotype::struct_input(
        item,
        crate::stereotype::Stereotype::Configuration,
        None,
        None,
        crate::descriptor::Scope::Singleton,
        // `#[auto_config]` keeps field injection (no `constructor = …` surface yet —
        // a trivial deferred follow-up per the design).
        None,
    )?;
    input.slice = crate::descriptor::Slice::AutoConfigs;
    // An auto-config registers at CandidateRole::FALLBACK: re-resolve the stereotype
    // annotation with `fallback = true` so the lowered meta carries FALLBACK.
    input.meta = crate::annotation::resolve(
        &crate::stereotype::Stereotype::Configuration
            .annotation()
            .with_attr("fallback", crate::annotation::AttrValue::Bool(true)),
    )
    .map_err(|e| EmitError { message: e.to_string() })?;
    Ok(input)
}

/// Emit the full `#[auto_config]` artifact: the const `::leaf_core::Descriptor`
/// submitted into the SEPARATE `AUTO_CONFIGS` slice at `CandidateRole::FALLBACK`,
/// plus the optional `#[conditional(...)]` guard (its `CONDITIONS` anchors + the
/// public guard-pairing const) when a guard is supplied.
///
/// # Errors
/// [`EmitError`] per [`auto_config_input`].
pub fn emit_auto_config(
    item: &syn::ItemStruct,
    guard: Option<&CondExpr>,
) -> Result<TokenStream, EmitError> {
    let input = auto_config_input(item)?;
    let descriptor = crate::descriptor::emit(&input)?;
    let guard_ts = match guard {
        Some(expr) => emit_guard(&item.ident.to_string(), expr),
        None => TokenStream::new(),
    };
    Ok(quote! {
        #descriptor
        #guard_ts
    })
}

// ─────────────────────────────── #[import] ──────────────────────────────────

/// Parse the `#[import(A, B, C)]` attribute body into the list of imported type
/// paths (the importee identities). The importer path-references each importee, so
/// the importer's force-link covers the importee's Layer-0 DCE for free.
///
/// # Errors
/// [`EmitError`] on a malformed body or a non-path argument.
pub fn parse_import(attr: TokenStream) -> Result<Vec<syn::Path>, EmitError> {
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[import(..)] requires at least one type to import".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated;
    let paths = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[import] arguments: {e}"),
    })?;
    Ok(paths.into_iter().collect())
}

/// Emit the `#[import(A, B)]` composition wiring for the importing element
/// `ident`: one public const `::leaf_core::ImportEdge` (the `from` → `to[]`
/// composition currency the assembly pass reads) keyed on the imported types'
/// stable `ContractId`s, plus an anti-DCE `use <Importee> as _;`-style reference so
/// the importer path-references each importee (closing Layer-0 DCE for the edge).
///
/// The `from`/`to` contracts are minted from each type's leading ident,
/// module-qualified at the definition site (a thin macro cannot resolve the
/// importer's module at expansion).
#[must_use]
pub fn emit_import(ident: &str, imports: &[syn::Path]) -> TokenStream {
    let mangled = mangle(ident);
    let edge_ident = format_ident!("__LEAF_IMPORT_{}", mangled);
    let refs_ident = format_ident!("__leaf_import_refs_{}", mangled);

    let to_contracts = imports.iter().map(|p| {
        let name = p
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        // Each importee's contract is its module-qualified `module::Ident`. The
        // import edge references the IMPORTEE's own module via its path, so we mint
        // the contract from the path's leading-ident name (a stable cross-build key).
        quote! { ::leaf_core::ContractId::of(#name) }
    });

    // The anti-DCE force-reference: name each importee in a const item so the
    // importer path-references it (the importee can never be DCE'd away while the
    // importer is linked).
    let refs = imports.iter().enumerate().map(|(i, p)| {
        let r = format_ident!("__r{}", i);
        quote! { let #r: ::core::option::Option<&#p> = ::core::option::Option::None; let _ = #r; }
    });

    quote! {
        #[allow(non_upper_case_globals)]
        pub const #edge_ident: ::leaf_core::ImportEdge = ::leaf_core::ImportEdge {
            from: ::leaf_core::ContractId::of(
                ::core::concat!(::core::module_path!(), "::", #ident)
            ),
            to: &[ #(#to_contracts),* ],
        };
        #[allow(non_snake_case)]
        #[allow(dead_code, clippy::let_unit_value)]
        fn #refs_ident() {
            #(#refs)*
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a `TokenStream` to a whitespace-collapsed string so assertions are
    /// robust against `quote!`'s token spacing.
    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn cond(src: &str) -> CondExpr {
        parse_conditional(syn::parse_str(src).expect("tokens")).expect("parses")
    }

    // ── on_property → a Runtime-tier Leaf ──────────────────────────────────────

    #[test]
    fn on_property_lowers_to_a_runtime_tier_condexpr_leaf() {
        // The headline case: #[conditional(on_property("k", having_value = "v"))]
        // lowers to one const ::leaf_core::CondExpr::Leaf carrying the OnProperty
        // ConditionId + its attrs, via absolute ::leaf_core paths.
        let expr = cond(r#"on_property("app.enabled", having_value = "true")"#);
        let s = flat(&expr.lower());
        assert!(s.contains("::leaf_core::CondExpr::Leaf"), "got: {s}");
        // The id is minted through contract_hash over the canonical kind path (the
        // SAME minting the runtime catalog uses — so the leaf resolves at runtime).
        assert!(
            s.contains(r#"::leaf_core::ConditionId(::leaf_core::contract_hash("leaf::condition::OnProperty")asu32)"#),
            "got: {s}"
        );
        // The property name (under the `"name"` key the runtime OnProperty reads) and
        // having_value lower to const ::leaf_core::Attr rows.
        assert!(s.contains(r#"::leaf_core::Attr::Str("name","app.enabled")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::Attr::Str("having_value","true")"#), "got: {s}");
    }

    #[test]
    fn on_property_match_if_missing_bool_attr() {
        let expr = cond(r#"on_property("k", match_if_missing = false)"#);
        let s = flat(&expr.lower());
        assert!(s.contains(r#"::leaf_core::Attr::Bool("match_if_missing",false)"#), "got: {s}");
    }

    #[test]
    fn on_property_requires_a_key() {
        let err = parse_conditional(syn::parse_str(r#"on_property(having_value = "x")"#).unwrap())
            .expect_err("a property leaf needs a key");
        assert!(err.message.contains("property key"), "got: {}", err.message);
    }

    // ── on_bean → a typed Leaf ─────────────────────────────────────────────────

    #[test]
    fn on_bean_carries_the_type_as_a_typeid_attr() {
        let expr = cond("on_bean(MyService)");
        let s = flat(&expr.lower());
        assert!(
            s.contains(r#"::leaf_core::ConditionId(::leaf_core::contract_hash("leaf::condition::OnBean")asu32)"#),
            "got: {s}"
        );
        // The bean type lowers to a const TypeId-of seam carried as an Attr::Type.
        assert!(
            s.contains(r#"::leaf_core::Attr::Type("type",const{::core::any::TypeId::of::<MyService>()})"#),
            "got: {s}"
        );
    }

    #[test]
    fn on_missing_bean_mints_its_own_kind_id() {
        let expr = cond("on_missing_bean(DataSource)");
        let s = flat(&expr.lower());
        assert!(
            s.contains(r#"::leaf_core::contract_hash("leaf::condition::OnMissingBean")"#),
            "got: {s}"
        );
    }

    // ── boolean composition: all/any/not are first-class nodes ─────────────────

    #[test]
    fn all_any_not_are_first_class_boolean_nodes() {
        let expr = cond(r#"all(on_property("x"), any(on_bean(Foo), not(on_class("redis"))))"#);
        let s = flat(&expr.lower());
        assert!(s.contains("::leaf_core::CondExpr::All(&["), "got: {s}");
        assert!(s.contains("::leaf_core::CondExpr::Any(&["), "got: {s}");
        assert!(s.contains("::leaf_core::CondExpr::Not(&"), "got: {s}");
        // The whole tree must be a valid Rust expression.
        syn::parse2::<syn::Expr>(expr.lower()).expect("a valid const expr");
    }

    #[test]
    fn not_requires_exactly_one_child() {
        let err = parse_conditional(syn::parse_str(r#"not(on_property("a"), on_property("b"))"#).unwrap())
            .expect_err("not takes one child");
        assert!(err.message.contains("exactly one"), "got: {}", err.message);
    }

    #[test]
    fn multiple_top_level_conditions_are_implicitly_anded() {
        // Spring stacks multiple @Conditional as AND; the comma-separated form ANDs.
        let expr = cond(r#"on_property("a"), on_bean(Foo)"#);
        match &expr {
            CondExpr::All(children) => assert_eq!(children.len(), 2),
            other => panic!("expected an All, got {other:?}"),
        }
    }

    #[test]
    fn empty_conditional_is_unconditional_all() {
        let expr = parse_conditional(TokenStream::new()).expect("empty parses");
        assert_eq!(expr, CondExpr::All(Vec::new()));
    }

    #[test]
    fn bool_literal_lowers_to_a_const_node() {
        let expr = cond("true");
        assert_eq!(expr, CondExpr::Const(true));
        assert!(flat(&expr.lower()).contains("::leaf_core::CondExpr::Const(true)"), "got");
    }

    #[test]
    fn unknown_kind_is_a_hard_error() {
        let err = parse_conditional(syn::parse_str("on_quux(1)").unwrap())
            .expect_err("unknown kind errors");
        assert!(err.message.contains("unknown"), "got: {}", err.message);
    }

    // ── the CONDITIONS anti-DCE anchor + guard pairing const ───────────────────

    #[test]
    fn emit_guard_emits_a_public_pairing_const_and_a_conditions_anchor() {
        let expr = cond(r#"on_property("app.enabled", having_value = "true")"#);
        let ts = emit_guard("MyBean", &expr);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The guard tree is a PUBLIC const under the deterministic pairing name so
        // the leaf-boot assembly pass can pair it with the element's Descriptor.
        assert!(
            s.contains("pubconst__leaf_guard_MyBean:::leaf_core::CondExpr"),
            "got: {s}"
        );
        // One CONDITIONS anti-DCE anchor per referenced kind, submitted into the
        // frozen ::leaf_core::CONDITIONS slice via the re-exported ::leaf_core::linkme
        // attr path + crate override.
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::CONDITIONS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::ConditionRow"), "got: {s}");
        // The guard is ALSO auto-collected into GUARD_PAIRINGS keyed by the gated
        // element's ContractId (the COMPONENTS auto-collect substrate, extended) so
        // leaf-boot's condition routing finds it with no hand-assembled `.with_guards`.
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::GUARD_PAIRINGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::GuardPairingRow{contract:"), "got: {s}");
        assert!(s.contains("guard:&__leaf_guard_MyBean"), "got: {s}");
    }

    #[test]
    fn emit_guard_emits_one_anchor_per_distinct_kind() {
        let expr = cond(r#"all(on_property("a"), on_property("b"), on_bean(Foo))"#);
        let s = flat(&emit_guard("E", &expr));
        // Two distinct kinds (OnProperty, OnBean) => two anchor rows.
        assert_eq!(s.matches("::leaf_core::ConditionRow{").count(), 2, "got: {s}");
    }

    // ── profiles ───────────────────────────────────────────────────────────────

    #[test]
    fn profile_string_parses_the_three_operator_algebra() {
        let expr = parse_profile("prod & (eu | us)").expect("parses");
        assert_eq!(
            expr,
            ProfileExpr::And(vec![
                ProfileExpr::Name("prod".into()),
                ProfileExpr::Or(vec![
                    ProfileExpr::Name("eu".into()),
                    ProfileExpr::Name("us".into()),
                ]),
            ])
        );
    }

    #[test]
    fn profile_negation_parses() {
        let expr = parse_profile("!test").expect("parses");
        assert_eq!(expr, ProfileExpr::Not(Box::new(ProfileExpr::Name("test".into()))));
    }

    #[test]
    fn profile_mixed_operators_without_parens_is_a_hard_error() {
        let err = parse_profile("a & b | c").expect_err("mixed operators error");
        assert!(err.message.contains("mixed"), "got: {}", err.message);
    }

    #[test]
    fn profile_expr_lowers_to_absolute_core_profile_expr() {
        let expr = parse_profile("prod & eu").expect("parses");
        let s = flat(&expr.lower());
        assert!(s.contains("::leaf_core::ProfileExpr::And(&["), "got: {s}");
        assert!(s.contains(r#"::leaf_core::ProfileExpr::Name("prod")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::ProfileExpr::Name("eu")"#), "got: {s}");
        syn::parse2::<syn::Expr>(expr.lower()).expect("a valid const expr");
    }

    #[test]
    fn parse_profile_attr_reads_a_string_literal() {
        let attr: TokenStream = syn::parse_str(r#""prod | staging""#).expect("tokens");
        let expr = parse_profile_attr(attr).expect("parses");
        assert_eq!(
            expr,
            ProfileExpr::Or(vec![
                ProfileExpr::Name("prod".into()),
                ProfileExpr::Name("staging".into()),
            ])
        );
    }

    #[test]
    fn empty_profile_attr_is_a_hard_error() {
        let err = parse_profile_attr(TokenStream::new()).expect_err("empty profile errors");
        assert!(err.message.contains("requires"), "got: {}", err.message);
    }

    #[test]
    fn profile_lowers_to_a_single_on_profile_cond_leaf() {
        // Profiles are a PRESET: the whole expression rides ONE ON_PROFILE leaf
        // whose attr carries the rendered expression string.
        let expr = parse_profile("prod & eu").expect("parses");
        let cond = profile_to_cond(&expr);
        let s = flat(&cond.lower());
        assert!(
            s.contains(r#"::leaf_core::contract_hash("leaf::condition::OnProfile")"#),
            "got: {s}"
        );
        // The runtime OnProfile reads the rendered expression from the `"expr"` attr
        // (NOT `"profiles"` — a prior mismatch made every #[profile] guard vacuously
        // active until the guard verdict was enforced).
        assert!(s.contains(r#"::leaf_core::Attr::Str("expr","prod&eu")"#), "got: {s}");
    }

    #[test]
    fn profile_render_round_trips_through_the_grammar() {
        // The rendered string must re-parse to the same expression (so the runtime
        // OnProfile leaf re-parses it identically via accepts_profiles).
        let expr = parse_profile("prod & (eu | us)").expect("parses");
        let cond = profile_to_cond(&expr);
        let rendered = match &cond {
            CondExpr::Leaf { attrs, .. } => match &attrs[0] {
                CondAttr::Str(_, v) => v.clone(),
                _ => panic!("expected a str attr"),
            },
            _ => panic!("expected a leaf"),
        };
        let reparsed = parse_profile(&rendered).expect("the rendered form re-parses");
        assert_eq!(reparsed, expr, "render must round-trip: {rendered}");
    }

    // ── #[auto_config] ─────────────────────────────────────────────────────────

    fn item(src: &str) -> syn::ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    #[test]
    fn auto_config_targets_the_auto_configs_slice_at_fallback() {
        // The headline: #[auto_config] emits the SAME const Descriptor into the
        // SEPARATE AUTO_CONFIGS slice at CandidateRole::FALLBACK (a user bean wins).
        let ts = emit_auto_config(&item("struct RedisAutoConfig;"), None).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]"),
            "got: {s}"
        );
        // FALLBACK candidate role (the auto_config_role()) rides the meta.
        assert!(s.contains("::leaf_core::CandidateRole::FALLBACK"), "got: {s}");
        // It must NOT land in the COMPONENTS channel.
        assert!(
            !s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "an auto-config must not be a COMPONENTS row: {s}"
        );
    }

    #[test]
    fn auto_config_with_a_guard_emits_the_guard_and_conditions_anchor() {
        // An #[auto_config] gated by #[conditional(on_property(...))] emits BOTH the
        // AUTO_CONFIGS Fallback row AND the guard (its CONDITIONS anchor + pairing const).
        let guard = cond(r#"on_property("redis.enabled", having_value = "true")"#);
        let ts = emit_auto_config(&item("struct RedisAutoConfig;"), Some(&guard)).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::AUTO_CONFIGS)]"),
            "got: {s}"
        );
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::CONDITIONS)]"),
            "got: {s}"
        );
        assert!(
            s.contains("pubconst__leaf_guard_RedisAutoConfig:::leaf_core::CondExpr"),
            "got: {s}"
        );
    }

    #[test]
    fn auto_config_rejects_a_generic_target() {
        let err = emit_auto_config(&item("struct Generic<T> { inner: T }"), None)
            .expect_err("a generic auto-config is a hard error");
        assert!(err.message.contains("register_component!") || err.message.contains("generic"), "got: {}", err.message);
    }

    // ── #[import] ──────────────────────────────────────────────────────────────

    #[test]
    fn import_parses_a_list_of_types() {
        let attr: TokenStream = syn::parse_str("Foo, bar::Baz").expect("tokens");
        let paths = parse_import(attr).expect("parses");
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn empty_import_is_a_hard_error() {
        let err = parse_import(TokenStream::new()).expect_err("empty import errors");
        assert!(err.message.contains("requires"), "got: {}", err.message);
    }

    #[test]
    fn import_emits_an_import_edge_and_an_anti_dce_reference() {
        let paths = parse_import(syn::parse_str("RedisAutoConfig, CacheAutoConfig").unwrap()).unwrap();
        let ts = emit_import("MyApp", &paths);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // One const ImportEdge (from = the importer, to[] = the importees).
        assert!(s.contains("pubconst__LEAF_IMPORT_MyApp:::leaf_core::ImportEdge"), "got: {s}");
        assert!(s.contains("::leaf_core::ImportEdge{"), "got: {s}");
        // The from contract is module-qualified at the definition site.
        assert!(
            s.contains(r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","MyApp"))"#),
            "got: {s}"
        );
        // The importee contracts ride the `to[]` array.
        assert!(s.contains(r#"::leaf_core::ContractId::of("RedisAutoConfig")"#), "got: {s}");
        // The anti-DCE force-reference names each importee so it cannot be DCE'd.
        assert!(s.contains("&RedisAutoConfig"), "got: {s}");
        assert!(s.contains("&CacheAutoConfig"), "got: {s}");
    }
}
