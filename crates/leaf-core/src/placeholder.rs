//! `${...}` placeholder resolution + escaping + the `${}`/`#{}` dispatch AST.
//!
//! Realizes environment-config `property-resolution` and binding-conversion
//! `extra-12`/`extra-13`: a hand-rolled, escape-aware, recursive char-scanner
//! parameterized by a borrowed, sealed [`PlaceholderSyntax`], plus the const
//! [`Segment`] dispatch AST and the one [`interpret`] ordering function.
//!
//! The scanner replicates Spring's `PropertyPlaceholderHelper`:
//! - scan for the `prefix` (`${`), match the NESTING `suffix` (`}`);
//! - recurse-on-key first (`${${meta}}` resolves the inner key first), look the
//!   resolved key up, then recurse-on-value (a resolved value is re-scanned);
//! - split on the first UNESCAPED `value_separator` (`:`) for `${key:default}`;
//! - a visited-key set + an explicit depth cap break cycles.
//!
//! Escaping (extra-13): at the `escape` char, if the next chars are the prefix
//! OR (inside a body) the separator OR the escape itself, the escape is consumed
//! and the token emitted LITERALLY (`\${` → literal `${`). The output is a
//! [`Cow`] — `Borrowed` when no placeholder and no escape fired (the zero-alloc
//! fast path), allocating only on actual expansion/de-escaping.
//!
//! Strict vs lenient is the two-method split: [`resolve_lenient`] leaves an
//! unresolved mandatory placeholder LITERAL; [`resolve_strict`] returns an
//! [`ErrorKind::UnresolvedValue`] [`LeafError`].

use std::borrow::Cow;

use crate::error::{Cause, ErrorKind, LeafError};
use crate::expr::ExpressionEvaluator;

/// The default recursion/visited depth cap (environment-config knob, default 64).
pub const DEFAULT_DEPTH_CAP: usize = 64;

/// The borrowed, sealed placeholder grammar (extra-13).
///
/// Built once at seal from the stack (`leaf.placeholder.*` keys) and frozen into
/// `EnvCore` — never a runtime-mutable global. The default is Spring's
/// `${key:default}` with `\` escaping.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlaceholderSyntax {
    /// The opening delimiter (`${`).
    pub prefix: Cow<'static, str>,
    /// The closing delimiter (`}`).
    pub suffix: Cow<'static, str>,
    /// The default-value separator (`Some(":")`); `None` disables defaulting.
    pub value_separator: Option<Cow<'static, str>>,
    /// The escape char (`Some('\\')`); `None` disables escaping.
    pub escape: Option<char>,
}

impl Default for PlaceholderSyntax {
    fn default() -> Self {
        PlaceholderSyntax {
            prefix: Cow::Borrowed("${"),
            suffix: Cow::Borrowed("}"),
            value_separator: Some(Cow::Borrowed(":")),
            escape: Some('\\'),
        }
    }
}

impl PlaceholderSyntax {
    /// The Spring-default grammar (`${key:default}`, `\` escape).
    #[must_use]
    pub fn spring() -> Self {
        PlaceholderSyntax::default()
    }

    /// A grammar with escaping disabled.
    #[must_use]
    pub fn without_escape(mut self) -> Self {
        self.escape = None;
        self
    }

    /// A grammar with default-value splitting disabled.
    #[must_use]
    pub fn without_default_separator(mut self) -> Self {
        self.value_separator = None;
        self
    }

    /// Reconfigure prefix/suffix (e.g. `%{`/`}`).
    #[must_use]
    pub fn with_delimiters(
        mut self,
        prefix: impl Into<Cow<'static, str>>,
        suffix: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.prefix = prefix.into();
        self.suffix = suffix.into();
        self
    }
}

/// Strictness for an unresolved mandatory placeholder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Leave an unresolved `${...}` literal in the output.
    Lenient,
    /// Error on an unresolved mandatory `${...}`.
    Strict,
}

