//! End-to-end checks on generated PDFs, verified with the poppler tools
//! (`pdfinfo`, `pdftotext`, `pdffonts`) when they are available.

use std::process::Command;

use webprint_pdf::{mm, PageGeometry, PrintJob, Renderer, Style};

fn job<'a>(content: &'a str, team: &'a str) -> PrintJob<'a> {
    PrintJob {
        team,
        site: "main-hall",
        label: "42",
        submitted_at: "2026-11-03 15:04:05",
        content,
        language: None,
    }
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-v")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
}

fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("webprint-pdf-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Extracted text, with layout preserved.
fn pdftotext(path: &std::path::Path) -> String {
    let out = Command::new("pdftotext")
        .args(["-layout", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .expect("pdftotext");
    assert!(out.status.success(), "pdftotext failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn single_page_job_has_header_and_footer() {
    let r = Renderer::new();
    let out = r.render(&job("hello world\nsecond line\n", "Team Alpha"));
    assert_eq!(out.pages, 1);
    assert_eq!(out.lines, 2);
    assert!(!out.truncated);
    assert!(out.bytes.starts_with(b"%PDF-1.7"));
    assert!(out.bytes.ends_with(b"%%EOF\n"));

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("single.pdf", &out.bytes));
    assert!(text.contains("Team Alpha"), "{text}");
    assert!(text.contains("hello world"), "{text}");
    assert!(text.contains("second line"), "{text}");
    // The footer carries the submission date and time, and the page counter.
    assert!(text.contains("2026-11-03 15:04:05"), "{text}");
    assert!(text.contains("Page 1 of 1"), "{text}");
    assert!(
        !text.contains("main-hall"),
        "the site must not be printed: {text}"
    );
    assert!(
        !text.contains("job 42"),
        "the job id must not be printed: {text}"
    );

    // The heading holds the team name alone: the line under it is the body.
    let heading_line = text.lines().position(|l| l.contains("Team Alpha")).unwrap();
    let after: Vec<&str> = text
        .lines()
        .skip(heading_line + 1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        after[0].contains("hello world"),
        "the body should follow the heading directly, got {:?}",
        after[0]
    );
}

#[test]
fn content_spans_multiple_pages_in_order() {
    let r = Renderer::new();
    let content: String = (1..=200).map(|i| format!("line-{i:03}\n")).collect();
    let out = r.render(&job(&content, "Team Beta"));
    assert!(out.pages >= 3, "expected several pages, got {}", out.pages);
    assert_eq!(out.lines, 200);
    assert!(!out.truncated);

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("multi.pdf", &out.bytes));
    for i in [1, 57, 60, 120, 200] {
        assert!(text.contains(&format!("line-{i:03}")), "missing line {i}");
    }
    let first = text.find("line-001").unwrap();
    let last = text.find("line-200").unwrap();
    assert!(first < last, "lines are out of order");
    // Every sheet identifies its team and carries the timestamp and counter,
    // so a page separated from the stack can still be handed back.
    assert_eq!(text.matches("Team Beta").count(), out.pages, "{text}");
    assert_eq!(text.matches("2026-11-03 15:04:05").count(), out.pages);
    for page in 1..=out.pages {
        assert!(
            text.contains(&format!("Page {page} of {}", out.pages)),
            "missing footer for page {page}"
        );
    }
}

#[test]
fn every_page_repeats_the_heading() {
    let r = Renderer::new();
    let content: String = (1..=400).map(|i| format!("line-{i:03}\n")).collect();
    let team = "B052 Kazakh-British TU: Master ka4ka (Perdebay, Syrgabayev, Torekeldi)";
    let out = r.render(&job(&content, team));
    assert!(out.pages >= 5, "expected several pages, got {}", out.pages);

    if !have("pdftotext") {
        return;
    }
    let path = write_tmp("repeated-heading.pdf", &out.bytes);
    // Page by page, so a heading cannot be missing from the middle.
    for page in 1..=out.pages {
        let out = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8", "-f"])
            .arg(page.to_string())
            .arg("-l")
            .arg(page.to_string())
            .arg(&path)
            .arg("-")
            .output()
            .expect("pdftotext");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("Kazakh-British TU"),
            "page {page} has no heading:\n{text}"
        );
        assert!(
            text.contains("Torekeldi"),
            "page {page} heading is cut:\n{text}"
        );
    }
}

#[test]
fn pages_hold_the_same_number_of_lines() {
    // The heading is on every page, so no page is taller than another.
    let r = Renderer::new();
    let content: String = (1..=400).map(|i| format!("line-{i:03}\n")).collect();
    let out = r.render(&job(&content, "Team Even"));

    if !have("pdftotext") {
        return;
    }
    let path = write_tmp("even-pages.rs.pdf", &out.bytes);
    let count_on = |page: usize| -> usize {
        let out = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8", "-f"])
            .arg(page.to_string())
            .arg("-l")
            .arg(page.to_string())
            .arg(&path)
            .arg("-")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("line-"))
            .count()
    };
    let first = count_on(1);
    assert!(first > 10, "suspiciously few lines on page 1: {first}");
    // Every page but the last holds exactly as many lines.
    for page in 2..out.pages {
        assert_eq!(count_on(page), first, "page {page} differs from page 1");
    }
}

