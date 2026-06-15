//! `OnExpression` — (Runtime, Parse) expression-gated leaf.
//!
//! conditions-autoconfig (phase3/05) condition-family: `OnExpression` is a
//! (Runtime, Parse) member that evaluates a boolean expression against the sealed
//! `Env`. leaf-core's expression backend is closure-only (`CondExprFn` const fn
//! pointers minted at macro time); a `#[conditional(on_expression(..))]` lowers
//! to one such fn. This runtime impl handles the placeholder-expression form
//! `"${some.flag}"` — the overwhelmingly common `@ConditionalOnExpression` shape
//! — by resolving `${...}` against the `Env` and reading the result's truthiness.
//!
//! NOTE: the full SpEL-analogue arbitrary-expression engine is leaf-aop-expr's
//! `ExpressionEvaluator`; wiring a `CondExprFn` through the `ConditionCtx` is
//! deferred to leaf-boot (the ctx does not yet carry an evaluator borrow). The
//! placeholder form covers the property-driven gating this crate is responsible
//! for without pulling the expression engine into the conditions DAG layer.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, PropertyResolver, ReasonMsg};

use crate::attrs;

const EXPR: &str = "expr";

/// The runtime `OnExpression` impl.
pub struct OnExpressionCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_EXPRESSION: OnExpressionCondition = OnExpressionCondition;

impl Condition for OnExpressionCondition {
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        let Some(raw) = attrs::str_of(attrs, EXPR) else {
            return ConditionOutcome::new(false, ReasonMsg::of("OnExpression"));
        };
        // Resolve any `${...}` placeholders against the sealed Env (lenient).
        let resolved = ctx.env.resolve_placeholders(raw);
        let matched = is_truthy(resolved.trim());
        ConditionOutcome::new(
            matched,
            ReasonMsg {
                kind: "OnExpression",
                expected: Some("truthy".to_string()),
                found: Some(resolved.into_owned()),
                gate: None,
            },
        )
    }
}

/// Boolean coercion: `true`/`1`/`yes`/`on` (case-insensitive) are truthy; an
/// unresolved/empty/`false`/`0`/`no`/`off` value is falsy.
fn is_truthy(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;

    #[test]
    fn truthy_literal_matches() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "true")];
        assert!(ON_EXPRESSION.matches(&ctx, &attrs).matched);
        let no: AttrSlice = &[Attr::Str(EXPR, "false")];
        assert!(!ON_EXPRESSION.matches(&ctx, &no).matched);
    }

    #[test]
    fn placeholder_expression_resolves_against_env() {
        let env = env_with(&[("feature.x", "on")]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "${feature.x}")];
        assert!(ON_EXPRESSION.matches(&ctx, &attrs).matched);
    }

    #[test]
    fn unresolved_placeholder_is_falsy() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "${missing.flag}")];
        assert!(!ON_EXPRESSION.matches(&ctx, &attrs).matched);
    }
}
