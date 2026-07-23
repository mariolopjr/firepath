//! Canonical writer: format parsed transactions back to ledger's `print` form
//!
//! The shape matches ledger's `print` (`src/print.cc`) so the `print` command
//! can lean on it directly:
//!
//! - the header is the date normalized to `YYYY/MM/DD` whatever separator it was
//!   read with, an optional `=`-joined auxiliary date, a single space, then the
//!   status (`* ` cleared, `! ` pending, nothing uncleared), an optional
//!   `(code) `, and the payee
//! - each posting is indented four spaces, its account left-justified to a
//!   36-column floor widened to the longest account in the transaction, and its
//!   amount right-justified in a 12-column field, with at least two spaces
//!   between the two
//! - an amountless posting writes its account alone, the way the balancing
//!   posting of a transaction is written
//!
//! Output is bytes, not text: a commodity symbol or account name may be Latin-1,
//! and this is the byte-exact path that keeps it. Alignment counts Unicode
//! scalar values where the bytes are UTF-8, matching ledger's `unistring` width,
//! and one byte per replacement character otherwise. The exact count only moves
//! cosmetic padding: the parser splits an account from its amount on two or more
//! spaces, so any alignment re-reads to the same posting.
//!
//! Two places this reads `print` differently on purpose. First, ledger elides the
//! second of two postings when both carry a simple amount of the same commodity,
//! since the second is the inverse of the first. This writer keeps both, so
//! writing then re-parsing preserves every amount the source wrote, and the
//! `print` command decides whether to elide when it lands. Second, ledger fills a
//! missing payee with the literal `<Unspecified payee>`; this writer omits it,
//! and the separator space that would otherwise trail the status or code with it,
//! since re-parsing that filler would read back a real payee where the source
//! had none

use std::io;

use crate::posting::{Posting, PostingKind};
use crate::span::Span;
use crate::transaction::{Status, Transaction, TransactionHeader};

/// Every posting line opens with this indent
const INDENT: &[u8] = b"    ";

/// The account column is at least this wide, widened to the longest account
const MIN_ACCOUNT_WIDTH: usize = 36;

/// The amount is right-justified in a field this wide
const AMOUNT_WIDTH: usize = 12;

/// Write a run of transactions in canonical form, one blank line between them
///
/// The blank line is a separator, not a terminator: it sits between two
/// transactions and none trails the last, the way `print` spaces its output
///
/// # Errors
/// Whatever `out` returns
pub fn write_transactions(
    out: &mut impl io::Write,
    source: &[u8],
    transactions: &[Transaction],
) -> io::Result<()> {
    for (index, transaction) in transactions.iter().enumerate() {
        if index > 0 {
            out.write_all(b"\n")?;
        }
        write_transaction(out, source, transaction)?;
    }
    Ok(())
}

/// Write one transaction: its header line, then a line for each posting
///
/// `source` is the bytes the transaction was parsed from: the account, code, and
/// payee are spans into it, resolved back to their text here
///
/// # Errors
/// Whatever `out` returns
pub fn write_transaction(
    out: &mut impl io::Write,
    source: &[u8],
    transaction: &Transaction,
) -> io::Result<()> {
    write_header(out, source, &transaction.header)?;

    // The account column is sized once for the whole transaction so every
    // amount lines up under the widest account
    let account_width = account_width(source, &transaction.postings);
    for posting in &transaction.postings {
        write_posting(out, source, posting, account_width)?;
    }
    Ok(())
}

/// Write the header line: date, optional aux date, status, code, payee
///
/// The line is built in memory first, with infallible `Vec` pushes, so the
/// trailing separator space a header with no payee would otherwise carry is
/// trimmed before the one write that can fail
fn write_header(
    out: &mut impl io::Write,
    source: &[u8],
    header: &TransactionHeader,
) -> io::Result<()> {
    let mut line = Vec::new();
    push_date(&mut line, header.date);
    if let Some(aux) = header.aux_date {
        line.push(b'=');
        push_date(&mut line, aux);
    }
    line.push(b' ');
    match header.status {
        Status::Cleared => line.extend_from_slice(b"* "),
        Status::Pending => line.extend_from_slice(b"! "),
        Status::Uncleared => {}
    }
    if let Some(code) = header.code {
        line.push(b'(');
        line.extend_from_slice(slice(source, code));
        line.extend_from_slice(b") ");
    }
    if let Some(payee) = header.payee {
        line.extend_from_slice(slice(source, payee));
    }

    // The line can only end in the separator space after the date, the status,
    // or the code, the one a payee-less header would otherwise trail. The payee
    // itself is trimmed at parse and the parts before it hold no trailing tab,
    // so trimming trailing whitespace here drops only that separator
    out.write_all(line.trim_ascii_end())?;
    out.write_all(b"\n")
}

