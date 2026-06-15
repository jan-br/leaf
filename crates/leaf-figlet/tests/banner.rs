//! Behavioural tests for the self-contained banner crate (TDD-first).
//!
//! These exercise the PUBLIC surface only: the default template, `${...}`
//! placeholder substitution, the `auto/always/never` color tri-state with
//! `NO_COLOR` + tty detection, and the high-level `render` entry point that
//! leaf-boot's `print_banner` step drives.

use leaf_figlet::{
    render, render_template, should_colorize, style, Color, ColorMode, Placeholders, Style,
    DEFAULT_TEMPLATE,
};

// ───────────────────────────── default template ─────────────────────────────

#[test]
fn default_template_is_nonempty_and_carries_placeholders() {
    assert!(!DEFAULT_TEMPLATE.is_empty());
    // The default template must reference at least the application + leaf versions
    // so the bootstrap step can substitute Cargo metadata.
    assert!(DEFAULT_TEMPLATE.contains("${application.version}"));
    assert!(DEFAULT_TEMPLATE.contains("${leaf.version}"));
}

#[test]
fn render_default_banner_substitutes_known_placeholders() {
    let mut ph = Placeholders::new();
    ph.set("application.version", "1.2.3");
    ph.set("application.title", "demo-svc");
    ph.set("leaf.version", "0.1.0");

    let out = render(DEFAULT_TEMPLATE, &ph, ColorMode::Never);

    assert!(out.contains("1.2.3"), "version not substituted: {out}");
    assert!(out.contains("0.1.0"), "leaf.version not substituted: {out}");
    // No raw placeholder syntax should survive when a value is present.
    assert!(!out.contains("${application.version}"), "placeholder leaked: {out}");
    assert!(!out.contains("${leaf.version}"));
    // Never mode emits no ANSI escapes.
    assert!(!out.contains('\u{1b}'), "ANSI escape in never-mode output: {out:?}");
}

// ──────────────────────── placeholder substitution rules ────────────────────

#[test]
fn placeholder_substitution_replaces_only_known_keys() {
    let mut ph = Placeholders::new();
    ph.set("application.version", "9.9.9");
    let out = render_template("v=${application.version} t=${application.title}", &ph);
    // Known key substituted...
    assert_eq!(out, "v=9.9.9 t=");
    // ...unknown key collapses to empty (Spring-faithful: missing → blank, never aborts).
}

#[test]
fn placeholder_default_value_after_colon_is_used_when_unset() {
    let ph = Placeholders::new();
    let out = render_template("title=${application.title:my-app}", &ph);
    assert_eq!(out, "title=my-app");
}

#[test]
fn placeholder_set_value_wins_over_default() {
    let mut ph = Placeholders::new();
    ph.set("application.title", "real");
    let out = render_template("title=${application.title:fallback}", &ph);
    assert_eq!(out, "title=real");
}

#[test]
fn literal_text_without_placeholders_passes_through() {
    let ph = Placeholders::new();
    assert_eq!(render_template("plain text :: no subs", &ph), "plain text :: no subs");
}

#[test]
fn unterminated_placeholder_is_left_verbatim() {
    let ph = Placeholders::new();
    // A dangling `${` with no closing brace must not panic and is emitted as-is.
    assert_eq!(render_template("oops ${broken", &ph), "oops ${broken");
}

#[test]
fn dollar_without_brace_is_literal() {
    let ph = Placeholders::new();
    assert_eq!(render_template("cost is $5", &ph), "cost is $5");
}

// ─────────────────────────────── color tri-state ────────────────────────────

#[test]
fn color_mode_never_never_colorizes() {
    // even on a tty, with no NO_COLOR
    assert!(!should_colorize(ColorMode::Never, /*is_tty*/ true, /*no_color*/ false));
}

#[test]
fn color_mode_always_always_colorizes_even_without_tty() {
    assert!(should_colorize(ColorMode::Always, /*is_tty*/ false, /*no_color*/ false));
}

#[test]
fn color_mode_always_yields_to_no_color() {
    // NO_COLOR is an across-the-board opt-out (https://no-color.org): even Always honors it.
    assert!(!should_colorize(ColorMode::Always, /*is_tty*/ true, /*no_color*/ true));
}

#[test]
fn color_mode_auto_colorizes_only_on_tty_without_no_color() {
    assert!(should_colorize(ColorMode::Auto, true, false));
    assert!(!should_colorize(ColorMode::Auto, false, false));
    assert!(!should_colorize(ColorMode::Auto, true, true));
}

// ─────────────────────────────── ANSI styling ───────────────────────────────

#[test]
fn style_wraps_with_ansi_when_enabled() {
    let s = Style::new().fg(Color::Green);
    let painted = style("hi", s, /*enabled*/ true);
    assert!(painted.starts_with('\u{1b}'));
    assert!(painted.ends_with("\u{1b}[0m"));
    assert!(painted.contains("hi"));
}

#[test]
fn style_is_passthrough_when_disabled() {
    let s = Style::new().fg(Color::Green).bold();
    assert_eq!(style("hi", s, /*enabled*/ false), "hi");
}

#[test]
fn style_bold_and_fg_combine() {
    let s = Style::new().fg(Color::Red).bold();
    let painted = style("x", s, true);
    // Bold (1) and red fg (31) both present in the SGR introducer.
    assert!(painted.contains("1"));
    assert!(painted.contains("31"));
    assert!(painted.contains('x'));
    assert!(painted.ends_with("\u{1b}[0m"));
}

// ─────────────────────── high-level render() color wiring ───────────────────

#[test]
fn render_always_emits_ansi_and_never_does_not() {
    let ph = Placeholders::new();
    let colored = render(":: app ::", &ph, ColorMode::Always);
    let plain = render(":: app ::", &ph, ColorMode::Never);
    assert!(colored.contains('\u{1b}'), "Always should colorize the rendered banner");
    assert!(!plain.contains('\u{1b}'), "Never must be escape-free");
    // Both carry the same visible text.
    assert!(colored.contains(":: app ::"));
    assert!(plain.contains(":: app ::"));
}
