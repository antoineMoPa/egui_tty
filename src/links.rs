//! Finding the URL under the pointer, so it can be underlined and opened.
//!
//! Two kinds of link end up on a terminal grid. A program can mark one explicitly with an
//! OSC 8 escape, which Ghostty records against the cells it covers; far more often it just
//! prints the URL as text, and the terminal is expected to notice. Both are looked for here,
//! the explicit one first because it says exactly which cells it covers.

use std::ops::Range;

use libghostty_vt::{
    Terminal,
    terminal::{Point, PointCoordinate},
};

/// What a printed URL has to start with to be treated as one. Anything else on a terminal
/// line — a bare `www.` or a path — is far more likely to be output than a link.
const SCHEMES: &[&str] = &["https://", "http://", "file://", "ftp://"];

/// Trailing characters a URL gives back: a link at the end of a sentence should not swallow
/// the full stop, and one inside brackets should not swallow the closing bracket.
const GIVEN_BACK: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '\'', '"'];

/// A URL on the grid: the cells it covers, row by row, and where it points.
///
/// A URL longer than the terminal is wide gets soft-wrapped onto the rows below, so one link
/// can cover the tail of one row and the head of the next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Link {
    pub(crate) segments: Vec<Segment>,
    pub(crate) url: String,
}

/// The run of a link's cells on one row of the viewport.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Segment {
    pub(crate) row: u16,
    pub(crate) columns: Range<u16>,
}

impl Link {
    /// The columns the link covers on a row, if it reaches that row at all.
    pub(crate) fn columns_on(&self, row: u16) -> Option<Range<u16>> {
        self.segments
            .iter()
            .find(|segment| segment.row == row)
            .map(|segment| segment.columns.clone())
    }
}

/// The link covering a cell of the viewport, if there is one.
pub(crate) fn link_at(terminal: &Terminal<'_, '_>, cols: u16, x: u16, y: u16) -> Option<Link> {
    if x >= cols || cols == 0 {
        return None;
    }
    let line = Line::around(terminal, cols, y);
    let at = line.index_of(x, y);
    marked_link_at(terminal, &line, at).or_else(|| printed_link_at(terminal, &line, at))
}

/// The rows the program wrote as one line: the row under the pointer plus every row the
/// terminal soft-wrapped it onto, or from. A position along it is `row * cols + column`,
/// counted from its first row.
struct Line {
    first_row: u16,
    rows: u16,
    cols: u16,
}

impl Line {
    fn around(terminal: &Terminal<'_, '_>, cols: u16, y: u16) -> Self {
        let screen_rows = terminal.rows().unwrap_or(y + 1).max(y + 1);
        let mut first_row = y;
        while first_row > 0 && is_wrapped(terminal, first_row - 1) {
            first_row -= 1;
        }
        let mut last_row = y;
        while last_row + 1 < screen_rows && is_wrapped(terminal, last_row) {
            last_row += 1;
        }
        Self {
            first_row,
            rows: last_row - first_row + 1,
            cols,
        }
    }

    fn len(&self) -> usize {
        usize::from(self.rows) * usize::from(self.cols)
    }

    fn index_of(&self, x: u16, y: u16) -> usize {
        usize::from(y - self.first_row) * usize::from(self.cols) + usize::from(x)
    }

    fn cell_of(&self, index: usize) -> (u16, u16) {
        let cols = usize::from(self.cols);
        ((index % cols) as u16, self.first_row + (index / cols) as u16)
    }

    /// A span along the line, cut at the row ends into the cells it covers on each row.
    fn segments(&self, span: Range<usize>) -> Vec<Segment> {
        let cols = usize::from(self.cols);
        let mut segments = Vec::new();
        let mut from = span.start;
        while from < span.end {
            let row_end = (from / cols + 1) * cols;
            let to = span.end.min(row_end);
            let (x, row) = self.cell_of(from);
            segments.push(Segment {
                row,
                columns: x..x + (to - from) as u16,
            });
            from = to;
        }
        segments
    }
}

/// Whether the terminal carried this viewport row on to the next one, rather than the
/// program ending the line there.
fn is_wrapped(terminal: &Terminal<'_, '_>, y: u16) -> bool {
    terminal
        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: u32::from(y) }))
        .and_then(|grid_ref| grid_ref.row())
        .and_then(|row| row.is_wrapped())
        .unwrap_or(false)
}