/// Push a date as `YYYY/MM/DD` onto a buffer, the separator normalized whatever
/// it was read with
fn push_date(line: &mut Vec<u8>, date: crate::Date) {
    let civil = date.civil();
    // A civil date in the 1400-9999 range formats to ten ASCII bytes, so this
    // never allocates past a small string
    line.extend_from_slice(
        format!(
            "{:04}/{:02}/{:02}",
            civil.year(),
            civil.month(),
            civil.day()
        )
        .as_bytes(),
    );
}

/// Write one posting line: the indent, the account, then the aligned amount
fn write_posting(
    out: &mut impl io::Write,
    source: &[u8],
    posting: &Posting,
    account_width: usize,
) -> io::Result<()> {
    out.write_all(INDENT)?;

    let (open, close) = brackets(posting.kind);
    let account = slice(source, posting.account);
    out.write_all(open)?;
    out.write_all(account)?;
    out.write_all(close)?;

    let Some(amount) = &posting.amount else {
        // An amountless posting writes its account alone
        return out.write_all(b"\n");
    };

    // The account name as it was just written, its Unicode width the basis for
    // the padding to the amount column
    let name_width = width(open)
        .saturating_add(width(account))
        .saturating_add(width(close));

    let mut amt = Vec::new();
    // A Vec write never fails, so the byte-exact amount always lands; the error
    // this returns cannot occur and is discarded rather than plumbed through
    let _ = amount.write_to(&mut amt);
    // The amount is right-justified in its field: pad_left is the leading space
    // that justification adds
    let pad_left = AMOUNT_WIDTH.saturating_sub(width(&amt));
    let slip = account_width.saturating_sub(name_width);

    // At least two spaces separate the account from the amount, counting the
    // account's own padding and the amount's leading space toward the two, the
    // way `print` does
    let already = slip.saturating_add(pad_left);
    let prefix = 2usize.saturating_sub(already);

    write_spaces(out, slip.saturating_add(prefix).saturating_add(pad_left))?;
    out.write_all(&amt)?;
    out.write_all(b"\n")
}

/// The widest account column the transaction needs: the floor, or the longest
/// account name with its brackets when that is wider
fn account_width(source: &[u8], postings: &[Posting]) -> usize {
    let mut widest = MIN_ACCOUNT_WIDTH;
    for posting in postings {
        let (open, close) = brackets(posting.kind);
        let name = width(open)
            .saturating_add(width(slice(source, posting.account)))
            .saturating_add(width(close));
        widest = widest.max(name);
    }
    widest
}

/// The brackets a posting's kind writes around its account
fn brackets(kind: PostingKind) -> (&'static [u8], &'static [u8]) {
    match kind {
        PostingKind::Real => (b"", b""),
        PostingKind::BalancedVirtual => (b"[", b"]"),
        PostingKind::UnbalancedVirtual => (b"(", b")"),
    }
}

/// Write `count` spaces
fn write_spaces(out: &mut impl io::Write, count: usize) -> io::Result<()> {
    const SPACES: &[u8; 32] = b"                                ";
    let mut left = count;
    while left > 0 {
        let take = left.min(SPACES.len());
        out.write_all(SPACES.get(..take).unwrap_or_default())?;
        left = left.saturating_sub(take);
    }
    Ok(())
}

/// The Unicode-scalar width of a byte slice, one per replacement character on a
/// byte that is not UTF-8, so a Latin-1 name counts near its byte length
fn width(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes).chars().count()
}

