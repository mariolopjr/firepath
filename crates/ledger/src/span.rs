//! Source spans and the handles that tie an item to the file it came from

/// A handle to a source file
///
/// The parser tags every item and error with the file it came from. The
/// registry that resolves a `FileId` back to a path lives in the journal crate,
/// so here it is only an opaque handle
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl FileId {
    /// Wrap a raw file index
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw file index
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A half-open byte range `start..end` into a single source file
///
/// Offsets are `u32`: a source file is highly unlikely to be larger
/// than 4 GiB
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    start: u32,
    end: u32,
}

impl Span {
    /// A span covering `start..end`
    ///
    /// The half-open range must have `start <= end`. Debug builds assert it so a
    /// reversed span is caught at its source. Release builds trust the caller and
    /// rely on `len` saturating to zero, so a bad span degrades rather than panics
    pub const fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    /// The start byte offset
    pub const fn start(self) -> u32 {
        self.start
    }

    /// The end byte offset, exclusive
    pub const fn end(self) -> u32 {
        self.end
    }

    /// The length in bytes, zero for a malformed span whose end precedes start
    pub const fn len(self) -> u32 {
        // saturating so a reversed span reads as empty instead of underflowing
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Saturate a byte offset into `u32`, matching the span offset width
pub(crate) fn clamp_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{FileId, Span};

    #[test]
    fn file_id_round_trips_its_index() {
        assert_eq!(FileId::new(7).index(), 7);
    }

    #[test]
    fn span_reports_its_bounds_and_length() {
        let span = Span::new(4, 10);
        assert_eq!(span.start(), 4);
        assert_eq!(span.end(), 10);
        assert_eq!(span.len(), 6);
        assert!(!span.is_empty());
    }

    #[test]
    fn an_equal_bounded_span_is_empty() {
        assert!(Span::new(5, 5).is_empty());
    }

    #[test]
    fn an_offset_wider_than_u32_saturates() {
        assert_eq!(super::clamp_u32(7), 7);
        // Unreachable through the scanners without a >4 GiB source, so the
        // saturation is pinned directly
        assert_eq!(super::clamp_u32(usize::MAX), u32::MAX);
    }

    #[test]
    fn a_reversed_span_saturates_to_zero_length() {
        // new forbids a reversed span in debug builds, so build one from its
        // fields to prove len still saturates as the release-only safety net
        let reversed = Span { start: 10, end: 4 };
        assert_eq!(reversed.len(), 0);
    }
}
