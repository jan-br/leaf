//! The embedded `${...}` / `#{...}` value-template parser + the message-bundle
//! parser the value / catalog codegen call (binding-conversion phase3/07 extra-12;
//! expr-i18n-resources phase3/11).
//!
//! Both are pure, allocation-light, unit-testable string parsers run at MACRO
//! time, so the emitted const data is split exactly once at compile time:
//!
//! 1. **[`parse_value_template`] — the `${}`/`#{}` splitter.** Splits a `#[value]`
//!    literal into a `Vec<Segment>` mirroring the frozen `leaf_core::ValueSegment`
//!    AST (`Literal` / `Placeholder { key, default }` / `Expr`), then
//!    [`emit_segments`] lowers it to a const `&'static [::leaf_core::ValueSegment]`.
//!    The grammar matches the runtime scanner's dispatch in
//!    `leaf_core::placeholder`: `${key}` / `${key:default}` is a property
//!    placeholder (resolved in phase 1); `#{...}` is an opaque expression body
//!    (phase 2). Brace nesting inside a body is balanced so `${a:${b}}` and
//!    `#{ {x:1} }` parse as one segment.
//! 2. **[`parse_message_bundle`] — the i18n catalog parser.** Parses a
//!    `.properties`-style message bundle (`key = pattern`, `#`/`!` comments, line
//!    continuations) into ordered [`MessageEntry`] rows and validates the
//!    MessageFormat `{n}` argument placeholders ([`max_arg_index`]) so the catalog
//!    codegen can emit each pattern as a const and reject a malformed `{` at
//!    BUILD time (Tier-0) rather than at first lookup.

use proc_macro2::TokenStream;
use quote::quote;

// ───────────────────────── the ${}/#{} value-template parser ─────────────────

/// A parsed value-template segment — the codegen mirror of the frozen
/// [`leaf_core::Segment`] AST (owned `String`s here; lowered to `Cow::Borrowed`
/// const literals by [`emit_segments`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// A literal run (de-escaped not at all here — escaping is the runtime
    /// scanner's job; this parser only splits on the unescaped delimiters).
    Literal(String),
    /// A `${key}` / `${key:default}` property placeholder.
    Placeholder {
        /// The raw key (whitespace-trimmed by the runtime scanner, not here).
        key: String,
        /// The default template body, if a `:` separator was present.
        default: Option<String>,
    },
    /// A `#{...}` expression body (opaque; phase-2 evaluated).
    Expr(String),
}

/// A malformed value template (an unbalanced `${`/`#{`) — a Tier-0 diagnostic the
/// `#[value]` macro turns into `compile_error!`, never a silent mis-split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The human-readable explanation.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

/// Split a value template into [`Segment`]s on the `${...}` / `#{...}` delimiters.
///
/// `${` and `#{` open a balanced-brace body terminated by the matching `}`; a
/// `${...}` body splits on the FIRST unescaped `:` into `key` + `default`. A `\$`
/// / `\#` / `\\` escape emits the following char literally (so a literal `${` is
/// `\${`). Everything outside a body is a `Literal` run. Adjacent literals are
/// coalesced so the output is minimal.
///
/// # Errors
/// Returns a [`ParseError`] on an unterminated `${`/`#{` (no matching `}`).
pub fn parse_value_template(text: &str) -> Result<Vec<Segment>, ParseError> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut literal = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let flush = |literal: &mut String, segments: &mut Vec<Segment>| {
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(literal)));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        // Escape: `\X` emits X literally (we keep escaping minimal + aligned with
        // the runtime scanner's `\${`, `\#{`, `\\`).
        if c == '\\' && i + 1 < chars.len() {
            literal.push(chars[i + 1]);
            i += 2;
            continue;
        }
        // `${` placeholder or `#{` expression.
        let is_dollar = c == '$' && i + 1 < chars.len() && chars[i + 1] == '{';
        let is_hash = c == '#' && i + 1 < chars.len() && chars[i + 1] == '{';
        if is_dollar || is_hash {
            flush(&mut literal, &mut segments);
            let (body, next) = read_braced_body(&chars, i + 1, c)?;
            i = next;
            if is_dollar {
                segments.push(split_placeholder(&body));
            } else {
                segments.push(Segment::Expr(body));
            }
            continue;
        }
        literal.push(c);
        i += 1;
    }
    flush(&mut literal, &mut segments);
    Ok(segments)
}

