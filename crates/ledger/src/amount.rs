//! Amount scanning: a quantity paired with its commodity
//!
//! An amount is a decimal quantity and the commodity it is denominated in. The
//! commodity is written on either side of the number: a prefix (`$5`) or a
//! suffix (`5 VTI`). A suffix commodity that holds spaces or other characters
//! that would otherwise end the symbol is quoted (`"MUTF: VFIAX"`).
//!
//! The quantity is [`rust_decimal::Decimal`] so money stays exact through the
//! whole ledger layer, f64 only appears later inside the projection engines.
//! Decimal preserves scale, so `$5.00` keeps its two fractional digits and
//! formats back with them.

use crate::error::ParseError;
use crate::span::{Span, clamp_u32};
use rust_decimal::Decimal;
use std::fmt;
use std::io;

/// Which side of the number the commodity is written on
///
/// Stored on the parsed amount so formatting can put the commodity back on the
/// side it came from, ensuring roundtripping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Commodity before the number, `$5`
    Prefix,
    /// Commodity after the number, `5 VTI`
    Suffix,
}

/// Which character marks the decimal point, the other of `.` and `,` being the
/// thousands separator
///
/// [`Period`](DecimalStyle::Period) is the American style, a period decimal and
/// comma thousands (`1,000.50`). [`Comma`](DecimalStyle::Comma) is the European
/// style with the two flipped (`1.000,50`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalStyle {
    /// Period decimal, comma thousands: `1,000.50`
    Period,
    /// Comma decimal, period thousands: `1.000,50`
    Comma,
}

impl DecimalStyle {
    /// The `(thousands separator, decimal mark)` bytes for this style
    fn marks(self) -> (u8, u8) {
        match self {
            Self::Period => (b',', b'.'),
            Self::Comma => (b'.', b','),
        }
    }
}

/// A commodity symbol together with the side it sits on
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commodity {
    // The symbol with any surrounding quotes stripped. Quoting is recomputed
    // from the content when formatting, so an input that quotes a symbol
    // needing no quotes still round-trips to an equal value
    symbol: Vec<u8>,
    placement: Placement,
}

impl Commodity {
    /// A commodity with the given symbol and placement
    ///
    /// # Errors
    /// The symbol must be nonempty and hold no `"` and no ASCII control byte:
    /// an empty symbol formats to nothing, a quote has no escape in the
    /// grammar, and a control byte would corrupt the single-line journal, so
    /// any of them would keep the commodity from formatting back to itself
    pub fn new(symbol: impl Into<Vec<u8>>, placement: Placement) -> Result<Self, SymbolError> {
        let symbol = symbol.into();
        if symbol_round_trips(&symbol) {
            Ok(Self { symbol, placement })
        } else {
            Err(SymbolError { symbol })
        }
    }

    /// A commodity scanned out of an amount. The scanner already refused the
    /// symbols [`new`](Commodity::new) rejects, so this cannot fail
    fn scanned(symbol: Vec<u8>, placement: Placement) -> Self {
        debug_assert!(
            symbol_round_trips(&symbol),
            "scanner let through a symbol that cannot round-trip: {symbol:?}"
        );
        Self { symbol, placement }
    }

    /// The symbol bytes, without surrounding quotes
    pub fn symbol(&self) -> &[u8] {
        &self.symbol
    }

    /// The symbol as text, `None` when it is not valid UTF-8
    ///
    /// For a caller that needs a `&str` and has somewhere to put the refusal.
    pub fn symbol_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.symbol).ok()
    }

    /// Which side of the number the commodity sits on
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Write the symbol back byte for byte, quoted when a bare symbol would not
    /// scan again as one token
    ///
    /// # Errors
    /// Whatever `out` returns
    pub fn write_to(&self, out: &mut impl io::Write) -> io::Result<()> {
        if needs_quotes(&self.symbol) {
            out.write_all(b"\"")?;
            out.write_all(&self.symbol)?;
            out.write_all(b"\"")
        } else {
            out.write_all(&self.symbol)
        }
    }
}

impl fmt::Display for Commodity {
    /// Lossy on a symbol that is not UTF-8: every invalid sequence becomes
    /// U+FFFD. Use [`write_to`](Commodity::write_to) to write a journal
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Quote only when the bare symbol would not scan back as one token: one
        // holding whitespace, a digit, or a character that ends an unquoted
        // symbol
        let symbol = String::from_utf8_lossy(&self.symbol);
        if needs_quotes(&self.symbol) {
            write!(f, "\"{symbol}\"")
        } else {
            f.write_str(&symbol)
        }
    }
}

/// The error from [`Commodity::new`]: a symbol that could not format back and
/// scan again as the same token
///
/// Carries the rejected symbol so the message names it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolError {
    symbol: Vec<u8>,
}

impl SymbolError {
    /// The rejected symbol, as it was given
    pub fn symbol(&self) -> &[u8] {
        &self.symbol
    }
}

