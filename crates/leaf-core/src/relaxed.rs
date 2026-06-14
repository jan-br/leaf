//! Relaxed-binding canonical key identity (`[relaxed-binding]`).
//!
//! The ONE owner of configuration-key identity consumed by the placeholder walk,
//! the binder tree-descent, `OnProperty` conditions, and config-metadata's
//! advertised form (binding-conversion phase3/07). There is exactly ONE
//! normalization rule so the asymmetric-placeholder contract cannot diverge
//! across the lookup paths.
//!
//! [`CanonicalName`] is the parsed, segment-structured key in kebab canonical
//! form (`my.app.db-pool[0].user-name`). [`CanonicalName::uniform`] is the
//! equivalence fold — lowercase, with non-alphanumeric separators dropped —
//! that maps the many Spring "relaxed" spellings of a property (`DB_POOL_SIZE`,
//! `dbPoolSize`, `db.pool-size`, `db_pool_size`) onto ONE identity.
//!
//! Two reconciled lookup paths under one rule:
//! - an ENUMERABLE source (a parsed file/JSON/env snapshot) is indexed once at
//!   seal by [`uniform_key`] so a relaxed `get` is one normalize + one map hit;
//! - a NON-enumerable source (raw env, `random.*`) is probed by the bounded
//!   [`env_var_candidates`] generator (≤ four whole-name forms — never a
//!   per-segment cartesian product), matching Spring's `SystemEnvironmentPropertySource`.
//!
//! Both paths share the one [`uniform_key`] fold so they can never disagree.

use std::borrow::Cow;
use std::fmt;

use crate::error::{Cause, ErrorKind, LeafError};

/// One segment of a parsed [`CanonicalName`].
///
/// A `Named` segment is a dotted property name component (normalized by the
/// uniform-form fold); an `Indexed` segment is a list `[n]` position; a `MapKey`
/// segment is a bracket-escaped key whose contents are preserved VERBATIM
/// (normalization is disabled inside the brackets, per the relaxed-binding
/// bracket-escape contract — `server.headers[Content-Type]`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Segment {
    /// A dotted name component (e.g. `db-pool-size`), uniform-normalizable.
    Named(Box<str>),
    /// A list index (`[0]`).
    Indexed(u32),
    /// A bracket-escaped map key — contents preserved verbatim, NOT normalized.
    MapKey(Box<str>),
}

impl Segment {
    /// The uniform-form fold of this single segment (lowercase + drop
    /// non-alphanumeric for a `Named`; verbatim-lowercased index/`MapKey`).
    fn write_uniform(&self, out: &mut String) {
        match self {
            Segment::Named(s) => fold_into(s, out),
            Segment::Indexed(n) => {
                use std::fmt::Write;
                let _ = write!(out, "{n}");
            }
            // A MapKey disables normalization for its contents, but the uniform
            // identity still lowercases so a relaxed lookup of the WHOLE name
            // stays case-tolerant outside the bracket. The contents are kept
            // verbatim (no separator dropping) per the bracket-escape contract.
            Segment::MapKey(s) => out.push_str(s),
        }
    }
}

/// A parsed configuration key in kebab canonical form.
///
/// `CanonicalName::parse` validates the surface syntax (dotted names, `[index]`
/// list positions, `[map-key]` bracket escapes) and yields the structured
/// segments the binder descends and the placeholder walk canonicalizes against.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CanonicalName {
    segments: Box<[Segment]>,
}

/// A parse error from [`CanonicalName::parse`] — a malformed key surface
/// (unbalanced bracket, empty segment, stray separator).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NameSyntaxError {
    /// The offending input.
    pub input: Box<str>,
    /// A short human reason.
    pub reason: &'static str,
}

impl fmt::Display for NameSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid config key `{}`: {}", self.input, self.reason)
    }
}

impl From<NameSyntaxError> for LeafError {
    fn from(e: NameSyntaxError) -> Self {
        LeafError::new(ErrorKind::BindError)
            .caused_by(Cause::plain("parsing config key", e.to_string()))
    }
}

