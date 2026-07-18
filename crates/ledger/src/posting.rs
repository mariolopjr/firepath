//! Posting scanning: one indented line under a transaction header
//!
//! A posting names an account and, optionally, an amount. The two split on the
//! first tab or run of two or more spaces, so an account name may hold single
//! spaces (`Assets:Investments:Fidelity 401(k)`) while the amount stays
//! cleanly separated. A posting with nothing after the account is amountless:
//! the amount is inferred later when the transaction is balanced.
//!
//! Brackets around the whole account name mark a virtual posting: `[account]`
//! is balanced, part of the transaction's balance alongside the real postings,
//! and `(account)` is unbalanced, exempt from it.
//!
//! Whether the account name is meaningful is not scanning's call. An empty name,
//! `[]` and its bare and unbalanced kin, scans without error the way ledger
//! accepts it, and a name that is empty, undeclared, or malformed is the
//! balancing layer's to reject.
//!
//! Costs, balance assertions, and trailing comments are not scanned yet, so a
//! posting carrying one errors as a malformed amount until the wave that adds
//! them lands

use crate::amount::Amount;
use crate::error::ParseError;
use crate::scan::{skip_ws, span_at, trim_end, widen_empty};
use crate::span::{Span, clamp_u32};

/// A posting's balancing role, decided by the brackets around its account
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostingKind {
    /// A plain posting, `Assets:Cash`
    Real,
    /// `[account]`: part of the combined balance with the real postings
    BalancedVirtual,
    /// `(account)`: exempt from balancing
    UnbalancedVirtual,
}

/// A parsed posting line
///
/// Spans are file-absolute through the `base` handed to
/// [`parse`](Posting::parse), so they line up with the block spans and need no
/// caller arithmetic. The fields are public like [`crate::Parsed`]'s:
/// consumers read them directly
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    /// Real, or a balanced or unbalanced virtual marked by brackets
    pub kind: PostingKind,
    /// The account name, the brackets stripped for a virtual posting
    pub account: Span,
    /// The amount, `None` for an amountless posting
    pub amount: Option<Amount>,
    /// The amount's text span, `None` when the amount is elided. Paired with
    /// [`amount`](Posting::amount) so the token layer can locate it
    pub amount_span: Option<Span>,
}

impl Posting {
    /// Scan one posting from `line`, a transaction block's indented child line
    /// with no trailing newline or carriage return. `base` is the line's byte
    /// offset into its file, so every span and error lands file-absolute
    ///
    /// # Errors
    /// A non-empty amount that does not scan errors with the amount scanner's
    /// message at the amount's span
    pub fn parse(line: &str, base: u32) -> Result<Self, ParseError> {
        let bytes = line.as_bytes();
        // Postings arrive indented under their transaction, so skip the
        // indentation to land the account span on the name
        let content_start = skip_ws(bytes, 0);

        // The account and amount split on the first tab or run of two or more
        // spaces, so an account name keeps its single spaces. Before the split
        // is the account, after it the amount
        let split = find_split(bytes, content_start).unwrap_or(bytes.len());
        let account_end = trim_end(bytes, content_start, split);

        // Brackets around the whole account name mark a virtual posting. They
        // are recognized only after the split, so the two-space rule that lets
        // an account hold single spaces stays the one rule and a stray bracket
        // inside a real name is left in place
        let first = bytes.get(content_start).copied();
        let last = bytes.get(account_end.saturating_sub(1)).copied();
        let inner_lo = content_start.saturating_add(1);
        let inner_hi = account_end.saturating_sub(1);
        let (kind, account) = match (first, last) {
            (Some(b'['), Some(b']')) => (
                PostingKind::BalancedVirtual,
                trim_span(bytes, base, inner_lo, inner_hi),
            ),
            (Some(b'('), Some(b')')) => (
                PostingKind::UnbalancedVirtual,
                trim_span(bytes, base, inner_lo, inner_hi),
            ),
            _ => (PostingKind::Real, span_at(base, content_start, account_end)),
        };

        // The amount is whatever follows the split, trimmed. Nothing, or only
        // whitespace, is an amountless posting
        let amount_lo = skip_ws(bytes, split);
        let amount_hi = trim_end(bytes, amount_lo, bytes.len());
        let (amount, amount_span) = if amount_lo < amount_hi {
            let text = line.get(amount_lo..amount_hi).unwrap_or_default();
            let amount =
                Amount::parse(text).map_err(|err| shift(err, base, amount_lo, line.len()))?;
            (Some(amount), Some(span_at(base, amount_lo, amount_hi)))
        } else {
            (None, None)
        };

        Ok(Self {
            kind,
            account,
            amount,
            amount_span,
        })
    }
}

