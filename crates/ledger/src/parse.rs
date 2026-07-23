//! Whole-file parse: group a source file into blocks, then hand each block to
//! the scanner that reads its kind
//!
//! A block whose construct has no scanner yet is refused by name rather than
//! skipped, so an emitter can never quietly outrun the parser: a periodic or
//! automated transaction errors here until its own wave lands, the same way an
//! unsupported directive does

use crate::Parsed;
use crate::directive::Directive;
use crate::error::ParseError;
use crate::lines::{Block, BlockKind, blocks};
use crate::posting::Posting;
use crate::span::{FileId, Span};
use crate::transaction::{Transaction, TransactionHeader};

/// Parse one source file into its transactions and every error in it
///
/// The parse never stops at the first error: each block is scanned on its own,
/// so one malformed line does not hide the next. Every transaction that scanned
/// lands in `items`, in source order, so a later pass can balance, print, or
/// query it without re-reading the file. A comment or directive block produces
/// no item, only the errors it raises
pub fn parse(file: FileId, source: &[u8]) -> Parsed<Transaction> {
    let grouped = blocks(file, source);
    let mut parsed = Parsed::new(file);
    // Unassociated-indentation errors from grouping come first, then each block
    // adds its own, so the whole error list stays in the order it did before
    // items were kept
    parsed.errors = grouped.errors;
    for block in &grouped.items {
        match block.kind {
            // A comment carries no error and no item
            BlockKind::Comment => {}
            BlockKind::Transaction => parse_transaction(source, block, &mut parsed),
            BlockKind::Directive => {
                if let Err(err) =
                    Directive::parse(slice(source, block.header), block.header.start())
                {
                    parsed.errors.push(err);
                }
            }
            // No scanner yet, so refuse rather than skip
            BlockKind::Periodic => parsed.errors.push(ParseError::new(
                "periodic transactions are not supported yet",
                block.header,
            )),
            BlockKind::Automated => parsed.errors.push(ParseError::new(
                "automated transactions are not supported yet",
                block.header,
            )),
        }
    }
    parsed
}

/// Scan a transaction's header and each of its posting lines into a
/// [`Transaction`], collecting every error along the way
///
/// The header and the postings are all scanned even when one fails, so a bad
/// header does not mask a bad posting below it. A transaction is kept only when
/// its header scanned: without a date there is nothing downstream can do with
/// it, and its postings are meaningless unattached. The postings kept are the
/// ones that scanned, so a good header over a bad posting still yields a
/// transaction with the bad posting absent and its error recorded. Every child
/// is a posting for now; inline comment and tag children get their own routing
/// when that grammar is supported
fn parse_transaction(source: &[u8], block: &Block, parsed: &mut Parsed<Transaction>) {
    let header = match TransactionHeader::parse(slice(source, block.header), block.header.start()) {
        Ok(header) => Some(header),
        Err(err) => {
            parsed.errors.push(err);
            None
        }
    };

    let mut postings = Vec::with_capacity(block.children.len());
    for &child in &block.children {
        match Posting::parse(slice(source, child), child.start()) {
            Ok(posting) => postings.push(posting),
            Err(err) => parsed.errors.push(err),
        }
    }

    if let Some(header) = header {
        parsed.items.push(Transaction { header, postings });
    }
}