/// Resolve `${...}` placeholders in `text` LENIENTLY (unresolved left literal).
///
/// `lookup(key)` returns the raw value for a resolved key (or `None`). Returns a
/// `Cow::Borrowed` when nothing fired (zero-alloc fast path).
#[must_use]
pub fn resolve_lenient<'a>(
    text: &'a str,
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Cow<'a, str> {
    match scan(text, syntax, lookup, Mode::Lenient, DEFAULT_DEPTH_CAP, &mut Vec::new()) {
        Ok(Resolved::Borrowed) => Cow::Borrowed(text),
        Ok(Resolved::Owned(s)) => Cow::Owned(s),
        // Lenient never errors on an unresolved key, but a cycle/depth blowout
        // still does; fall back to the literal text on that pathological case.
        Err(_) => Cow::Borrowed(text),
    }
}

/// Resolve `${...}` placeholders in `text` STRICTLY.
///
/// # Errors
/// [`ErrorKind::UnresolvedValue`] if a mandatory `${...}` (without a default)
/// cannot be resolved, or on a placeholder cycle / depth-cap blowout.
pub fn resolve_strict(
    text: &str,
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, LeafError> {
    match scan(text, syntax, lookup, Mode::Strict, DEFAULT_DEPTH_CAP, &mut Vec::new())? {
        Resolved::Borrowed => Ok(text.to_string()),
        Resolved::Owned(s) => Ok(s),
    }
}

/// The internal scan result: `Borrowed` means nothing changed (fast path).
enum Resolved {
    Borrowed,
    Owned(String),
}

/// The escape-aware recursive scanner (extra-13 + property-resolution).
fn scan(
    text: &str,
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
    mode: Mode,
    depth: usize,
    visited: &mut Vec<String>,
) -> Result<Resolved, LeafError> {
    if depth == 0 {
        return Err(LeafError::new(ErrorKind::UnresolvedValue).caused_by(Cause::plain(
            "resolving placeholders",
            "placeholder recursion depth cap exceeded",
        )));
    }
    let prefix = syntax.prefix.as_ref();
    let suffix = syntax.suffix.as_ref();
    let bytes = text.as_bytes();

    // Fast path: if neither the prefix nor an escape char appears, borrow.
    let has_escape_char =
        syntax.escape.is_some_and(|e| text.contains(e));
    if !text.contains(prefix) && !has_escape_char {
        return Ok(Resolved::Borrowed);
    }

    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        // ── escape handling (de-escape BEFORE the placeholder-open decision) ──
        if let Some(esc) = syntax.escape
            && text[i..].starts_with(esc)
        {
            let esc_len = esc.len_utf8();
            let after = &text[i + esc_len..];
            if after.starts_with(prefix) {
                // `\${` => literal prefix.
                out.push_str(prefix);
                i += esc_len + prefix.len();
                changed = true;
                continue;
            } else if after.starts_with(esc) {
                // `\\` => literal escape char.
                out.push(esc);
                i += esc_len + esc_len;
                changed = true;
                continue;
            } else if let Some(sep) = syntax.value_separator.as_ref()
                && after.starts_with(sep.as_ref())
            {
                // `\:` inside a body => literal separator.
                out.push_str(sep.as_ref());
                i += esc_len + sep.len();
                changed = true;
                continue;
            }
            // The escape did not "bite" a special token: emit it verbatim.
            out.push(esc);
            i += esc_len;
            continue;
        }

        // ── placeholder open ──
        if text[i..].starts_with(prefix) {
            let body_start = i + prefix.len();
            let Some(close_rel) = find_matching_suffix(&text[body_start..], prefix, suffix) else {
                // No matching suffix: emit the prefix verbatim and move on.
                out.push_str(prefix);
                i = body_start;
                continue;
            };
            let body = &text[body_start..body_start + close_rel];
            let resolved = resolve_one(body, syntax, lookup, mode, depth, visited)?;
            match resolved {
                Some(value) => out.push_str(&value),
                None => {
                    // Lenient + unresolved-without-default: leave literal.
                    out.push_str(prefix);
                    out.push_str(body);
                    out.push_str(suffix);
                }
            }
            changed = true;
            i = body_start + close_rel + suffix.len();
            continue;
        }

        // ── ordinary char ──
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    if changed {
        Ok(Resolved::Owned(out))
    } else {
        Ok(Resolved::Borrowed)
    }
}

/// Resolve ONE placeholder body (`key` or `key:default`), returning the resolved
/// string, or `None` when lenient-unresolved-without-default.
fn resolve_one(
    body: &str,
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
    mode: Mode,
    depth: usize,
    visited: &mut Vec<String>,
) -> Result<Option<String>, LeafError> {
    // Split on the first UNESCAPED separator for `key:default` BEFORE any
    // de-escaping, so an escaped separator inside the key (`sub\://host`) stays
    // part of the key and is not mistaken for the default split.
    let (raw_key, raw_default) = split_key_default(body, syntax);

    // Recurse-on-key (so `${${meta}}` resolves the inner key) AND de-escape the
    // key's own escaped separators (`\:` => `:`).
    let key_owned = match scan(raw_key, syntax, lookup, mode, depth - 1, visited)? {
        Resolved::Borrowed => raw_key.to_string(),
        Resolved::Owned(s) => s,
    };
    let key = key_owned.trim();
    let default = raw_default;

    // Cycle detection: a key resolving (transitively) to itself.
    if visited.iter().any(|k| k == key) {
        return Err(LeafError::new(ErrorKind::UnresolvedValue).caused_by(Cause::plain(
            "resolving placeholder",
            format!("circular placeholder reference involving `{key}`"),
        )));
    }

    if let Some(raw) = lookup(key) {
        // Recurse-on-value: a resolved value is re-scanned with the same syntax.
        visited.push(key.to_string());
        let value = match scan(&raw, syntax, lookup, mode, depth - 1, visited)? {
            Resolved::Borrowed => raw,
            Resolved::Owned(s) => s,
        };
        visited.pop();
        return Ok(Some(value));
    }

    // Not found: use the default if present (recurse into it).
    if let Some(def) = default {
        let value = match scan(def, syntax, lookup, mode, depth - 1, visited)? {
            Resolved::Borrowed => def.to_string(),
            Resolved::Owned(s) => s,
        };
        return Ok(Some(value));
    }

    // No value, no default.
    match mode {
        Mode::Lenient => Ok(None),
        Mode::Strict => Err(LeafError::new(ErrorKind::UnresolvedValue).caused_by(Cause::plain(
            "resolving placeholder",
            format!("could not resolve placeholder `{key}`"),
        ))),
    }
}

/// Find the matching suffix index in `s` (relative to its start), honoring
/// NESTED `prefix`/`suffix` pairs so `${a${b}}` matches the OUTER `}`.
fn find_matching_suffix(s: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(prefix) {
            depth += 1;
            i += prefix.len();
        } else if s[i..].starts_with(suffix) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
            i += suffix.len();
        } else {
            let ch = s[i..].chars().next().unwrap();
            i += ch.len_utf8();
        }
    }
    None
}

