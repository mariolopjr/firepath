//! Transaction header scanning: the column-0 line a transaction block opens
//! with
//!
//! A header is a date, an optional `=`-joined auxiliary date, an optional
//! status marker `*` or `!`, an optional parenthesized code, and an optional
//! payee.
//!
//! A trailing `;` comment is not split off yet
//!
//! A [`Transaction`] pairs a parsed header with the postings under it, the whole
//! block after [`parse`](crate::parse) has scanned it

use crate::date::Date;
use crate::error::ParseError;
use crate::posting::Posting;
use crate::scan::{skip_ws, span_at, trim_end, widen_empty};
use crate::span::{Span, clamp_u32};

/// A whole transaction: its header and the postings under it
///
/// This is what [`parse`](crate::parse) keeps for each transaction block, the
/// header and postings that used to be scanned only to be dropped. A transaction
/// exists only when its header scanned, since without a date there is nothing to
/// date, balance, or print. Its postings are the ones that scanned, so a block
/// whose header is good but whose posting is malformed still yields a
/// transaction, with the bad posting absent and its error recorded separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// The header line's parsed parts: date, status, code, payee
    pub header: TransactionHeader,
    /// The postings under the header, in source order, each one that scanned
    pub postings: Vec<Posting>,
}

/// The clearing status a header carries between the date and the payee
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No marker: the transaction is not reconciled yet
    Uncleared,
    /// `!`: the transaction is pending reconciliation
    Pending,
    /// `*`: the transaction is cleared
    Cleared,
}

/// A parsed transaction header line
///
/// Spans are file-absolute through the `base` handed to
/// [`parse`](TransactionHeader::parse), so they line up with the block spans and
/// need no caller arithmetic. The fields are public like [`crate::Parsed`]'s:
/// consumers read them directly
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionHeader {
    /// The transaction's date
    pub date: Date,
    /// The actual date's text, always the line's first ten bytes
    pub date_span: Span,
    /// The `=`-joined auxiliary date, `None` when the header has none. It is
    /// held so the writer emits it back, and its text always sits in the ten
    /// bytes past the `=` that follows `date_span`
    pub aux_date: Option<Date>,
    /// The clearing status, [`Status::Uncleared`] when no marker is written
    pub status: Status,
    /// The text between the code parentheses, `None` when no code is written
    pub code: Option<Span>,
    /// The payee text, trailing whitespace dropped. `None` when the line
    /// ends before one, the header ledger reports as `<Unspecified payee>`
    pub payee: Option<Span>,
}

