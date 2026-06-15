//! The built-in constraint checkers (validation, phase3/09 §validation).
//!
//! Each constraint is a plain `fn(&T, &Params) -> Option<Violation>` over an
//! already-converted value — the "compiled-in, path-referenced checker" model the
//! design pins (R1): a constraint is NAMED at the derive/check site, never
//! runtime-discovered, and is generics-friendly (monomorphized per use). The
//! `#[derive(Validate)]` derive emits calls to exactly these fns (a hand-written
//! `impl ValidateInto` calls the same fns directly via the [`Cascade`](crate::Cascade)
//! helpers in [`crate::cascade`]).
//!
//! A constraint returns `Some(Violation)` on FAILURE (collect-all: the caller pushes
//! it into the [`ValidationContext`](leaf_core::ValidationContext) and keeps going — never fail-first) and `None`
//! when the value satisfies the constraint. The [`Violation`] stores a stable
//! `constraint_id` ([`ContractId`]), a `message_key` + `params` (resolved LATER at
//! error-render time against messages-i18n + the ambient locale — the sync validate
//! hot path never touches a bean), and the rendered `rejected` value.

use leaf_core::{ContractId, Violation};

/// The stable id of the `not_empty` constraint.
#[must_use]
pub fn not_empty_id() -> ContractId {
    ContractId::of("leaf::validation::NotEmpty")
}

/// The stable id of the `min` (numeric lower-bound) constraint.
#[must_use]
pub fn min_id() -> ContractId {
    ContractId::of("leaf::validation::Min")
}

/// The stable id of the `max` (numeric upper-bound) constraint.
#[must_use]
pub fn max_id() -> ContractId {
    ContractId::of("leaf::validation::Max")
}

/// The stable id of the `range` (numeric closed-interval) constraint.
#[must_use]
pub fn range_id() -> ContractId {
    ContractId::of("leaf::validation::Range")
}

/// The stable id of the `email` constraint.
#[must_use]
pub fn email_id() -> ContractId {
    ContractId::of("leaf::validation::Email")
}

/// The stable id of the `pattern` constraint.
#[must_use]
pub fn pattern_id() -> ContractId {
    ContractId::of("leaf::validation::Pattern")
}

fn violation(
    constraint_id: ContractId,
    message_key: &'static str,
    params: Vec<(&'static str, String)>,
    rejected: String,
) -> Violation {
    Violation {
        path: String::new(),
        constraint_id,
        message_key,
        params: params.into_boxed_slice(),
        rejected,
    }
}

/// `@NotEmpty` — the string (after trimming) must be non-empty. A blank/empty
/// string is a violation (Spring's `@NotBlank` intent — trims whitespace).
#[must_use]
pub fn not_empty(value: &str) -> Option<Violation> {
    if value.trim().is_empty() {
        Some(violation(not_empty_id(), "validation.not_empty", vec![], render_str(value)))
    } else {
        None
    }
}

/// `@Min(min)` — the numeric value must be `>= min`.
#[must_use]
pub fn min(value: i64, lower: i64) -> Option<Violation> {
    if value < lower {
        Some(violation(
            min_id(),
            "validation.min",
            vec![("min", lower.to_string())],
            value.to_string(),
        ))
    } else {
        None
    }
}

/// `@Max(max)` — the numeric value must be `<= max`.
#[must_use]
pub fn max(value: i64, upper: i64) -> Option<Violation> {
    if value > upper {
        Some(violation(
            max_id(),
            "validation.max",
            vec![("max", upper.to_string())],
            value.to_string(),
        ))
    } else {
        None
    }
}

/// `@Range(min, max)` — the numeric value must be in the closed interval
/// `[min, max]` (a fused lower+upper bound; one violation carrying both params).
#[must_use]
pub fn range(value: i64, lower: i64, upper: i64) -> Option<Violation> {
    if value < lower || value > upper {
        Some(violation(
            range_id(),
            "validation.range",
            vec![("min", lower.to_string()), ("max", upper.to_string())],
            value.to_string(),
        ))
    } else {
        None
    }
}

/// `@Email` — a deliberately conservative structural check (NOT a full RFC-5322
/// validator): exactly one `@`, a non-empty local part, and a domain with at least
/// one dot and no leading/trailing dot. (The escape hatch for stricter rules is a
/// `with = fn` custom checker — phase3/09 §validation.)
#[must_use]
pub fn email(value: &str) -> Option<Violation> {
    if is_email(value) {
        None
    } else {
        Some(violation(email_id(), "validation.email", vec![], render_str(value)))
    }
}

fn is_email(value: &str) -> bool {
    let mut parts = value.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false; // zero or more than one '@'
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    // The domain must contain a dot, with no leading/trailing/empty label.
    if domain.starts_with('.') || domain.ends_with('.') || !domain.contains('.') {
        return false;
    }
    !domain.split('.').any(str::is_empty)
}

/// `@Pattern(glob)` — a minimal glob match (`*` = any run, `?` = one char). NOT a
/// regex engine (leaf-core carries no regex dep); the design's `pattern` constraint
/// over the compile-everything derive is a structural shape check, with `with = fn`
/// as the full-regex escape hatch.
#[must_use]
pub fn pattern(value: &str, glob: &'static str) -> Option<Violation> {
    if glob_matches(glob, value) {
        None
    } else {
        Some(violation(
            pattern_id(),
            "validation.pattern",
            vec![("pattern", glob.to_string())],
            render_str(value),
        ))
    }
}

/// A tiny recursive glob matcher (`*` = any run incl. empty, `?` = exactly one).
fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_rec(&p, &t)
}

