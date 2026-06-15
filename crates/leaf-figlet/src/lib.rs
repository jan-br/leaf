//! `leaf-figlet` — the self-contained startup-banner renderer.
//!
//! Realizes the `banner` feature of phase3/14 (`bootstrap-diagnostics`) as a
//! SMALL optional crate with NO dependencies (not even `leaf-core`): a default
//! banner template + `${...}` placeholder substitution + a `auto/always/never`
//! ANSI color tri-state honoring `NO_COLOR` and tty detection
//! (`std::io::IsTerminal`).
//!
//! Per TOPOLOGY this crate is deliberately dependency-free and owns no framework
//! ABI. leaf-boot's `print_banner` step (driven by `spring.main.banner-mode`)
//! drives this crate: it builds a [`Placeholders`] table from the frozen `Env`
//! (Cargo metadata — `application.version`, `application.title`, `leaf.version`),
//! maps its `BannerMode` onto a [`ColorMode`], and calls [`render`] / [`render_now`].
//!
//! ## Pieces
//! - [`DEFAULT_TEMPLATE`] + [`Placeholders`] + [`render_template`] — the template
//!   and its single-pass `${key}` / `${key:default}` substitution
//!   ([`template`]).
//! - [`ColorMode`] + [`should_colorize`] — the pure `auto/always/never` decision
//!   over `(is_tty, no_color)`, plus the ambient probes ([`color`]).
//! - [`Style`] + [`Color`] + [`style`] — minimal ANSI SGR styling ([`ansi`]).
//! - [`render`] / [`render_now`] — the high-level "template + placeholders +
//!   color policy → final string" entry the bootstrap step prints.
//!
//! The banner is the ONE deliberate fail-fast exception (charter §1.7): nothing
//! here can panic on malformed input — a missing placeholder collapses to empty
//! and an unterminated `${` is emitted verbatim, so a banner never aborts a boot.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod ansi;
pub mod color;
pub mod template;

pub use ansi::{style, Color, Style, RESET};
pub use color::{colorize_now, no_color_env, should_colorize, stdout_is_terminal, ColorMode};
pub use template::{render_template, Placeholders, DEFAULT_TEMPLATE};

/// The default banner accent style (cyan + bold) applied when colorizing — kept
/// modest so a plain template stays readable.
const BANNER_STYLE: Style = Style::new().fg(Color::Cyan).bold();

/// Render `template`, substituting `${...}` placeholders from `ph` and applying
/// ANSI styling according to `mode` against the SUPPLIED facts — the pure,
/// fully-testable form (no ambient I/O).
///
/// When colorizing is enabled the whole rendered body is wrapped in the default
/// banner accent; otherwise it is returned escape-free.
#[must_use]
pub fn render_with(
    template: &str,
    ph: &Placeholders,
    mode: ColorMode,
    is_tty: bool,
    no_color: bool,
) -> String {
    let body = render_template(template, ph);
    style(&body, BANNER_STYLE, should_colorize(mode, is_tty, no_color))
}

/// Render `template` with `ph`, deciding color from `mode` treating the output
/// as a terminal with `NO_COLOR` unset.
///
/// This is the deterministic form used in tests and by callers that have already
/// made the tty/`NO_COLOR` decision elsewhere: `Always` colorizes, `Never` does
/// not, and `Auto` colorizes (assuming a tty). For the ambient-aware form that
/// probes the real environment, use [`render_now`].
#[must_use]
pub fn render(template: &str, ph: &Placeholders, mode: ColorMode) -> String {
    render_with(template, ph, mode, /*is_tty*/ true, /*no_color*/ false)
}

/// Render `template` with `ph`, deciding color from `mode` against the REAL
/// ambient environment (stdout tty + `NO_COLOR`) — what the bootstrap banner
/// step calls in production.
#[must_use]
pub fn render_now(template: &str, ph: &Placeholders, mode: ColorMode) -> String {
    render_with(template, ph, mode, stdout_is_terminal(), no_color_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_with_respects_no_color_under_always() {
        let ph = Placeholders::new();
        let out = render_with("x", &ph, ColorMode::Always, true, /*no_color*/ true);
        assert!(!out.contains('\u{1b}'), "NO_COLOR must suppress escapes: {out:?}");
        assert_eq!(out, "x");
    }

    #[test]
    fn render_now_off_equivalent_is_never() {
        let ph = Placeholders::new();
        // Never is environment-independent.
        assert_eq!(render_now("plain", &ph, ColorMode::Never), "plain");
    }
}