impl TransactionHeader {
    /// Scan one header from `line`, a transaction block's opening line with
    /// no trailing newline or carriage return. `base` is the line's byte
    /// offset into its file, so every span and error lands file-absolute
    ///
    /// # Errors
    /// A malformed actual or auxiliary date errors where that date sits, with
    /// the date scanner's message. An empty date, whose own span is empty,
    /// widens the span to one neighboring byte so the error stays visible
    pub fn parse(line: &[u8], base: u32) -> Result<Self, ParseError> {
        let bytes = line;

        // The date token runs to the first whitespace. An `=` inside it joins
        // the auxiliary date to the actual one
        let token_end = memchr::memchr2(b' ', b'\t', bytes).unwrap_or(bytes.len());
        let token = line.get(..token_end).unwrap_or_default();

        let (date, aux_date, date_len) = match memchr::memchr(b'=', token) {
            Some(join) => {
                let actual = token.get(..join).unwrap_or_default();
                let aux = token.get(join.saturating_add(1)..).unwrap_or_default();
                let date = Date::parse(actual).map_err(|err| shift(err, base, 0, line.len()))?;
                let aux = Date::parse(aux)
                    .map_err(|err| shift(err, base, actual.len().saturating_add(1), line.len()))?;
                (date, Some(aux), actual.len())
            }
            None => (
                Date::parse(token).map_err(|err| shift(err, base, 0, line.len()))?,
                None,
                token.len(),
            ),
        };
        let date_span = span_at(base, 0, date_len);

        // A single status byte, whitespace allowed on both sides of it
        let mut pos = skip_ws(bytes, token_end);
        let status = match bytes.get(pos) {
            Some(b'*') => {
                pos = pos.saturating_add(1);
                Status::Cleared
            }
            Some(b'!') => {
                pos = pos.saturating_add(1);
                Status::Pending
            }
            _ => Status::Uncleared,
        };
        pos = skip_ws(bytes, pos);

        // A code is parenthesized and closed on the same line. The `(` is
        // consumed either way: without its closer no code is set and the
        // payee starts right past the `(`, leading whitespace kept
        let mut code = None;
        if bytes.get(pos) == Some(&b'(') {
            let open = pos.saturating_add(1);
            pos = match memchr::memchr(b')', bytes.get(open..).unwrap_or_default()) {
                Some(i) => {
                    let close = open.saturating_add(i);
                    code = Some(span_at(base, open, close));
                    skip_ws(bytes, close.saturating_add(1))
                }
                None => open,
            };
        }

        // The payee is the rest of the line, trailing whitespace dropped.
        // Internal runs of spaces stay: the payee has no column split
        let end = trim_end(bytes, pos, bytes.len());
        let payee = (pos < end).then(|| span_at(base, pos, end));

        Ok(Self {
            date,
            date_span,
            aux_date,
            status,
            code,
            payee,
        })
    }
}

