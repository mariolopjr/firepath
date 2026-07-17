//! Parse errors and the line index that renders their location

use crate::span::Span;

/// A one-based line and column into a source file
///
/// The column counts raw bytes from the start of the line, which is enough for
/// a `file:line:col` message. Note: A multi-byte UTF-8 char spans several columns
/// and a `\r` from a CRLF ending counts as a column. The editor does its own
/// UTF-16 mapping separately
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    /// One-based line number
    pub line: usize,
    /// One-based column, in bytes from the line start
    pub column: usize,
}

/// The byte offset of each newline in a source, used to turn a byte offset into
/// a line and column
///
/// Only newlines are stored. Line 1 is the text before the first newline, so it
/// needs no entry. Offsets are `u32` to match the spans that query them, so a
/// newline past `u32::MAX` cannot be represented and is dropped at construction
#[derive(Debug)]
pub struct LineIndex {
    // byte offset of each newline, ascending. line 1 is the text before the first
    newlines: Vec<u32>,
}

impl LineIndex {
    /// Build the index for a source string
    pub fn new(source: &str) -> Self {
        let mut newlines = Vec::new();
        // try_from drops any newlines past u32::MAX
        newlines.extend(
            memchr::memchr_iter(b'\n', source.as_bytes()).filter_map(|nl| u32::try_from(nl).ok()),
        );
        Self { newlines }
    }

    /// Map a byte offset to its one-based line and column
    ///
    /// Offsets are assumed to fall within the source. An offset at end-of-input
    /// resolves to the start of the trailing line, so an error there still
    /// renders a usable location. A far past-the-end offset only degrades to a
    /// column counted from the last line start which is not a meaningful position
    pub fn line_col(&self, offset: u32) -> LineCol {
        // line 1 is the text before the first newline, so start there and count a
        // line for each newline strictly before the offset. a newline is the last
        // column of its own line, so one exactly at the offset starts no new line.
        // newlines ascend, so stop at the first one at or past the offset
        let mut line = 1usize;
        let mut line_start = 0u32;
        for &newline in &self.newlines {
            if newline >= offset {
                break;
            }
            line = line.saturating_add(1);
            line_start = newline.saturating_add(1);
        }
        let column = (offset.saturating_sub(line_start) as usize).saturating_add(1);
        LineCol { line, column }
    }
}

/// A parse failure: an error message anchored to a source span
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong
    pub message: String,
    /// Where in the source the failure is anchored
    pub span: Span,
}

impl ParseError {
    /// Build an error anchored at a span
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    /// Render as `file:line:col: message`, resolving the span start through the
    /// line index of the file the error came from
    pub fn render(&self, file: &str, index: &LineIndex) -> String {
        let LineCol { line, column } = index.line_col(self.span.start());
        format!("{file}:{line}:{column}: {}", self.message)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{LineIndex, ParseError};
    use crate::span::Span;

    #[test]
    fn line_col_locates_offsets_across_lines() {
        // offsets: a0 b1 c2 \n3 d4 e5 f6 \n7, line starts at 0, 4, 8
        let index = LineIndex::new("abc\ndef\n");
        assert_eq!(index.line_col(0), lc(1, 1)); // byte 0 is 'a'
        assert_eq!(index.line_col(1), lc(1, 2)); // byte 1 is 'b'
        // byte 3 is '\n', the last column of its own line
        assert_eq!(index.line_col(3), lc(1, 4));
        assert_eq!(index.line_col(4), lc(2, 1)); // byte 4 is 'd'
        assert_eq!(index.line_col(6), lc(2, 3)); // byte 6 is 'f'
    }

    #[test]
    fn columns_count_bytes_not_chars() {
        // é is two bytes, so the byte after it lands in column three, and the
        // \r of a CRLF ending is a column of its own line
        let index = LineIndex::new("é.\r\nx");
        assert_eq!(index.line_col(2), lc(1, 3)); // the dot after the two-byte é
        assert_eq!(index.line_col(3), lc(1, 4)); // the \r itself
        assert_eq!(index.line_col(5), lc(2, 1)); // x, first byte after the CRLF
    }

    #[test]
    fn an_offset_past_the_end_resolves_against_the_last_line() {
        let index = LineIndex::new("abc\ndef\n");
        // offset 8 is one past the final newline, on the empty trailing line
        assert_eq!(index.line_col(8), lc(3, 1));
    }

    #[test]
    fn an_empty_source_has_one_line() {
        let index = LineIndex::new("");
        assert_eq!(index.line_col(0), lc(1, 1));
    }

    #[test]
    fn an_error_renders_as_file_line_col_message() {
        let index = LineIndex::new("abc\ndef\n");
        let error = ParseError::new("unexpected token", Span::new(5, 6));
        assert_eq!(
            error.render("ledgers/main.ledger", &index),
            "ledgers/main.ledger:2:2: unexpected token"
        );
    }

    fn lc(line: usize, column: usize) -> super::LineCol {
        super::LineCol { line, column }
    }
}
