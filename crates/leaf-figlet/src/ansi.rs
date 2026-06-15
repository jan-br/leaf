//! Minimal self-contained ANSI SGR styling (no external crate).
//!
//! Only what the banner needs: a small 8-color palette + bold/dim, wrapped in a
//! single SGR introducer and terminated with the universal reset. When styling
//! is disabled (see [`crate::color`]) the text passes through untouched, so a
//! caller can always build a [`Style`] and let the color policy decide.

use std::fmt::Write as _;

/// A basic ANSI foreground color (the 8-color palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Black (SGR 30).
    Black,
    /// Red (SGR 31).
    Red,
    /// Green (SGR 32).
    Green,
    /// Yellow (SGR 33).
    Yellow,
    /// Blue (SGR 34).
    Blue,
    /// Magenta (SGR 35).
    Magenta,
    /// Cyan (SGR 36).
    Cyan,
    /// White (SGR 37).
    White,
}

impl Color {
    /// The SGR foreground parameter for this color.
    #[must_use]
    const fn fg_code(self) -> u8 {
        match self {
            Color::Black => 30,
            Color::Red => 31,
            Color::Green => 32,
            Color::Yellow => 33,
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
            Color::White => 37,
        }
    }
}

/// An accumulated text style (foreground + bold/dim attributes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    fg: Option<Color>,
    bold: bool,
    dim: bool,
}

impl Style {
    /// An empty style (no color, no attributes).
    #[must_use]
    pub const fn new() -> Self {
        Self { fg: None, bold: false, dim: false }
    }

    /// Set the foreground color.
    #[must_use]
    pub const fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    /// Enable the bold (bright) attribute.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Enable the dim (faint) attribute.
    #[must_use]
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// Whether this style would emit any escape (i.e. is non-empty).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.fg.is_none() && !self.bold && !self.dim
    }

    /// The SGR parameter list (e.g. `"1;31"`), or `None` for an empty style.
    fn sgr_params(self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut params = String::new();
        if self.bold {
            params.push('1');
        }
        if self.dim {
            push_param(&mut params, "2");
        }
        if let Some(c) = self.fg {
            push_param(&mut params, &c.fg_code().to_string());
        }
        Some(params)
    }
}

fn push_param(params: &mut String, code: &str) {
    if params.is_empty() {
        params.push_str(code);
    } else {
        let _ = write!(params, ";{code}");
    }
}

/// The universal SGR reset sequence.
pub const RESET: &str = "\u{1b}[0m";

/// Wrap `text` in `style`'s ANSI escapes when `enabled`; otherwise return the
/// text unchanged.
///
/// An empty style (or `enabled == false`) is a clean passthrough — no stray
/// escapes are emitted.
#[must_use]
pub fn style(text: &str, style: Style, enabled: bool) -> String {
    if !enabled {
        return text.to_owned();
    }
    match style.sgr_params() {
        Some(params) => format!("\u{1b}[{params}m{text}{RESET}"),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_style_is_passthrough_even_when_enabled() {
        assert_eq!(style("x", Style::new(), true), "x");
    }

    #[test]
    fn dim_and_color_combine_with_semicolon() {
        let painted = style("x", Style::new().dim().fg(Color::Blue), true);
        assert_eq!(painted, "\u{1b}[2;34mx\u{1b}[0m");
    }

    #[test]
    fn bold_first_then_color() {
        let painted = style("x", Style::new().fg(Color::Cyan).bold(), true);
        assert_eq!(painted, "\u{1b}[1;36mx\u{1b}[0m");
    }
}