impl fmt::Display for SymbolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The symbol is rendered lossily: this is a message, and a symbol that
        // is not text is exactly one of the shapes that lands here
        write!(
            f,
            "commodity symbol must be nonempty with no quote or control byte: {:?}",
            String::from_utf8_lossy(&self.symbol)
        )
    }
}

impl std::error::Error for SymbolError {}

/// A quantity paired with its commodity
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
    /// The signed quantity, exact and scale-preserving
    pub quantity: Decimal,
    /// The commodity the quantity is denominated in
    pub commodity: Commodity,
    /// The decimal style the amount was read in, used to format it back
    pub style: DecimalStyle,
}

impl Amount {
    /// Scan one amount from `src` in the American period style (period decimal,
    /// comma thousands), which must hold exactly the amount and nothing else
    ///
    /// This is [`parse_styled`](Amount::parse_styled) with [`DecimalStyle::Period`].
    /// Use `parse_styled` with [`DecimalStyle::Comma`] for European `1.000,50`
    ///
    /// # Errors
    /// See [`parse_styled`](Amount::parse_styled)
    pub fn parse(src: &[u8]) -> Result<Self, ParseError> {
        Self::parse_styled(src, DecimalStyle::Period)
    }

    /// Scan one amount from `src` in `style`, which must hold exactly the amount
    /// and nothing else
    ///
    /// `style` fixes which of `.` and `,` is the decimal mark and which is the
    /// thousands separator. The same digits parse to different amounts under
    /// each style, so the caller chooses it, and it is kept on the result so the
    /// amount formats back in it.
    ///
    /// The input is the whole amount, not a prefix of a larger line: leading and
    /// trailing whitespace and any characters past the amount are errors, not
    /// trimmed or ignored. A caller scanning an amount out of a line trims the
    /// slice to the amount first
    ///
    /// # Errors
    /// The span is a byte range into `src`. Errors: leading or trailing
    /// whitespace, an unterminated or empty quoted commodity, a control byte
    /// inside a quoted commodity, a missing number or commodity, more than one
    /// sign, malformed thousands grouping, a quantity too large or too precise
    /// for [`Decimal`], or characters left over after the amount
    pub fn parse_styled(src: &[u8], style: DecimalStyle) -> Result<Self, ParseError> {
        let mut s = Scanner::new(src);
        let (_, decimal) = style.marks();

        // The input is exactly the amount, so leading whitespace is malformed.
        // Catch it here for a clear message instead of failing later as a
        // missing commodity or number
        if matches!(s.peek(), Some(b' ' | b'\t')) {
            let start = s.pos;
            s.skip_spaces();
            return Err(ParseError::new(
                "leading whitespace before the amount",
                s.span_from(start),
            ));
        }

        // A leading sign binds to the number no matter which side the commodity
        // is on, and only one sign is allowed in the whole amount
        let mut negative = false;
        let mut sign_seen = false;
        if let Some(neg) = s.take_sign() {
            negative = neg;
            sign_seen = true;
        }

        let (quantity, commodity) = match s.peek() {
            // A digit or the decimal mark next means the number leads and the
            // commodity, if any, follows it: `5 VTI`
            Some(b) if is_number_start(b, decimal) => {
                let quantity = s.take_number(negative, style)?;
                s.skip_spaces();
                let symbol = s.take_commodity()?;
                (quantity, Commodity::scanned(symbol, Placement::Suffix))
            }
            // Anything else that is not end-of-input starts a prefix commodity:
            // `$5`, and a sign may sit between commodity and number: `$-5`
            Some(_) => {
                let symbol = s.take_commodity()?;
                s.skip_spaces();
                // A sign may sit between the commodity and the number, `$-5`, but
                // only if the amount did not already lead with one
                let sign_pos = s.pos;
                if let Some(neg) = s.take_sign() {
                    if sign_seen {
                        return Err(ParseError::new(
                            "amount has more than one sign",
                            s.span_from(sign_pos),
                        ));
                    }
                    negative = neg;
                }
                let quantity = s.take_number(negative, style)?;
                (quantity, Commodity::scanned(symbol, Placement::Prefix))
            }
            None => return Err(ParseError::new("expected an amount", s.rest_from(0))),
        };

        if !s.at_end() {
            // Trailing whitespace is a clearer, distinct failure from arbitrary
            // leftover characters, since the input is meant to be only the amount
            let message = if s.rest_is_whitespace() {
                "trailing whitespace after the amount"
            } else {
                "unexpected characters after the amount"
            };
            return Err(ParseError::new(message, s.rest_from(s.pos)));
        }
        Ok(Self {
            quantity,
            commodity,
            style,
        })
    }
}