/// Read a balanced-brace body starting at `open` (the index of the `{`), returning
/// the inner body string and the index just past the closing `}`.
fn read_braced_body(chars: &[char], open: usize, kind: char) -> Result<(String, usize), ParseError> {
    debug_assert_eq!(chars[open], '{');
    let mut depth = 1usize;
    let mut body = String::new();
    let mut i = open + 1;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            // Keep escapes verbatim inside a body (the inner sub-template / expr
            // owns its own escaping).
            body.push(c);
            body.push(chars[i + 1]);
            i += 2;
            continue;
        }
        match c {
            '{' => {
                depth += 1;
                body.push(c);
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((body, i + 1));
                }
                body.push(c);
            }
            _ => body.push(c),
        }
        i += 1;
    }
    Err(ParseError {
        message: format!("unterminated `{kind}{{` in value template (missing `}}`)"),
    })
}

/// Split a `${...}` body on the FIRST unescaped `:` into key + default.
fn split_placeholder(body: &str) -> Segment {
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        // A `:` inside a nested `${...}`/`#{...}` body is NOT the separator.
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                let key: String = chars[..i].iter().collect();
                let default: String = chars[i + 1..].iter().collect();
                return Segment::Placeholder {
                    key,
                    default: Some(default),
                };
            }
            _ => {}
        }
        i += 1;
    }
    Segment::Placeholder {
        key: body.to_string(),
        default: None,
    }
}

/// `true` iff `segments` contains an [`Segment::Expr`] (gates phase 2 — mirrors
/// [`leaf_core::has_expr`]).
#[must_use]
pub fn has_expr(segments: &[Segment]) -> bool {
    segments.iter().any(|s| matches!(s, Segment::Expr(_)))
}