impl CanonicalName {
    /// Parse a dotted/bracketed key into its canonical segments.
    ///
    /// Accepts `a.b-c`, `a.list[0]`, `a.map[Some.Key]` (bracket escape preserves
    /// the contents). A bracketed segment whose contents are all ASCII digits is
    /// an [`Segment::Indexed`]; otherwise it is a verbatim [`Segment::MapKey`].
    ///
    /// # Errors
    /// Returns [`NameSyntaxError`] on an unbalanced bracket, an empty name
    /// segment, or a stray separator.
    pub fn parse(s: &str) -> Result<Self, NameSyntaxError> {
        let mut segments: Vec<Segment> = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        // A name segment is accumulated until a `.` or `[`; a bracket runs to
        // the matching `]`.
        let mut cur = String::new();
        let err = |reason: &'static str| NameSyntaxError {
            input: s.into(),
            reason,
        };
        while i < bytes.len() {
            match bytes[i] {
                b'.' => {
                    if cur.is_empty() {
                        // A leading dot or `..` is only allowed if the previous
                        // token was a bracket (e.g. `list[0].name`).
                        if !matches!(segments.last(), Some(Segment::Indexed(_) | Segment::MapKey(_)))
                        {
                            return Err(err("empty name segment"));
                        }
                    } else {
                        segments.push(Segment::Named(cur.as_str().into()));
                        cur.clear();
                    }
                    i += 1;
                }
                b'[' => {
                    if !cur.is_empty() {
                        segments.push(Segment::Named(cur.as_str().into()));
                        cur.clear();
                    }
                    // Scan to the matching `]`.
                    let start = i + 1;
                    let mut j = start;
                    while j < bytes.len() && bytes[j] != b']' {
                        j += 1;
                    }
                    if j >= bytes.len() {
                        return Err(err("unterminated `[`"));
                    }
                    let inner = &s[start..j];
                    if inner.is_empty() {
                        return Err(err("empty bracket segment"));
                    }
                    if inner.bytes().all(|b| b.is_ascii_digit()) {
                        match inner.parse::<u32>() {
                            Ok(n) => segments.push(Segment::Indexed(n)),
                            Err(_) => return Err(err("index out of range")),
                        }
                    } else {
                        segments.push(Segment::MapKey(inner.into()));
                    }
                    i = j + 1;
                }
                b']' => return Err(err("unmatched `]`")),
                _ => {
                    cur.push(bytes[i] as char);
                    i += 1;
                }
            }
        }
        if !cur.is_empty() {
            segments.push(Segment::Named(cur.as_str().into()));
        }
        if segments.is_empty() {
            return Err(err("empty key"));
        }
        Ok(CanonicalName {
            segments: segments.into_boxed_slice(),
        })
    }

    /// The parsed segments.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// `true` iff this key has no segments (never constructed via `parse`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Append a child segment, producing a new descended name (binder descent).
    #[must_use]
    pub fn child(&self, seg: Segment) -> Self {
        let mut v = self.segments.to_vec();
        v.push(seg);
        CanonicalName {
            segments: v.into_boxed_slice(),
        }
    }

    /// The uniform-form equivalence key: lowercase, non-alphanumeric separators
    /// dropped between segments (segments are joined directly). Two names are
    /// "relaxed-equal" iff their uniform forms are byte-equal.
    #[must_use]
    pub fn uniform(&self) -> UniformName {
        let mut out = String::new();
        for seg in self.segments.iter() {
            seg.write_uniform(&mut out);
        }
        UniformName(out)
    }

    /// Render the canonical kebab dotted form (`a.b-c.list[0].map[Key]`).
    #[must_use]
    pub fn to_dotted(&self) -> String {
        let mut out = String::new();
        for (i, seg) in self.segments.iter().enumerate() {
            match seg {
                Segment::Named(s) => {
                    if i > 0 {
                        out.push('.');
                    }
                    out.push_str(s);
                }
                Segment::Indexed(n) => {
                    use std::fmt::Write;
                    let _ = write!(out, "[{n}]");
                }
                Segment::MapKey(s) => {
                    out.push('[');
                    out.push_str(s);
                    out.push(']');
                }
            }
        }
        out
    }
}

impl fmt::Debug for CanonicalName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CanonicalName({:?})", self.to_dotted())
    }
}

impl fmt::Display for CanonicalName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_dotted())
    }
}

/// The uniform-form equivalence key of a [`CanonicalName`] (an owned lowercase,
/// separator-stripped string). Equality/hash on this IS relaxed-equality.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct UniformName(String);

