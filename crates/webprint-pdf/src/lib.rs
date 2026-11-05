//! PDF generation for webprint jobs.
//!
//! ```
//! use webprint_pdf::{PrintJob, Renderer};
//!
//! let renderer = Renderer::new();
//! let out = renderer.render(&PrintJob {
//!     team: "Team Hello",
//!     site: "main-hall",
//!     label: "42",
//!     submitted_at: "2026-11-03 15:00:00",
//!     content: "fn main() {}\n",
//!     language: None,
//! });
//! assert_eq!(out.pages, 1);
//! assert!(out.bytes.starts_with(b"%PDF-1.7"));
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

pub mod font;
#[cfg(feature = "highlight")]
pub mod highlight;
pub mod layout;
pub mod style;
mod writer;

pub use font::{Font, FontSet};
pub use style::{Span, TextStyle};

use font::Shaper;
use writer::{glyph_string, num, text_string, Pdf, Ref};

/// Errors that can occur while building a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The font could not be parsed or lacks required metrics.
    Font(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Font(msg) => write!(f, "font error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Converts millimetres to PDF points (1 pt = 1/72 in).
pub const fn mm(v: f64) -> f64 {
    v * 72.0 / 25.4
}

/// Page size and padding, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeometry {
    pub width: f64,
    pub height: f64,
    /// Padding applied to all four sides.
    pub margin: f64,
}

impl PageGeometry {
    /// A4 portrait (210 x 297 mm) with 1.25 cm padding.
    pub const fn a4() -> Self {
        Self {
            width: mm(210.0),
            height: mm(297.0),
            margin: mm(12.5),
        }
    }

    fn content_width(&self) -> f64 {
        self.width - 2.0 * self.margin
    }
}

impl Default for PageGeometry {
    fn default() -> Self {
        Self::a4()
    }
}

/// Typography and limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// Body font size in points.
    pub font_size: f64,
    /// Line height as a multiple of the body font size.
    pub line_spacing: f64,
    /// Tab stop width in columns.
    pub tab_width: usize,
    /// Print a line-number gutter.
    pub line_numbers: bool,
    /// Apply syntax highlighting when a language is known.
    pub highlight: bool,
    /// Most lines the team name may occupy before it is cut short.
    pub max_title_lines: usize,
    pub footer_font_size: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_size: 10.0,
            line_spacing: 1.25,
            tab_width: 4,
            line_numbers: true,
            highlight: true,
            max_title_lines: 3,
            footer_font_size: 7.5,
        }
    }
}

/// The content of one print job.
#[derive(Clone, Copy, Debug)]
pub struct PrintJob<'a> {
    /// Team name, printed as the document heading.
    pub team: &'a str,
    /// Site slug, shown in the metadata line.
    pub site: &'a str,
    /// Short job identifier, shown in the metadata line and footer.
    pub label: &'a str,
    /// Human-readable submission time.
    pub submitted_at: &'a str,
    /// The text to print.
    pub content: &'a str,
    /// Language token for highlighting: `None` or `"auto"` detects it,
    /// `"plain"` disables highlighting.
    pub language: Option<&'a str>,
}

/// Estimated size of a print job on the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// Pages the job will take.
    pub pages: usize,
    /// Line numbers after tab expansion and wrapping.
    pub lines: usize,
}

/// A rendered document.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub bytes: Vec<u8>,
    pub pages: usize,
    /// Physical lines laid out (after tab expansion and wrapping).
    pub lines: usize,
    /// Name of the grammar used for highlighting, if any.
    pub language: Option<String>,
}

/// Derived page metrics, in points.
#[derive(Clone, Copy, Debug)]
struct Frame {
    /// Total columns across the content width.
    columns: usize,
    /// Columns taken by the line-number gutter.
    gutter: usize,
    cell: f64,
    leading: f64,
    /// Baseline of the first body line. The heading is repeated on every page,
    /// so this is the same on all of them.
    first_baseline: f64,
    /// Baseline of the first heading line; later ones step down by `leading`.
    title_baseline: f64,
    rule_y: f64,
    footer_baseline: f64,
    lines_per_page: usize,
}

impl Frame {
    /// Columns available for text.
    fn text_columns(&self) -> usize {
        self.columns.saturating_sub(self.gutter).max(1)
    }