impl Amount {
    /// The quantity in its own style: digits and at most one mark, the decimal
    /// mark written as this amount's style writes it
    ///
    /// The canonical form drops thousands grouping, so the comma style differs
    /// from the period style only in swapping the one decimal dot for a comma
    fn quantity_text(&self) -> String {
        match self.style {
            DecimalStyle::Period => self.quantity.to_string(),
            DecimalStyle::Comma => self.quantity.to_string().replace('.', ","),
        }
    }

    /// Write the amount back byte for byte, the commodity on the side it was
    /// read from
    ///
    /// The byte-exact counterpart to [`Display`](fmt::Display), which is lossy
    /// on a commodity symbol that is not UTF-8. A journal is written through
    /// this
    ///
    /// # Errors
    /// Whatever `out` returns
    pub fn write_to(&self, out: &mut impl io::Write) -> io::Result<()> {
        // The sign lives in the number, so a negative prefix amount writes as
        // `$-5.00`, which scans back to the same value
        let number = self.quantity_text();
        match self.commodity.placement {
            Placement::Prefix => {
                self.commodity.write_to(out)?;
                out.write_all(number.as_bytes())
            }
            Placement::Suffix => {
                out.write_all(number.as_bytes())?;
                out.write_all(b" ")?;
                self.commodity.write_to(out)
            }
        }
    }
}

impl fmt::Display for Amount {
    /// Lossy on a commodity symbol that is not UTF-8. Use
    /// [`write_to`](Amount::write_to) to write a journal
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let number = self.quantity_text();
        match self.commodity.placement {
            Placement::Prefix => write!(f, "{}{number}", self.commodity),
            Placement::Suffix => write!(f, "{number} {}", self.commodity),
        }
    }
}

/// A byte cursor over the amount source
struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Whether every byte from the cursor to the end is a space or tab
    fn rest_is_whitespace(&self) -> bool {
        self.bytes
            .get(self.pos..)
            .unwrap_or_default()
            .iter()
            .all(|&b| matches!(b, b' ' | b'\t'))
    }

    fn skip_spaces(&mut self) {
        // Amounts live on one line, so only spaces and tabs separate the parts
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.bump();
        }
    }

    /// Take an optional leading sign, `true` for negative
    fn take_sign(&mut self) -> Option<bool> {
        match self.peek() {
            Some(b'-') => {
                self.bump();
                Some(true)
            }
            Some(b'+') => {
                self.bump();
                Some(false)
            }
            _ => None,
        }
    }

    /// Scan the integer part with its thousands separators, an optional
    /// fraction, and the sign into a [`Decimal`], reading `style` for which byte
    /// is the decimal mark and which is the thousands separator
    ///
    /// The thousands separator groups the integer part only: the group before
    /// the first one is one to three digits and not all zeros, and every group
    /// after it is exactly three digits. The decimal mark is the sole decimal,
    /// so a separator that does not fit the grouping is malformed, not the other
    /// mark: under the period style `1,00`, `0,075`, and `5,` are errors, and
    /// under the comma style `1.00`, `0.075`, and `5.` are errors as well
    fn take_number(&mut self, negative: bool, style: DecimalStyle) -> Result<Decimal, ParseError> {
        let (thousands, decimal) = style.marks();
        let start = self.pos;
        // Build the clean string decimal wants: sign, digits, dot, fraction. The
        // thousands separators carry no value and are dropped once their
        // placement checks out, and the decimal mark is always written as a dot
        let mut text = String::new();
        if negative {
            text.push('-');
        }
        let mut has_digit = false;
        // Validate the grouping while scanning the integer part. group_len counts
        // digits in the open group, group_closed marks that a group has closed,
        // and first_group_all_zeros guards the `0,075` shape
        let mut group_len = 0u32;
        let mut group_closed = false;
        let mut first_group_all_zeros = true;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                text.push(char::from(b));
                has_digit = true;
                group_len = group_len.saturating_add(1);
                if !group_closed && b != b'0' {
                    first_group_all_zeros = false;
                }
                self.bump();
            } else if b == thousands {
                if group_closed {
                    // A group after the first must be exactly three digits
                    if group_len != 3 {
                        return Err(ParseError::new(
                            "thousands groups must be three digits",
                            self.span_from(start),
                        ));
                    }
                } else if group_len == 0 || group_len > 3 || first_group_all_zeros {
                    // The first group is one to three digits and not all zeros
                    return Err(ParseError::new(
                        "misplaced thousands separator",
                        self.span_from(start),
                    ));
                }
                group_closed = true;
                group_len = 0;
                self.bump();
            } else {
                break;
            }
        }
        // With any separator, the last integer group must also be three digits,
        // which rejects a trailing separator and a short final group like `1,00`
        if group_closed && group_len != 3 {
            return Err(ParseError::new(
                "thousands groups must be three digits",
                self.span_from(start),
            ));
        }
        if self.peek() == Some(decimal) {
            self.bump();
            text.push('.');
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    text.push(char::from(b));
                    has_digit = true;
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if !has_digit {
            return Err(ParseError::new("expected a number", self.span_from(start)));
        }
        // The text holds only a sign, digits, and one dot, so the only failures
        // are a magnitude too large for a decimal or a fraction with more digits
        // than a decimal can hold
        Decimal::from_str_exact(&text).map_err(|err| {
            let message = match err {
                rust_decimal::Error::Underflow => {
                    "number has more fractional digits than a decimal can represent"
                }
                _ => "number is too large to represent",
            };
            ParseError::new(message, self.span_from(start))
        })
    }

    /// Scan a commodity symbol, quoted or bare, returning it without quotes
    fn take_commodity(&mut self) -> Result<Vec<u8>, ParseError> {
        match self.peek() {
            Some(b'"') => {
                let open = self.pos;
                self.bump();
                let content = self.pos;
                while let Some(b) = self.peek() {
                    if b == b'"' {
                        let symbol = self.slice(content, self.pos);
                        self.bump();
                        // Empty quotes would allow in an amount with no
                        // commodity, which the grammar refuses when written bare
                        if symbol.is_empty() {
                            return Err(ParseError::new(
                                "empty commodity",
                                Span::new(clamp_u32(open), clamp_u32(self.pos)),
                            ));
                        }
                        return Ok(symbol);
                    }
                    // A quote has no escape and amounts live on one line, so a
                    // control byte inside the quotes could not format back as one
                    // token. Reject it at the offending byte
                    if b.is_ascii_control() {
                        return Err(ParseError::new(
                            "control character in commodity",
                            Span::new(clamp_u32(self.pos), clamp_u32(self.pos.saturating_add(1))),
                        ));
                    }
                    self.bump();
                }
                Err(ParseError::new(
                    "unterminated commodity quote",
                    self.rest_from(open),
                ))
            }
            Some(b) if is_commodity_byte(b) => {
                let start = self.pos;
                while matches!(self.peek(), Some(b) if is_commodity_byte(b)) {
                    self.bump();
                }
                Ok(self.slice(start, self.pos))
            }
            _ => Err(ParseError::new(
                "expected a commodity",
                self.rest_from(self.pos),
            )),
        }
    }

    fn slice(&self, start: usize, end: usize) -> Vec<u8> {
        // Start and end come from the cursor, so they are in bounds. The release
        // fallback still degrades to empty rather than an unwrap
        self.bytes.get(start..end).unwrap_or_default().to_vec()
    }

    /// A span from `start` to the cursor
    fn span_from(&self, start: usize) -> Span {
        Span::new(clamp_u32(start), clamp_u32(self.pos))
    }

    /// A span from `start` to end-of-input, for an error that runs off the end
    fn rest_from(&self, start: usize) -> Span {
        Span::new(clamp_u32(start), clamp_u32(self.bytes.len()))
    }
}