/// The bytes a span covers, empty for a span that does not land in `source`
fn slice(source: &[u8], span: Span) -> &[u8] {
    source
        .get(span.start() as usize..span.end() as usize)
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::parse;
    use crate::span::{FileId, Span};
    use crate::transaction::Status;

    const FILE: FileId = FileId::new(0);

    // Resolve a span back to its text so expectations read as source text
    // instead of byte offsets
    fn text(src: &str, span: Span) -> &str {
        let range = usize::try_from(span.start()).unwrap()..usize::try_from(span.end()).unwrap();
        src.get(range).unwrap()
    }

    #[test]
    fn a_clean_file_yields_its_transaction_with_no_errors() {
        // A header comment, an include, and a transaction with two postings:
        // one of each supported kind, all well-formed
        let src = "\
; a header comment
include 2020.ledger
2020-01-02 * Grocery
    Expenses:Food    $50.00
    Assets:Checking
";
        let parsed = parse(FILE, src.as_bytes());
        assert!(!parsed.has_errors());
        // The comment and the include produce no item; the transaction does,
        // and it keeps both postings, the amountless balancing one included
        assert_eq!(parsed.items.len(), 1);
        let transaction = parsed.items.first().unwrap();
        assert_eq!(transaction.header.status, Status::Cleared);
        assert_eq!(transaction.postings.len(), 2);
        assert_eq!(transaction.postings.get(1).unwrap().amount, None);
    }

    #[test]
    fn every_transaction_in_a_file_becomes_its_own_item() {
        // Two transactions with a comment between them: two items, in source
        // order, each carrying its own postings
        let src = "\
2020-01-02 One
    Expenses:Food    $5.00
    Assets:Cash
; a note between them
2020-01-03 Two
    Expenses:Coffee    $4.00
    Assets:Cash
";
        let parsed = parse(FILE, src.as_bytes());
        assert!(!parsed.has_errors());
        let payees: Vec<_> = parsed
            .items
            .iter()
            .map(|t| text(src, t.header.payee.unwrap()))
            .collect();
        assert_eq!(payees, ["One", "Two"]);
        assert!(parsed.items.iter().all(|t| t.postings.len() == 2));
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
        let parsed = parse(FILE, src.as_bytes());
        let mut messages: Vec<&str> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
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
    fn a_bad_header_yields_no_item_but_still_reports_its_postings() {
        // The bad date and the bad amount are both reported, proving the
        // postings are scanned even when the header fails. No date means no
        // transaction to keep, so the good and bad postings alike are dropped
        // with the header they belonged to
        let src = "2020-13-01 Grocery\n    Assets:Cash    $5.00\n    Expenses:Food    $\n";
        let parsed = parse(FILE, src.as_bytes());
        assert_eq!(parsed.items.len(), 0);
        let mut messages: Vec<&str> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
        messages.sort_unstable();
        assert_eq!(
            messages,
            [
                "2020-13-01 is not a real calendar date",
                "expected a number"
            ]
        );
    }

    #[test]
    fn a_good_header_over_a_bad_posting_keeps_the_transaction_without_it() {
        // The header scans, so the transaction is kept, but the one malformed
        // posting is left out and its error recorded. The good posting stays
        let src = "2020-01-02 Grocery\n    Assets:Cash    $5.00\n    Expenses:Food    @@@\n";
        let parsed = parse(FILE, src.as_bytes());
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            parsed.errors.first().unwrap().message,
            "expected a commodity"
        );
        assert_eq!(parsed.items.len(), 1);
        // Only the posting that scanned is attached
        let transaction = parsed.items.first().unwrap();
        assert_eq!(transaction.postings.len(), 1);
        assert_eq!(
            text(src, transaction.postings.first().unwrap().account),
            "Assets:Cash"
        );
    }

    #[test]
    fn a_header_only_transaction_is_kept_with_no_postings() {
        // A header that scans with nothing under it is still an item. Whether a
        // postingless transaction is meaningful is the balancer's call, not the
        // parser's, so the scan keeps it with an empty postings list rather than
        // dropping it
        let src = "2020-01-02 Grocery\n";
        let parsed = parse(FILE, src.as_bytes());
        assert!(!parsed.has_errors());
        assert_eq!(parsed.items.len(), 1);
        let transaction = parsed.items.first().unwrap();
        assert_eq!(text(src, transaction.header.payee.unwrap()), "Grocery");
        assert!(transaction.postings.is_empty());
    }

    #[test]
    fn an_include_directive_is_accepted() {
        let parsed = parse(FILE, "include transactions/2020.ledger\n".as_bytes());
        assert!(!parsed.has_errors());
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn periodic_and_automated_blocks_are_refused() {
        let periodic = parse(FILE, "~ monthly\n    Budget:Food    $400.00\n".as_bytes());
        assert!(periodic.items.is_empty());
        assert_eq!(periodic.errors.len(), 1);
        assert_eq!(
            periodic.errors.first().unwrap().message,
            "periodic transactions are not supported yet"
        );

        let automated = parse(FILE, "= /Food/\n    Budget:Food    $10.00\n".as_bytes());
        assert!(automated.items.is_empty());
        assert_eq!(automated.errors.len(), 1);
        assert_eq!(
            automated.errors.first().unwrap().message,
            "automated transactions are not supported yet"
        );
    }

    #[test]
    fn a_comment_region_produces_no_error_and_no_item() {
        // The region swallows transaction-shaped lines raw, so nothing inside
        // is scanned, no error is produced, and no transaction is kept
        let src = "comment\n2020-13-01 not parsed at all\nend comment\n";
        let parsed = parse(FILE, src.as_bytes());
        assert!(!parsed.has_errors());
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn an_empty_source_reports_no_errors_and_no_items() {
        let parsed = parse(FILE, "".as_bytes());
        assert!(!parsed.has_errors());
        assert!(parsed.items.is_empty());
    }
}