    /// X position of the first text column.
    fn text_x(&self, margin: f64) -> f64 {
        margin + self.gutter as f64 * self.cell
    }
}

/// Renders print jobs to PDF.
pub struct Renderer {
    fonts: FontSet,
    #[cfg(feature = "highlight")]
    highlighter: Option<highlight::Highlighter>,
    pub geometry: PageGeometry,
    pub style: Style,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            fonts: FontSet::bundled(),
            #[cfg(feature = "highlight")]
            highlighter: None,
            geometry: PageGeometry::a4(),
            style: Style::default(),
        }
    }

    /// Loads the syntax grammars for highlighting.
    #[cfg(feature = "highlight")]
    pub fn with_highlighting() -> Self {
        Self {
            highlighter: Some(highlight::Highlighter::new()),
            ..Self::new()
        }
    }

    /// Uses a pre-built highlighter, e.g. one with extra grammars loaded.
    #[cfg(feature = "highlight")]
    pub fn with_highlighter(mut self, highlighter: highlight::Highlighter) -> Self {
        self.highlighter = Some(highlighter);
        self
    }

    /// Languages that can be requested, for building a menu.
    #[cfg(feature = "highlight")]
    pub fn languages(&self) -> Vec<highlight::Language> {
        self.highlighter
            .as_ref()
            .map(|h| h.languages())
            .unwrap_or_default()
    }

    /// Overrides the font family.
    pub fn with_fonts(mut self, fonts: FontSet) -> Self {
        self.fonts = fonts;
        self
    }

    pub fn with_geometry(mut self, geometry: PageGeometry) -> Self {
        self.geometry = geometry;
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Lines of body text a page holds. Every page carries the heading, so
    /// they all hold the same number.
    pub fn lines_per_page(&self, logical_lines: usize) -> usize {
        self.frame(logical_lines, 1).lines_per_page
    }

    /// Total character columns across the content width, gutter included.
    pub fn columns(&self) -> usize {
        self.frame(1, 1).columns
    }

    /// Columns available for text once the gutter is accounted for, for a job
    /// of `logical_lines` lines.
    pub fn text_columns(&self, logical_lines: usize) -> usize {
        self.frame(logical_lines, 1).text_columns()
    }

    fn frame(&self, logical_lines: usize, title_lines: usize) -> Frame {
        let g = &self.geometry;
        let s = &self.style;

        let cell = self.fonts.regular.cell_width(s.font_size);
        let columns = ((g.content_width() / cell).floor() as usize).max(1);
        let gutter = layout::gutter_cols(logical_lines, s.line_numbers).min(columns / 2);
        let leading = s.font_size * s.line_spacing;

        let metrics = self.fonts.regular.metrics();
        let ascent = metrics.ascent / 1000.0;
        let descent = -metrics.descent / 1000.0;
        let top = g.height - g.margin;

        // The heading is set at body size, so it shares the body's metrics.
        let title_baseline = top - s.font_size * ascent;
        let title_last = title_baseline - (title_lines.max(1) - 1) as f64 * leading;
        let rule_y = title_last - s.font_size * 0.75;

        let footer_baseline = g.margin + s.footer_font_size * descent;
        let body_bottom = footer_baseline + s.footer_font_size * 1.1 + s.font_size * descent;

        let first_baseline = rule_y - s.font_size * (ascent + 0.55);

        let fit = |first: f64| -> usize {
            if first < body_bottom {
                1
            } else {
                ((first - body_bottom) / leading).floor() as usize + 1
            }
        };

        Frame {
            columns,
            gutter,
            cell,
            leading,
            first_baseline,
            title_baseline,
            rule_y,
            footer_baseline,
            lines_per_page: fit(first_baseline),
        }
    }

    /// Measures `job` size on paper without building the document.
    pub fn measure(&self, job: &PrintJob<'_>) -> Measured {
        let logical = layout::normalize(job.content, self.style.tab_width);
        let paginated = self.paginate(job.team, &plain_lines(&logical), logical.len());
        Measured {
            pages: paginated.pages.len(),
            lines: paginated.wrapped_lines,
        }
    }

    pub fn render(&self, job: &PrintJob<'_>) -> Rendered {
        let logical = layout::normalize(job.content, self.style.tab_width);
        let (styled, language) = self.style_lines(&logical, job.language);

        let columns = self.frame(logical.len(), 1).columns;
        let title = layout::wrap_words(job.team, columns, self.style.max_title_lines);
        let frame = self.frame(logical.len(), title.len());
        let paginated = self.paginate(job.team, &styled, logical.len());

        let bytes = self.emit(job, &frame, &paginated, &title);
        Rendered {
            bytes,
            pages: paginated.pages.len(),
            lines: paginated.wrapped_lines,
            language,
        }
    }

    /// Wraps and paginates styled lines starting with header.
    fn paginate(
        &self,
        team: &str,
        styled: &[Vec<Span>],
        logical_lines: usize,
    ) -> layout::Paginated {
        let columns = self.frame(logical_lines, 1).columns;
        let title = layout::wrap_words(team, columns, self.style.max_title_lines);
        let frame = self.frame(logical_lines, title.len());
        let wrapped = layout::wrap(styled, frame.text_columns());
        layout::paginate(wrapped, logical_lines, frame.lines_per_page)
    }

    /// Applies highlighting, returning the styled lines and the grammar name.
    #[cfg(feature = "highlight")]
    fn style_lines(
        &self,
        logical: &[String],
        language: Option<&str>,
    ) -> (Vec<Vec<Span>>, Option<String>) {
        let Some(highlighter) = self.highlighter.as_ref().filter(|_| self.style.highlight) else {
            return (plain_lines(logical), None);
        };

        let token = language.unwrap_or(highlight::AUTO).trim();
        let syntax = if token.eq_ignore_ascii_case(highlight::PLAIN_TEXT) {
            None
        } else if token.is_empty() || token.eq_ignore_ascii_case(highlight::AUTO) {
            highlighter.detect(&logical.join("\n"))
        } else {
            highlighter.resolve(token)
        };

        let name = syntax.map(|s| s.name.clone());
        (highlighter.highlight(logical, syntax), name)
    }

    #[cfg(not(feature = "highlight"))]
    fn style_lines(
        &self,
        logical: &[String],
        _language: Option<&str>,
    ) -> (Vec<Vec<Span>>, Option<String>) {
        (plain_lines(logical), None)
    }

    fn emit(
        &self,
        job: &PrintJob<'_>,
        frame: &Frame,
        pages: &layout::Paginated,
        title: &[String],
    ) -> Vec<u8> {
        let shapers = Shapers::new(&self.fonts);
        let mut used = UsedGlyphs::default();

        let mut pdf = Pdf::new();
        let catalog = pdf.alloc();
        let pages_ref = pdf.alloc();
        let info = pdf.alloc();

        let g = &self.geometry;
        let s = &self.style;
        let total = pages.pages.len();
        let digits = layout::number_digits(pages.logical_lines);
        let text_x = frame.text_x(g.margin);
        let mut content_refs = Vec::with_capacity(total);

        for (idx, lines) in pages.pages.iter().enumerate() {
            let mut canvas = Canvas::default();
            // The heading goes on every sheet: pages get separated at a shared
            // printer, and a page that cannot be traced back to a team is of
            // no use to anyone.
            self.draw_header(&mut canvas, frame, title, &shapers, &mut used);
            let first = frame.first_baseline;

            // Line-number gutter: grey numbers and a hairline rule, so the
            // numbers never get confused with the code itself.
            if s.line_numbers && frame.gutter > 0 && !lines.is_empty() {
                for (row, line) in lines.iter().enumerate() {
                    let Some(number) = line.number else { continue };
                    let text = number.to_string();
                    let x =
                        g.margin + (digits - text.chars().count().min(digits)) as f64 * frame.cell;
                    canvas.text(
                        FontKey::Regular,
                        s.font_size,
                        x,
                        first - row as f64 * frame.leading,
                        &text,
                        &shapers,
                        &mut used,
                        Some(0.55),
                    );
                }
                let rule_x = g.margin + (digits as f64 + 1.5) * frame.cell;
                let metrics = self.fonts.regular.metrics();
                canvas.rule(
                    rule_x,
                    first + s.font_size * (metrics.ascent / 1000.0) * 0.85,
                    rule_x,
                    first - (lines.len() - 1) as f64 * frame.leading
                        + s.font_size * (metrics.descent / 1000.0),
                    0.4,
                    0.78,
                );
            }

            for (row, line) in lines.iter().enumerate() {
                let baseline = first - row as f64 * frame.leading;
                let mut col = 0usize;
                for span in &line.spans {
                    let width = span.text.chars().count();
                    // Whitespace draws nothing; just advance the cursor.
                    if !span.text.trim().is_empty() {
                        canvas.text(
                            FontKey::for_style(span.style),
                            s.font_size,
                            text_x + col as f64 * frame.cell,
                            baseline,
                            &span.text,
                            &shapers,
                            &mut used,
                            Some(span.style.gray).filter(|g| *g > 0.0),
                        );
                    }
                    col += width;
                }
            }

            self.draw_footer(&mut canvas, job, frame, idx + 1, total, &shapers, &mut used);

            let content_ref = pdf.alloc();
            pdf.set_stream(content_ref, "", canvas.finish().as_bytes(), true);
            content_refs.push(content_ref);
        }

        // Only embed the faces this job actually used.
        let mut resources = String::new();
        for key in FontKey::ALL {
            let glyphs = used.get(key);
            if glyphs.is_empty() {
                continue;
            }
            let refs = FontRefs::alloc(&mut pdf);
            refs.emit(
                &mut pdf,
                self.fonts.pick(key.bold(), key.italic()),
                glyphs,
                shapers.get(key),
            );
            let _ = write!(resources, "/{} {} ", key.name(), refs.type0);
        }

        let page_refs: Vec<Ref> = content_refs
            .into_iter()
            .map(|content_ref| {
                let page_ref = pdf.alloc();
                pdf.set(
                    page_ref,
                    format!(
                        "<< /Type /Page /Parent {pages_ref} /MediaBox [0 0 {} {}] \
                         /Resources << /Font << {} >> /ProcSet [/PDF /Text] >> \
                         /Contents {content_ref} >>",
                        num(g.width),
                        num(g.height),
                        resources.trim_end(),
                    ),
                );
                page_ref
            })
            .collect();

        let kids = page_refs
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        pdf.set(
            pages_ref,
            format!(
                "<< /Type /Pages /Kids [{kids}] /Count {} >>",
                page_refs.len()
            ),
        );
        pdf.set(catalog, format!("<< /Type /Catalog /Pages {pages_ref} >>"));
        pdf.set(
            info,
            format!(
                "<< /Title {} /Author {} /Subject {} /Creator (webprint) /Producer (webprint-pdf) >>",
                text_string(&format!("Print job {} - {}", job.label, job.team)),
                text_string(job.team),
                text_string(&format!("site {}", job.site)),
            ),
        );

        pdf.finish(catalog, Some(info))
    }

    fn draw_header(
        &self,
        canvas: &mut Canvas,
        frame: &Frame,
        title: &[String],
        shapers: &Shapers<'_>,
        used: &mut UsedGlyphs,
    ) {
        let g = &self.geometry;
        let s = &self.style;

        // The team name is set in bold at body size, over as many lines as it
        // needs: contest team names run long.
        for (row, line) in title.iter().enumerate() {
            canvas.text(
                FontKey::Bold,
                s.font_size,
                g.margin,
                frame.title_baseline - row as f64 * frame.leading,
                line,
                shapers,
                used,
                None,
            );
        }

        canvas.rule(
            g.margin,
            frame.rule_y,
            g.width - g.margin,
            frame.rule_y,
            0.6,
            0.55,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_footer(
        &self,
        canvas: &mut Canvas,
        job: &PrintJob<'_>,
        frame: &Frame,
        page: usize,
        total: usize,
        shapers: &Shapers<'_>,
        used: &mut UsedGlyphs,
    ) {
        let g = &self.geometry;
        let s = &self.style;
        let cell = self.fonts.regular.cell_width(s.footer_font_size);

        // Left: when the job was submitted, in the site's own time zone.
        canvas.text(
            FontKey::Regular,
            s.footer_font_size,
            g.margin,
            frame.footer_baseline,
            job.submitted_at,
            shapers,
            used,
            Some(0.45),
        );

        // Right: which sheet this is, for reassembling a dropped stack.
        let counter = format!("Page {page} of {total}");
        canvas.text(
            FontKey::Regular,
            s.footer_font_size,
            g.width - g.margin - counter.chars().count() as f64 * cell,
            frame.footer_baseline,
            &counter,
            shapers,
            used,
            Some(0.45),
        );
    }
}

fn plain_lines(logical: &[String]) -> Vec<Vec<Span>> {
    logical
        .iter()
        .map(|l| vec![Span::plain(l.clone())])
        .collect()
}

/// The four body faces, as PDF resource names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FontKey {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontKey {
    const ALL: [FontKey; 4] = [
        FontKey::Regular,
        FontKey::Bold,
        FontKey::Italic,
        FontKey::BoldItalic,
    ];

    fn for_style(style: TextStyle) -> Self {
        match (style.bold, style.italic) {
            (false, false) => FontKey::Regular,
            (true, false) => FontKey::Bold,
            (false, true) => FontKey::Italic,
            (true, true) => FontKey::BoldItalic,
        }
    }

    fn name(self) -> &'static str {
        match self {
            FontKey::Regular => "F1",
            FontKey::Bold => "F2",
            FontKey::Italic => "F3",
            FontKey::BoldItalic => "F4",
        }
    }

    fn bold(self) -> bool {
        matches!(self, FontKey::Bold | FontKey::BoldItalic)
    }

    fn italic(self) -> bool {
        matches!(self, FontKey::Italic | FontKey::BoldItalic)
    }

    fn index(self) -> usize {
        match self {
            FontKey::Regular => 0,
            FontKey::Bold => 1,
            FontKey::Italic => 2,
            FontKey::BoldItalic => 3,
        }
    }
}

