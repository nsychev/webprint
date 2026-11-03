//! Styling primitives shared by the highlighter and the PDF layout.
//!
//! These types carry no syntax knowledge, so they are available even when the
//! optional `highlight` feature (and with it `syntect`) is disabled.

/// How one run of characters should be drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    /// 0.0 is black, 1.0 is white.
    pub gray: f64,
}

impl TextStyle {
    pub const PLAIN: Self = Self {
        bold: false,
        italic: false,
        gray: 0.0,
    };

    pub fn bold() -> Self {
        Self {
            bold: true,
            ..Self::PLAIN
        }
    }

    pub fn is_plain(&self) -> bool {
        !self.bold && !self.italic && self.gray <= f64::EPSILON
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::PLAIN
    }
}

/// A run of characters sharing one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: TextStyle,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: TextStyle::PLAIN,
        }
    }
}

/// A language offered in the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    /// Stable identifier submitted by the client.
    pub token: String,
    /// Human-readable name.
    pub name: String,
}

/// Identifier meaning "do not highlight".
pub const PLAIN_TEXT: &str = "plain";
/// Identifier meaning "guess the language from the content".
pub const AUTO: &str = "auto";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_the_default_and_reports_itself() {
        assert_eq!(TextStyle::default(), TextStyle::PLAIN);
        assert!(TextStyle::PLAIN.is_plain());
        assert!(!TextStyle::bold().is_plain());
        assert!(!TextStyle {
            italic: true,
            ..TextStyle::PLAIN
        }
        .is_plain());
        assert!(!TextStyle {
            gray: 0.5,
            ..TextStyle::PLAIN
        }
        .is_plain());
    }

    #[test]
    fn spans_carry_text_and_style() {
        let s = Span::plain("abc");
        assert_eq!(s.text, "abc");
        assert!(s.style.is_plain());
    }
}
