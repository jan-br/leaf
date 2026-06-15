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
//! The full SpEL-analogue `#{...}` form rides the optional
//! [`ExpressionEvaluator`](leaf_core::ExpressionEvaluator) borrow now carried on
//! the `ConditionCtx` (`ctx.expr`): a whole-string `#{...}` body is evaluated
//! through it and its result coerced to truthiness. The scaffolding + the
//! `None`-degradation are in place; it only becomes USEFUL once leaf-boot
//! installs a concrete evaluator (a later ecosystem step). Until then a `#{...}`
//! body with no evaluator degrades to an honest non-match — never a false pass.

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
        let trimmed = raw.trim();

        // `#{...}` form: full expression evaluation rides `ctx.expr`. When an
        // evaluator is installed, evaluate the inner body and coerce truthiness;
        // when it is absent, degrade to an honest non-match (never a false pass).
        if let Some(body) = hash_body(trimmed) {
            let Some(evaluator) = ctx.expr else {
                return ConditionOutcome::new(
                    false,
                    ReasonMsg {
                        kind: "OnExpression",
                        expected: Some("truthy".to_string()),
                        found: Some("no expression evaluator installed".to_string()),
                        gate: None,
                    },
                );
            };
            return match evaluator.eval(body) {
                Ok(value) => ConditionOutcome::new(
                    is_truthy(value.trim()),
                    ReasonMsg {
                        kind: "OnExpression",
                        expected: Some("truthy".to_string()),
                        found: Some(value),
                        gate: None,
                    },
                ),
                Err(e) => ConditionOutcome::new(
                    false,
                    ReasonMsg {
                        kind: "OnExpression",
                        expected: Some("truthy".to_string()),
                        found: Some(format!("eval error: {e}")),
                        gate: None,
                    },
                ),
            };
        }

        // `${...}` placeholder fast-path: resolve against the sealed Env (lenient).
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

/// The inner body of a whole-string `#{...}` expression, or `None` if `s` is not
/// exactly one `#{...}` form (a placeholder `${...}` / bare literal stays on the
/// `Env` fast-path).
fn hash_body(s: &str) -> Option<&str> {
    s.strip_prefix("#{").and_then(|rest| rest.strip_suffix('}'))
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
    use leaf_core::{Attr, ExpressionEvaluator, LeafError};

    // A toy evaluator: it returns the body verbatim, so `#{true}` evaluates to
    // "true" (truthy) and `#{false}` to "false" (falsy). Mirrors the cfg(test)
    // `UpcaseEval` seam-prover in leaf-core's placeholder module.
    struct EchoEval;
    impl ExpressionEvaluator for EchoEval {
        fn eval(&self, body: &str) -> Result<String, LeafError> {
            Ok(body.to_string())
        }
    }

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

    #[test]
    fn hash_expression_evaluates_through_the_evaluator_when_present() {
        let env = env_with(&[]);
        let eval = EchoEval;
        let (ctx, _s) = ctx_over(&env);
        let ctx = ctx.with_expr(&eval);
        let truthy: AttrSlice = &[Attr::Str(EXPR, "#{true}")];
        assert!(ON_EXPRESSION.matches(&ctx, &truthy).matched);

        let (ctx, _s) = ctx_over(&env);
        let ctx = ctx.with_expr(&eval);
        let falsy: AttrSlice = &[Attr::Str(EXPR, "#{false}")];
        assert!(!ON_EXPRESSION.matches(&ctx, &falsy).matched);
    }

    #[test]
    fn hash_expression_without_an_evaluator_is_an_honest_non_match() {
        // No `ctx.expr` installed: a `#{...}` body cannot be decided, so it
        // degrades to a non-match (never a false positive).
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(EXPR, "#{true}")];
        assert!(!ON_EXPRESSION.matches(&ctx, &attrs).matched);
    }
}