/// Glyph lookups for each face, parsed once per document.
struct Shapers<'a> {
    inner: [Shaper<'a>; 4],
}

impl<'a> Shapers<'a> {
    fn new(fonts: &'a FontSet) -> Self {
        Self {
            inner: [
                fonts.regular.shaper(),
                fonts.bold.shaper(),
                fonts.italic.shaper(),
                fonts.bold_italic.shaper(),
            ],
        }
    }

    fn get(&self, key: FontKey) -> &Shaper<'a> {
        &self.inner[key.index()]
    }
}

/// Glyphs referenced per face, for `/W` and `/ToUnicode`.
#[derive(Default)]
struct UsedGlyphs {
    inner: [BTreeMap<u16, char>; 4],
}

impl UsedGlyphs {
    fn get(&self, key: FontKey) -> &BTreeMap<u16, char> {
        &self.inner[key.index()]
    }

    fn get_mut(&mut self, key: FontKey) -> &mut BTreeMap<u16, char> {
        &mut self.inner[key.index()]
    }
}

/// The five indirect objects that make up one embedded CID font.
struct FontRefs {
    type0: Ref,
    cid: Ref,
    descriptor: Ref,
    file: Ref,
    to_unicode: Ref,
}

impl FontRefs {
    fn alloc(pdf: &mut Pdf) -> Self {
        Self {
            type0: pdf.alloc(),
            cid: pdf.alloc(),
            descriptor: pdf.alloc(),
            file: pdf.alloc(),
            to_unicode: pdf.alloc(),
        }
    }

