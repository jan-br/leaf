//! Aggregating accumulated [`Violation`]s into the ONE [`LeafError`] spine
//! (validation, phase3/09 §validation + §error-model).
//!
//! Validation is collect-all: a [`ValidationContext`] holds EVERY violation found
//! across a typed boundary. When the boundary fails (the method-validation
//! short-circuit, or a config-bind node), the whole set is folded into a single
//! `ErrorKind::ValidationError` [`LeafError`] whose causal chain carries one
//! [`Cause`] per violation (path + constraint + rejected value) — so the caller sees
//! the AGGREGATED report, not the first failure. Messages stay UNRESOLVED here (the
//! `message_key` + `params` ride in the cause detail); i18n rendering against the
//! ambient locale is the error-render-time concern (the sync validate path never
//! touches a bean).

use leaf_core::{Cause, ErrorKind, LeafError, Violation};

/// `true` iff the slice carries at least one violation (the boundary failed).
#[must_use]
pub fn has_violations(violations: &[Violation]) -> bool {
    !violations.is_empty()
}

/// Render one [`Violation`] as a human-legible detail line:
/// `"<path>: <message_key> [k=v, …] (rejected: <value>)"` — the unresolved form
/// (the `message_key`/params resolve LATER against messages-i18n).
#[must_use]
pub fn render_violation(v: &Violation) -> String {
    let mut s = String::new();
    if v.path.is_empty() {
        s.push_str("<value>");
    } else {
        s.push_str(&v.path);
    }
    s.push_str(": ");
    s.push_str(v.message_key);
    if !v.params.is_empty() {
        let params: Vec<String> =
            v.params.iter().map(|(k, val)| format!("{k}={val}")).collect();
        s.push_str(" [");
        s.push_str(&params.join(", "));
        s.push(']');
    }
    s.push_str(" (rejected: ");
    s.push_str(&v.rejected);
    s.push(')');
    s
}

/// Fold a set of [`Violation`]s into ONE aggregated `ValidationError`
/// [`LeafError`], one [`Cause`] node per violation (path + rendered detail).
///
/// `context` names WHAT was being validated (e.g. `"validating method arguments"`
/// or `"binding @ConfigurationProperties app.*"`) — it is the `what` of each cause
/// node. Returns `None` when there are no violations (the boundary passed).
#[must_use]
pub fn aggregate(context: &'static str, violations: &[Violation]) -> Option<LeafError> {
    if violations.is_empty() {
        return None;
    }
    let mut err = LeafError::new(ErrorKind::ValidationError);
    for v in violations {
        err = err.caused_by(Cause::plain(context, render_violation(v)));
    }
    Some(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::ContractId;

    fn v(path: &str, key: &'static str, params: Vec<(&'static str, String)>, rejected: &str) -> Violation {
        Violation {
            path: path.to_string(),
            constraint_id: ContractId::of("leaf::validation::Test"),
            message_key: key,
            params: params.into_boxed_slice(),
            rejected: rejected.to_string(),
        }
    }

    #[test]
    fn no_violations_aggregates_to_none() {
        assert!(aggregate("ctx", &[]).is_none(), "a clean validation is not an error");
        assert!(!has_violations(&[]));
    }

    #[test]
    fn violations_aggregate_into_one_validation_error_with_a_cause_per_violation() {
        let vs = vec![
            v("name", "validation.not_empty", vec![], ""),
            v("age", "validation.range", vec![("min", "0".into()), ("max", "150".into())], "200"),
        ];
        assert!(has_violations(&vs));
        let err = aggregate("validating method arguments", &vs).expect("an aggregated error");
        assert_eq!(err.kind, ErrorKind::ValidationError, "the one validation kind");
        assert_eq!(err.chain.len(), 2, "one cause node per violation (aggregated, not first-fail)");
        // The detail carries the path + key + params + rejected value (unresolved).
        let details: Vec<String> = err.chain.iter().map(|c| c.detail.to_string()).collect();
        assert!(details.iter().any(|d| d.contains("name: validation.not_empty")));
        assert!(
            details.iter().any(|d| d.contains("age: validation.range [min=0, max=150] (rejected: 200)")),
            "params + rejected value render in the cause detail"
        );
    }

    #[test]
    fn a_pathless_violation_renders_as_value() {
        let err = aggregate("ctx", &[v("", "validation.email", vec![], "bad")]).unwrap();
        assert!(err.chain[0].detail.to_string().contains("<value>: validation.email"));
    }
}
