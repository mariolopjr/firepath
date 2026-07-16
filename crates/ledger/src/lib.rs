//! `firepath-ledger`: the parser for the ledger journal grammar
//!
//! The scaffold lands the types the rest of the parser is built on: source
//! spans and file handles ([`Span`], [`FileId`]), parse errors that render as
//! `file:line:col` through a [`LineIndex`] ([`ParseError`]), and the per-file
//! result container ([`Parsed`]).
//!
//! On top of that sits the first scanner: [`Amount`], a [`Commodity`] and an
//! exact `Decimal` quantity, with the [`Placement`] and [`DecimalStyle`] that
//! let it format back to the shape it was read from

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod amount;
mod error;
mod span;

pub use amount::{Amount, Commodity, DecimalStyle, Placement};
pub use error::{LineCol, LineIndex, ParseError};
pub use span::{FileId, Span};

/// The result of parsing one source file: the items parsed plus every error
///
/// Parsing is error-tolerant, so items and errors accumulate together rather
/// than stopping at the first failure. `T` is the item type
///
/// The fields are public so a consumer can read them directly. Their source
/// order is an invariant the parser upholds by appending as it advances through
/// the file, not something enforced here, so code that mutates them is trusted
/// to keep them ordered
#[derive(Debug)]
pub struct Parsed<T> {
    /// The source this is the parse of
    pub file: FileId,
    /// The items parsed, in source order
    pub items: Vec<T>,
    /// Every error found, in source order
    pub errors: Vec<ParseError>,
}

impl<T> Parsed<T> {
    /// An empty result for a file, before any items or errors are recorded
    pub fn new(file: FileId) -> Self {
        Self {
            file,
            items: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Whether any errors were recorded
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{FileId, ParseError, Parsed, Span};

    #[test]
    fn a_fresh_parse_has_no_items_or_errors() {
        let parsed = Parsed::<()>::new(FileId::new(0));
        assert!(parsed.items.is_empty());
        assert!(!parsed.has_errors());
    }

    #[test]
    fn recording_an_error_is_visible() {
        let mut parsed = Parsed::<()>::new(FileId::new(1));
        parsed.errors.push(ParseError::new("boom", Span::new(0, 1)));
        assert!(parsed.has_errors());
        assert_eq!(parsed.file, FileId::new(1));
    }
}
