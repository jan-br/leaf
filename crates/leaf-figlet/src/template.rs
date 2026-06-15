//! The compile-time-lowered default banner template + `${...}` placeholder
//! substitution.
//!
//! The renderer is deliberately tiny and self-contained: it walks the template
//! once, copying literal text and expanding `${key}` / `${key:default}` spans.
//! A missing key with no default collapses to the empty string (Spring-faithful
//! — a banner never aborts a boot), and a malformed (unterminated) `${` span is
//! emitted verbatim rather than panicking.

use std::collections::HashMap;

/// The embedded default banner template (bootstrap-diagnostics `banner` feature).
///
/// `${...}` placeholders resolve against the frozen `Env` (via [`Placeholders`])
/// — mostly Cargo metadata known at compile time. The bootstrap step substitutes
/// `application.version`, `application.title`, and `leaf.version`.
pub const DEFAULT_TEMPLATE: &str = r#"
  _             __
 | |   ___ __ _ / _|
 | |__/ -_) _` |  _|
 |____\___\__,_|_|     :: ${application.title} ::  (v${application.version})

 leaf v${leaf.version}
"#;

/// A flat `key → value` substitution table for banner placeholders.
///
/// Kept dependency-free (a plain `HashMap`) so the crate stays self-contained;
/// the bootstrap step populates it from the frozen `Env`.
#[derive(Debug, Clone, Default)]
pub struct Placeholders {
    values: HashMap<String, String>,
}

impl Placeholders {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `key` to `value` (last write wins).
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// Look up a key, if bound.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// Expand `${...}` placeholders in `template` against `ph`, returning the
/// rendered text (no ANSI styling — that is layered on top by [`crate::render`]).
///
/// Grammar (intentionally minimal, single-pass, non-recursive):
/// - `${key}` → the bound value, or empty when unbound.
/// - `${key:default}` → the bound value, else `default` (which may be empty).
/// - A `$` not followed by `{` is literal.
/// - An unterminated `${...` (no closing `}`) is emitted verbatim.
#[must_use]
pub fn render_template(template: &str, ph: &Placeholders) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the matching closing brace.
            if let Some(rel_end) = template[i + 2..].find('}') {
                let end = i + 2 + rel_end;
                let inner = &template[i + 2..end];
                out.push_str(&resolve_one(inner, ph));
                i = end + 1;
                continue;
            }
            // Unterminated: copy the rest verbatim and stop.
            out.push_str(&template[i..]);
            break;
        }
        // Copy one UTF-8 char (we may be mid multi-byte sequence; index by char).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&template[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Resolve a single `key` or `key:default` placeholder body.
fn resolve_one(inner: &str, ph: &Placeholders) -> String {
    match inner.split_once(':') {
        Some((key, default)) => ph
            .get(key.trim())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| default.to_owned()),
        None => ph.get(inner.trim()).unwrap_or_default().to_owned(),
    }
}

/// Length in bytes of the UTF-8 sequence whose lead byte is `b`.
#[inline]
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_template_renders_empty() {
        assert_eq!(render_template("", &Placeholders::new()), "");
    }

    #[test]
    fn multibyte_literals_survive() {
        let ph = Placeholders::new();
        // A non-ASCII literal must round-trip without being split.
        assert_eq!(render_template("café ${x:☕}", &ph), "café ☕");
    }

    #[test]
    fn whitespace_in_key_is_trimmed() {
        let mut ph = Placeholders::new();
        ph.set("a.b", "v");
        assert_eq!(render_template("${ a.b }", &ph), "v");
    }
}