/// A link the program marked with OSC 8. Its extent is however far the same URI runs.
fn marked_link_at(terminal: &Terminal<'_, '_>, line: &Line, at: usize) -> Option<Link> {
    let uri_at = |index: usize| {
        let (x, y) = line.cell_of(index);
        hyperlink_uri(terminal, x, y)
    };
    let url = uri_at(at)?;

    let mut start = at;
    while start > 0 && uri_at(start - 1).as_deref() == Some(url.as_str()) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < line.len() && uri_at(end).as_deref() == Some(url.as_str()) {
        end += 1;
    }

    Some(Link {
        segments: line.segments(start..end),
        url,
    })
}

fn hyperlink_uri(terminal: &Terminal<'_, '_>, x: u16, y: u16) -> Option<String> {
    let grid_ref = terminal
        .grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }))
        .ok()?;

    let mut buffer = [0u8; 2048];
    let written = grid_ref.hyperlink_uri(&mut buffer).ok()?;
    if written == 0 {
        return None;
    }
    String::from_utf8(buffer[..written].to_vec()).ok()
}

/// A URL the program simply printed, found by reading the line back as text.
fn printed_link_at(terminal: &Terminal<'_, '_>, line: &Line, at: usize) -> Option<Link> {
    let text = line_text(terminal, line);
    let found = find_url(&text, at)?;

    Some(Link {
        url: text[found.clone()].iter().collect(),
        segments: line.segments(found),
    })
}

/// The line as one char per cell, so a position along the line is an index of the text.
///
/// A cell holding a grapheme cluster contributes only its first character. That keeps the
/// indexing honest, and a URL is made of characters that stand alone anyway.
fn line_text(terminal: &Terminal<'_, '_>, line: &Line) -> Vec<char> {
    let mut out = Vec::with_capacity(line.len());
    let mut graphemes = [' '; 8];

    for index in 0..line.len() {
        let (x, y) = line.cell_of(index);
        let character = terminal
            .grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }))
            .ok()
            .and_then(|grid_ref| {
                let written = grid_ref.graphemes(&mut graphemes).ok()?;
                (written > 0).then(|| graphemes[0])
            })
            .unwrap_or(' ');
        out.push(character);
    }
    out
}

/// The span of the URL covering `at`, if the row has one there.
fn find_url(row: &[char], at: usize) -> Option<Range<usize>> {
    for scheme in SCHEMES {
        let mut from = 0;
        while let Some(start) = find_from(row, from, scheme) {
            let end = url_end(row, start + scheme.chars().count());
            if (start..end).contains(&at) {
                return Some(start..end);
            }
            from = start + 1;
        }
    }
    None
}

fn find_from(row: &[char], from: usize, needle: &str) -> Option<usize> {
    let width = needle.chars().count();
    (from..row.len().saturating_sub(width) + 1).find(|start| {
        row[*start..start + width]
            .iter()
            .copied()
            .eq(needle.chars())
    })
}