/// Whether a byte can appear in a bare, unquoted commodity symbol
///
/// The set of bytes that end a bare symbol is ledger's own `invalid_chars` table
/// (`src/commodity.cc`): whitespace, a digit, and the punctuation the amount and
/// expression grammars claim, which is
/// `! & ( ) * + , - . / : ; < = > ? @ [ ] ^ { | } ~`. A symbol holding any of
/// these must be quoted, and unquoted it ends at the first one, both matching
/// ledger so a commodity reads and writes the same way it does there.
///
/// Two bytes are stricter here than in ledger's table, which marks them valid:
/// an ASCII control byte and a `"`. firepath refuses a symbol holding either at
/// construction, since neither can be written to a single-line journal and read
/// back, so treating them as symbol-ending keeps the parser from building one
/// the constructor would only reject. A non-ascii byte is always part of the
/// symbol
fn is_commodity_byte(b: u8) -> bool {
    !(b.is_ascii_whitespace()
        || b.is_ascii_control()
        || b.is_ascii_digit()
        || matches!(
            b,
            b'!' | b'"'
                | b'&'
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b';'
                | b'<'
                | b'='
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b']'
                | b'^'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
        ))
}

/// Whether a symbol must be quoted to scan back as one token
///
/// The symbol is nonempty, the [`Commodity`] constructor's contract
fn needs_quotes(symbol: &[u8]) -> bool {
    symbol.iter().any(|&b| !is_commodity_byte(b))
}

/// Whether a symbol can format back and scan again as the same one token
///
/// Quoting recovers spaces and grammar characters, but an empty symbol formats
/// to nothing, a `"` has no escape, and an ASCII control byte would corrupt
/// the single-line journal, so a symbol that is empty or holds either cannot
/// round-trip
fn symbol_round_trips(symbol: &[u8]) -> bool {
    !symbol.is_empty() && !symbol.iter().any(|&b| b == b'"' || b.is_ascii_control())
}

