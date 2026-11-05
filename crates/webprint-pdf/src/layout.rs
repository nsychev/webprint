//! Text normalization, wrapping and pagination.

use crate::style::Span;

/// One physical (post-wrap) line of output.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
    /// Logical line number, or `None` on a wrapped continuation.
    pub number: Option<usize>,
    pub continuation: bool,
}

impl Line {
    /// The line's text, styling discarded. Useful in tests and logs.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Debug)]
pub struct Paginated {
    pub pages: Vec<Vec<Line>>,
    /// Logical (pre-wrap) line count of the submitted text.
    pub logical_lines: usize,
    /// Physical line count actually laid out.
    pub wrapped_lines: usize,
}

/// Splits `content` into logical lines, expanding tabs to the next tab stop and
/// dropping control characters that have no printable form.
pub fn normalize(content: &str, tab_width: usize) -> Vec<String> {
    let tab_width = tab_width.max(1);
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = Vec::new();
    // \r\n and lone \r both count as line breaks.
    for raw in content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
    {
        let mut out = String::with_capacity(raw.len());
        let mut col = 0usize;
        for ch in raw.chars() {
            match ch {
                '\t' => {
                    let pad = tab_width - (col % tab_width);
                    out.extend(std::iter::repeat_n(' ', pad));
                    col += pad;
                }
                // Control characters and other non-printables are dropped so a
                // stray byte cannot corrupt the page.
                c if c.is_control() => {}
                c => {
                    out.push(c);
                    col += 1;
                }
            }
        }
        lines.push(out);
    }
    // A trailing newline should not produce a dangling empty line.
    if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Columns consumed by the line-number gutter (number, gap, rule, gap).
pub fn gutter_cols(logical_lines: usize, line_numbers: bool) -> usize {
    if line_numbers {
        number_digits(logical_lines) + 3
    } else {
        0
    }
}

/// Width in digits of the largest line number.
pub fn number_digits(logical_lines: usize) -> usize {
    logical_lines.max(1).to_string().len()
}

/// Wraps prose on word boundaries for the heading.
pub fn wrap_words(text: &str, cols: usize, max_lines: usize) -> Vec<String> {
    let cols = cols.max(1);
    let max_lines = max_lines.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let current_len = current.chars().count();

        if current.is_empty() {
            current.push_str(word);
        } else if current_len + 1 + word_len <= cols {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }

        // A single word longer than the line gets hard-broken.
        while current.chars().count() > cols {
            let head: String = current.chars().take(cols).collect();
            let tail: String = current.chars().skip(cols).collect();
            lines.push(head);
            current = tail;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }

    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let kept: String = last.chars().take(cols.saturating_sub(1)).collect();
            *last = format!("{kept}…");
        }
    }
    lines
}

/// Hard-wraps styled logical lines to `cols` columns, splitting spans at the
/// column boundary and preserving each run's style.
pub fn wrap(styled: &[Vec<Span>], cols: usize) -> Vec<Line> {
    let cols = cols.max(1);
    let mut out: Vec<Line> = Vec::with_capacity(styled.len());

    for (idx, spans) in styled.iter().enumerate() {
        let mut current: Vec<Span> = Vec::new();
        let mut col = 0usize;
        let mut emitted = 0usize;

        for span in spans {
            let mut buf = String::new();
            for ch in span.text.chars() {
                buf.push(ch);
                col += 1;
                if col == cols {
                    current.push(Span {
                        text: std::mem::take(&mut buf),
                        style: span.style,
                    });
                    out.push(Line {
                        spans: std::mem::take(&mut current),
                        number: (emitted == 0).then_some(idx + 1),
                        continuation: emitted > 0,
                    });
                    emitted += 1;
                    col = 0;
                }
            }
            if !buf.is_empty() {
                current.push(Span {
                    text: buf,
                    style: span.style,
                });
            }
        }

        // Flush the tail, and always emit at least one line per logical line so
        // blank lines in the input survive.
        if !current.is_empty() || emitted == 0 {
            out.push(Line {
                spans: if current.is_empty() {
                    vec![Span::plain(String::new())]
                } else {
                    current
                },
                number: (emitted == 0).then_some(idx + 1),
                continuation: emitted > 0,
            });
        }
    }
    out
}

