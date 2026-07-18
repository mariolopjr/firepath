//! Byte-cursor helpers shared by the per-line scanners
//!
//! [`TransactionHeader`](crate::TransactionHeader) and [`Posting`](crate::Posting)
//! each walk one line as bytes, skipping and trimming the ASCII spaces and tabs
//! that separate a line's fields and turning line-relative offsets into
//! file-absolute spans

use crate::span::{Span, clamp_u32};

/// The first index at or past `pos` holding neither a space nor a tab
pub(crate) fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while matches!(bytes.get(pos), Some(b' ' | b'\t')) {
        pos = pos.saturating_add(1);
    }
    pos
}

/// Trim trailing spaces and tabs off the `start..end` range, returning the new
/// end
pub(crate) fn trim_end(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && matches!(bytes.get(end.saturating_sub(1)), Some(b' ' | b'\t')) {
        end = end.saturating_sub(1);
    }
    end
}

/// A file-absolute span for the `start..end` byte range within a line at `base`
pub(crate) fn span_at(base: u32, start: usize, end: usize) -> Span {
    Span::new(
        base.saturating_add(clamp_u32(start)),
        base.saturating_add(clamp_u32(end)),
    )
}

/// Widen an empty span to one neighboring byte so an editor can show it
///
/// An error at a zero-width point renders as nothing, so an empty span widens
/// to the byte after it, or the byte before when it sits at the line's end.
/// `base` and `line_len` bound the line the span lands in, so the widening
/// stays inside it. An empty line has no byte to widen to, so the span is left
/// empty
pub(crate) fn widen_empty(span: Span, base: u32, line_len: usize) -> Span {
    let mut start = span.start();
    let mut end = span.end();
    if start == end {
        let line_end = base.saturating_add(clamp_u32(line_len));
        if end < line_end {
            end = end.saturating_add(1);
        } else if start > base {
            start = start.saturating_sub(1);
        }
    }
    Span::new(start, end)
}
