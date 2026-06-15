//! The event-listener codegen the thin `#[event_listener]` /
//! `#[transactional_event_listener]` macros call (events phase3/12).
//!
//! A listener method lowers to ONE flat const identity row in the frozen
//! `EVENT_LISTENERS` `linkme` slice (`EventListenerRow { contract, order }`) PLUS a
//! public pairing const carrying the dispatch metadata the leaf-boot events pass
//! binds to a live [`leaf_core::ListenerDescriptor`] at refresh: the dispatch
//! `order`, the defer/transactional `phase`, and the `condition` slot (present-or-
//! absent flag — the actual `CondExprFn` is the expr-backend's concern). The live
//! `ListenerDescriptor` also needs the event `TypeId` and the erased `adapter` fn
//! (which downcast the live host `Arc` + the event payload and invoke the typed
//! body); those bind to the RESOLVED host at refresh, so this unit emits the
//! anti-DCE identity row + the const dispatch metadata and the host/adapter binding
//! is the events pass's concern (NOTE below).
//!
//! Every emitted path is ABSOLUTE `::leaf_core::…` (the thin-macro rule, charter
//! §2.10). A `#[transactional_event_listener]` is structurally an `#[event_listener]`
//! that DEFERS to a transaction-synchronization [`leaf_core::TxPhase`]; a plain
//! `#[event_listener]` fires inline (no defer).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

/// The dispatch-time deferral of a listener — inline (the default) or deferred to a
/// transaction-synchronization [`leaf_core::TxPhase`]
/// (`#[transactional_event_listener(phase = "after_commit")]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Defer {
    /// Fire inline at publish time (a plain `#[event_listener]`).
    #[default]
    Inline,
    /// Before the commit `.await` (a failure can veto the commit).
    BeforeCommit,
    /// After a successful commit (the transactional default).
    AfterCommit,
    /// After a rollback.
    AfterRollback,
    /// After completion, regardless of outcome.
    AfterCompletion,
}

impl Defer {
    /// Parse a transactional `phase = "…"` value to a [`Defer`].
    ///
    /// # Errors
    /// [`EmitError`] on an unrecognised phase.
    pub fn parse(value: &str) -> Result<Self, EmitError> {
        match value {
            "before_commit" => Ok(Defer::BeforeCommit),
            "after_commit" => Ok(Defer::AfterCommit),
            "after_rollback" => Ok(Defer::AfterRollback),
            "after_completion" => Ok(Defer::AfterCompletion),
            other => Err(EmitError {
                message: format!(
                    "unknown transactional phase `{other}` (expected \
                     before_commit/after_commit/after_rollback/after_completion)"
                ),
            }),
        }
    }