#[test]
fn page_geometry_is_a4_with_1_25cm_padding() {
    let r = Renderer::new();
    let out = r.render(&job("x", "Team Gamma"));
    assert_eq!(r.geometry, PageGeometry::a4());
    // 210x297mm in points.
    assert!((r.geometry.width - 595.2756).abs() < 0.01);
    assert!((r.geometry.height - 841.8898).abs() < 0.01);
    assert!((r.geometry.margin - mm(12.5)).abs() < 1e-9);

    if !have("pdfinfo") {
        return;
    }
    let path = write_tmp("geom.pdf", &out.bytes);
    let info = Command::new("pdfinfo").arg(&path).output().unwrap();
    let info = String::from_utf8_lossy(&info.stdout);
    let size_line = info.lines().find(|l| l.starts_with("Page size:")).unwrap();
    assert!(size_line.contains("595.276"), "{size_line}");
    assert!(size_line.contains("841.89"), "{size_line}");
    assert!(size_line.contains("A4"), "{size_line}");
    let pages_line = info.lines().find(|l| l.starts_with("Pages:")).unwrap();
    assert_eq!(pages_line.split_whitespace().nth(1), Some("1"), "{info}");
}

#[test]
fn jetbrains_mono_is_embedded_and_nothing_else() {
    let r = Renderer::new();
    let out = r.render(&job("fn main() {}", "Team Font"));
    let raw = String::from_utf8_lossy(&out.bytes);
    assert!(raw.contains("/JetBrainsMono"), "font not embedded by name");
    assert!(raw.contains("/FontFile2"), "font program not embedded");

    if !have("pdffonts") {
        return;
    }
    let path = write_tmp("fonts.pdf", &out.bytes);
    let listing = Command::new("pdffonts").arg(&path).output().unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout);
    let rows: Vec<&str> = listing
        .lines()
        .skip(2)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(!rows.is_empty(), "no fonts listed:\n{listing}");
    for row in &rows {
        assert!(row.contains("JetBrainsMono"), "unexpected font: {row}");
        // Column 3 is "emb": every face must be embedded, not referenced.
        assert!(row.contains("yes"), "font is not embedded: {row}");
    }
}

#[test]
fn utf8_team_names_and_content_survive_the_round_trip() {
    let r = Renderer::new();
    let out = r.render(&job(
        "Привет, мир!\nελληνικά\nnaïve café — ±½\n",
        "Команда «Мечта»",
    ));
    assert_eq!(out.pages, 1);

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("utf8.pdf", &out.bytes));
    assert!(text.contains("Команда"), "{text}");
    assert!(text.contains("Привет, мир!"), "{text}");
    assert!(text.contains("ελληνικά"), "{text}");
    assert!(text.contains("naïve café"), "{text}");
}

#[test]
fn long_lines_wrap_instead_of_overflowing() {
    let r = Renderer::new();
    // One logical line, so the gutter is one digit wide.
    let cols = r.text_columns(1);
    let long = "A".repeat(cols * 2 + 5);
    let out = r.render(&job(&long, "Team Wrap"));
    assert_eq!(out.lines, 3, "expected 3 wrapped lines for {cols} columns");

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("wrap.pdf", &out.bytes));
    let runs: Vec<usize> = text
        .lines()
        .map(|l| l.chars().filter(|c| *c == 'A').count())
        .filter(|n| *n > 0)
        .collect();
    assert_eq!(runs, vec![cols, cols, 5], "{runs:?}");
}

#[test]
fn line_numbers_are_printed_by_default() {
    let r = Renderer::new();
    assert!(r.style.line_numbers);
    let out = r.render(&job("alpha\nbeta\ngamma\n", "Team Numbered"));

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("numbered.pdf", &out.bytes));
    // Each body line is preceded by its number in the gutter.
    for (n, word) in [(1, "alpha"), (2, "beta"), (3, "gamma")] {
        let line = text
            .lines()
            .find(|l| l.contains(word))
            .unwrap_or_else(|| panic!("no line with {word}:\n{text}"));
        let trimmed = line.trim_start();
        assert!(
            trimmed.starts_with(&n.to_string()),
            "line {n} is missing its number: {line:?}"
        );
    }
}