/// Split a placeholder body into `(key, Some(default))` at the first UNESCAPED
/// separator, or `(body, None)` if there is no separator / it is disabled.
fn split_key_default<'a>(
    body: &'a str,
    syntax: &PlaceholderSyntax,
) -> (&'a str, Option<&'a str>) {
    let Some(sep) = syntax.value_separator.as_ref() else {
        return (body, None);
    };
    let sep = sep.as_ref();
    if sep.is_empty() {
        return (body, None);
    }
    // Find the first separator that is not escaped.
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find(sep) {
        let at = search_from + rel;
        let escaped = syntax.escape.is_some_and(|e| {
            // Preceded by an (unescaped) escape char.
            at >= e.len_utf8() && body[..at].ends_with(e)
        });
        if escaped {
            search_from = at + sep.len();
            continue;
        }
        return (&body[..at], Some(&body[at + sep.len()..]));
    }
    (body, None)
}

// ───────────────────────── the ${}/#{} dispatch AST (extra-12) ──────────────

/// A const segment of a value template (extra-12 dispatch grammar).
///
/// The `#[value]` macro splits a literal once at compile time into a const
/// `&'static [Segment]`; the SAME grammar is used for a runtime string. PHASE 1
/// expands every [`Segment::Placeholder`] via property-resolution; PHASE 2 (only
/// if any [`Segment::Expr`] is present) hands the result to the expr evaluator.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    /// A literal run.
    Literal(Cow<'static, str>),
    /// A `${key}` / `${key:default}` placeholder.
    Placeholder {
        /// The (raw) key.
        key: Cow<'static, str>,
        /// The default template, if any (its own sub-segments).
        default: Option<Cow<'static, str>>,
    },
    /// A `#{...}` expression body (opaque; evaluated in phase 2).
    Expr(Cow<'static, str>),
}

/// `true` iff `segments` contains a [`Segment::Expr`] (gates phase 2).
#[must_use]
pub fn has_expr(segments: &[Segment]) -> bool {
    segments.iter().any(|s| matches!(s, Segment::Expr(_)))
}

/// The one ordering interpreter (extra-12) WITHOUT an expression engine: PHASE 1
/// expands `${}` over the env; a template containing a [`Segment::Expr`] yields a
/// canonical [`ErrorKind::UnresolvedValue`] error so the absent-engine case is
/// loud, never a silent literal.
///
/// This is exactly [`interpret_with`] with no evaluator. The `#{}`-evaluating
/// callers (`@Value`, the value dispatcher) call [`interpret_with`] threading the
/// expr unit's `Option<&dyn ExpressionEvaluator>`.
///
/// # Errors
/// [`ErrorKind::UnresolvedValue`] for an unresolved strict placeholder or for a
/// `#{}` segment (no evaluator is wired through this entry point).
pub fn interpret(
    segments: &[Segment],
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
    strict: bool,
) -> Result<String, LeafError> {
    interpret_with(segments, syntax, lookup, strict, None)
}

/// The one ordering interpreter (extra-12) threading the OPTIONAL expression
/// evaluator (binding-conversion phase3/07): PHASE 1 expands every `${}` over the
/// env; PHASE 2 (only for [`Segment::Expr`]) hands the body to `expr`.
///
/// The two-phase law is encoded HERE in exactly one place: `${}` resolution
/// (placeholder strict/lenient) runs first; a `#{...}` body is then evaluated by
/// the threaded [`ExpressionEvaluator`]. When `expr` is `None`, a `#{}` segment is
/// the canonical loud [`ErrorKind::UnresolvedValue`] — never a silent literal.
/// Once phase 2 begins, its output is NOT re-scanned for `${}` (non-recursion
/// across the seam — the evaluator owns its own `#root` context).
///
/// # Errors
/// [`ErrorKind::UnresolvedValue`] for an unresolved strict placeholder or for a
/// `#{}` segment when `expr` is `None`; the evaluator's own [`LeafError`] on an
/// expression fault.
pub fn interpret_with(
    segments: &[Segment],
    syntax: &PlaceholderSyntax,
    lookup: &dyn Fn(&str) -> Option<String>,
    strict: bool,
    expr: Option<&dyn ExpressionEvaluator>,
) -> Result<String, LeafError> {
    let mut out = String::new();
    for seg in segments {
        match seg {
            Segment::Literal(s) => out.push_str(s),
            Segment::Placeholder { key, default } => {
                if let Some(v) = lookup(key.trim()) {
                    // Recurse-on-value through the scanner.
                    let v = if strict {
                        resolve_strict(&v, syntax, lookup)?
                    } else {
                        resolve_lenient(&v, syntax, lookup).into_owned()
                    };
                    out.push_str(&v);
                } else if let Some(def) = default {
                    let v = if strict {
                        resolve_strict(def, syntax, lookup)?
                    } else {
                        resolve_lenient(def, syntax, lookup).into_owned()
                    };
                    out.push_str(&v);
                } else if strict {
                    return Err(LeafError::new(ErrorKind::UnresolvedValue).caused_by(
                        Cause::plain(
                            "interpreting value template",
                            format!("could not resolve placeholder `{key}`"),
                        ),
                    ));
                } else {
                    // Lenient: leave the literal `${key}`.
                    out.push_str(syntax.prefix.as_ref());
                    out.push_str(key);
                    out.push_str(syntax.suffix.as_ref());
                }
            }
            Segment::Expr(body) => match expr {
                // PHASE 2: hand the #{} body to the (opaque) expression engine.
                Some(evaluator) => out.push_str(&evaluator.eval(body)?),
                // No engine wired: the absent-engine case is loud, never silent.
                None => {
                    return Err(LeafError::new(ErrorKind::UnresolvedValue).caused_by(
                        Cause::plain(
                            "interpreting value template",
                            format!(
                                "expression segment `#{{{body}}}` requires an expression \
                                 evaluator (none wired)"
                            ),
                        ),
                    ));
                }
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    // ── basic resolution ────────────────────────────────────────────────────

    #[test]
    fn resolves_a_simple_placeholder() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("name", "world")]);
        assert_eq!(resolve_lenient("hello ${name}", &syn, &l), "hello world");
    }

    #[test]
    fn no_placeholder_borrows_unchanged() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        assert!(matches!(resolve_lenient("plain text", &syn, &l), Cow::Borrowed(_)));
    }

    #[test]
    fn default_value_used_when_key_absent() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        assert_eq!(resolve_lenient("${port:8080}", &syn, &l), "8080");
        // Present key wins over default.
        let l2 = map_lookup(&[("port", "9000")]);
        assert_eq!(resolve_lenient("${port:8080}", &syn, &l2), "9000");
    }

    #[test]
    fn nested_placeholder_in_key_resolves_inner_first() {
        let syn = PlaceholderSyntax::spring();
        // ${${meta}} -> meta=real -> ${real} -> value
        let l = map_lookup(&[("meta", "real"), ("real", "value")]);
        assert_eq!(resolve_lenient("${${meta}}", &syn, &l), "value");
    }

    #[test]
    fn resolved_value_is_rescanned() {
        let syn = PlaceholderSyntax::spring();
        // a -> ${b}, b -> deep
        let l = map_lookup(&[("a", "${b}"), ("b", "deep")]);
        assert_eq!(resolve_lenient("${a}", &syn, &l), "deep");
    }

    // ── strict vs lenient ───────────────────────────────────────────────────

    #[test]
    fn lenient_leaves_unresolved_literal_strict_errors() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        assert_eq!(resolve_lenient("${missing}", &syn, &l), "${missing}");
        let err = resolve_strict("${missing}", &syn, &l).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnresolvedValue);
    }

    #[test]
    fn strict_succeeds_with_default() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        assert_eq!(resolve_strict("${x:fallback}", &syn, &l).unwrap(), "fallback");
    }

    // ── escaping (extra-13) ─────────────────────────────────────────────────

    #[test]
    fn escaped_prefix_is_literal_not_a_placeholder() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("name", "X")]);
        // \${name} => literal ${name}
        assert_eq!(resolve_lenient(r"\${name}", &syn, &l), "${name}");
    }

    #[test]
    fn escaped_separator_inside_body_is_not_a_default_split() {
        let syn = PlaceholderSyntax::spring();
        // key is `sub://host` — the first `:` is escaped so it is part of the key.
        let l = map_lookup(&[("sub://host", "ok")]);
        assert_eq!(resolve_lenient(r"${sub\://host}", &syn, &l), "ok");
    }

    #[test]
    fn double_escape_is_a_literal_escape_char() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        assert_eq!(resolve_lenient(r"a\\b", &syn, &l), r"a\b");
    }

    #[test]
    fn escape_disabled_keeps_backslash_verbatim() {
        let syn = PlaceholderSyntax::spring().without_escape();
        let l = map_lookup(&[("name", "X")]);
        // With escaping off, \${name} expands the placeholder, keeping the slash.
        assert_eq!(resolve_lenient(r"\${name}", &syn, &l), r"\X");
    }

    #[test]
    fn custom_delimiters_work_via_substring_compare() {
        let syn = PlaceholderSyntax::spring().with_delimiters("%{", "}");
        let l = map_lookup(&[("k", "v")]);
        assert_eq!(resolve_lenient("%{k}", &syn, &l), "v");
        // The old `${...}` is now inert.
        assert_eq!(resolve_lenient("${k}", &syn, &l), "${k}");
    }

    // ── cycle + depth ───────────────────────────────────────────────────────

    #[test]
    fn circular_reference_is_caught_strict() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("a", "${b}"), ("b", "${a}")]);
        let err = resolve_strict("${a}", &syn, &l).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnresolvedValue);
    }

    // ── interpret + dispatch AST (extra-12) ─────────────────────────────────

    #[test]
    fn interpret_expands_placeholder_segments() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("host", "localhost")]);
        let segs = vec![
            Segment::Literal("http://".into()),
            Segment::Placeholder {
                key: "host".into(),
                default: None,
            },
            Segment::Literal(":8080".into()),
        ];
        assert_eq!(interpret(&segs, &syn, &l, true).unwrap(), "http://localhost:8080");
    }

    #[test]
    fn interpret_uses_placeholder_default() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        let segs = vec![Segment::Placeholder {
            key: "port".into(),
            default: Some("8080".into()),
        }];
        assert_eq!(interpret(&segs, &syn, &l, true).unwrap(), "8080");
    }

    #[test]
    fn interpret_expr_without_engine_is_a_loud_error() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        let segs = vec![Segment::Expr("1 + 1".into())];
        assert!(has_expr(&segs));
        let err = interpret(&segs, &syn, &l, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnresolvedValue);
    }

    // ── #{} expression evaluation via the threaded evaluator (closure 3) ──

    // A toy evaluator that uppercases the body (proving the seam is wired —
    // the real engine is leaf-codegen's #{...} lowerer, opaque here).
    struct UpcaseEval;
    impl crate::expr::ExpressionEvaluator for UpcaseEval {
        fn eval(&self, body: &str) -> Result<String, LeafError> {
            Ok(body.to_uppercase())
        }
    }

    #[test]
    fn interpret_with_evaluator_evaluates_an_expr_segment() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        let segs = vec![
            Segment::Literal("v=".into()),
            Segment::Expr("hi".into()),
        ];
        let eval = UpcaseEval;
        // Threading an evaluator turns the #{} segment from a loud error into
        // the evaluated value.
        let out = interpret_with(&segs, &syn, &l, true, Some(&eval)).unwrap();
        assert_eq!(out, "v=HI");
    }

    #[test]
    fn interpret_with_no_evaluator_still_errors_loudly_on_expr() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        let segs = vec![Segment::Expr("1 + 1".into())];
        let err = interpret_with(&segs, &syn, &l, true, None).unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnresolvedValue);
    }

    #[test]
    fn interpret_with_runs_phase1_placeholders_before_phase2_expr() {
        // PHASE 1: ${} expands first; PHASE 2: the #{} body is handed to the
        // evaluator (the body itself is NOT placeholder-expanded here — the
        // evaluator owns its own #root context).
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("host", "localhost")]);
        let segs = vec![
            Segment::Placeholder { key: "host".into(), default: None },
            Segment::Literal("/".into()),
            Segment::Expr("path".into()),
        ];
        let eval = UpcaseEval;
        let out = interpret_with(&segs, &syn, &l, true, Some(&eval)).unwrap();
        assert_eq!(out, "localhost/PATH");
    }

    #[test]
    fn interpret_delegates_to_interpret_with_none_evaluator() {
        // The original interpret() is exactly interpret_with(.., None): the
        // #{} arm stays a loud error, preserving the prior contract.
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[("host", "h")]);
        let segs = vec![Segment::Placeholder { key: "host".into(), default: None }];
        assert_eq!(
            interpret(&segs, &syn, &l, true).unwrap(),
            interpret_with(&segs, &syn, &l, true, None).unwrap()
        );
    }

    #[test]
    fn interpret_strict_missing_placeholder_errors() {
        let syn = PlaceholderSyntax::spring();
        let l = map_lookup(&[]);
        let segs = vec![Segment::Placeholder {
            key: "missing".into(),
            default: None,
        }];
        assert_eq!(
            interpret(&segs, &syn, &l, true).unwrap_err().kind,
            ErrorKind::UnresolvedValue
        );
        // Lenient leaves the literal.
        assert_eq!(interpret(&segs, &syn, &l, false).unwrap(), "${missing}");
    }
}
