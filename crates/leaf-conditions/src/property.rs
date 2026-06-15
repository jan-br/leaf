//! `OnProperty` / `OnBooleanProperty` — (Runtime, Parse) Env-reading leaves.
//!
//! conditions-autoconfig (phase3/05) condition-family: OnProperty multi-name
//! semantics = ALL listed names must pass; `having_value` is the expected value
//! (default: present-and-not-"false"); `match_if_missing` (default false)
//! decides an absent key; keys read through the `Env`'s relaxed-binding view.
//! `OnBooleanProperty` is OnProperty pinned to `having_value = "true"`.

use leaf_core::{AttrSlice, Condition, ConditionCtx, ConditionOutcome, PropertyResolver, ReasonMsg};

use crate::attrs;
use crate::kinds::{kind_id, OnBooleanProperty, OnProperty};

/// The shared attribute keys for the property family.
pub const NAME: &str = "name";
const PREFIX: &str = "prefix";
const HAVING_VALUE: &str = "having_value";
const MATCH_IF_MISSING: &str = "match_if_missing";

/// The runtime `OnProperty` impl.
pub struct OnPropertyCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_PROPERTY: OnPropertyCondition = OnPropertyCondition;

impl Condition for OnPropertyCondition {
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        eval_property(ctx, attrs, None, "OnProperty")
    }
}

/// The runtime `OnBooleanProperty` impl: OnProperty pinned to `"true"`.
pub struct OnBooleanPropertyCondition;

/// The singleton row pointer for the `CONDITIONS` slice.
pub static ON_BOOLEAN_PROPERTY: OnBooleanPropertyCondition = OnBooleanPropertyCondition;

impl Condition for OnBooleanPropertyCondition {
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome {
        eval_property(ctx, attrs, Some("true"), "OnBooleanProperty")
    }
}

/// Shared property-matching core. `forced_value` pins `having_value` (the boolean
/// variant); otherwise `having_value` is read from the attrs.
fn eval_property(
    ctx: &ConditionCtx<'_>,
    attrs: &AttrSlice,
    forced_value: Option<&str>,
    kind: &'static str,
) -> ConditionOutcome {
    let prefix = attrs::str_of(attrs, PREFIX).unwrap_or("");
    let having = forced_value.or_else(|| attrs::str_of(attrs, HAVING_VALUE));
    let match_if_missing = attrs::bool_or(attrs, MATCH_IF_MISSING, false);
    let names = attrs::all_str(attrs, NAME);

    if names.is_empty() {
        // No names declared: nothing to gate on — vacuously matches.
        return ConditionOutcome::new(true, ReasonMsg::of(kind));
    }

    for name in &names {
        let full = if prefix.is_empty() {
            (*name).to_string()
        } else {
            format!("{prefix}.{name}")
        };
        match ctx.env.get(&full) {
            None => {
                if !match_if_missing {
                    return ConditionOutcome::new(
                        false,
                        ReasonMsg {
                            kind,
                            expected: having.map(str::to_string).or(Some("present".to_string())),
                            found: Some("absent".to_string()),
                            gate: None,
                        },
                    );
                }
                // match_if_missing = true: an absent key passes.
            }
            Some(value) => {
                let raw = value.raw;
                let ok = match having {
                    // Default semantics (Spring): present and NOT literally "false".
                    None => !raw.eq_ignore_ascii_case("false"),
                    Some(expected) => raw == expected,
                };
                if !ok {
                    return ConditionOutcome::new(
                        false,
                        ReasonMsg {
                            kind,
                            expected: Some(having.unwrap_or("not false").to_string()),
                            found: Some(raw),
                            gate: None,
                        },
                    );
                }
            }
        }
    }

    ConditionOutcome::new(
        true,
        ReasonMsg {
            kind,
            expected: having.map(str::to_string),
            found: None,
            gate: None,
        },
    )
}

// ── ConditionKind tier-map rows (Runtime, Parse) are declared in `kinds`. ──
// Compile-assert the kind ids match the slice singletons by re-export shape.
const _: fn() = || {
    let _ = kind_id::<OnProperty>;
    let _ = kind_id::<OnBooleanProperty>;
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx_over, env_with};
    use leaf_core::Attr;

    #[test]
    fn present_truthy_property_matches() {
        let env = env_with(&[("leaf.feature.enabled", "true")]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(NAME, "leaf.feature.enabled")];
        assert!(ON_PROPERTY.matches(&ctx, &attrs).matched);
    }

    #[test]
    fn property_set_to_false_does_not_match_by_default() {
        let env = env_with(&[("leaf.feature.enabled", "false")]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(NAME, "leaf.feature.enabled")];
        let out = ON_PROPERTY.matches(&ctx, &attrs);
        assert!(!out.matched);
        assert_eq!(out.reason.found.as_deref(), Some("false"));
    }

    #[test]
    fn absent_property_does_not_match_unless_match_if_missing() {
        let env = env_with(&[]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(NAME, "leaf.feature.enabled")];
        assert!(!ON_PROPERTY.matches(&ctx, &attrs).matched);

        let attrs2: AttrSlice = &[
            Attr::Str(NAME, "leaf.feature.enabled"),
            Attr::Bool(MATCH_IF_MISSING, true),
        ];
        assert!(ON_PROPERTY.matches(&ctx, &attrs2).matched);
    }

    #[test]
    fn having_value_must_match_exactly() {
        let env = env_with(&[("mode", "fast")]);
        let (ctx, _s) = ctx_over(&env);
        let ok: AttrSlice = &[Attr::Str(NAME, "mode"), Attr::Str(HAVING_VALUE, "fast")];
        let no: AttrSlice = &[Attr::Str(NAME, "mode"), Attr::Str(HAVING_VALUE, "slow")];
        assert!(ON_PROPERTY.matches(&ctx, &ok).matched);
        assert!(!ON_PROPERTY.matches(&ctx, &no).matched);
    }

    #[test]
    fn prefix_is_joined_with_each_name() {
        let env = env_with(&[("leaf.redis.url", "redis://x")]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(PREFIX, "leaf.redis"), Attr::Str(NAME, "url")];
        assert!(ON_PROPERTY.matches(&ctx, &attrs).matched);
    }

    #[test]
    fn all_names_must_pass() {
        let env = env_with(&[("a", "true")]);
        let (ctx, _s) = ctx_over(&env);
        let attrs: AttrSlice = &[Attr::Str(NAME, "a"), Attr::Str(NAME, "b")];
        assert!(!ON_PROPERTY.matches(&ctx, &attrs).matched, "b is absent");
    }

    #[test]
    fn boolean_property_pins_true() {
        let env = env_with(&[("flag", "true"), ("flag2", "yes")]);
        let (ctx, _s) = ctx_over(&env);
        let yes: AttrSlice = &[Attr::Str(NAME, "flag")];
        let no: AttrSlice = &[Attr::Str(NAME, "flag2")];
        assert!(ON_BOOLEAN_PROPERTY.matches(&ctx, &yes).matched);
        assert!(
            !ON_BOOLEAN_PROPERTY.matches(&ctx, &no).matched,
            "`yes` is not literally `true`"
        );
    }
}