#[test]
fn line_numbers_can_be_turned_off() {
    let r = Renderer::new().with_style(Style {
        line_numbers: false,
        ..Style::default()
    });
    let out = r.render(&job("alpha\nbeta\n", "Team Plain"));

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("unnumbered.pdf", &out.bytes));
    let line = text.lines().find(|l| l.contains("alpha")).unwrap();
    assert!(!line.trim_start().starts_with('1'), "{line:?}");
}

#[test]
fn oversized_jobs_are_capped_and_marked() {
    let r = Renderer::new().with_style(Style {
        max_pages: 2,
        ..Style::default()
    });
    let content: String = (0..500).map(|i| format!("{i}\n")).collect();
    let out = r.render(&job(&content, "Team Flood"));
    assert_eq!(out.pages, 2);
    assert!(out.truncated);

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("capped.pdf", &out.bytes));
    assert!(text.contains("truncated"), "{text}");
}

#[test]
fn empty_and_whitespace_jobs_still_produce_a_valid_page() {
    let r = Renderer::new();
    for content in ["", "\n", "   ", "\n\n\n"] {
        let out = r.render(&job(content, "Team Empty"));
        assert_eq!(out.pages, 1, "content {content:?}");
        assert!(out.bytes.ends_with(b"%%EOF\n"));
    }
}

#[test]
fn a_long_team_name_wraps_into_the_heading() {
    // Real contest team names run long; the heading must not be cut off.
    let team = "B052 Kazakh-British TU: Master ka4ka (Perdebay, Syrgabayev, Torekeldi)";
    let r = Renderer::new();
    let out = r.render(&job("body line\n", team));
    assert_eq!(out.pages, 1);

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("long-name.pdf", &out.bytes));
    // Every word of the name survives, across however many lines it took.
    for word in team.split_whitespace() {
        assert!(text.contains(word), "heading lost {word:?}:\n{text}");
    }
    assert!(text.contains("body line"), "{text}");
}

#[test]
fn an_extremely_long_team_name_cannot_eat_the_page() {
    let team = (1..=60)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let r = Renderer::new();
    let out = r.render(&job("body line\n", &team));
    // The heading is capped, so the body still gets printed on page one.
    assert_eq!(out.pages, 1);

    if !have("pdftotext") {
        return;
    }
    let text = pdftotext(&write_tmp("absurd-name.pdf", &out.bytes));
    assert!(text.contains("body line"), "{text}");
    assert!(
        text.contains('…'),
        "the cut heading should be marked: {text}"
    );
}

#[test]
fn the_heading_is_set_at_body_size() {
    // Same size as the code means the heading occupies one body line.
    let r = Renderer::new();
    let short = r.render(&job("x\n", "T"));
    let long = r.render(&job("x\n", &"word ".repeat(40)));
    assert_eq!(short.pages, 1);
    assert_eq!(long.pages, 1);

    if !have("pdftotext") {
        return;
    }
    // Both documents put their body on the page; the longer heading simply
    // pushes it down.
    for (name, out) in [("heading-short.pdf", short), ("heading-long.pdf", long)] {
        let text = pdftotext(&write_tmp(name, &out.bytes));
        assert!(text.contains('x'), "{text}");
    }
}

#[test]
fn measuring_agrees_with_rendering_but_builds_nothing() {
    let r = Renderer::with_highlighting();
    let cases: Vec<String> = vec![
        String::new(),
        "one line\n".into(),
        (1..=200).map(|i| format!("line-{i}\n")).collect(),
        (1..=20)
            .map(|i| format!("int x{i} = {i}; // comment\n"))
            .collect(),
        "a".repeat(5_000),
        "Привет, мир!\nстрока\n".into(),
    ];
    for content in &cases {
        let job = job(
            content,
            "B052 Kazakh-British TU: Master ka4ka (Perdebay, Syrgabayev)",
        );
        let measured = r.measure(&job);
        let rendered = r.render(&job);
        assert!(!rendered.truncated, "{content:.40?}");
        assert_eq!(measured.pages, rendered.pages, "{content:.40?}");
        assert_eq!(measured.lines, rendered.lines, "{content:.40?}");
    }
}

#[test]
fn measuring_reports_the_true_length_of_an_oversized_job() {
    // Rendering caps the document; measuring says how long it really is, so
    // a caller can tell a team by how much it is over.
    let r = Renderer::new().with_style(Style {
        max_pages: 2,
        ..Style::default()
    });
    let content: String = (0..500).map(|i| format!("{i}\n")).collect();
    let measured = r.measure(&job(&content, "Team Flood"));
    let rendered = r.render(&job(&content, "Team Flood"));
    assert_eq!(rendered.pages, 2, "rendering stops at the limit");
    assert!(rendered.truncated);
    assert!(measured.pages > 2, "measuring reports the real length");
    assert_eq!(measured.lines, 500);
}