    /// The const dispatch-metadata token for this deferral: `None` for inline, or
    /// `Some(::leaf_core::TxPhase::…)` for a transactional phase.
    #[must_use]
    pub fn tokens(self) -> TokenStream {
        let phase = match self {
            Defer::Inline => return quote! { ::core::option::Option::None },
            Defer::BeforeCommit => quote! { ::leaf_core::TxPhase::BeforeCommit },
            Defer::AfterCommit => quote! { ::leaf_core::TxPhase::AfterCommit },
            Defer::AfterRollback => quote! { ::leaf_core::TxPhase::AfterRollback },
            Defer::AfterCompletion => quote! { ::leaf_core::TxPhase::AfterCompletion },
        };
        quote! { ::core::option::Option::Some(#phase) }
    }
}

/// The parsed `#[event_listener]` / `#[transactional_event_listener]` attribute
/// arguments: an optional explicit `order`, the (transactional) defer `phase`, and
/// whether a `condition = "…"` guard was declared.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListenerArgs {
    /// An explicit integer dispatch order; `None` uses the implicit floor.
    pub order: Option<i32>,
    /// The defer/transactional phase (`Inline` for a plain listener).
    pub defer: Defer,
    /// `true` iff a `condition = "…"` SpEL-style guard was declared.
    pub has_condition: bool,
}

/// Parse the `#[event_listener(order = N, condition = "…")]` /
/// `#[transactional_event_listener(phase = "after_commit", …)]` attribute body.
///
/// `transactional` flags the transactional form (so a `phase` is allowed and the
/// default defer is `AfterCommit`); a plain `#[event_listener]` rejects `phase`.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, a non-integer `order`, a
/// `phase` on a non-transactional listener, or an unrecognised phase.
pub fn parse_listener_args(
    attr: TokenStream,
    transactional: bool,
) -> Result<ListenerArgs, EmitError> {
    let mut args = ListenerArgs {
        // The transactional default fires after a successful commit.
        defer: if transactional { Defer::AfterCommit } else { Defer::Inline },
        ..ListenerArgs::default()
    };
    if attr.is_empty() {
        return Ok(args);
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[event_listener] arguments: {e}"),
    })?;
    for expr in &exprs {
        let syn::Expr::Assign(assign) = expr else {
            return Err(EmitError {
                message: format!(
                    "#[event_listener] arguments must be `key = value` pairs, got `{}`",
                    quote! { #expr }
                ),
            });
        };
        let key = assign_ident(&assign.left)?;
        match key.as_str() {
            "order" => args.order = Some(int_value(&assign.right)?),
            "condition" => {
                // The condition is a string SpEL-style guard; we only record its
                // PRESENCE here (the CondExprFn binding is the expr-backend's job).
                let _ = str_value(&assign.right).ok_or_else(|| EmitError {
                    message: "`condition` must be a string expression".into(),
                })?;
                args.has_condition = true;
            }
            "phase" => {
                if !transactional {
                    return Err(EmitError {
                        message: "`phase` is only valid on #[transactional_event_listener]".into(),
                    });
                }
                let value = str_value(&assign.right).ok_or_else(|| EmitError {
                    message: "`phase` must be a string".into(),
                })?;
                args.defer = Defer::parse(&value)?;
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown #[event_listener] argument `{other}` \
                         (expected `order`/`condition`/`phase`)"
                    ),
                });
            }
        }
    }
    Ok(args)
}

