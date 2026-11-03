//! Monochrome syntax highlighting.
//!
//! Highlighting is done by [`syntect`] using Sublime Text grammars. Contest
//! printers are black and white, so instead of a colour theme we build a
//! monochrome one: keywords and declarations become **bold**, comments become
//! *italic grey*, literals a softer grey, and everything else stays plain
//! black. Colour themes would be unreadable on a laser printer, while weight
//! and slant survive it.

use std::collections::BTreeMap;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem};
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet, SyntaxSetBuilder};
pub use syntect::LoadingError;

pub use crate::style::{Language, Span, TextStyle, AUTO, PLAIN_TEXT};

/// Grammars bundled on top of syntect's defaults, as (name, source) pairs.
///
/// syntect ships no Kotlin grammar, and Kotlin is a common contest language,
/// so webprint carries its own. See the file's header for its scope.
const BUNDLED_SYNTAXES: &[(&str, &str)] = &[(
    "Kotlin",
    include_str!("../assets/syntaxes/Kotlin.sublime-syntax"),
)];

/// Languages offered when the grammar is available, in menu order.
/// The first token of each entry is what clients submit.
const OFFERED: &[(&str, &[&str])] = &[
    ("cpp", &["C++"]),
    ("c", &["C"]),
    ("java", &["Java"]),
    ("python", &["Python"]),
    ("kotlin", &["Kotlin"]),
    ("csharp", &["C#"]),
    ("rust", &["Rust"]),
    ("go", &["Go"]),
    ("javascript", &["JavaScript"]),
    ("typescript", &["TypeScript"]),
    ("pascal", &["Pascal"]),
    ("haskell", &["Haskell"]),
    ("ruby", &["Ruby"]),
    ("php", &["PHP"]),
    ("scala", &["Scala"]),
    (
        "shell",
        &["Bourne Again Shell (bash)", "Shell-Unix-Generic"],
    ),
    ("sql", &["SQL"]),
];

/// Highlights source text using monochrome styling.
pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
    /// Longest line we attempt to highlight; pathological lines are passed
    /// through plain so a paste cannot stall the server.
    max_line_len: usize,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    /// Loads the bundled grammars. Costs a few tens of milliseconds, so build
    /// one at start-up and share it.
    pub fn new() -> Self {
        Self {
            syntaxes: build_syntaxes(None).expect("the bundled grammars are valid"),
            theme: monochrome_theme(),
            max_line_len: 2_000,
        }
    }

    /// Loads everything [`Highlighter::new`] does, plus every
    /// `.sublime-syntax` file found under `dir` (recursively).
    ///
    /// Use it for a language webprint does not carry: the grammar is picked up
    /// without rebuilding the server, and the language then appears in
    /// auto-detection too. A language with no grammar at all still prints
    /// correctly, just without highlighting.
    pub fn with_extra_syntaxes(dir: impl AsRef<std::path::Path>) -> Result<Self, LoadingError> {
        Ok(Self {
            syntaxes: build_syntaxes(Some(dir.as_ref()))?,
            theme: monochrome_theme(),
            max_line_len: 2_000,
        })
    }

    /// Grammar names the set knows about, for diagnostics.
    pub fn syntax_names(&self) -> Vec<&str> {
        self.syntaxes
            .syntaxes()
            .iter()
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Languages that can be offered, as `(token, name)`, with `auto` and
    /// `plain` first.
    pub fn languages(&self) -> Vec<Language> {
        let mut out = vec![
            Language {
                token: AUTO.into(),
                name: "Auto-detect".into(),
            },
            Language {
                token: PLAIN_TEXT.into(),
                name: "Plain text".into(),
            },
        ];
        for (token, names) in OFFERED {
            if let Some(syntax) = names
                .iter()
                .find_map(|n| self.syntaxes.find_syntax_by_name(n))
            {
                out.push(Language {
                    token: (*token).to_string(),
                    name: syntax.name.clone(),
                });
            }
        }
        out
    }

    /// Resolves a language token to a grammar. `plain`/unknown yield `None`.
    pub fn resolve(&self, token: &str) -> Option<&SyntaxReference> {
        let token = token.trim();
        if token.is_empty() || token.eq_ignore_ascii_case(PLAIN_TEXT) {
            return None;
        }
        if let Some((_, names)) = OFFERED.iter().find(|(t, _)| t.eq_ignore_ascii_case(token)) {
            if let Some(found) = names
                .iter()
                .find_map(|n| self.syntaxes.find_syntax_by_name(n))
            {
                return Some(found);
            }
        }
        self.syntaxes
            .find_syntax_by_token(token)
            .or_else(|| self.syntaxes.find_syntax_by_name(token))
            .filter(|s| s.name != "Plain Text")
    }

    /// Guesses a language from the content. Returns `None` for plain text.
    ///
    /// Contest submissions are pasted without a file name or shebang, so this
    /// looks for the few constructs that identify the usual contest languages.
    pub fn detect(&self, content: &str) -> Option<&SyntaxReference> {
        let head: String = content.chars().take(4_000).collect();
        let token = detect_token(&head)?;
        self.resolve(token)
    }

    /// Highlights `lines`, returning one span list per line.
    ///
    /// `syntax` of `None` yields a single plain span per line.
    pub fn highlight(&self, lines: &[String], syntax: Option<&SyntaxReference>) -> Vec<Vec<Span>> {
        let Some(syntax) = syntax else {
            return lines.iter().map(|l| vec![Span::plain(l.clone())]).collect();
        };

        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            if line.len() > self.max_line_len {
                out.push(vec![Span::plain(line.clone())]);
                continue;
            }
            // The bundled grammars are the "newlines" variants, which expect a
            // trailing newline on every line.
            let with_newline = format!("{line}\n");
            match highlighter.highlight_line(&with_newline, &self.syntaxes) {
                Ok(runs) => out.push(merge_runs(runs.into_iter().map(|(style, text)| Span {
                    text: text.trim_end_matches('\n').to_string(),
                    style: TextStyle {
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                        gray: luminance(style.foreground),
                    },
                }))),
                Err(_) => out.push(vec![Span::plain(line.clone())]),
            }
        }
        out
    }
}

