//! Font loading and glyph lookup.
//!
//! Fonts are kept as raw TrueType bytes so they can be embedded into the
//! generated PDF verbatim (`/FontFile2`). A [`ttf_parser::Face`] is parsed on
//! demand for metrics and glyph lookups; parsing is cheap (table offsets only)
//! and happens once per rendered document.

use std::sync::Arc;

use ttf_parser::{Face, GlyphId};

use crate::Error;

/// Bundled JetBrains Mono faces. They are embedded in the binary so no font
/// needs to be installed on the server or the printer host (SIL OFL 1.1, see
/// `assets/JetBrainsMono-OFL.txt`).
pub const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
pub const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/JetBrainsMono-Bold.ttf");
pub const JETBRAINS_MONO_ITALIC: &[u8] = include_bytes!("../assets/JetBrainsMono-Italic.ttf");
pub const JETBRAINS_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/JetBrainsMono-BoldItalic.ttf");

/// A TrueType font held in memory, ready to be embedded into a PDF.
#[derive(Clone)]
pub struct Font {
    data: Arc<[u8]>,
    ps_name: String,
    metrics: Metrics,
}

/// Font-wide metrics, expressed in PDF glyph space (1/1000 em).
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub units_per_em: f64,
    pub ascent: f64,
    pub descent: f64,
    pub cap_height: f64,
    pub bbox: [f64; 4],
    /// Advance width of `0`, used as the cell width for monospace layout.
    pub advance: f64,
    pub italic_angle: f64,
}

impl Font {
    /// Parses and validates a TrueType/OpenType font from raw bytes.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Result<Self, Error> {
        let data: Arc<[u8]> = bytes.into();
        let face = Face::parse(&data, 0).map_err(|e| Error::Font(e.to_string()))?;

        let upem = f64::from(face.units_per_em());
        if upem <= 0.0 {
            return Err(Error::Font("font has no units_per_em".into()));
        }
        let scale = 1000.0 / upem;

        let advance = face
            .glyph_index('0')
            .and_then(|g| face.glyph_hor_advance(g))
            .ok_or_else(|| Error::Font("font has no advance width for '0'".into()))?;

        let bbox = face.global_bounding_box();
        // The PostScript name may be stored as UTF-16BE (Windows/Unicode
        // platforms) or as a single-byte Mac Roman string, which
        // `Name::to_string` does not decode; fall back to the raw bytes when
        // they are plain ASCII.
        let ps_name = face
            .names()
            .into_iter()
            .filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .filter_map(|n| {
                n.to_string().or_else(|| {
                    n.name
                        .is_ascii()
                        .then(|| String::from_utf8_lossy(n.name).into_owned())
                })
            })
            .find(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "EmbeddedFont".to_string());

        let metrics = Metrics {
            units_per_em: upem,
            ascent: f64::from(face.ascender()) * scale,
            descent: f64::from(face.descender()) * scale,
            cap_height: face
                .capital_height()
                .map(|v| f64::from(v) * scale)
                .unwrap_or(700.0),
            bbox: [
                f64::from(bbox.x_min) * scale,
                f64::from(bbox.y_min) * scale,
                f64::from(bbox.x_max) * scale,
                f64::from(bbox.y_max) * scale,
            ],
            advance: f64::from(advance) * scale,
            italic_angle: face.italic_angle().into(),
        };

        Ok(Self {
            data,
            ps_name: sanitize_ps_name(&ps_name),
            metrics,
        })
    }

    /// The bundled JetBrains Mono regular face.
    pub fn bundled_regular() -> Self {
        Self::from_bytes(JETBRAINS_MONO_REGULAR).expect("bundled regular font is valid")
    }

    /// The bundled JetBrains Mono bold face.
    pub fn bundled_bold() -> Self {
        Self::from_bytes(JETBRAINS_MONO_BOLD).expect("bundled bold font is valid")
    }

    /// The bundled JetBrains Mono italic face.
    pub fn bundled_italic() -> Self {
        Self::from_bytes(JETBRAINS_MONO_ITALIC).expect("bundled italic font is valid")
    }

    /// The bundled JetBrains Mono bold italic face.
    pub fn bundled_bold_italic() -> Self {
        Self::from_bytes(JETBRAINS_MONO_BOLD_ITALIC).expect("bundled bold italic font is valid")
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn ps_name(&self) -> &str {
        &self.ps_name
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Width of one monospace cell at `font_size`, in points.
    pub fn cell_width(&self, font_size: f64) -> f64 {
        self.metrics.advance / 1000.0 * font_size
    }

    fn face(&self) -> Face<'_> {
        Face::parse(&self.data, 0).expect("font validated on construction")
    }

    /// Opens a short-lived view of the font for glyph lookups.
    pub fn shaper(&self) -> Shaper<'_> {
        let face = self.face();
        let fallback = face
            .glyph_index('?')
            .or_else(|| face.glyph_index(' '))
            .unwrap_or(GlyphId(0));
        Shaper {
            face,
            metrics: self.metrics,
            fallback,
        }
    }
}