/// Emit the const listener artifact for one listener element `ident`: the minimal
/// anti-DCE `::leaf_core::EventListenerRow { contract, order }` submitted into the
/// frozen `EVENT_LISTENERS` slice, plus a PUBLIC pairing const carrying the dispatch
/// metadata (order + defer phase + condition-presence) the leaf-boot events pass
/// binds to a live `ListenerDescriptor` (named `__leaf_listener_<Ident>`).
///
/// `ident` is the listener host bean ident (the pairing key).
#[must_use]
pub fn emit_listener(ident: &str, args: &ListenerArgs) -> TokenStream {
    let mangled = mangle(ident);
    let row_ident = format_ident!("__LEAF_LISTENER_{}", mangled);
    let order_ident = format_ident!("__leaf_listener_order_{}", mangled);
    let phase_ident = format_ident!("__leaf_listener_phase_{}", mangled);
    let cond_ident = format_ident!("__leaf_listener_has_condition_{}", mangled);

    let order = match args.order {
        Some(n) => quote! {
            ::leaf_core::OrderKey { value: #n, source: ::leaf_core::OrderSource::Annotation }
        },
        None => quote! { ::leaf_core::OrderKey::implicit() },
    };
    let phase = args.defer.tokens();
    let has_condition = args.has_condition;
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };

    quote! {
        // The PUBLIC dispatch-metadata pairing consts: the leaf-boot events pass
        // reads these beside the row to build the live ListenerDescriptor (whose
        // event TypeId + erased adapter bind to the RESOLVED host at refresh — they
        // are NOT const-constructible at macro time).
        // NOTE (cross-crate): the frozen `ListenerDescriptor.adapter`/`event_type`
        // bind to the live host `Arc` at refresh, so the macro emits the anti-DCE
        // identity row + this const dispatch metadata; the host/adapter binding is
        // the leaf-boot events pass's concern (the same Descriptor→seed pairing
        // pattern as `__leaf_seed_<Ident>`).
        #[allow(non_upper_case_globals)]
        pub const #order_ident: ::leaf_core::OrderKey = #order;
        #[allow(non_upper_case_globals)]
        pub const #phase_ident: ::core::option::Option<::leaf_core::TxPhase> = #phase;
        #[allow(non_upper_case_globals)]
        pub const #cond_ident: bool = #has_condition;
        // The minimal anti-DCE listener IDENTITY row in the frozen EVENT_LISTENERS
        // slice (a dropped listener silently never fires — the expected-vs-found
        // self-check catches it). Dispatch ORDER is read via the one cmp_order,
        // never this slice's link order.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::EVENT_LISTENERS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::EventListenerRow = ::leaf_core::EventListenerRow {
            contract: #contract,
            order: #order_ident,
        };
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

/// The string value of a `key = "literal"` right-hand side, if it is a string lit.
fn str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
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
    fn emits_an_absolute_core_event_listener_row_into_the_slice() {
        // The headline: a listener lowers to one const ::leaf_core::EventListenerRow
        // submitted into the frozen EVENT_LISTENERS slice via the re-exported
        // ::leaf_core::linkme attr path + crate override, the SLICE absolute
        // ::leaf_core::EVENT_LISTENERS.
        let ts = emit_listener("OnUserCreated", &ListenerArgs::default());
        syn::parse2::<syn::File>(ts.clone()).expect("valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::EVENT_LISTENERS)]"),
            "got: {s}"
        );
        assert!(s.contains("#[linkme(crate=::leaf_core::linkme)]"), "got: {s}");
        assert!(s.contains("::leaf_core::EventListenerRow{"), "got: {s}");
    }

    #[test]
    fn listener_contract_is_module_qualified_at_the_definition_site() {
        let s = flat(&emit_listener("OnUserCreated", &ListenerArgs::default()));
        assert!(
            s.contains(
                r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","OnUserCreated"))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn a_plain_listener_defers_to_none_inline() {
        // A plain #[event_listener] fires inline: the phase pairing const is None.
        let s = flat(&emit_listener("L", &ListenerArgs::default()));
        assert!(
            s.contains(
                "pubconst__leaf_listener_phase_L:::core::option::Option<::leaf_core::TxPhase>=::core::option::Option::None"
            ),
            "got: {s}"
        );
    }

    #[test]
    fn a_transactional_listener_carries_its_tx_phase() {
        // A #[transactional_event_listener(phase = "after_commit")] defers to the
        // AfterCommit TxPhase in its dispatch-metadata pairing const.
        let args =
            parse_listener_args(syn::parse_str(r#"phase = "after_commit""#).unwrap(), true)
                .expect("parses");
        assert_eq!(args.defer, Defer::AfterCommit);
        let s = flat(&emit_listener("OnOrder", &args));
        assert!(
            s.contains("::core::option::Option::Some(::leaf_core::TxPhase::AfterCommit)"),
            "got: {s}"
        );
    }

    #[test]
    fn the_transactional_default_phase_is_after_commit() {
        let args = parse_listener_args(TokenStream::new(), true).expect("parses");
        assert_eq!(args.defer, Defer::AfterCommit);
    }

    #[test]
    fn a_condition_guard_sets_the_condition_slot_flag() {
        let args =
            parse_listener_args(syn::parse_str(r#"condition = "event.active""#).unwrap(), false)
                .expect("parses");
        assert!(args.has_condition);
        let s = flat(&emit_listener("L", &args));
        assert!(
            s.contains("pubconst__leaf_listener_has_condition_L:bool=true"),
            "got: {s}"
        );
    }

    #[test]
    fn an_explicit_order_rides_the_pairing_const() {
        let args =
            parse_listener_args(syn::parse_str("order = 10").unwrap(), false).expect("parses");
        assert_eq!(args.order, Some(10));
        let s = flat(&emit_listener("L", &args));
        assert!(s.contains("value:10i32"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Annotation"), "got: {s}");
    }

    #[test]
    fn phase_on_a_plain_listener_is_a_hard_error() {
        let err = parse_listener_args(syn::parse_str(r#"phase = "after_commit""#).unwrap(), false)
            .expect_err("phase needs the transactional form");
        assert!(
            err.message.contains("transactional_event_listener"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn an_unknown_phase_is_a_hard_error() {
        let err = parse_listener_args(syn::parse_str(r#"phase = "during_commit""#).unwrap(), true)
            .expect_err("unknown phase errors");
        assert!(err.message.contains("unknown transactional phase"), "got: {}", err.message);
    }

    #[test]
    fn an_unknown_listener_arg_is_a_hard_error() {
        let err = parse_listener_args(syn::parse_str("bogus = 1").unwrap(), false)
            .expect_err("unknown arg errors");
        assert!(err.message.contains("unknown #[event_listener] argument"), "got: {}", err.message);
    }
}
