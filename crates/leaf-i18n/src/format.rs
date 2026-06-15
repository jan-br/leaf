//! Message-format argument substitution (expr-i18n-resources phase3/11).
//!
//! A resolved [`MessagePattern`](leaf_core::MessagePattern) carries `{0}`/`{1}`
//! positional placeholders (the Java `MessageFormat` lineage, trimmed to the
//! positional subset). [`format_pattern`] substitutes the positional
//! [`Arg`]s into the pattern.
//!
//! Scope note (honest): this is the POSITIONAL, locale-INSENSITIVE substitution
//! only. Locale-sensitive number/date/plural formatting is delegated to the
//! type-conversion neighbour per phase3/11 ("DELEGATES to the type-conversion
//! neighbour's locale-aware formatters, NOT a private MessageFormat"); until that
//! seam lands here, an `{n}` placeholder renders its argument with the plain
//! `Display`-style rendering below. `{{`/`}}` escape to a literal brace.

use leaf_core::Arg;

/// Render `arg` as the plain (locale-insensitive) string a `{n}` placeholder
/// expands to.
fn render_arg(arg: &Arg<'_>) -> String {
    match arg {
        Arg::Str(s) => (*s).to_string(),
        Arg::Int(i) => i.to_string(),
        Arg::Float(f) => f.to_string(),
        Arg::Bool(b) => b.to_string(),
    }
}

/// Substitute the positional `args` into `pattern`'s `{n}` placeholders.
///
/// - `{0}`, `{1}`, … expand to the rendering of the corresponding argument.
/// - An out-of-range index (`{9}` with two args) is left VERBATIM (the Spring
///   `MessageFormat` posture: a missing argument is not an error, the brace stays
///   so the gap is visible).
/// - `{{` and `}}` escape to a single literal `{` / `}`.
/// - A malformed brace run (`{`, `{x}`, an unclosed `{`) is left verbatim — this
///   formatter never errors, matching the never-panic render contract.
#[must_use]
pub fn format_pattern(pattern: &str, args: &[Arg<'_>]) -> String {
    // Fast path: no brace, nothing to do (the overwhelmingly common catalog
    // entry has no placeholder).
    if !pattern.contains('{') && !pattern.contains('}') {
        return pattern.to_string();
    }

    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.char_indices().peekable();

    while let Some((_, c)) = chars.next() {
        match c {
            '{' => {
                // `{{` → literal `{`.
                if let Some((_, '{')) = chars.peek() {
                    chars.next();
                    out.push('{');
                    continue;
                }
                // Collect digits up to the closing `}`.
                let mut digits = String::new();
                let mut closed = false;
                while let Some(&(_, nc)) = chars.peek() {
                    if nc == '}' {
                        chars.next();
                        closed = true;
                        break;
                    } else if nc.is_ascii_digit() {
                        digits.push(nc);
                        chars.next();
                    } else {
                        // Not a positional placeholder — bail out verbatim.
                        break;
                    }
                }
                if closed && !digits.is_empty() {
                    // `digits` is all ASCII digits; parse, then index args.
                    match digits.parse::<usize>() {
                        Ok(idx) if idx < args.len() => out.push_str(&render_arg(&args[idx])),
                        // Out of range (or overflow): leave the placeholder verbatim.
                        _ => {
                            out.push('{');
                            out.push_str(&digits);
                            out.push('}');
                        }
                    }
                } else {
                    // Malformed: emit what we consumed verbatim.
                    out.push('{');
                    out.push_str(&digits);
                    // We never consumed a `}` here (closed implies digits empty
                    // for this branch, or we hit a non-digit char); leave the
                    // rest of the stream alone.
                }
            }
            '}' => {
                // `}}` → literal `}`.
                if let Some((_, '}')) = chars.peek() {
                    chars.next();
                }
                out.push('}');
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_placeholder_returns_verbatim() {
        assert_eq!(format_pattern("hello world", &[]), "hello world");
    }

    #[test]
    fn substitutes_positional_args() {
        let args = [Arg::Str("Jan"), Arg::Int(3)];
        assert_eq!(
            format_pattern("Hi {0}, you have {1} messages", &args),
            "Hi Jan, you have 3 messages"
        );
    }

    #[test]
    fn args_can_repeat_and_reorder() {
        let args = [Arg::Str("a"), Arg::Str("b")];
        assert_eq!(format_pattern("{1}-{0}-{1}", &args), "b-a-b");
    }

    #[test]
    fn out_of_range_index_left_verbatim() {
        let args = [Arg::Str("only")];
        assert_eq!(format_pattern("{0} then {5}", &args), "only then {5}");
    }

    #[test]
    fn escaped_braces() {
        assert_eq!(format_pattern("{{not a placeholder}}", &[]), "{not a placeholder}");
    }

    #[test]
    fn renders_each_arg_kind() {
        let args = [Arg::Int(-7), Arg::Float(1.5), Arg::Bool(true), Arg::Str("x")];
        assert_eq!(format_pattern("{0} {1} {2} {3}", &args), "-7 1.5 true x");
    }

    #[test]
    fn malformed_brace_is_not_a_panic() {
        // Unclosed / non-digit content is left as-is; the formatter never errors.
        assert_eq!(format_pattern("a { b", &[]), "a { b");
        assert_eq!(format_pattern("{x}", &[Arg::Str("v")]), "{x}");
    }
}