    fn emit(&self, pdf: &mut Pdf, font: &Font, used: &BTreeMap<u16, char>, shaper: &Shaper<'_>) {
        let name = font.ps_name();
        let m = font.metrics();

        pdf.set(
            self.type0,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H \
                 /DescendantFonts [{}] /ToUnicode {} >>",
                self.cid, self.to_unicode
            ),
        );

        // Per-glyph widths for the glyphs actually used; /DW covers the rest.
        let mut widths = String::new();
        for glyph in used.keys() {
            let _ = write!(widths, "{glyph} [{}] ", num(shaper.advance(*glyph)));
        }
        pdf.set(
            self.cid,
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {} /DW {} /W [{}] /CIDToGIDMap /Identity >>",
                self.descriptor,
                num(m.advance),
                widths.trim_end(),
            ),
        );

        // Flags: FixedPitch (1) | Nonsymbolic (32), plus Italic (64) when slanted.
        let flags = if m.italic_angle != 0.0 { 33 | 64 } else { 33 };
        pdf.set(
            self.descriptor,
            format!(
                "<< /Type /FontDescriptor /FontName /{name} /Flags {flags} \
                 /FontBBox [{} {} {} {}] /ItalicAngle {} /Ascent {} /Descent {} \
                 /CapHeight {} /StemV 80 /FontFile2 {} >>",
                num(m.bbox[0]),
                num(m.bbox[1]),
                num(m.bbox[2]),
                num(m.bbox[3]),
                num(m.italic_angle),
                num(m.ascent),
                num(m.descent),
                num(m.cap_height),
                self.file,
            ),
        );

        pdf.set_stream(
            self.file,
            &format!("/Length1 {}", font.data().len()),
            font.data(),
            true,
        );
        pdf.set_stream(self.to_unicode, "", to_unicode_cmap(used).as_bytes(), true);
    }
}