/// Whether a byte starts a number: a digit, or the decimal mark for a leading
/// fraction like `.5` under the period style or `,5` under the comma style
fn is_number_start(b: u8, decimal: u8) -> bool {
    b.is_ascii_digit() || b == decimal
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::{Amount, Commodity, DecimalStyle, Placement};
    use crate::span::Span;

    // The grouping error messages, shared so the period and comma style tests
    // pin one wording
    const MISPLACED: &str = "misplaced thousands separator";
    const THREE: &str = "thousands groups must be three digits";

    // Parse a period-style shape, check its parts, then prove the value survives
    // a format then re-parse
    fn check(input: &str, quantity: &str, symbol: &str, placement: Placement) {
        check_styled(input, quantity, symbol, placement, DecimalStyle::Period);
    }

    // Parse a shape in the given style, check the quantity, symbol, placement,
    // and style, then prove it survives a format then re-parse in that style
    fn check_styled(
        input: &str,
        quantity: &str,
        symbol: &str,
        placement: Placement,
        style: DecimalStyle,
    ) {
        let amount = Amount::parse_styled(input.as_bytes(), style).unwrap();
        assert_eq!(
            amount.quantity.to_string(),
            quantity,
            "quantity of {input:?}"
        );
        assert_eq!(
            amount.commodity.symbol(),
            symbol.as_bytes(),
            "symbol of {input:?}"
        );
        assert_eq!(
            amount.commodity.placement(),
            placement,
            "placement of {input:?}"
        );
        assert_eq!(amount.style, style, "style of {input:?}");

        // The round-trip goes through write_to, the byte-exact output path a
        // journal is written with, not through the lossy Display
        let mut written = Vec::new();
        amount.write_to(&mut written).unwrap();
        let reparsed = Amount::parse_styled(&written, style).unwrap();
        assert_eq!(
            reparsed,
            amount,
            "round-trip of {input:?} via {:?}",
            String::from_utf8_lossy(&written)
        );
    }

    // Write an amount through the byte-exact path and hand back the bytes
    fn written(amount: &Amount) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let capacity = buf.len();
        let free = {
            let mut out: &mut [u8] = &mut buf;
            amount.write_to(&mut out).unwrap();
            out.len()
        };
        buf.get(..capacity.saturating_sub(free))
            .unwrap_or_default()
            .to_vec()
    }

    #[test]
    fn a_commodity_symbol_that_is_not_utf8_survives_a_round_trip() {
        // `caf\xe9` is Latin-1, which the ledger binary accepts and never
        // decodes. The symbol must come back byte for byte, not as U+FFFD
        let src: &[u8] = b"3 caf\xe9";
        let amount = Amount::parse(src).unwrap();
        assert_eq!(amount.commodity.symbol(), b"caf\xe9");
        assert_eq!(amount.commodity.symbol_str(), None);
        assert_eq!(written(&amount), src);
        assert_eq!(Amount::parse(&written(&amount)).unwrap(), amount);
    }

    #[test]
    fn display_is_lossy_where_write_to_is_exact() {
        // Pins the split the two output paths make. Display is for messages and
        // replaces the invalid byte, write_to is what a journal is written with
        let amount = Amount::parse(b"3 caf\xe9").unwrap();
        assert_eq!(amount.to_string(), "3 caf\u{fffd}");
        assert_eq!(written(&amount), b"3 caf\xe9");
    }

    #[test]
    fn a_quoted_symbol_that_is_not_utf8_keeps_its_quotes() {
        // A high byte does not need quoting on its own, but a space does, and
        // the quoting decision is made on bytes
        let amount = Amount::parse(b"3 \"caf\xe9 au lait\"").unwrap();
        assert_eq!(amount.commodity.symbol(), b"caf\xe9 au lait");
        assert_eq!(written(&amount), b"3 \"caf\xe9 au lait\"");
        assert_eq!(Amount::parse(&written(&amount)).unwrap(), amount);
    }

    #[test]
    fn display_renders_each_placement_and_quotes_the_same_symbols_write_to_does() {
        // Display and write_to are separate implementations of one format, so
        // they can drift. This pins Display's own output and asserts the two
        // agree byte for byte wherever the symbol is text
        for src in [
            &b"$5.00"[..],        // prefix, bare
            b"5.00 VTI",          // suffix, bare
            b"3 \"MUTF: VFIAX\"", // suffix, quoted for the space
            b"-$5.00",            // the sign rides the number
        ] {
            let amount = Amount::parse(src).unwrap();
            assert_eq!(
                amount.to_string().as_bytes(),
                written(&amount),
                "Display and write_to disagree on {:?}",
                String::from_utf8_lossy(src)
            );
        }

        // The rendered text itself, so a change to either path has to be
        // deliberate rather than silently agreed on by both
        assert_eq!(Amount::parse(b"$5.00").unwrap().to_string(), "$5.00");
        assert_eq!(Amount::parse(b"5.00 VTI").unwrap().to_string(), "5.00 VTI");
        assert_eq!(
            Amount::parse(b"3 \"MUTF: VFIAX\"").unwrap().to_string(),
            "3 \"MUTF: VFIAX\""
        );
    }

    #[test]
    fn a_writer_that_runs_out_of_room_surfaces_the_error() {
        // A `&mut [u8]` is a writer with a fixed capacity: once full, write_all
        // returns WriteZero, so sizing the buffer picks which write fails
        for (src, capacity, failing_write) in [
            (&b"\"a b\"5"[..], 0, "the opening quote of a quoted symbol"),
            (b"\"a b\"5", 1, "the symbol inside its quotes"),
            (b"$5", 0, "a bare prefix symbol"),
            (b"5 VTI", 0, "the number of a suffix amount"),
            (b"5 VTI", 1, "the space between number and suffix symbol"),
        ] {
            let amount = Amount::parse(src).unwrap();
            let mut buf = vec![0u8; capacity];
            let mut out: &mut [u8] = &mut buf;
            let err = amount.write_to(&mut out).unwrap_err();
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::WriteZero,
                "writing {failing_write} of {:?} should fail",
                String::from_utf8_lossy(src)
            );
        }
    }

    #[test]
    fn a_symbol_with_a_grammar_character_reads_and_writes_the_way_ledger_does() {
        // ledger's `invalid_chars` table ends a bare symbol at any of these, so
        // a symbol holding one has to be quoted and comes back quoted. `M&M`
        // from ledger's own `regress/A28CF697.test` is the case that drove this
        for symbol in ["M&M", "a/b", "x:y", "p?q", "u^v", "m|n", "t~s", "z!w"] {
            let quoted = format!("1 \"{symbol}\"");
            let amount = Amount::parse(quoted.as_bytes()).unwrap();
            assert_eq!(amount.commodity.symbol(), symbol.as_bytes());
            assert_eq!(
                written(&amount),
                quoted.as_bytes(),
                "round-trip of {symbol:?}"
            );
        }

        // Unquoted, the symbol ends at the grammar character, so the rest is
        // leftover the amount scanner refuses, the same boundary ledger draws
        let err = Amount::parse(b"1 M&M").unwrap_err();
        assert_eq!(err.message, "unexpected characters after the amount");
    }

    #[test]
    fn a_rejected_symbol_is_handed_back_as_the_bytes_that_were_given() {
        // The constructor refuses a symbol that cannot format back and scan
        // again. The error carries the offending bytes verbatim, so a caller
        // can report the symbol it actually passed rather than a lossy
        // rendering of it
        let err = Commodity::new(b"ca\xe9\"fe".to_vec(), Placement::Suffix).unwrap_err();
        assert_eq!(err.symbol(), b"ca\xe9\"fe");
        // The message is text, so it renders the invalid byte lossily
        assert!(err.to_string().contains('\u{fffd}'), "{err}");

        // An empty symbol is refused for a different reason and reports as the
        // empty bytes it was given, not as a missing symbol
        let err = Commodity::new(Vec::new(), Placement::Prefix).unwrap_err();
        assert_eq!(err.symbol(), b"");
    }

    #[test]
    fn prefix_commodity_shapes() {
        check("$5", "5", "$", Placement::Prefix);
        check("$5.00", "5.00", "$", Placement::Prefix);
        check("$1,000.50", "1000.50", "$", Placement::Prefix);
        // A sign on either side of a prefix commodity means the same amount
        check("-$5", "-5", "$", Placement::Prefix);
        check("$-5", "-5", "$", Placement::Prefix);
        check("+$5", "5", "$", Placement::Prefix);
        // Spaces around the prefix and its sign are tolerated
        check("$ 5", "5", "$", Placement::Prefix);
        check("$ -5", "-5", "$", Placement::Prefix);
    }

    #[test]
    fn suffix_commodity_shapes() {
        check("5 VTI", "5", "VTI", Placement::Suffix);
        check("-5 VTI", "-5", "VTI", Placement::Suffix);
        check("1,234.56 EUR", "1234.56", "EUR", Placement::Suffix);
        // The separating space is optional, the digit boundary is unambiguous
        check("5VTI", "5", "VTI", Placement::Suffix);
        // A bare fraction keeps its leading zero when formatted back
        check(".5 VTI", "0.5", "VTI", Placement::Suffix);
    }

    #[test]
    fn a_trailing_decimal_mark_is_tolerated() {
        // A decimal mark with no fraction still reads as the integer, and the
        // canonical form drops it. Contrast with the trailing thousands
        // separator in the malformed grouping tests, which is an error
        check("$5.", "5", "$", Placement::Prefix);
        check_styled("5, EUR", "5", "EUR", Placement::Suffix, DecimalStyle::Comma);
    }

    #[test]
    fn well_formed_thousands_groups_parse() {
        // The first group is one to three digits, every later group is three,
        // and the commas drop out of the canonical form
        check("$1,000", "1000", "$", Placement::Prefix);
        check("$12,345", "12345", "$", Placement::Prefix);
        check("$123,456", "123456", "$", Placement::Prefix);
        check("$1,000,000", "1000000", "$", Placement::Prefix);
        check("$1,234,567.89", "1234567.89", "$", Placement::Prefix);
    }

    #[test]
    fn malformed_thousands_grouping_is_an_error() {
        // A comma is a thousands separator only, never a decimal, so a comma that
        // is not well-formed grouping is rejected rather than reinterpreted the
        // way ledger-cli would read `1,00` as `1.00` or `0,075` as `0.075`. A
        // separator in an impossible position is misplaced; groups of the wrong
        // width get the three-digits error
        for (input, message) in [
            ("$1,00", THREE),         // short final group
            ("$1,2,3", THREE),        // short middle group
            ("$1,00,00", THREE),      // short groups
            ("$9,99,99,999", THREE),  // lakh grouping, two-digit second group
            ("$0,075", MISPLACED),    // zero integer group
            ("$5,", THREE),           // trailing comma leaves an empty group
            ("$,5", MISPLACED),       // leading comma
            ("$1234,000", MISPLACED), // first group longer than three digits
        ] {
            let err = Amount::parse(input.as_bytes()).unwrap_err();
            assert_eq!(err.message, message, "error for {input:?}");
        }
    }

    #[test]
    fn european_style_flips_the_marks() {
        // Comma is the decimal, period is the thousands separator, same grouping
        check_styled(
            "€1.000",
            "1000",
            "€",
            Placement::Prefix,
            DecimalStyle::Comma,
        );
        check_styled(
            "1.234,56 EUR",
            "1234.56",
            "EUR",
            Placement::Suffix,
            DecimalStyle::Comma,
        );
        check_styled(
            "1.000.000,00 EUR",
            "1000000.00",
            "EUR",
            Placement::Suffix,
            DecimalStyle::Comma,
        );
        // A comma decimal, including a leading one, mirrors the period style
        check_styled("€1,50", "1.50", "€", Placement::Prefix, DecimalStyle::Comma);
        check_styled(
            ",5 EUR",
            "0.5",
            "EUR",
            Placement::Suffix,
            DecimalStyle::Comma,
        );
    }

    #[test]
    fn european_amount_formats_back_with_a_comma() {
        // The style is kept so the amount prints in the style it was read in,
        // and grouping still drops out of the canonical form
        let amount = Amount::parse_styled("1.234,56 EUR".as_bytes(), DecimalStyle::Comma).unwrap();
        assert_eq!(amount.to_string(), "1234,56 EUR");
    }

    #[test]
    fn the_same_digits_differ_by_style() {
        // `1,000` is a thousands group under the period style and a comma decimal
        // under the comma style, so the caller's choice decides the value
        let anglo = Amount::parse_styled("1,000 X".as_bytes(), DecimalStyle::Period).unwrap();
        let euro = Amount::parse_styled("1,000 X".as_bytes(), DecimalStyle::Comma).unwrap();
        assert_eq!(anglo.quantity.to_string(), "1000");
        assert_eq!(euro.quantity.to_string(), "1.000");
    }

    #[test]
    fn malformed_european_grouping_is_an_error() {
        // The period is a thousands separator only under the comma style, so the
        // mirror of the period-style malformed cases is rejected
        for (input, message) in [
            ("€1.00", THREE),      // short final group
            ("€1.2.3", THREE),     // short middle group
            ("€0.075", MISPLACED), // zero integer group
            ("€5.", THREE),        // trailing separator
            ("€.5", MISPLACED),    // leading separator
        ] {
            let err = Amount::parse_styled(input.as_bytes(), DecimalStyle::Comma).unwrap_err();
            assert_eq!(err.message, message, "error for {input:?}");
        }
    }

    #[test]
    fn quoted_commodity_carries_spaces() {
        check(
            "100.00 \"MUTF: VFIAX\"",
            "100.00",
            "MUTF: VFIAX",
            Placement::Suffix,
        );
    }

    #[test]
    fn quotes_are_dropped_when_not_needed() {
        // An input may quote a symbol that needs none, the value is unchanged
        // and the canonical form drops the quotes
        let amount = Amount::parse("5 \"VTI\"".as_bytes()).unwrap();
        assert_eq!(amount.commodity.symbol(), b"VTI");
        assert_eq!(amount.to_string(), "5 VTI");
    }

    #[test]
    fn unterminated_quote_errors_at_the_quote() {
        // The quote opens at byte 2 (`5`, space, then `"`) and the error spans
        // from there to the end of input
        let err = Amount::parse("5 \"MUTF: VFIAX".as_bytes()).unwrap_err();
        assert_eq!(err.message, "unterminated commodity quote");
        assert_eq!(err.span, Span::new(2, 14));
    }

    #[test]
    fn an_empty_input_is_an_error() {
        let err = Amount::parse("".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected an amount");
        assert_eq!(err.span, Span::new(0, 0));
        // A lone sign runs out of input the same way, spanning the sign
        let err = Amount::parse("-".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected an amount");
        assert_eq!(err.span, Span::new(0, 1));
    }

    #[test]
    fn a_missing_commodity_is_an_error() {
        let err = Amount::parse("5".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected a commodity");
    }

    #[test]
    fn a_grammar_byte_where_the_commodity_belongs_is_an_error() {
        // `@` ends a bare symbol, so it cannot start one, in either position
        let err = Amount::parse("5 @".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected a commodity");
        let err = Amount::parse("@5".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected a commodity");
    }

    #[test]
    fn a_control_byte_ends_a_bare_commodity() {
        // The symbol ends at the control byte, which is then left over
        let err = Amount::parse("5 VTI\u{1}".as_bytes()).unwrap_err();
        assert_eq!(err.message, "unexpected characters after the amount");
        assert_eq!(err.span, Span::new(5, 6));
    }

    #[test]
    fn an_empty_quoted_commodity_is_an_error() {
        // Empty quotes would allow in an amount with no commodity, which the
        // grammar refuses when written bare, and the span covers the quotes
        let err = Amount::parse("5 \"\"".as_bytes()).unwrap_err();
        assert_eq!(err.message, "empty commodity");
        assert_eq!(err.span, Span::new(2, 4));
    }

    #[test]
    fn a_symbol_that_cannot_round_trip_is_refused() {
        // An empty symbol formats to nothing, a quote has no escape, and a
        // control byte would corrupt the single-line journal, so the
        // constructor refuses all three and its error names the symbol
        for symbol in ["", "A\"B", "A\u{1}B"] {
            let err = Commodity::new(symbol, Placement::Suffix).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "commodity symbol must be nonempty with no quote or control byte: {symbol:?}"
                ),
                "refusal expected for {symbol:?}"
            );
        }
    }

    #[test]
    fn a_round_trippable_symbol_constructs() {
        let usd = Commodity::new("USD", Placement::Prefix).unwrap();
        assert_eq!(usd.symbol(), b"USD");
        assert_eq!(usd.placement(), Placement::Prefix);
    }

    #[test]
    fn a_missing_number_is_an_error() {
        let err = Amount::parse("$".as_bytes()).unwrap_err();
        assert_eq!(err.message, "expected a number");
    }

    #[test]
    fn a_number_too_large_is_an_error() {
        // 29 nines exceed Decimal's 96-bit maximum magnitude
        let err = Amount::parse("$99999999999999999999999999999".as_bytes()).unwrap_err();
        assert_eq!(err.message, "number is too large to represent");
    }

    #[test]
    fn a_number_too_precise_is_an_error() {
        // 29 fractional digits is one past what a decimal holds, and rounding it
        // would silently drop precision, so scanning rejects it instead
        let err = Amount::parse("0.12345678901234567890123456789 VTI".as_bytes()).unwrap_err();
        assert_eq!(
            err.message,
            "number has more fractional digits than a decimal can represent"
        );
    }

    #[test]
    fn a_control_byte_in_a_quoted_commodity_is_an_error() {
        // A tab inside the quotes has no escape and would break the single-line
        // journal, and the span points at the offending byte
        let err = Amount::parse("5 \"A\tB\"".as_bytes()).unwrap_err();
        assert_eq!(err.message, "control character in commodity");
        assert_eq!(err.span, Span::new(4, 5));
    }

    #[test]
    fn trailing_characters_are_an_error() {
        // The span covers everything left over, the space included
        let err = Amount::parse("$5 x".as_bytes()).unwrap_err();
        assert_eq!(err.message, "unexpected characters after the amount");
        assert_eq!(err.span, Span::new(2, 4));
    }

    #[test]
    fn leading_whitespace_is_a_clear_error() {
        // The input is exactly the amount, so a leading space is reported as
        // such rather than as a missing commodity, spanning the whitespace run
        let err = Amount::parse("  $5".as_bytes()).unwrap_err();
        assert_eq!(err.message, "leading whitespace before the amount");
        assert_eq!(err.span, Span::new(0, 2));
    }

    #[test]
    fn trailing_whitespace_is_a_clear_error() {
        let err = Amount::parse("$5 ".as_bytes()).unwrap_err();
        assert_eq!(err.message, "trailing whitespace after the amount");
        assert_eq!(err.span, Span::new(2, 3));
    }

    #[test]
    fn a_second_sign_is_a_clear_error() {
        // The leading sign already bound to the number, so the sign between the
        // commodity and the number is the second one, reported at that byte
        let err = Amount::parse("-$-5".as_bytes()).unwrap_err();
        assert_eq!(err.message, "amount has more than one sign");
        assert_eq!(err.span, Span::new(2, 3));
    }
}
