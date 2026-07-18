//! Whole-file parse: group a source file into blocks, then hand each block to
//! the scanner that reads its kind
//!
//! A block whose construct has no scanner yet is refused by name rather than
//! skipped, so an emitter can never quietly outrun the parser: a periodic or
//! automated transaction errors here until its own wave lands, the same way an
//! unsupported directive does

use crate::directive::Directive;
use crate::error::ParseError;
use crate::lines::{Block, BlockKind, blocks};
use crate::posting::Posting;
use crate::span::{FileId, Span};
use crate::transaction::TransactionHeader;

/// Parse one source file, returning every error in it
///
/// The parse never stops at the first error: each block is scanned on its own,
/// so one malformed line does not hide the next. `file` tags the parse for when
/// items carry it; today only errors are returned
pub fn parse(file: FileId, source: &str) -> Vec<ParseError> {
    let grouped = blocks(file, source);
    // Unassociated-indentation errors from grouping come first. Each block then adds
    // its own
    let mut errors = grouped.errors;
    for block in &grouped.items {
        match block.kind {
            // A comment carries no error, and no item yet
            BlockKind::Comment => {}
            BlockKind::Transaction => parse_transaction(source, block, &mut errors),
            BlockKind::Directive => {
                if let Err(err) =
                    Directive::parse(slice(source, block.header), block.header.start())
                {
                    errors.push(err);
                }
            }
            // No scanner yet, so refuse rather than skip
            BlockKind::Periodic => errors.push(ParseError::new(
                "periodic transactions are not supported yet",
                block.header,
            )),
            BlockKind::Automated => errors.push(ParseError::new(
                "automated transactions are not supported yet",
                block.header,
            )),
        }
    }
    errors
}

/// Scan a transaction's header and each of its posting lines, collecting every
/// error
///
/// The header and the postings are all scanned even when one fails, so a bad
/// header does not mask a bad posting below it. Every child is a posting for
/// now. Inline comment and tag children get their own routing when that grammar
/// is supported
fn parse_transaction(source: &str, block: &Block, errors: &mut Vec<ParseError>) {
    if let Err(err) = TransactionHeader::parse(slice(source, block.header), block.header.start()) {
        errors.push(err);
    }
    for &child in &block.children {
        if let Err(err) = Posting::parse(slice(source, child), child.start()) {
            errors.push(err);
        }
    }
}

/// The text a span covers, empty for a span that does not land in `source`
fn slice(source: &str, span: Span) -> &str {
    source
        .get(span.start() as usize..span.end() as usize)
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::parse;
    use crate::span::FileId;

    const FILE: FileId = FileId::new(0);

    #[test]
    fn a_clean_file_reports_no_errors() {
        // A header comment, an include, and a transaction with two postings:
        // one of each supported kind, all well-formed
        let src = "\
; a header comment
include 2020.ledger
2020-01-02 * Grocery
    Expenses:Food    $50.00
    Assets:Checking
";
        assert!(parse(FILE, src).is_empty());
    }

    #[test]
    fn every_error_in_a_file_is_collected() {
        // An unassociated indent, a refused directive, a bad header date, and a bad
        // posting amount all surface in one parse. The order errors land in is
        // not a contract yet, grouping errors lead and strict source order is
        // not supported right now, so the collected set is compared sorted.
        // Written with explicit newlines so the leading whitespace of the
        // indented lines survives the string literal
        let src = "    unassociated line\naccount Assets:Cash\n2020-13-01 Grocery\n    Expenses:Food    $\n";
        let errors = parse(FILE, src);
        let mut messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        messages.sort_unstable();
        let mut expected = vec![
            "indented line has no transaction or directive to attach to",
            "the \"account\" directive is not supported yet",
            "2020-13-01 is not a real calendar date",
            "expected a number",
        ];
        expected.sort_unstable();
        assert_eq!(messages, expected);
    }

    #[test]
    fn a_header_error_does_not_hide_a_posting_error() {
        // The bad date and the bad amount are both reported, proving the
        // postings are scanned even when the header fails
        let src = "2020-13-01 Grocery\n    Expenses:Food    $\n";
        assert_eq!(parse(FILE, src).len(), 2);
    }

    #[test]
    fn a_posting_error_under_a_good_header_is_reported() {
        // A well-formed header, one malformed posting amount
        let src = "2020-01-02 Grocery\n    Expenses:Food    @@@\n";
        let errors = parse(FILE, src);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.first().unwrap().message, "expected a commodity");
    }

    #[test]
    fn an_include_directive_is_accepted() {
        assert!(parse(FILE, "include transactions/2020.ledger\n").is_empty());
    }

    #[test]
    fn periodic_and_automated_blocks_are_refused() {
        let periodic = parse(FILE, "~ monthly\n    Budget:Food    $400.00\n");
        assert_eq!(periodic.len(), 1);
        assert_eq!(
            periodic.first().unwrap().message,
            "periodic transactions are not supported yet"
        );

        let automated = parse(FILE, "= /Food/\n    Budget:Food    $10.00\n");
        assert_eq!(automated.len(), 1);
        assert_eq!(
            automated.first().unwrap().message,
            "automated transactions are not supported yet"
        );
    }

    #[test]
    fn a_comment_region_produces_no_error() {
        // The region swallows transaction-shaped lines raw, so nothing inside
        // is scanned and no error is produced
        let src = "comment\n2020-13-01 not parsed at all\nend comment\n";
        assert!(parse(FILE, src).is_empty());
    }

    #[test]
    fn an_empty_source_reports_no_errors() {
        assert!(parse(FILE, "").is_empty());
    }
}