impl UniformName {
    /// Borrow the uniform string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UniformName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Fold one raw key into its uniform form WITHOUT first parsing into segments —
/// the fast path the index builder and the env-probe path share.
///
/// Lowercases ASCII and drops every char that is not ASCII alphanumeric. So
/// `DB_POOL_SIZE`, `db.pool-size`, `dbPoolSize`, and `db_pool_size` all fold to
/// `dbpoolsize`. Bracket characters are dropped too, so a raw `list[0]` folds to
/// `list0` — consistent with [`CanonicalName::uniform`] on the parsed form.
#[must_use]
pub fn uniform_key(raw: &str) -> UniformName {
    let mut out = String::with_capacity(raw.len());
    fold_into(raw, &mut out);
    UniformName(out)
}

/// The shared fold: lowercase ASCII, drop non-alphanumeric.
fn fold_into(s: &str, out: &mut String) {
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c.is_alphanumeric() {
            // Non-ASCII alphanumerics are kept lowercased (defensive; config keys
            // are ASCII in practice but we must not silently drop a letter).
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
        // else: a separator/bracket/space — dropped.
    }
}

/// Map an OS environment variable name to its canonical kebab key form.
///
/// This is the relaxed-binding inverse of the env mapper: `DB_POOL_SIZE` →
/// `db.pool-size`. The rule (matching Spring's `SystemEnvironmentPropertySource`
/// intent): lowercase, a single `_` becomes a `.` segment boundary, and a `__`
/// (double underscore) becomes a literal `-` (so `MY__VAR` → `my-var`). A
/// trailing/leading underscore is dropped.
///
/// NOTE: the framework keys env on its UNIFORM identity (so `DB_POOL_SIZE`,
/// `db.pool-size`, and `dbPoolSize` all resolve a `db.pool-size` field); this
/// canonical mapping is the *advertised* / diagnostic form. The actual match is
/// via [`uniform_key`], which folds both spellings to `dbpoolsize`.
#[must_use]
pub fn env_var_to_canonical(var: &str) -> Cow<'_, str> {
    if var.is_empty() {
        return Cow::Borrowed(var);
    }
    let mut out = String::with_capacity(var.len());
    let bytes = var.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'_' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'_' {
                // `__` => literal `-` within a segment.
                out.push('-');
                i += 2;
            } else {
                // single `_` => segment boundary `.` (unless leading/trailing).
                if !out.is_empty() && i + 1 < bytes.len() {
                    out.push('.');
                }
                i += 1;
            }
        } else {
            out.push((b as char).to_ascii_lowercase());
            i += 1;
        }
    }
    Cow::Owned(out)
}