/// Builds a `/ToUnicode` CMap mapping glyph ids back to characters, so the
/// generated PDF can be searched, copied from and text-extracted.
fn to_unicode_cmap(used: &BTreeMap<u16, char>) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = used.iter().map(|(g, c)| (*g, *c)).collect();
    // A bfchar section may hold at most 100 entries (PDF 32000-1, 9.10.3).
    for chunk in entries.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (glyph, ch) in chunk {
            let mut utf16 = String::new();
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                let _ = write!(utf16, "{unit:04X}");
            }
            let _ = writeln!(out, "<{glyph:04X}> <{utf16}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

/// Accumulates one page's content stream.
#[derive(Default)]
struct Canvas {
    out: String,
}

impl Canvas {
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        key: FontKey,
        size: f64,
        x: f64,
        y: f64,
        s: &str,
        shapers: &Shapers<'_>,
        used: &mut UsedGlyphs,
        gray: Option<f64>,
    ) {
        if s.is_empty() {
            return;
        }
        let glyphs = shape(s, shapers.get(key), used.get_mut(key));
        self.out.push_str("q\n");
        if let Some(g) = gray {
            let _ = writeln!(self.out, "{} g", num(g));
        }
        let _ = writeln!(
            self.out,
            "BT /{} {} Tf 1 0 0 1 {} {} Tm {} Tj ET",
            key.name(),
            num(size),
            num(x),
            num(y),
            glyph_string(&glyphs)
        );
        self.out.push_str("Q\n");
    }

    fn rule(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, gray: f64) {
        let _ = writeln!(
            self.out,
            "q {} w {} G {} {} m {} {} l S Q",
            num(width),
            num(gray),
            num(x1),
            num(y1),
            num(x2),
            num(y2)
        );
    }

    fn finish(self) -> String {
        self.out
    }
}