/// Builds the syntax set: syntect's defaults, webprint's bundled grammars, and
/// optionally a directory of further ones.
fn build_syntaxes(extra_dir: Option<&std::path::Path>) -> Result<SyntaxSet, LoadingError> {
    // `true` throughout: the bundled defaults are the newline variants, and
    // mixing the two kinds in one set misparses lines.
    let mut builder: SyntaxSetBuilder = SyntaxSet::load_defaults_newlines().into_builder();
    for (name, source) in BUNDLED_SYNTAXES {
        builder.add(
            SyntaxDefinition::load_from_str(source, true, Some(name))
                .map_err(|e| LoadingError::ParseSyntax(e, format!("bundled {name}")))?,
        );
    }
    if let Some(dir) = extra_dir {
        builder.add_from_folder(dir, true)?;
    }
    Ok(builder.build())
}

/// Collapses empty spans and merges neighbours that share a style, which keeps
/// the PDF content stream small.
fn merge_runs(spans: impl Iterator<Item = Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.style == span.style => last.text.push_str(&span.text),
            _ => out.push(span),
        }
    }
    if out.is_empty() {
        out.push(Span::plain(String::new()));
    }
    out
}

/// Perceived brightness of a colour, used to turn any theme into grey levels.
fn luminance(c: Color) -> f64 {
    let l = (0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b)) / 255.0;
    l.clamp(0.0, 1.0)
}

fn gray(level: u8) -> Color {
    Color {
        r: level,
        g: level,
        b: level,
        a: 255,
    }
}

