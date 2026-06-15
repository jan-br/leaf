//! The `auto / always / never` color tri-state + the environment policy that
//! decides whether ANSI escapes are emitted (the banner feature's "small
//! self-contained ANSI styling with NO_COLOR + tty detection").
//!
//! The pure decision lives in [`should_colorize`] (fully testable, no I/O); the
//! ambient probes ([`no_color_env`], [`stdout_is_terminal`]) are thin wrappers
//! over `std::env` / `std::io::IsTerminal` so the policy can be unit-tested
//! without touching the real environment.

use std::io::IsTerminal;

/// How the banner decides whether to emit ANSI color escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// Colorize only when stdout is a terminal and `NO_COLOR` is unset (default).
    #[default]
    Auto,
    /// Always colorize (still yields to `NO_COLOR`).
    Always,
    /// Never colorize.
    Never,
}

impl ColorMode {
    /// Parse a mode string (case-insensitive): `auto` / `always` / `never`.
    /// Unknown values fall back to [`ColorMode::Auto`].
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "always" | "force" | "on" => ColorMode::Always,
            "never" | "off" => ColorMode::Never,
            _ => ColorMode::Auto,
        }
    }
}

/// The PURE color decision: given the mode and the two ambient facts, decide
/// whether to colorize.
///
/// Rules (NO_COLOR is an across-the-board opt-out per <https://no-color.org>):
/// - `Never` → never.
/// - `no_color` set → never (even under `Always`).
/// - `Always` → yes (when not vetoed by `no_color`).
/// - `Auto` → only when `is_tty`.
#[must_use]
pub fn should_colorize(mode: ColorMode, is_tty: bool, no_color: bool) -> bool {
    match mode {
        ColorMode::Never => false,
        _ if no_color => false,
        ColorMode::Always => true,
        ColorMode::Auto => is_tty,
    }
}

/// Whether the `NO_COLOR` environment variable is set to any non-empty value
/// (per the spec, presence — not a particular value — is the opt-out).
#[must_use]
pub fn no_color_env() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Whether the process stdout is connected to a terminal.
#[must_use]
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// The convenience decision that consults the real ambient environment — what
/// the bootstrap banner step calls when it has no test overrides.
#[must_use]
pub fn colorize_now(mode: ColorMode) -> bool {
    should_colorize(mode, stdout_is_terminal(), no_color_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive_and_lenient() {
        assert_eq!(ColorMode::parse("ALWAYS"), ColorMode::Always);
        assert_eq!(ColorMode::parse(" never "), ColorMode::Never);
        assert_eq!(ColorMode::parse("auto"), ColorMode::Auto);
        assert_eq!(ColorMode::parse("garbage"), ColorMode::Auto);
    }

    #[test]
    fn never_always_loses() {
        for tty in [true, false] {
            for nc in [true, false] {
                assert!(!should_colorize(ColorMode::Never, tty, nc));
            }
        }
    }
}