fn shape(s: &str, shaper: &Shaper<'_>, used: &mut BTreeMap<u16, char>) -> Vec<u16> {
    s.chars()
        .map(|ch| {
            let glyph = shaper.glyph(ch);
            used.entry(glyph).or_insert(ch);
            glyph
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job<'a>(content: &'a str) -> PrintJob<'a> {
        PrintJob {
            team: "Team",
            site: "site",
            label: "1",
            submitted_at: "2026-11-03 15:00:00",
            content,
            language: None,
        }
    }

    #[test]
    fn frame_reserves_a_gutter_only_when_numbering() {
        let numbered = Renderer::new();
        assert!(numbered.style.line_numbers);
        assert!(numbered.text_columns(10) < numbered.columns());

        let plain = Renderer::new().with_style(Style {
            line_numbers: false,
            ..Style::default()
        });
        assert_eq!(plain.text_columns(10), plain.columns());
    }

    #[test]
    fn only_used_faces_are_embedded() {
        // Plain text uses the regular face for the body and bold for the title.
        let out = Renderer::new().render(&job("plain body text"));
        let text = String::from_utf8_lossy(&out.bytes);
        assert!(text.contains("/F1 "), "regular face missing");
        assert!(text.contains("/F2 "), "bold face (title) missing");
        assert!(!text.contains("/F3 "), "italic face should not be embedded");
        assert!(!text.contains("/F4 "), "bold italic should not be embedded");
    }

    #[test]
    fn unused_font_objects_are_never_allocated() {
        // `Pdf::finish` panics on an allocated-but-unset object, so a clean
        // render proves the lazy allocation is consistent.
        let out = Renderer::new().render(&job("x"));
        assert!(out.bytes.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn a_narrow_page_still_leaves_room_for_text() {
        let tiny = Renderer::new().with_geometry(PageGeometry {
            width: mm(40.0),
            height: mm(60.0),
            margin: mm(12.5),
        });
        assert!(tiny.text_columns(100_000) >= 1);
        let out = tiny.render(&job("hello world this will wrap a lot"));
        assert!(out.pages >= 1);
    }
}