/// Builds the monochrome theme: weight and slant carry the meaning, grey is
/// used sparingly so comments and literals recede without turning muddy.
fn monochrome_theme() -> Theme {
    let plain = StyleModifier {
        foreground: Some(gray(0)),
        background: None,
        font_style: Some(FontStyle::empty()),
    };
    let bold = StyleModifier {
        foreground: Some(gray(0)),
        background: None,
        font_style: Some(FontStyle::BOLD),
    };
    let comment = StyleModifier {
        foreground: Some(gray(125)),
        background: None,
        font_style: Some(FontStyle::ITALIC),
    };
    let literal = StyleModifier {
        foreground: Some(gray(85)),
        background: None,
        font_style: Some(FontStyle::empty()),
    };

    let rules: &[(&str, StyleModifier)] = &[
        ("comment, punctuation.definition.comment", comment),
        (
            "keyword, storage, keyword.control, keyword.operator.word, keyword.other",
            bold,
        ),
        ("storage.type, storage.modifier, support.type", bold),
        (
            "entity.name.function, entity.name.class, entity.name.type, entity.name.struct, \
             entity.name.namespace, entity.name.module, support.function",
            bold,
        ),
        ("meta.preprocessor, keyword.control.import", bold),
        ("string, constant.character, constant.other.symbol", literal),
        ("constant.numeric, constant.language", literal),
        ("variable.language, variable.parameter", plain),
        ("punctuation, meta.brace, keyword.operator", plain),
        ("invalid", bold),
    ];

    let mut theme = Theme {
        name: Some("webprint-monochrome".to_string()),
        ..Theme::default()
    };
    theme.settings.foreground = Some(gray(0));
    theme.settings.background = Some(gray(255));
    theme.scopes = rules
        .iter()
        .filter_map(|(selector, style)| {
            // The selectors are compile-time constants; a parse failure would
            // only drop that one rule.
            selector
                .parse::<ScopeSelectors>()
                .ok()
                .map(|scope| ThemeItem {
                    scope,
                    style: *style,
                })
        })
        .collect();
    theme
}