/// The bytes a span covers, empty for a span that does not land in `source`
fn slice(source: &[u8], span: Span) -> &[u8] {
    source
        .get(span.start() as usize..span.end() as usize)
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the tests terse")]
mod tests {
    use super::{slice, write_transaction, write_transactions};
    use crate::parse::parse;
    use crate::span::FileId;

    const FILE: FileId = FileId::new(0);

    // Parse a source and write its transactions back, returning the bytes as a
    // string so expectations read as text
    fn round(source: &str) -> String {
        let parsed = parse(FILE, source.as_bytes());
        assert!(
            !parsed.has_errors(),
            "the source parses cleanly: {source:?}"
        );
        let mut out = Vec::new();
        write_transactions(&mut out, source.as_bytes(), &parsed.items).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_transaction_writes_in_canonical_print_form() {
        // ledger's own `regress/A28CF697.test`: the dash date normalizes to
        // slashes, the quoted commodity keeps its quotes, and the two postings
        // align under the 36-column account floor. This is byte-for-byte what
        // `ledger print` emits for the same input
        let written = round(
            "2010-02-05 * Flight SN2094\n    Assets:Rewards:Airmiles   125 \"M&M\"\n    Income:Rewards\n",
        );
        assert_eq!(
            written,
            "2010/02/05 * Flight SN2094\n    Assets:Rewards:Airmiles                125 \"M&M\"\n    Income:Rewards\n"
        );
    }

    #[test]
    fn a_code_and_an_aux_date_are_kept() {
        let written = round(
            "2020/01/02=2020/01/05 ! (INV-9) Acme\n    Expenses:Office    $12.00\n    Assets:Checking\n",
        );
        assert_eq!(
            written,
            "2020/01/02=2020/01/05 ! (INV-9) Acme\n    Expenses:Office                           $12.00\n    Assets:Checking\n"
        );
    }

    #[test]
    fn a_payeeless_header_writes_without_a_trailing_space() {
        // Date, status, no code, no payee. ledger fills the empty payee with the
        // literal `<Unspecified payee>`; this writer omits it, and the separator
        // space that would otherwise trail the status with it, since re-parsing
        // that filler would read back a payee the source never wrote
        let source = "2020/03/01 *\n    Assets:Cash    $1.00\n    Equity\n";
        let written = round(source);
        let first = written.lines().next().unwrap();
        assert_eq!(first, "2020/03/01 *");
        assert!(!first.ends_with(' '));

        // The omission is a fixed point: the payee-less line parses back to no
        // payee, so writing it again is byte-identical, not a re-widened `*  `
        assert_eq!(round(&written), written);
    }

    #[test]
    fn virtual_postings_keep_their_brackets() {
        let written = round(
            "2020/01/01 Budgeting\n    [Assets:Savings]    $100.00\n    (Equity:Adjust)    $-100.00\n",
        );
        assert!(written.contains("    [Assets:Savings]"));
        assert!(written.contains("    (Equity:Adjust)"));
        // The bracket counts toward the account width, so both amounts still
        // align in the 12-wide field under the floor
        for line in written.lines().skip(1) {
            assert!(line.contains('$'));
        }
    }

    #[test]
    fn a_long_account_widens_the_column_and_keeps_two_spaces() {
        // An account past the 36-column floor pushes the amount right, and the
        // two-space minimum still holds between the name and the amount
        let account = "Assets:Investments:Brokerage:Retirement:Roth";
        let written = round(&format!(
            "2020/01/01 Contribution\n    {account}    $6000.00\n    Assets:Checking\n"
        ));
        let posting = written.lines().nth(1).unwrap();
        let after = posting.strip_prefix("    ").unwrap();
        let gap = &after[account.len()..];
        assert!(gap.starts_with("  "), "at least two spaces: {gap:?}");
        assert_eq!(gap.trim_start(), "$6000.00");
    }

    #[test]
    fn transactions_are_separated_by_one_blank_line() {
        let written =
            round("2020/01/01 One\n    A    $1.00\n    B\n2020/01/02 Two\n    A    $2.00\n    B\n");
        // One blank line between the two, none trailing
        assert!(written.contains("    B\n\n2020/01/02 Two"));
        assert!(!written.ends_with("\n\n"));
    }

    #[test]
    fn writing_the_canonical_form_again_changes_nothing() {
        // The output of the writer is a fixed point: parsing and writing it a
        // second time yields the identical bytes, which is what makes it canonical
        for source in [
            "2010-02-05 * Flight\n    Assets:Rewards:Airmiles   125 \"M&M\"\n    Income:Rewards\n",
            "2020/01/02=2020/01/05 ! (INV-9) Acme\n    Expenses:Office  $12.00\n    Assets:Checking\n",
            "2020.03.01 Payee\n    [Assets:Savings]  $100.00\n    (Equity:Adjust)  $-100.00\n    Assets:Checking\n",
        ] {
            let once = round(source);
            let twice = round(&once);
            assert_eq!(once, twice, "not a fixed point: {source:?}");
        }
    }

    #[test]
    fn writing_then_parsing_preserves_every_transaction() {
        // The semantic round-trip: writing loses nothing the parse read back.
        // Compared by value, since the spans point into different buffers
        let source = "2010-02-05 * (A1) Flight\n    Assets:Rewards:Airmiles   125 \"M&M\"\n    [Income:Rewards]  $-5.00\n2020/06/07 Coffee\n    Expenses:Coffee    $4.00\n    Assets:Cash\n";
        let before = parse(FILE, source.as_bytes());
        let mut out = Vec::new();
        write_transactions(&mut out, source.as_bytes(), &before.items).unwrap();
        let after = parse(FILE, &out);

        assert_eq!(before.items.len(), after.items.len());
        for (a, b) in before.items.iter().zip(&after.items) {
            assert_eq!(a.header.date.civil(), b.header.date.civil());
            assert_eq!(
                a.header.aux_date.map(crate::Date::civil),
                b.header.aux_date.map(crate::Date::civil)
            );
            assert_eq!(a.header.status, b.header.status);
            assert_eq!(
                a.header.code.map(|s| slice(source.as_bytes(), s).to_vec()),
                b.header.code.map(|s| slice(&out, s).to_vec())
            );
            assert_eq!(
                a.header.payee.map(|s| slice(source.as_bytes(), s).to_vec()),
                b.header.payee.map(|s| slice(&out, s).to_vec())
            );
            assert_eq!(a.postings.len(), b.postings.len());
            for (pa, pb) in a.postings.iter().zip(&b.postings) {
                assert_eq!(pa.kind, pb.kind);
                assert_eq!(
                    slice(source.as_bytes(), pa.account),
                    slice(&out, pb.account)
                );
                assert_eq!(pa.amount, pb.amount);
            }
        }
    }

    #[test]
    fn a_transaction_with_no_postings_writes_its_header_alone() {
        let parsed = parse(FILE, "2020/01/01 Note only\n".as_bytes());
        let transaction = parsed.items.first().unwrap();
        let mut out = Vec::new();
        write_transaction(&mut out, "2020/01/01 Note only\n".as_bytes(), transaction).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "2020/01/01 Note only\n");
    }

    #[test]
    fn a_writer_that_runs_out_of_room_surfaces_the_error() {
        // A `&mut [u8]` is a writer with a fixed capacity: once full, write_all
        // returns WriteZero. Sizing the buffer from zero up to one byte short of
        // the full output walks the failure through every write the writer does,
        // so each error path is exercised. The input carries an aux date, a
        // code, a virtual posting, an amount, and an amountless posting, and two
        // transactions, so every write site is on the path
        let source = "2010-02-05=2010-02-06 * (A1) Flight\n    [Assets:Rewards:Airmiles]   $125.00\n    Income:Rewards\n2020/06/07 Coffee\n    Expenses:Coffee    $4.00\n    Assets:Cash\n";
        let parsed = parse(FILE, source.as_bytes());
        assert!(!parsed.has_errors());

        let full = round(source).len();
        for capacity in 0..full {
            let mut buf = vec![0u8; capacity];
            let mut out: &mut [u8] = &mut buf;
            assert!(
                write_transactions(&mut out, source.as_bytes(), &parsed.items).is_err(),
                "a writer holding {capacity} of {full} bytes should fail"
            );
        }
        // The exact capacity writes without error
        let mut buf = vec![0u8; full];
        let mut out: &mut [u8] = &mut buf;
        assert!(write_transactions(&mut out, source.as_bytes(), &parsed.items).is_ok());
    }

    #[test]
    fn an_amount_wider_than_its_field_keeps_two_spaces() {
        // A 13-character amount overflows the 12-wide field, so justification
        // adds no leading space; the two-space minimum has to come from the gap
        let written = round("2020/01/01 Big\n    A    1234567890 VTI\n    B\n");
        let posting = written.lines().nth(1).unwrap();
        assert!(posting.contains("A  ") || posting.contains("A   "));
        assert!(posting.trim_end().ends_with("1234567890 VTI"));
    }
}