/// Maps characters to glyph ids for a single font.
pub struct Shaper<'a> {
    face: Face<'a>,
    metrics: Metrics,
    fallback: GlyphId,
}

impl Shaper<'_> {
    /// Glyph id for `ch`, falling back to `?` when the font lacks a glyph.
    pub fn glyph(&self, ch: char) -> u16 {
        match self.face.glyph_index(ch) {
            Some(g) if g.0 != 0 => g.0,
            _ => self.fallback.0,
        }
    }

    /// Advance width of a glyph in PDF glyph space (1/1000 em).
    pub fn advance(&self, glyph: u16) -> f64 {
        let scale = 1000.0 / self.metrics.units_per_em;
        self.face
            .glyph_hor_advance(GlyphId(glyph))
            .map(|a| f64::from(a) * scale)
            .unwrap_or(self.metrics.advance)
    }
}

/// PostScript names may not contain whitespace or PDF delimiters.
fn sanitize_ps_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| {
            c.is_ascii_graphic()
                && !matches!(
                    c,
                    '(' | ')' | '<' | '>' | '[' | ']' | '{' | '}' | '/' | '%' | '#'
                )
        })
        .collect();
    if cleaned.is_empty() {
        "EmbeddedFont".to_string()
    } else {
        cleaned
    }
}

/// The four faces used for body text: upright, bold, italic and bold italic.
#[derive(Clone)]
pub struct FontSet {
    pub regular: Font,
    pub bold: Font,
    pub italic: Font,
    pub bold_italic: Font,
}

impl FontSet {
    /// The bundled JetBrains Mono family.
    pub fn bundled() -> Self {
        Self {
            regular: Font::bundled_regular(),
            bold: Font::bundled_bold(),
            italic: Font::bundled_italic(),
            bold_italic: Font::bundled_bold_italic(),
        }
    }

    /// Picks a face for a weight/slant combination.
    pub fn pick(&self, bold: bool, italic: bool) -> &Font {
        match (bold, italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }

    pub fn faces(&self) -> [&Font; 4] {
        [&self.regular, &self.bold, &self.italic, &self.bold_italic]
    }
}

impl Default for FontSet {
    fn default() -> Self {
        Self::bundled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_load_with_sane_metrics() {
        for font in FontSet::bundled().faces() {
            let m = font.metrics();
            assert!(m.ascent > 0.0 && m.descent < 0.0, "{m:?}");
            assert!(
                font.ps_name().starts_with("JetBrainsMono"),
                "{}",
                font.ps_name()
            );
            assert!(!font.ps_name().contains(' '));
        }
    }

    #[test]
    fn every_face_shares_the_same_advance_so_columns_line_up() {
        let set = FontSet::bundled();
        let widths: Vec<f64> = set.faces().iter().map(|f| f.metrics().advance).collect();
        assert!(
            widths.windows(2).all(|w| (w[0] - w[1]).abs() < 0.5),
            "faces disagree on advance width: {widths:?}"
        );
        // JetBrains Mono is a 600/1000 em monospace face.
        assert!((widths[0] - 600.0).abs() < 1.0, "{widths:?}");
    }

    #[test]
    fn pick_selects_the_matching_face() {
        let set = FontSet::bundled();
        assert_eq!(set.pick(false, false).ps_name(), set.regular.ps_name());
        assert!(set.pick(true, false).ps_name().contains("Bold"));
        assert!(set.pick(false, true).ps_name().contains("Italic"));
        let bi = set.pick(true, true).ps_name();
        assert!(bi.contains("Bold") && bi.contains("Italic"), "{bi}");
    }

    #[test]
    fn shaper_maps_unicode_and_falls_back() {
        let font = Font::bundled_regular();
        let sh = font.shaper();
        assert_ne!(sh.glyph('A'), 0);
        // Cyrillic is covered by JetBrains Mono.
        assert_ne!(sh.glyph('\u{416}'), 0);
        assert_ne!(sh.glyph('\u{416}'), sh.glyph('A'));
        // CJK is not; it must degrade to the '?' glyph rather than glyph 0.
        assert_eq!(sh.glyph('\u{6f22}'), sh.glyph('?'));
    }

    #[test]
    fn cell_width_scales_with_font_size() {
        let font = Font::bundled_regular();
        assert!((font.cell_width(10.0) - 6.0).abs() < 0.02);
        assert!((font.cell_width(20.0) - font.cell_width(10.0) * 2.0).abs() < 1e-9);
    }
}