/// Lower a parsed segment list to a const `&'static [::leaf_core::ValueSegment]`
/// expression via ABSOLUTE paths (the `#[value]` macro drops it into the const
/// row). Each owned `String` lowers to a `::std::borrow::Cow::Borrowed("…")`.
///
/// The emitted type is the absolute `::leaf_core::ValueSegment` (the re-export of
/// `placeholder::Segment`), NOT the bare `::leaf_core::Segment` — the latter
/// resolves to the DISTINCT relaxed-binding `Segment`, so the value-template AST
/// must name the placeholder segment unambiguously.
#[must_use]
pub fn emit_segments(segments: &[Segment]) -> TokenStream {
    let rows = segments.iter().map(|seg| match seg {
        Segment::Literal(s) => {
            let lit = s.as_str();
            quote! { ::leaf_core::ValueSegment::Literal(::std::borrow::Cow::Borrowed(#lit)) }
        }
        Segment::Placeholder { key, default } => {
            let key = key.as_str();
            let default = match default {
                Some(d) => {
                    let d = d.as_str();
                    quote! { ::core::option::Option::Some(::std::borrow::Cow::Borrowed(#d)) }
                }
                None => quote! { ::core::option::Option::None },
            };
            quote! {
                ::leaf_core::ValueSegment::Placeholder {
                    key: ::std::borrow::Cow::Borrowed(#key),
                    default: #default,
                }
            }
        }
        Segment::Expr(s) => {
            let lit = s.as_str();
            quote! { ::leaf_core::ValueSegment::Expr(::std::borrow::Cow::Borrowed(#lit)) }
        }
    });
    quote! { &[ #(#rows),* ] }
}

/// Parse then emit — the one entry point the `#[value]` macro calls.
///
/// # Errors
/// Propagates [`parse_value_template`]'s [`ParseError`] (an unterminated body).
pub fn parse_and_emit(text: &str) -> Result<TokenStream, ParseError> {
    let segments = parse_value_template(text)?;
    Ok(emit_segments(&segments))
}

// ──────────────────────────── the message-bundle parser ─────────────────────

/// One parsed entry of a message bundle: a `code` and its MessageFormat
/// `pattern` (with `{0}`/`{1}` argument placeholders), in declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageEntry {
    /// The message code (the catalog key).
    pub code: String,
    /// The raw MessageFormat pattern.
    pub pattern: String,
}

/// Parse a `.properties`-style message bundle into ordered [`MessageEntry`] rows.
///
/// Grammar (Java `.properties` subset, the i18n catalog lingua franca):
/// - blank lines and `#`/`!` comment lines are skipped;
/// - `key = value` or `key : value` (the first unescaped `=`/`:` splits);
/// - surrounding whitespace on key and value is trimmed;
/// - a trailing `\` continues the value onto the next line (a line continuation).
///
/// Keys preserve declaration order (a `Vec`, not a map) so the catalog codegen
/// emits a stable const table; a later duplicate-key check is the caller's
/// concern (mirrors the metadata rollup's duplicate-prefix guard).
#[must_use]
pub fn parse_message_bundle(text: &str) -> Vec<MessageEntry> {
    let mut entries = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, first_value)) = split_kv(line) else {
            continue;
        };
        // Line continuation: a value ending in an unescaped `\` joins the next.
        let mut value = first_value;
        while ends_with_continuation(&value) {
            value.pop(); // drop the trailing `\`
            match lines.next() {
                Some(cont) => value.push_str(cont.trim_start()),
                None => break,
            }
        }
        entries.push(MessageEntry {
            code: key.trim().to_string(),
            pattern: value.trim().to_string(),
        });
    }
    entries
}

/// Split a property line on the first unescaped `=` or `:` separator.
fn split_kv(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if c == '=' || c == ':' {
            let key: String = chars[..i].iter().collect();
            let value: String = chars[i + 1..].iter().collect();
            return Some((key, value.trim().to_string()));
        }
        i += 1;
    }
    None
}

/// `true` iff `value` ends in an ODD number of trailing backslashes (an unescaped
/// continuation marker).
fn ends_with_continuation(value: &str) -> bool {
    value.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}

/// The highest `{n}` argument index referenced by a MessageFormat `pattern`, or
/// `None` if it references no positional arguments. `{{`/`}}` are literal braces.
///
/// The catalog codegen uses this to record an arg-count hint and to validate the
/// pattern at build time.
///
/// # Errors
/// Returns a [`ParseError`] on a malformed placeholder: an unbalanced `{`, or a
/// `{...}` whose body is not a non-negative integer index.
pub fn max_arg_index(pattern: &str) -> Result<Option<u32>, ParseError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut max: Option<u32> = None;
    while i < chars.len() {
        let c = chars[i];
        // Escaped literal braces: `{{` / `}}`.
        if (c == '{' || c == '}') && i + 1 < chars.len() && chars[i + 1] == c {
            i += 2;
            continue;
        }
        if c == '}' {
            return Err(ParseError {
                message: format!("unbalanced `}}` in message pattern: {pattern:?}"),
            });
        }
        if c == '{' {
            // Read until the matching `}`.
            let mut j = i + 1;
            let mut body = String::new();
            while j < chars.len() && chars[j] != '}' {
                body.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() {
                return Err(ParseError {
                    message: format!("unterminated `{{` in message pattern: {pattern:?}"),
                });
            }
            // MessageFormat `{index}` or `{index,type,...}` — the index is the
            // leading run before a comma.
            let index_str = body.split(',').next().unwrap_or("").trim();
            let index: u32 = index_str.parse().map_err(|_| ParseError {
                message: format!(
                    "message placeholder `{{{body}}}` is not a `{{n}}` argument index in {pattern:?}"
                ),
            })?;
            max = Some(max.map_or(index, |m| m.max(index)));
            i = j + 1;
            continue;
        }
        i += 1;
    }
    Ok(max)
}

/// Lower one [`MessageEntry`] to a const `(code, ::leaf_core::MessagePattern)`
/// tuple expression via ABSOLUTE paths — the row the catalog codegen tables.
#[must_use]
pub fn emit_message_entry(entry: &MessageEntry) -> TokenStream {
    let code = entry.code.as_str();
    let pattern = entry.pattern.as_str();
    quote! {
        (#code, ::leaf_core::MessagePattern(::std::sync::Arc::from(#pattern)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    // ── ${}/#{} value-template parsing ─────────────────────────────────────────

    #[test]
    fn parses_a_plain_literal_as_one_segment() {
        let segs = parse_value_template("just text").expect("parses");
        assert_eq!(segs, vec![Segment::Literal("just text".into())]);
    }

    #[test]
    fn parses_a_bare_placeholder() {
        let segs = parse_value_template("${server.port}").expect("parses");
        assert_eq!(
            segs,
            vec![Segment::Placeholder {
                key: "server.port".into(),
                default: None
            }]
        );
    }

    #[test]
    fn parses_a_placeholder_with_a_default() {
        let segs = parse_value_template("${server.port:8080}").expect("parses");
        assert_eq!(
            segs,
            vec![Segment::Placeholder {
                key: "server.port".into(),
                default: Some("8080".into())
            }]
        );
    }

    #[test]
    fn parses_mixed_literal_and_placeholder_runs() {
        // `http://${host}:8080` → Literal, Placeholder, Literal.
        let segs = parse_value_template("http://${host}:8080").expect("parses");
        assert_eq!(
            segs,
            vec![
                Segment::Literal("http://".into()),
                Segment::Placeholder {
                    key: "host".into(),
                    default: None
                },
                Segment::Literal(":8080".into()),
            ]
        );
    }

    #[test]
    fn parses_a_hash_expression_segment() {
        let segs = parse_value_template("#{ 1 + 1 }").expect("parses");
        assert_eq!(segs, vec![Segment::Expr(" 1 + 1 ".into())]);
        assert!(has_expr(&segs));
    }

    #[test]
    fn nested_braces_in_a_default_are_balanced_into_one_segment() {
        // `${a:${b}}` — the default body keeps its inner `${b}` (a sub-template).
        let segs = parse_value_template("${a:${b}}").expect("parses");
        assert_eq!(
            segs,
            vec![Segment::Placeholder {
                key: "a".into(),
                default: Some("${b}".into())
            }]
        );
    }

    #[test]
    fn the_separator_inside_a_nested_body_is_not_the_split_point() {
        // The `:` inside `${b:c}` must NOT split the OUTER placeholder.
        let segs = parse_value_template("${a:${b:c}}").expect("parses");
        assert_eq!(
            segs,
            vec![Segment::Placeholder {
                key: "a".into(),
                default: Some("${b:c}".into())
            }]
        );
    }

    #[test]
    fn an_escaped_dollar_brace_is_a_literal() {
        let segs = parse_value_template(r"\${literal}").expect("parses");
        assert_eq!(segs, vec![Segment::Literal("${literal}".into())]);
    }

    #[test]
    fn an_unterminated_placeholder_is_a_loud_parse_error() {
        let err = parse_value_template("${oops").expect_err("must error");
        assert!(err.message.contains("unterminated"), "{}", err.message);
    }

    // ── lowering segments to const ::leaf_core::ValueSegment data ──────────────

    #[test]
    fn emit_segments_lowers_through_absolute_core_paths_and_parses() {
        let segs = parse_value_template("http://${host:localhost}/#{path}").expect("parses");
        let ts = emit_segments(&segs);
        syn::parse2::<syn::Expr>(ts.clone()).expect("emitted segments are a valid expression");
        let s = flat(&ts);
        // The emitted type is the absolute `::leaf_core::ValueSegment` (placeholder's
        // Segment), NOT the bare `::leaf_core::Segment` (the distinct relaxed one).
        assert!(s.contains("::leaf_core::ValueSegment::Literal(::std::borrow::Cow::Borrowed(\"http://\"))"), "got: {s}");
        assert!(s.contains("::leaf_core::ValueSegment::Placeholder{key:::std::borrow::Cow::Borrowed(\"host\")"), "got: {s}");
        assert!(s.contains("::core::option::Option::Some(::std::borrow::Cow::Borrowed(\"localhost\"))"), "got: {s}");
        assert!(s.contains("::leaf_core::ValueSegment::Expr(::std::borrow::Cow::Borrowed(\"path\"))"), "got: {s}");
    }

    #[test]
    fn parse_and_emit_is_the_one_entry_point() {
        let ts = parse_and_emit("${a}").expect("parses+emits");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::ValueSegment::Placeholder"), "got: {s}");
    }

    // ── message-bundle parsing ─────────────────────────────────────────────────

    #[test]
    fn parses_simple_key_value_messages_in_order() {
        let bundle = "greeting = Hello, {0}!\nfarewell = Bye, {0}.";
        let entries = parse_message_bundle(bundle);
        assert_eq!(
            entries,
            vec![
                MessageEntry {
                    code: "greeting".into(),
                    pattern: "Hello, {0}!".into()
                },
                MessageEntry {
                    code: "farewell".into(),
                    pattern: "Bye, {0}.".into()
                },
            ]
        );
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let bundle = "# a comment\n! also a comment\n\nkey = value\n";
        let entries = parse_message_bundle(bundle);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].code, "key");
        assert_eq!(entries[0].pattern, "value");
    }

    #[test]
    fn accepts_a_colon_separator_too() {
        let entries = parse_message_bundle("key : value");
        assert_eq!(entries[0].pattern, "value");
    }

    #[test]
    fn joins_a_line_continuation() {
        let bundle = "long = first \\\nsecond";
        let entries = parse_message_bundle(bundle);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pattern, "first second");
    }

    // ── MessageFormat {n} placeholder validation ───────────────────────────────

    #[test]
    fn max_arg_index_counts_positional_placeholders() {
        assert_eq!(max_arg_index("Hello {0}, you have {1} messages").unwrap(), Some(1));
        assert_eq!(max_arg_index("no args here").unwrap(), None);
    }

    #[test]
    fn max_arg_index_reads_the_index_before_a_format_type() {
        // MessageFormat `{0,number,integer}` — the index is the leading run.
        assert_eq!(max_arg_index("count: {2,number}").unwrap(), Some(2));
    }

    #[test]
    fn max_arg_index_treats_doubled_braces_as_literals() {
        assert_eq!(max_arg_index("a literal {{0}} brace").unwrap(), None);
    }

    #[test]
    fn max_arg_index_rejects_a_non_numeric_placeholder() {
        let err = max_arg_index("hello {name}").expect_err("must reject");
        assert!(err.message.contains("argument index"), "{}", err.message);
    }

    #[test]
    fn max_arg_index_rejects_an_unterminated_brace() {
        let err = max_arg_index("oops {0").expect_err("must reject");
        assert!(err.message.contains("unterminated"), "{}", err.message);
    }

    #[test]
    fn emit_message_entry_lowers_to_a_const_message_pattern() {
        let entry = MessageEntry {
            code: "greeting".into(),
            pattern: "Hello, {0}!".into(),
        };
        let ts = emit_message_entry(&entry);
        syn::parse2::<syn::Expr>(ts.clone()).expect("emitted entry is a valid expression");
        // The raw (un-collapsed) rendering preserves the literal pattern verbatim;
        // `flat` would strip the spaces INSIDE the string literal, so assert on
        // the raw form for the pattern and the collapsed form for the path.
        let raw = ts.to_string();
        // The pattern literal is preserved verbatim (with its interior spaces);
        // `flat` would collapse them, so assert on the raw token string here.
        assert!(raw.contains(r#"("Hello, {0}!")"#), "got: {raw}");
        let s = flat(&ts);
        assert!(s.contains(r#"("greeting",::leaf_core::MessagePattern("#), "got: {s}");
        assert!(s.contains("::std::sync::Arc::from("), "got: {s}");
    }
}