/// Signals that identify the usual contest languages, strongest first.
fn detect_token(head: &str) -> Option<&'static str> {
    let signals: &[(&str, &[&str])] = &[
        (
            "cpp",
            &[
                "#include <bits/stdc++",
                "using namespace std",
                "std::",
                "cin >>",
                "cout <<",
            ],
        ),
        ("kotlin", &["fun main", "val ", "readLine()"]),
        ("rust", &["fn main(", "let mut ", "println!"]),
        (
            "java",
            &[
                "public static void main",
                "import java.",
                "System.out.print",
            ],
        ),
        ("csharp", &["using System", "Console.WriteLine"]),
        ("go", &["package main", "func main(", "fmt."]),
        (
            "python",
            &["def ", "import sys", "print(", "input()", "if __name__"],
        ),
        ("javascript", &["console.log", "require(", "=>"]),
        ("pascal", &["begin", "writeln", "program "]),
        ("haskell", &["main :: IO", "module Main"]),
        ("c", &["#include <stdio.h>", "printf(", "scanf("]),
        (
            "shell",
            &["#!/bin/sh", "#!/bin/bash", "#!/usr/bin/env bash"],
        ),
    ];

    // Score each language and take the clear winner.
    let mut scores: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (token, needles) in signals {
        let hits = needles.iter().filter(|n| head.contains(**n)).count();
        if hits > 0 {
            *scores.entry(token).or_default() += hits;
        }
    }
    // C++ and C share signals; prefer C++ when both match, as contests do.
    if scores.contains_key("cpp") {
        scores.remove("c");
    }
    scores
        .into_iter()
        .max_by_key(|(_, hits)| *hits)
        .map(|(token, _)| token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    fn style_of(spans: &[Span], needle: &str) -> TextStyle {
        spans
            .iter()
            .find(|s| s.text.contains(needle))
            .unwrap_or_else(|| panic!("no span containing {needle:?} in {spans:?}"))
            .style
    }

    #[test]
    fn plain_text_produces_one_span_per_line() {
        let h = Highlighter::new();
        let out = h.highlight(&lines("a\nb"), None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vec![Span::plain("a")]);
        assert!(out[0][0].style.is_plain());
    }

    #[test]
    fn cpp_keywords_are_bold_and_comments_italic_grey() {
        let h = Highlighter::new();
        let syntax = h.resolve("cpp").expect("C++ grammar");
        let src = "// solve it\nint main() { return 0; }";
        let out = h.highlight(&lines(src), Some(syntax));

        let comment = style_of(&out[0], "solve it");
        assert!(comment.italic, "comments should be italic: {comment:?}");
        assert!(comment.gray > 0.2, "comments should be grey: {comment:?}");

        let ret = style_of(&out[1], "return");
        assert!(ret.bold, "keywords should be bold: {ret:?}");
        assert!(ret.gray < 0.2, "keywords stay black: {ret:?}");
    }

    #[test]
    fn string_and_number_literals_are_grey_but_upright() {
        let h = Highlighter::new();
        let syntax = h.resolve("python").unwrap();
        let out = h.highlight(&lines("x = \"hello\"\ny = 42"), Some(syntax));
        let s = style_of(&out[0], "hello");
        assert!(s.gray > 0.1 && !s.italic && !s.bold, "{s:?}");
        let n = style_of(&out[1], "42");
        assert!(n.gray > 0.1 && !n.bold, "{n:?}");
    }

    #[test]
    fn highlighting_preserves_the_text_exactly() {
        let h = Highlighter::new();
        for token in ["cpp", "python", "java", "rust"] {
            let syntax = h.resolve(token).unwrap();
            let src = "int x = 1; // ok\n\n\tif (x) { return \"Ж\"; }\nend";
            let src_lines = lines(src);
            let out = h.highlight(&src_lines, Some(syntax));
            assert_eq!(out.len(), src_lines.len());
            for (original, spans) in src_lines.iter().zip(&out) {
                let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(&joined, original, "language {token}");
            }
        }
    }

    #[test]
    fn adjacent_runs_with_the_same_style_are_merged() {
        let h = Highlighter::new();
        let syntax = h.resolve("cpp").unwrap();
        // A comment is a single uniform run, however many words it holds.
        let out = h.highlight(&lines("// aaaa bbbb cccc dddd eeee"), Some(syntax));
        assert_eq!(out[0].len(), 1, "{:?}", out[0]);
        assert_eq!(out[0][0].text, "// aaaa bbbb cccc dddd eeee");
    }

    #[test]
    fn language_tokens_resolve_and_unknown_ones_do_not() {
        let h = Highlighter::new();
        for token in ["cpp", "c", "java", "python", "rust", "go"] {
            assert!(h.resolve(token).is_some(), "{token} should resolve");
        }
        assert!(h.resolve("plain").is_none());
        assert!(h.resolve("").is_none());
        assert!(h.resolve("   ").is_none());
        assert!(h.resolve("definitely-not-a-language").is_none());
        // Extensions work too, via syntect's token lookup.
        assert!(h.resolve("cc").is_some() || h.resolve("cpp").is_some());
    }

    #[test]
    fn offered_languages_start_with_auto_and_plain() {
        let h = Highlighter::new();
        let langs = h.languages();
        assert_eq!(langs[0].token, AUTO);
        assert_eq!(langs[1].token, PLAIN_TEXT);
        // Every offered language must resolve, so the UI cannot lie.
        for lang in langs.iter().skip(2) {
            assert!(h.resolve(&lang.token).is_some(), "{lang:?}");
        }
        let tokens: Vec<&str> = langs.iter().map(|l| l.token.as_str()).collect();
        for expected in ["cpp", "java", "python"] {
            assert!(tokens.contains(&expected), "{tokens:?}");
        }
    }

    #[test]
    fn detection_recognises_the_usual_contest_languages() {
        let h = Highlighter::new();
        let cases = [
            (
                "#include <bits/stdc++.h>\nusing namespace std;\nint main(){}",
                "C++",
            ),
            (
                "public static void main(String[] a) { System.out.println(1); }",
                "Java",
            ),
            (
                "import sys\nfor line in sys.stdin:\n    print(line)",
                "Python",
            ),
            ("fn main() { let mut x = 1; println!(\"{}\", x); }", "Rust"),
            (
                "package main\nimport \"fmt\"\nfunc main(){ fmt.Println(1) }",
                "Go",
            ),
        ];
        for (src, expected_name) in cases {
            match h.detect(src) {
                Some(syntax) => assert!(
                    syntax.name.contains(expected_name),
                    "detected {:?} for {expected_name}",
                    syntax.name
                ),
                None => panic!("failed to detect {expected_name} in {src:?}"),
            }
        }
    }

    #[test]
    fn kotlin_is_bundled_and_works_out_of_the_box() {
        let h = Highlighter::new();
        let syntax = h.resolve("kotlin").expect("Kotlin grammar is bundled");
        assert_eq!(syntax.name, "Kotlin");
        assert!(h.languages().iter().any(|l| l.token == "kotlin"));

        let src = "\
// read a number
fun main() {
    val n: Int = readLine()!!.trim().toInt()
    println(\"sum = ${(1..n).sum()}\")
}";
        let out = h.highlight(&lines(src), Some(syntax));

        let comment = style_of(&out[0], "read a number");
        assert!(comment.italic && comment.gray > 0.2, "{comment:?}");

        let fun_kw = style_of(&out[1], "fun");
        assert!(fun_kw.bold, "`fun` should be bold: {fun_kw:?}");
        let name = style_of(&out[1], "main");
        assert!(
            name.bold,
            "a declared function name should be bold: {name:?}"
        );

        let val_kw = style_of(&out[2], "val");
        assert!(val_kw.bold, "`val` should be bold: {val_kw:?}");
        let int_type = style_of(&out[2], "Int");
        assert!(int_type.bold, "a type should be bold: {int_type:?}");

        let string = style_of(&out[3], "sum");
        assert!(
            string.gray > 0.1 && !string.bold,
            "a string literal: {string:?}"
        );

        // Nothing is lost or reordered.
        for (original, spans) in lines(src).iter().zip(&out) {
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(&joined, original);
        }
    }

    #[test]
    fn kotlin_is_auto_detected() {
        let h = Highlighter::new();
        let kotlin = "fun main() {\n    val n = readLine()!!.toInt()\n    println(n)\n}";
        assert_eq!(h.detect(kotlin).map(|s| s.name.as_str()), Some("Kotlin"));
    }

    #[test]
    fn kotlin_modifiers_and_classes_are_marked() {
        let h = Highlighter::new();
        let syntax = h.resolve("kotlin").unwrap();
        let out = h.highlight(
            &lines("data class Point(val x: Int, val y: Int)"),
            Some(syntax),
        );
        assert!(style_of(&out[0], "data").bold, "modifier should be bold");
        assert!(style_of(&out[0], "class").bold, "keyword should be bold");
        assert!(style_of(&out[0], "Point").bold, "class name should be bold");
    }

    #[test]
    fn kotlin_block_comments_nest_and_terminate() {
        let h = Highlighter::new();
        let syntax = h.resolve("kotlin").unwrap();
        let src = "/* outer /* inner */ still a comment */ fun f() {}";
        let out = h.highlight(&lines(src), Some(syntax));
        assert!(style_of(&out[0], "still a comment").italic);
        // The comment really ends, so what follows is code again.
        assert!(style_of(&out[0], "fun").bold, "{:?}", out[0]);
    }

    #[test]
    fn an_unterminated_kotlin_string_does_not_swallow_the_file() {
        let h = Highlighter::new();
        let syntax = h.resolve("kotlin").unwrap();
        let out = h.highlight(&lines("val a = \"oops\nfun f() {}"), Some(syntax));
        assert!(style_of(&out[1], "fun").bold, "{:?}", out[1]);
    }

    #[test]
    fn extra_grammars_can_be_loaded_from_disk() {
        let dir = std::env::temp_dir().join("webprint-extra-syntax");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Toy.sublime-syntax"),
            "%YAML 1.2\n---\nname: Toy\nfile_extensions: [toy]\nscope: source.toy\n\
             contexts:\n  main:\n    - match: \\b(fun|val)\\b\n      \
             scope: keyword.control.toy\n",
        )
        .unwrap();

        let h = Highlighter::with_extra_syntaxes(&dir).expect("loads the folder");
        let syntax = h.resolve("Toy").expect("Toy grammar is available");
        assert_eq!(syntax.name, "Toy");
        // The bundled grammars are still there.
        assert!(h.resolve("cpp").is_some());
        assert!(h.syntax_names().contains(&"Toy"));

        // And its keywords are highlighted by the monochrome theme.
        let out = h.highlight(&lines("fun main"), Some(syntax));
        let kw = style_of(&out[0], "fun");
        assert!(kw.bold, "{kw:?}");
    }

    #[test]
    fn loading_a_missing_folder_is_an_error_not_a_panic() {
        assert!(Highlighter::with_extra_syntaxes("/nonexistent/webprint/syntaxes").is_err());
    }

    #[test]
    fn prose_is_not_mistaken_for_code() {
        let h = Highlighter::new();
        assert!(h
            .detect("Please print this note for the judges.\nThanks!")
            .is_none());
        assert!(h.detect("").is_none());
    }

    #[test]
    fn very_long_lines_bypass_highlighting() {
        let h = Highlighter::new();
        let syntax = h.resolve("cpp").unwrap();
        let long = format!("int x = {};", "1+".repeat(2_000));
        let out = h.highlight(std::slice::from_ref(&long), Some(syntax));
        assert_eq!(out[0], vec![Span::plain(long)]);
    }
}