/// The bounded env-var candidate generator (≤ 4 WHOLE-name forms).
///
/// Given a canonical dotted key, produce the set of OS-env-var spellings Spring
/// probes a non-enumerable system-env source with: the dash-removed and
/// legacy-underscore forms × upper/lower case. Capped at four forms — NEVER a
/// per-segment cartesian product (matching Spring's bound). The returned forms
/// are deduplicated, preserving order.
///
/// e.g. `db.pool-size` → `["DB_POOL_SIZE", "db_pool_size", "DB.POOL-SIZE",
/// "db.pool-size"]` (dedup keeps distinct spellings only).
#[must_use]
pub fn env_var_candidates(canonical: &str) -> Vec<String> {
    // Underscore form: `.`/`-` → `_`.
    let underscore: String = canonical
        .chars()
        .map(|c| if c == '.' || c == '-' { '_' } else { c })
        .collect();
    // Legacy form keeps `.`/`-` (some sources expose the dotted name verbatim).
    let dotted = canonical.to_string();

    let mut out: Vec<String> = Vec::with_capacity(4);
    let mut push_unique = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    push_unique(underscore.to_ascii_uppercase());
    push_unique(underscore.to_ascii_lowercase());
    push_unique(dotted.to_ascii_uppercase());
    push_unique(dotted.to_ascii_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse + segment structure ──────────────────────────────────────────

    #[test]
    fn parse_dotted_name_into_named_segments() {
        let n = CanonicalName::parse("db.pool-size").expect("parses");
        assert_eq!(
            n.segments(),
            &[
                Segment::Named("db".into()),
                Segment::Named("pool-size".into())
            ]
        );
        assert_eq!(n.to_dotted(), "db.pool-size");
    }

    #[test]
    fn parse_indexed_and_mapkey_segments() {
        let n = CanonicalName::parse("servers[0].host").expect("parses");
        assert_eq!(
            n.segments(),
            &[
                Segment::Named("servers".into()),
                Segment::Indexed(0),
                Segment::Named("host".into())
            ]
        );
        let m = CanonicalName::parse("headers[Content-Type]").expect("parses");
        assert_eq!(
            m.segments(),
            &[
                Segment::Named("headers".into()),
                Segment::MapKey("Content-Type".into())
            ]
        );
        // MapKey contents are preserved verbatim on render.
        assert_eq!(m.to_dotted(), "headers[Content-Type]");
    }

    #[test]
    fn parse_rejects_malformed_keys() {
        assert!(CanonicalName::parse("a..b").is_err());
        assert!(CanonicalName::parse("a[1").is_err());
        assert!(CanonicalName::parse("a]").is_err());
        assert!(CanonicalName::parse("a[]").is_err());
        assert!(CanonicalName::parse("").is_err());
    }

    // ── uniform-form equivalence (the relaxed identity) ────────────────────

    #[test]
    fn uniform_form_collapses_relaxed_spellings_to_one_identity() {
        // The canonical case: every Spring "relaxed" spelling of one property
        // folds to one uniform identity.
        let canon = uniform_key("db.pool-size");
        assert_eq!(uniform_key("DB_POOL_SIZE"), canon);
        assert_eq!(uniform_key("dbPoolSize"), canon);
        assert_eq!(uniform_key("db_pool_size"), canon);
        assert_eq!(uniform_key("DB-POOL-SIZE"), canon);
        assert_eq!(canon.as_str(), "dbpoolsize");
    }

    #[test]
    fn parsed_uniform_matches_raw_uniform() {
        let n = CanonicalName::parse("db.pool-size").expect("parses");
        assert_eq!(n.uniform(), uniform_key("DB_POOL_SIZE"));
    }

    #[test]
    fn distinct_properties_have_distinct_uniform_identity() {
        assert_ne!(uniform_key("db.pool-size"), uniform_key("db.pool-max"));
    }

    #[test]
    fn indexed_uniform_includes_the_index_digits() {
        let n = CanonicalName::parse("servers[10].host").expect("parses");
        assert_eq!(n.uniform().as_str(), "servers10host");
    }

    // ── env-var canonicalization (env-var -> canonical key) ────────────────

    #[test]
    fn env_var_db_pool_size_maps_to_dotted_kebab() {
        // The headline case from the unit spec: DB_POOL_SIZE -> db.pool-size.
        assert_eq!(env_var_to_canonical("DB_POOL_SIZE"), "db.pool.size");
        // And it folds to the same uniform identity as the kebab spelling.
        assert_eq!(
            uniform_key(&env_var_to_canonical("DB_POOL_SIZE")),
            uniform_key("db.pool-size")
        );
    }

    #[test]
    fn env_var_double_underscore_is_a_literal_dash() {
        // MY__VAR -> my-var (double underscore escapes a dash within a segment).
        assert_eq!(env_var_to_canonical("MY__VAR"), "my-var");
        assert_eq!(env_var_to_canonical("APP_MY__VAR_NAME"), "app.my-var.name");
    }

    #[test]
    fn env_var_leading_and_trailing_underscores_dropped() {
        assert_eq!(env_var_to_canonical("_LEADING"), "leading");
        assert_eq!(env_var_to_canonical("TRAILING_"), "trailing");
    }

    #[test]
    fn env_var_empty_is_borrowed_unchanged() {
        assert!(matches!(env_var_to_canonical(""), Cow::Borrowed("")));
    }

    // ── bounded candidate generator (≤ 4 whole-name forms) ─────────────────

    #[test]
    fn env_candidates_are_bounded_to_four_whole_name_forms() {
        let cands = env_var_candidates("db.pool-size");
        assert!(cands.len() <= 4, "must be bounded: {cands:?}");
        assert!(cands.contains(&"DB_POOL_SIZE".to_string()));
        assert!(cands.contains(&"db_pool_size".to_string()));
    }

    #[test]
    fn env_candidates_dedup_when_spellings_coincide() {
        // A key with no separators yields fewer than four distinct forms.
        let cands = env_var_candidates("port");
        assert!(cands.contains(&"PORT".to_string()));
        assert!(cands.contains(&"port".to_string()));
        // No duplicates.
        let mut sorted = cands.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), cands.len());
    }

    #[test]
    fn child_descends_a_new_segment() {
        let base = CanonicalName::parse("server").expect("parses");
        let child = base.child(Segment::Named("port".into()));
        assert_eq!(child.to_dotted(), "server.port");
    }
}