/// Re-anchor a scanner error from token-relative to file-absolute offsets,
/// for a token starting `token_start` bytes into the line
///
/// An empty token errors with an empty span, so the re-anchored span is widened
/// to stay visible, bounded by the line it lands in
fn shift(err: ParseError, base: u32, token_start: usize, line_len: usize) -> ParseError {
    let delta = base.saturating_add(clamp_u32(token_start));
    let span = Span::new(
        err.span.start().saturating_add(delta),
        err.span.end().saturating_add(delta),
    );
    ParseError::new(err.message, widen_empty(span, base, line_len))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::{Status, TransactionHeader};
    use crate::span::Span;

    // Resolve a span back to its text so expectations read as source text
    // instead of byte offsets
    fn text(line: &str, span: Span) -> &str {
        let range = usize::try_from(span.start()).unwrap()..usize::try_from(span.end()).unwrap();
        line.get(range).unwrap()
    }

    // Parse at base 0 and check every field but the aux date, returning the
    // header so a caller can check that too. The date is checked through its
    // epoch day
    fn check(
        line: &str,
        epoch: i32,
        status: Status,
        code: Option<&str>,
        payee: Option<&str>,
    ) -> TransactionHeader {
        let header = TransactionHeader::parse(line.as_bytes(), 0).unwrap();
        assert_eq!(header.date.epoch_day(), epoch, "date of {line:?}");
        assert_eq!(header.date_span, Span::new(0, 10), "date span of {line:?}");
        assert_eq!(header.status, status, "status of {line:?}");
        assert_eq!(
            header.code.map(|span| text(line, span)),
            code,
            "code of {line:?}"
        );
        assert_eq!(
            header.payee.map(|span| text(line, span)),
            payee,
            "payee of {line:?}"
        );
        header
    }

    // 2020-01-02 sits 18263 days after 1970-01-01, 2020-01-05 three more
    const EPOCH_DAY: i32 = 18263;
    const AUX_EPOCH_DAY: i32 = 18266;

    #[test]
    fn a_plain_header_is_a_date_and_a_payee() {
        let header = check(
            "2020-01-02 Grocery Store",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery Store"),
        );
        assert_eq!(header.aux_date, None);
        // The other separator forms scan the same way
        check(
            "2020/01/02 Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery"),
        );
        check(
            "2020.01.02 Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery"),
        );
    }

    #[test]
    fn status_markers_read_as_cleared_and_pending() {
        check(
            "2020-01-02 * Grocery",
            EPOCH_DAY,
            Status::Cleared,
            None,
            Some("Grocery"),
        );
        check(
            "2020-01-02 ! Grocery",
            EPOCH_DAY,
            Status::Pending,
            None,
            Some("Grocery"),
        );
        // No space between the marker and the payee
        check(
            "2020-01-02 *Grocery",
            EPOCH_DAY,
            Status::Cleared,
            None,
            Some("Grocery"),
        );
    }

    #[test]
    fn a_code_sits_between_status_and_payee() {
        check(
            "2020-01-02 * (A1) Grocery",
            EPOCH_DAY,
            Status::Cleared,
            Some("A1"),
            Some("Grocery"),
        );
        check(
            "2020-01-02 (A1) Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            Some("A1"),
            Some("Grocery"),
        );
        // An empty code is still a code, distinct from no parentheses at all
        check(
            "2020-01-02 () Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            Some(""),
            Some("Grocery"),
        );
    }

    #[test]
    fn an_unclosed_code_paren_is_consumed_without_a_code() {
        // No closer on the line, so no code is set, but the `(` is still
        // consumed and whatever follows it is the payee, leading whitespace
        // kept
        check(
            "2020-01-02 (oops Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("oops Grocery"),
        );
        check(
            "2020-01-02 (  spaced",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("  spaced"),
        );
    }

    #[test]
    fn parens_in_payee_position_are_payee_text() {
        // The payee does not start with `(`, so its parens are just text
        check(
            "2020-01-02 Grocery (Main St)",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery (Main St)"),
        );
    }

    #[test]
    fn an_aux_date_is_kept_alongside_the_actual_date() {
        let header = check(
            "2020-01-02=2020-01-05 Grocery",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery"),
        );
        assert_eq!(header.aux_date.unwrap().epoch_day(), AUX_EPOCH_DAY);
        let header = check(
            "2020-01-02=2020-01-05 * (A1) Grocery",
            EPOCH_DAY,
            Status::Cleared,
            Some("A1"),
            Some("Grocery"),
        );
        assert_eq!(header.aux_date.unwrap().epoch_day(), AUX_EPOCH_DAY);
    }

    #[test]
    fn an_aux_date_joins_only_inside_the_date_token() {
        // Whitespace before the `=` ends the date token, so the rest reads
        // as payee text, matching the ledger binary
        let header = check(
            "2020-01-02 =2020-01-05 x",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("=2020-01-05 x"),
        );
        assert_eq!(header.aux_date, None);
    }

    #[test]
    fn a_bad_aux_date_is_an_error_where_the_aux_date_sits() {
        let err =
            TransactionHeader::parse("2020-01-02=2020-13-01 Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(err.message, "2020-13-01 is not a real calendar date");
        assert_eq!(err.span, Span::new(11, 21));

        let err = TransactionHeader::parse("2020-01-02=jan Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(
            err.message,
            "expected a date in YYYY-MM-DD, YYYY/MM/DD, or YYYY.MM.DD form"
        );
        assert_eq!(err.span, Span::new(11, 14));
    }

    #[test]
    fn a_bad_date_is_an_error_where_the_date_sits() {
        let err = TransactionHeader::parse("2020-13-01 Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(err.message, "2020-13-01 is not a real calendar date");
        assert_eq!(err.span, Span::new(0, 10));

        let err = TransactionHeader::parse("2020-1-2 Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(
            err.message,
            "expected a date in YYYY-MM-DD, YYYY/MM/DD, or YYYY.MM.DD form"
        );
        assert_eq!(err.span, Span::new(0, 8));

        // A bad actual date still errors on itself when an aux date follows
        let err =
            TransactionHeader::parse("2020-13-01=2020-01-05 Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(err.message, "2020-13-01 is not a real calendar date");
        assert_eq!(err.span, Span::new(0, 10));
    }

    #[test]
    fn an_empty_date_widens_its_error_span_to_stay_visible() {
        // An empty date errors with an empty span, so it widens to the byte
        // after it, here the line's end, so the byte before: the `=`
        let err = TransactionHeader::parse("2020-01-02=".as_bytes(), 0).unwrap_err();
        assert_eq!(
            err.message,
            "expected a date in YYYY-MM-DD, YYYY/MM/DD, or YYYY.MM.DD form"
        );
        assert_eq!(err.span, Span::new(10, 11));

        // Mid-line the widened span covers the byte after the empty date
        let err = TransactionHeader::parse("2020-01-02= Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(err.span, Span::new(11, 12));

        // An empty actual date widens forward over the `=`
        let err = TransactionHeader::parse("=2020-01-05 Grocery".as_bytes(), 0).unwrap_err();
        assert_eq!(err.span, Span::new(0, 1));

        // An empty line has no byte to widen to, so the span stays empty
        let err = TransactionHeader::parse("".as_bytes(), 0).unwrap_err();
        assert_eq!(err.span, Span::new(0, 0));
    }

    #[test]
    fn a_missing_payee_parses_as_none() {
        // ledger accepts a payee-less header, reporting <Unspecified payee>,
        // so the parse succeeds and the optional parts still land
        check("2020-01-02", EPOCH_DAY, Status::Uncleared, None, None);
        check("2020-01-02 ", EPOCH_DAY, Status::Uncleared, None, None);
        check("2020-01-02 *", EPOCH_DAY, Status::Cleared, None, None);
        check("2020-01-02 !  ", EPOCH_DAY, Status::Pending, None, None);
        check(
            "2020-01-02 (A1)",
            EPOCH_DAY,
            Status::Uncleared,
            Some("A1"),
            None,
        );
        check(
            "2020-01-02 * (A1)  ",
            EPOCH_DAY,
            Status::Cleared,
            Some("A1"),
            None,
        );
        // The consumed unclosed `(` leaves nothing behind it
        check("2020-01-02 (", EPOCH_DAY, Status::Uncleared, None, None);
        let header = check(
            "2020-01-02=2020-01-05",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            None,
        );
        assert_eq!(header.aux_date.unwrap().epoch_day(), AUX_EPOCH_DAY);
    }

    #[test]
    fn tabs_separate_fields_and_trailing_whitespace_drops() {
        check(
            "2020-01-02\t*\t(A1)\tGrocery Store",
            EPOCH_DAY,
            Status::Cleared,
            Some("A1"),
            Some("Grocery Store"),
        );
        check(
            "2020-01-02 Grocery \t ",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery"),
        );
        // Internal runs of spaces stay in the payee
        check(
            "2020-01-02 Grocery  Store",
            EPOCH_DAY,
            Status::Uncleared,
            None,
            Some("Grocery  Store"),
        );
    }

    #[test]
    fn spans_are_file_absolute_through_the_base() {
        let line = "2020-01-02 * (A1) Grocery";
        let header = TransactionHeader::parse(line.as_bytes(), 100).unwrap();
        assert_eq!(header.date_span, Span::new(100, 110));
        assert_eq!(header.code, Some(Span::new(114, 116)));
        assert_eq!(header.payee, Some(Span::new(118, 125)));

        // Errors shift the same way, the date's own span included
        let err = TransactionHeader::parse("2020-13-01 Grocery".as_bytes(), 100).unwrap_err();
        assert_eq!(err.span, Span::new(100, 110));
        let err =
            TransactionHeader::parse("2020-01-02=2020-13-01 Grocery".as_bytes(), 100).unwrap_err();
        assert_eq!(err.span, Span::new(111, 121));
        // A widened empty span shifts with the line too
        let err = TransactionHeader::parse("2020-01-02=".as_bytes(), 100).unwrap_err();
        assert_eq!(err.span, Span::new(110, 111));
    }
}