/// The index of the account and amount split: the first tab or the first of a
/// run of two or more spaces at or past `from`, or `None` when neither is found
fn find_split(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(b'\t') => return Some(i),
            Some(b' ') if bytes.get(i.saturating_add(1)) == Some(&b' ') => return Some(i),
            _ => {}
        }
        i = i.saturating_add(1);
    }
    None
}

/// A file-absolute span for `lo..hi` with any surrounding spaces and tabs
/// trimmed, so a bracketed account written `[ Cash ]` spans just the name
fn trim_span(bytes: &[u8], base: u32, lo: usize, hi: usize) -> Span {
    let lo = skip_ws(bytes, lo).min(hi);
    let hi = trim_end(bytes, lo, hi);
    span_at(base, lo, hi)
}

/// Re-anchor a scanner error from amount-relative to file-absolute offsets,
/// for an amount starting `offset` bytes into the line
///
/// A missing number errors with an empty span at the point it was expected, so
/// the re-anchored span is widened to stay visible, bounded by the line it
/// lands in
fn shift(err: ParseError, base: u32, offset: usize, line_len: usize) -> ParseError {
    let delta = base.saturating_add(clamp_u32(offset));
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
    use super::{Posting, PostingKind};
    use crate::amount::{Amount, Placement};
    use crate::span::Span;

    // Resolve a span back to its text so expectations read as source text
    // instead of byte offsets
    fn text(line: &str, span: Span) -> &str {
        let range = usize::try_from(span.start()).unwrap()..usize::try_from(span.end()).unwrap();
        line.get(range).unwrap()
    }

    // Parse at base 0, check the kind and account text, then check the amount:
    // its span must cover exactly the bytes that scan back to the amount, or
    // both the amount and its span are absent
    fn check(
        line: &str,
        kind: PostingKind,
        account: &str,
        amount: Option<(&str, &str, Placement)>,
    ) {
        let posting = Posting::parse(line, 0).unwrap();
        assert_eq!(posting.kind, kind, "kind of {line:?}");
        assert_eq!(text(line, posting.account), account, "account of {line:?}");
        // The amount and its span are present together or absent together
        assert_eq!(
            posting.amount.is_some(),
            amount.is_some(),
            "amount presence of {line:?}"
        );
        assert_eq!(
            posting.amount_span.is_some(),
            amount.is_some(),
            "amount span presence of {line:?}"
        );
        if let (Some((quantity, symbol, placement)), Some(got), Some(span)) =
            (amount, &posting.amount, posting.amount_span)
        {
            assert_eq!(got.quantity.to_string(), quantity, "quantity of {line:?}");
            assert_eq!(got.commodity.symbol(), symbol, "symbol of {line:?}");
            assert_eq!(
                got.commodity.placement(),
                placement,
                "placement of {line:?}"
            );
            // The span covers exactly the amount and nothing else
            assert_eq!(
                &Amount::parse(text(line, span)).unwrap(),
                got,
                "span of {line:?}"
            );
        }
    }

    #[test]
    fn a_real_posting_splits_account_from_amount() {
        check(
            "    Expenses:Food    $50.00",
            PostingKind::Real,
            "Expenses:Food",
            Some(("50.00", "$", Placement::Prefix)),
        );
        // Exactly two spaces is enough to split
        check(
            "    Assets:Cash  -$20",
            PostingKind::Real,
            "Assets:Cash",
            Some(("-20", "$", Placement::Prefix)),
        );
        // A suffix commodity comes through the split intact
        check(
            "    Assets:Broker  5 VTI",
            PostingKind::Real,
            "Assets:Broker",
            Some(("5", "VTI", Placement::Suffix)),
        );
    }

    #[test]
    fn an_account_name_keeps_its_single_spaces() {
        // The single space inside the name is not a split, and the parentheses
        // in `401(k)` do not make it virtual because the name does not start
        // with a bracket
        check(
            "    Assets:Investments:Fidelity 401(k)    $100.00",
            PostingKind::Real,
            "Assets:Investments:Fidelity 401(k)",
            Some(("100.00", "$", Placement::Prefix)),
        );
    }

    #[test]
    fn a_tab_splits_account_from_amount() {
        // A tab as the whole separator, and a tab after a space, both split
        check(
            "\tExpenses:Food\t$50.00",
            PostingKind::Real,
            "Expenses:Food",
            Some(("50.00", "$", Placement::Prefix)),
        );
        check(
            "    Assets:Cash \t $5",
            PostingKind::Real,
            "Assets:Cash",
            Some(("5", "$", Placement::Prefix)),
        );
    }

    #[test]
    fn virtual_postings_are_marked_and_unbracketed() {
        check(
            "    [Assets:Cash]  $10.00",
            PostingKind::BalancedVirtual,
            "Assets:Cash",
            Some(("10.00", "$", Placement::Prefix)),
        );
        check(
            "    (Equity:Adjust)  $5.00",
            PostingKind::UnbalancedVirtual,
            "Equity:Adjust",
            Some(("5.00", "$", Placement::Prefix)),
        );
        // Whitespace inside the brackets is trimmed off the account span
        check(
            "    [ Assets:Cash ]  $10.00",
            PostingKind::BalancedVirtual,
            "Assets:Cash",
            Some(("10.00", "$", Placement::Prefix)),
        );
        // A short unbalanced account is still recognized
        check(
            "    (k)  $5.00",
            PostingKind::UnbalancedVirtual,
            "k",
            Some(("5.00", "$", Placement::Prefix)),
        );
    }

    #[test]
    fn an_amountless_posting_has_no_amount() {
        // The balancing posting of a transaction carries no amount
        check(
            "    Assets:Checking",
            PostingKind::Real,
            "Assets:Checking",
            None,
        );
        // Trailing whitespace alone is not an amount
        check(
            "    Assets:Checking   ",
            PostingKind::Real,
            "Assets:Checking",
            None,
        );
        // A virtual posting can be amountless too, brackets still stripped
        check(
            "    [Assets:Cash]",
            PostingKind::BalancedVirtual,
            "Assets:Cash",
            None,
        );
    }

    #[test]
    fn an_empty_account_scans_without_error() {
        // ledger accepts an empty bracketed account, printing it back as `[]`,
        // so scanning keeps it: the brackets still set the kind and strip to an
        // empty name, which the balancing layer judges, not the scanner
        check(
            "    []  $10",
            PostingKind::BalancedVirtual,
            "",
            Some(("10", "$", Placement::Prefix)),
        );
        check(
            "    ()  $5",
            PostingKind::UnbalancedVirtual,
            "",
            Some(("5", "$", Placement::Prefix)),
        );
        // A line with no content degrades to an empty real posting instead of
        // panicking. Grouping never hands one down, a blank line closes a block,
        // so this only guards the direct-call contract
        check("", PostingKind::Real, "", None);
        check("     ", PostingKind::Real, "", None);
    }

    #[test]
    fn an_unmatched_bracket_stays_a_real_account() {
        // A name that opens with `[` but does not close with `]` before the
        // split is a real account with the bracket kept as a literal byte
        check(
            "    [Assets:Cash  $5.00",
            PostingKind::Real,
            "[Assets:Cash",
            Some(("5.00", "$", Placement::Prefix)),
        );
        // A closing bracket without an opener is likewise left in the name
        check(
            "    Liabilities:Loan (Car)  $5.00",
            PostingKind::Real,
            "Liabilities:Loan (Car)",
            Some(("5.00", "$", Placement::Prefix)),
        );
    }

    #[test]
    fn spans_are_file_absolute_through_the_base() {
        // `    Expenses:Food    $50.00` at base 1000: the account is bytes
        // 4..17 of the line, the amount 21..27
        let line = "    Expenses:Food    $50.00";
        let posting = Posting::parse(line, 1000).unwrap();
        assert_eq!(posting.account, Span::new(1004, 1017));
        assert_eq!(posting.amount_span, Some(Span::new(1021, 1027)));

        // A virtual account's span is the bracket interior, still shifted
        let posting = Posting::parse("  [Assets:Cash]  $1", 1000).unwrap();
        assert_eq!(posting.account, Span::new(1003, 1014));
    }

    #[test]
    fn a_malformed_amount_errors_at_its_span() {
        // A commodity with no number errors where the number should be, an
        // empty span one byte past the `$` at offset 21. The empty span widens
        // back over the `$` so an editor can show it, and shifts file-absolute
        let err = Posting::parse("    Expenses:Food    $", 0).unwrap_err();
        assert_eq!(err.message, "expected a number");
        assert_eq!(err.span, Span::new(21, 22));

        // The same amount at a nonzero base shifts by the base
        let err = Posting::parse("    Expenses:Food    $", 1000).unwrap_err();
        assert_eq!(err.message, "expected a number");
        assert_eq!(err.span, Span::new(1021, 1022));

        // Leftover text after the amount is the amount scanner's error, so a
        // cost annotation reads as malformed until its wave lands
        let err = Posting::parse("    Assets:Broker  5 VTI @ $10", 0).unwrap_err();
        assert_eq!(err.message, "unexpected characters after the amount");
    }
}