#[test]
fn a_page_holds_the_lines_it_says_it_does() {
    let r = Renderer::new();
    let per_page = r.lines_per_page(1_000);
    let roomy = Renderer::new().with_style(Style {
        max_pages: 10_000,
        ..Style::default()
    });
    // Exactly one page's worth, then one line more.
    let exact: String = (0..per_page).map(|_| "x\n").collect();
    let one_more: String = (0..per_page + 1).map(|_| "x\n").collect();
    assert_eq!(roomy.measure(&job(&exact, "T")).pages, 1);
    assert_eq!(roomy.measure(&job(&one_more, "T")).pages, 2);
}

#[test]
fn custom_geometry_changes_the_column_count() {
    let narrow = Renderer::new().with_geometry(PageGeometry {
        width: mm(105.0),
        height: mm(148.0),
        margin: mm(12.5),
    });
    assert!(narrow.columns() < Renderer::new().columns());
    let out = narrow.render(&job("hello", "Team A5"));
    assert_eq!(out.pages, 1);
}

#[cfg(feature = "highlight")]
mod highlighting {
    use super::*;

    const CPP: &str = r#"#include <bits/stdc++.h>
using namespace std;

// Reads n, prints the sum.
int main() {
    int n;
    cin >> n;
    long long sum = 0;
    for (int i = 0; i < n; ++i) { sum += i; }
    cout << sum << "\n";
    return 0;
}
"#;

    fn lang_job<'a>(content: &'a str, language: Option<&'a str>) -> PrintJob<'a> {
        PrintJob {
            team: "Team Highlight",
            site: "main-hall",
            label: "7",
            submitted_at: "2026-11-03 15:04:05",
            content,
            language,
        }
    }

    #[test]
    fn code_is_highlighted_and_the_language_is_reported() {
        let r = Renderer::with_highlighting();
        let out = r.render(&lang_job(CPP, Some("cpp")));
        assert_eq!(out.language.as_deref(), Some("C++"));

        let raw = String::from_utf8_lossy(&out.bytes);
        // Keywords pull in the bold face, the comment pulls in the italic one.
        assert!(raw.contains("/F2 "), "bold face missing");
        assert!(raw.contains("/F3 "), "italic face missing (comments)");
    }

    #[test]
    fn highlighting_does_not_alter_the_printed_text() {
        let r = Renderer::with_highlighting();
        let out = r.render(&lang_job(CPP, Some("cpp")));
        if !have("pdftotext") {
            return;
        }
        let text = pdftotext(&write_tmp("highlighted.pdf", &out.bytes));
        for needle in [
            "#include <bits/stdc++.h>",
            "using namespace std;",
            "Reads n, prints the sum.",
            "long long sum = 0;",
            "for (int i = 0; i < n; ++i) { sum += i; }",
            "return 0;",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn the_language_is_detected_when_not_specified() {
        let r = Renderer::with_highlighting();
        assert_eq!(
            r.render(&lang_job(CPP, None)).language.as_deref(),
            Some("C++")
        );
        assert_eq!(
            r.render(&lang_job(CPP, Some("auto"))).language.as_deref(),
            Some("C++")
        );
        let python = "import sys\nfor line in sys.stdin:\n    print(line.strip())\n";
        assert_eq!(
            r.render(&lang_job(python, None)).language.as_deref(),
            Some("Python")
        );
    }

    #[test]
    fn plain_text_disables_highlighting() {
        let r = Renderer::with_highlighting();
        let out = r.render(&lang_job(CPP, Some("plain")));
        assert_eq!(out.language, None);
        let raw = String::from_utf8_lossy(&out.bytes);
        assert!(!raw.contains("/F3 "), "italic face should not be embedded");
    }

    #[test]
    fn prose_is_not_highlighted() {
        let r = Renderer::with_highlighting();
        let out = r.render(&lang_job("Please print this note.\nThank you!\n", None));
        assert_eq!(out.language, None);
    }

    #[test]
    fn a_renderer_without_grammars_offers_no_languages() {
        assert!(Renderer::new().languages().is_empty());
        assert!(Renderer::with_highlighting().languages().len() > 5);
    }
}

#[test]
fn the_line_count_is_what_lands_on_paper_not_what_was_typed() {
    let r = Renderer::new();
    let cols = r.text_columns(3);

    // Three typed lines, the middle one two and a bit times over the width.
    let content = format!("short\n{}\nshort\n", "x".repeat(cols * 2 + 5));
    let measured = r.measure(&job(&content, "Team Wrap"));
    assert_eq!(content.lines().count(), 3, "three lines were typed");
    assert_eq!(
        measured.lines, 5,
        "the long line wraps to three, so five lines are printed"
    );
    assert_eq!(measured.lines, r.render(&job(&content, "Team Wrap")).lines);
}