fn glob_rec(p: &[char], t: &[char]) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some((&'*', rest)) => {
            // '*' matches zero-or-more: try consuming none, then one more char.
            glob_rec(rest, t) || (!t.is_empty() && glob_rec(p, &t[1..]))
        }
        Some((&'?', rest)) => !t.is_empty() && glob_rec(rest, &t[1..]),
        Some((&c, rest)) => t.first() == Some(&c) && glob_rec(rest, &t[1..]),
    }
}

fn render_str(value: &str) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_empty_rejects_blank_and_passes_filled() {
        assert!(not_empty("").is_some(), "empty is a violation");
        assert!(not_empty("   ").is_some(), "whitespace-only is a violation");
        assert!(not_empty("ok").is_none(), "a filled string passes");
        let v = not_empty("").unwrap();
        assert_eq!(v.constraint_id, not_empty_id());
        assert_eq!(v.message_key, "validation.not_empty");
    }

    #[test]
    fn min_enforces_lower_bound() {
        assert!(min(4, 5).is_some(), "below min is a violation");
        assert!(min(5, 5).is_none(), "exactly min passes");
        assert!(min(6, 5).is_none(), "above min passes");
        let v = min(4, 5).unwrap();
        assert_eq!(v.constraint_id, min_id());
        assert_eq!(&*v.params, &[("min", "5".to_string())]);
        assert_eq!(v.rejected, "4");
    }

    #[test]
    fn max_enforces_upper_bound() {
        assert!(max(11, 10).is_some());
        assert!(max(10, 10).is_none());
        assert!(max(9, 10).is_none());
        assert_eq!(max(11, 10).unwrap().constraint_id, max_id());
    }

    #[test]
    fn range_enforces_a_closed_interval() {
        assert!(range(0, 1, 10).is_some(), "below range");
        assert!(range(11, 1, 10).is_some(), "above range");
        assert!(range(1, 1, 10).is_none(), "lower edge passes");
        assert!(range(10, 1, 10).is_none(), "upper edge passes");
        let v = range(0, 1, 10).unwrap();
        assert_eq!(v.constraint_id, range_id());
        assert_eq!(&*v.params, &[("min", "1".to_string()), ("max", "10".to_string())]);
    }

    #[test]
    fn email_accepts_well_formed_and_rejects_malformed() {
        assert!(email("a@b.com").is_none());
        assert!(email("jan.brachthaeuser@gmail.com").is_none());
        assert!(email("").is_some(), "empty");
        assert!(email("no-at-sign").is_some());
        assert!(email("a@b@c.com").is_some(), "two @");
        assert!(email("a@nodot").is_some(), "no dot in domain");
        assert!(email("@b.com").is_some(), "empty local");
        assert!(email("a@.com").is_some(), "leading dot");
        assert!(email("a@b.").is_some(), "trailing dot");
        assert_eq!(email("bad").unwrap().constraint_id, email_id());
    }

    #[test]
    fn pattern_globs() {
        assert!(pattern("abc", "a*c").is_none());
        assert!(pattern("ac", "a*c").is_none(), "* matches empty");
        assert!(pattern("axyzc", "a*c").is_none());
        assert!(pattern("abc", "a?c").is_none());
        assert!(pattern("abbc", "a?c").is_some(), "? is exactly one");
        assert!(pattern("xyz", "a*").is_some());
        assert_eq!(pattern("xyz", "a*").unwrap().constraint_id, pattern_id());
        assert_eq!(
            &*pattern("xyz", "a*").unwrap().params,
            &[("pattern", "a*".to_string())]
        );
    }
}