/// A URL runs to the first space or control character, minus whatever punctuation it ends on.
fn url_end(row: &[char], from: usize) -> usize {
    let mut end = from;
    while end < row.len() && !row[end].is_whitespace() && !row[end].is_control() {
        end += 1;
    }
    while end > from && GIVEN_BACK.contains(&row[end - 1]) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::TerminalOptions;

    fn terminal_showing(text: &str) -> Terminal<'static, 'static> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 60,
            rows: 4,
            max_scrollback: 100,
        })
        .expect("expected a terminal");
        terminal.vt_write(text.as_bytes());
        terminal
    }

    /// The whole way through: a URL printed by a program, read back off the real grid.
    #[test]
    fn a_url_printed_to_the_grid_is_found_under_the_pointer() {
        let terminal = terminal_showing("go to https://example.com/docs for more");

        let link = link_at(&terminal, 60, 10, 0).expect("expected a link");
        assert_eq!(link.url, "https://example.com/docs");
        assert_eq!(link.segments, vec![Segment { row: 0, columns: 6..30 }]);

        assert!(link_at(&terminal, 60, 2, 0).is_none(), "before the url");
        assert!(link_at(&terminal, 60, 40, 0).is_none(), "after it");
        assert!(link_at(&terminal, 60, 10, 1).is_none(), "a blank row");
    }

    /// An OSC 8 link says where it points regardless of what it shows, so the cells it
    /// covers have to come from the escape rather than from the text.
    #[test]
    fn an_osc_8_link_is_found_by_the_uri_it_carries() {
        let terminal = terminal_showing(
            "see \x1b]8;;https://example.com/deep\x1b\\click here\x1b]8;;\x1b\\ ok",
        );

        let link = link_at(&terminal, 60, 6, 0).expect("expected a link");
        assert_eq!(link.url, "https://example.com/deep");
        assert_eq!(
            link.columns_on(0),
            Some(4..14),
            "the link covers exactly the cells between the escapes"
        );
        assert!(link_at(&terminal, 60, 1, 0).is_none(), "outside the escape");
    }

    fn narrow_terminal_showing(text: &str) -> Terminal<'static, 'static> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 4,
            max_scrollback: 100,
        })
        .expect("expected a terminal");
        terminal.vt_write(text.as_bytes());
        terminal
    }

    /// A URL longer than the row is wide wraps onto the next rows, and is still one link
    /// from whichever of its rows the pointer is on.
    #[test]
    fn a_url_the_terminal_wrapped_is_one_link_across_its_rows() {
        let url = "https://example.com/a/very/long/path/x";
        let terminal = narrow_terminal_showing(&format!("see {url} ok"));

        for (x, y) in [(6, 0), (3, 1), (1, 2)] {
            let link = link_at(&terminal, 20, x, y).expect("expected a link");
            assert_eq!(link.url, url, "from {x},{y}");
            assert_eq!(
                link.segments,
                vec![
                    Segment { row: 0, columns: 4..20 },
                    Segment { row: 1, columns: 0..20 },
                    Segment { row: 2, columns: 0..2 },
                ]
            );
        }
        assert!(link_at(&terminal, 20, 4, 2).is_none(), "the word after it");
    }

    #[test]
    fn an_osc_8_link_the_terminal_wrapped_covers_every_row() {
        let terminal = narrow_terminal_showing(
            "0123456789\x1b]8;;https://example.com/deep\x1b\\a label that wraps\x1b]8;;\x1b\\ ok",
        );

        let link = link_at(&terminal, 20, 2, 1).expect("expected a link");
        assert_eq!(link.url, "https://example.com/deep");
        assert_eq!(link.columns_on(0), Some(10..20));
        assert_eq!(link.columns_on(1), Some(0..8));
    }

    /// A program that ends its own line does not make one link of two rows.
    #[test]
    fn rows_the_program_ended_itself_are_not_joined() {
        let terminal = narrow_terminal_showing("https://example.com/\r\nmore-text");

        let link = link_at(&terminal, 20, 3, 0).expect("expected a link");
        assert_eq!(link.url, "https://example.com/");
        assert!(link_at(&terminal, 20, 3, 1).is_none());
    }

    #[test]
    fn a_column_past_the_end_of_the_grid_has_no_link() {
        let terminal = terminal_showing("https://example.com");

        assert!(link_at(&terminal, 60, 60, 0).is_none());
    }

    fn row(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn a_url_is_found_from_any_cell_it_covers() {
        let line = row("see https://example.com/x now");
        let span = find_url(&line, 4).expect("the first character of it");

        assert_eq!(
            line[span.clone()].iter().collect::<String>(),
            "https://example.com/x"
        );
        assert_eq!(find_url(&line, 10), Some(span.clone()), "from the middle");
        assert_eq!(
            find_url(&line, span.end - 1),
            Some(span),
            "from the last one"
        );
    }

    #[test]
    fn a_cell_outside_the_url_has_no_link() {
        let line = row("see https://example.com now");
        assert!(find_url(&line, 0).is_none(), "before it");
        assert!(find_url(&line, 25).is_none(), "after it");
    }

    #[test]
    fn punctuation_a_sentence_put_there_is_not_part_of_the_link() {
        let line = row("read https://example.com/page.");
        let span = find_url(&line, 10).expect("a url");

        assert_eq!(
            line[span].iter().collect::<String>(),
            "https://example.com/page"
        );
    }

    #[test]
    fn a_bracketed_url_keeps_its_own_path_but_not_the_bracket() {
        let line = row("(https://example.com/a(b))");
        let span = find_url(&line, 5).expect("a url");

        assert_eq!(
            line[span].iter().collect::<String>(),
            "https://example.com/a(b"
        );
    }

    #[test]
    fn a_row_with_no_scheme_has_no_link() {
        assert!(find_url(&row("example.com is not a link"), 2).is_none());
        assert!(find_url(&row("           "), 4).is_none());
    }

    /// A row that ends mid-URL still yields what is there, rather than running off the end.
    #[test]
    fn a_url_running_to_the_edge_of_the_row_is_still_found() {
        let line = row("https://example.com/very/long");
        let span = find_url(&line, 28).expect("a url");

        assert_eq!(span, 0..line.len());
    }

    #[test]
    fn the_second_url_on_a_row_is_found_too() {
        let line = row("http://one.example http://two.example");
        let span = find_url(&line, 25).expect("a url");

        assert_eq!(line[span].iter().collect::<String>(), "http://two.example");
    }
}