/// Distributes lines over pages.
pub fn paginate(lines: Vec<Line>, logical_lines: usize, lines_per_page: usize) -> Paginated {
    let lines_per_page = lines_per_page.max(1);
    let wrapped_lines = lines.len();

    let mut pages: Vec<Vec<Line>> = Vec::new();
    let mut rest = lines.as_slice();
    loop {
        let take = rest.len().min(lines_per_page);
        pages.push(rest[..take].to_vec());
        rest = &rest[take..];
        if rest.is_empty() {
            break;
        }
    }

    Paginated {
        pages,
        logical_lines,
        wrapped_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::TextStyle;

    fn plain(lines: &[&str]) -> Vec<Vec<Span>> {
        lines.iter().map(|l| vec![Span::plain(*l)]).collect()
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    #[test]
    fn tabs_expand_to_stops_and_crlf_is_handled() {
        let lines = normalize("a\tb\r\n\tx\n", 4);
        assert_eq!(lines, vec!["a   b".to_string(), "    x".to_string()]);
    }

    #[test]
    fn control_characters_are_dropped_but_unicode_is_kept() {
        let lines = normalize("a\u{0}b\u{7}\u{1b}[0m c Ж", 4);
        assert_eq!(lines, vec!["ab[0m c Ж".to_string()]);
    }

    #[test]
    fn empty_input_is_a_single_empty_line() {
        assert_eq!(normalize("", 4), vec![String::new()]);
        assert_eq!(normalize("\n", 4), vec![String::new()]);
    }

    #[test]
    fn blank_lines_in_the_middle_survive() {
        assert_eq!(
            normalize("a\n\nb", 4),
            vec!["a".into(), "".into(), "b".to_string()]
        );
        let wrapped = wrap(&plain(&["a", "", "b"]), 10);
        assert_eq!(texts(&wrapped), vec!["a", "", "b"]);
        assert_eq!(wrapped[1].number, Some(2));
    }

    #[test]
    fn wrapping_splits_on_column_boundary_and_numbers_only_the_first_chunk() {
        let wrapped = wrap(&plain(&["abcdefg", "hi"]), 3);
        assert_eq!(texts(&wrapped), vec!["abc", "def", "g", "hi"]);
        assert_eq!(
            wrapped.iter().map(|l| l.number).collect::<Vec<_>>(),
            vec![Some(1), None, None, Some(2)]
        );
        assert_eq!(
            wrapped.iter().map(|l| l.continuation).collect::<Vec<_>>(),
            vec![false, true, true, false]
        );
    }

    #[test]
    fn wrapping_is_char_based_not_byte_based() {
        let wrapped = wrap(&plain(&["ЖЖЖЖ"]), 2);
        assert_eq!(texts(&wrapped), vec!["ЖЖ", "ЖЖ"]);
    }

    #[test]
    fn wrapping_keeps_span_styles_and_splits_across_them() {
        let styled = vec![vec![
            Span::plain("ab"),
            Span {
                text: "CDEF".into(),
                style: TextStyle::bold(),
            },
            Span::plain("gh"),
        ]];
        let wrapped = wrap(&styled, 3);
        assert_eq!(texts(&wrapped), vec!["abC", "DEF", "gh"]);
        // The first line holds a plain run and a bold run.
        assert_eq!(wrapped[0].spans.len(), 2);
        assert!(!wrapped[0].spans[0].style.bold);
        assert!(wrapped[0].spans[1].style.bold);
        // The whole middle line is bold.
        assert_eq!(wrapped[1].spans.len(), 1);
        assert!(wrapped[1].spans[0].style.bold);
        assert!(!wrapped[2].spans[0].style.bold);
    }

    #[test]
    fn team_names_wrap_on_word_boundaries() {
        // A real team name from a regional contest.
        let name = "B052 Kazakh-British TU: Master ka4ka (Perdebay, Syrgabayev, Torekeldi)";
        let lines = wrap_words(name, 40, 4);
        assert_eq!(
            lines,
            vec![
                "B052 Kazakh-British TU: Master ka4ka".to_string(),
                "(Perdebay, Syrgabayev, Torekeldi)".to_string(),
            ]
        );
        // Nothing is lost and no line is over the limit.
        assert_eq!(lines.join(" "), name);
        assert!(lines.iter().all(|l| l.chars().count() <= 40));
    }

    #[test]
    fn a_short_name_stays_on_one_line() {
        assert_eq!(
            wrap_words("Hello World", 40, 4),
            vec!["Hello World".to_string()]
        );
        assert_eq!(wrap_words("", 40, 4), vec![String::new()]);
        assert_eq!(wrap_words("   ", 40, 4), vec![String::new()]);
    }

    #[test]
    fn an_unbreakable_word_is_split_rather_than_overflowing() {
        let lines = wrap_words(&"x".repeat(25), 10, 4);
        assert_eq!(lines, vec!["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]);
    }

    #[test]
    fn an_absurd_name_is_capped_with_an_ellipsis() {
        let name = (1..=40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let lines = wrap_words(&name, 20, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'), "{:?}", lines[1]);
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
    }

    #[test]
    fn unicode_names_wrap_by_characters_not_bytes() {
        let lines = wrap_words("Команда «Мечта» из Москвы", 16, 4);
        assert!(lines.iter().all(|l| l.chars().count() <= 16), "{lines:?}");
        assert_eq!(lines.join(" "), "Команда «Мечта» из Москвы");
    }

    #[test]
    fn gutter_width_tracks_the_line_count() {
        assert_eq!(gutter_cols(1, true), 4);
        assert_eq!(gutter_cols(9, true), 4);
        assert_eq!(gutter_cols(10, true), 5);
        assert_eq!(gutter_cols(1234, true), 7);
        assert_eq!(gutter_cols(1234, false), 0);
        assert_eq!(number_digits(0), 1);
        assert_eq!(number_digits(100), 3);
    }

    #[test]
    fn pagination_fills_equal_pages_in_order() {
        let src: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = src.iter().map(String::as_str).collect();
        let p = paginate(wrap(&plain(&refs), 80), 10, 4);
        assert_eq!(p.pages.len(), 3);
        assert_eq!(p.pages[0].len(), 4);
        assert_eq!(p.pages[1].len(), 4);
        assert_eq!(p.pages[2].len(), 2);
        assert_eq!(p.wrapped_lines, 10);
        // Order is preserved across the page break.
        assert_eq!(p.pages[0][0].text(), "0");
        assert_eq!(p.pages[1][0].text(), "4");
        assert_eq!(p.pages[2][0].text(), "8");
    }

    #[test]
    fn every_line_is_laid_out_however_long_the_job() {
        let src: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = src.iter().map(String::as_str).collect();
        let p = paginate(wrap(&plain(&refs), 80), 100, 5);
        assert_eq!(p.pages.len(), 20);
        assert_eq!(p.wrapped_lines, 100);
        assert_eq!(p.pages.last().unwrap().last().unwrap().text(), "99");
    }

    #[test]
    fn a_short_job_takes_a_single_page() {
        let p = paginate(wrap(&plain(&["one", "two"]), 80), 2, 60);
        assert_eq!(p.pages.len(), 1);
    }
}
