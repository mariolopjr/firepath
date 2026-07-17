//! Date scanning: a calendar date in one of three separator forms
//!
//! A date is written as `YYYY-MM-DD`, `YYYY/MM/DD`, or `YYYY.MM.DD`: a
//! four-digit year, a two-digit month, and a two-digit day joined by a
//! separator that is the same character in both positions. The fields are
//! zero-padded, so the token is always exactly ten bytes. The loose `2020-1-2`
//! shape is refused, matching what the `ledger` binary accepts.
//!
//! The parsed date is a [`jiff::civil::Date`], which range-checks the month and
//! the day against the calendar including leap years. The year must fall in
//! 1400 through 9999, the same range of the boost calendar behind the `ledger`
//! binary, so both tools refuse the same dates

use crate::error::ParseError;
use crate::span::{Span, clamp_u32};
use jiff::civil::Date as Civil;
use std::fmt;
use std::ops::Range;
use std::str::FromStr;

/// 1970-01-01, the anchor the epoch-day count is measured from
const EPOCH: Civil = Civil::constant(1970, 1, 1);

/// The exact byte length of a well-formed date, `YYYY?MM?DD`
const DATE_LEN: usize = 10;

/// The earliest accepted year: the floor of the boost calendar the `ledger`
/// binary uses. The four-digit form caps the year at 9999, the same ceiling
/// boost has, so only the floor needs its own check
const MIN_YEAR: i16 = 1400;

/// Shown when the input is not one of the three accepted date forms
const BAD_FORM: &str = "expected a date in YYYY-MM-DD, YYYY/MM/DD, or YYYY.MM.DD form";

/// The separator between a date's fields, one per accepted form
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// `-`, the `YYYY-MM-DD` form
    Dash,
    /// `/`, the `YYYY/MM/DD` form
    Slash,
    /// `.`, the `YYYY.MM.DD` form
    Dot,
}

impl Separator {
    /// The separator for a byte, or `None` if the byte is not one of the three
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'-' => Some(Self::Dash),
            b'/' => Some(Self::Slash),
            b'.' => Some(Self::Dot),
            _ => None,
        }
    }

    /// The separator byte, for formatting the date back
    fn byte(self) -> u8 {
        match self {
            Self::Dash => b'-',
            Self::Slash => b'/',
            Self::Dot => b'.',
        }
    }
}

/// A calendar date: the civil date and the separator it was written with
///
/// The separator is kept so the date formats back to the form it was read in.
/// Equality includes it, so two dates naming the same day with different
/// separators are not equal. Compare [`civil`](Date::civil) or
/// [`epoch_day`](Date::epoch_day) to order dates by the day they fall on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    civil: Civil,
    separator: Separator,
}

impl Date {
    /// Scan one date from `src`, which must hold exactly the date and nothing
    /// else: no surrounding whitespace and no trailing characters
    ///
    /// # Errors
    /// The span is a byte range into `src`. A shape that is not one of the three
    /// accepted forms errors naming them; a year before 1400 errors naming the
    /// floor; a well-formed shape whose month or day is not a real calendar
    /// date, February 29 outside a leap year included, errors saying the date
    /// does not exist
    pub fn parse(src: &str) -> Result<Self, ParseError> {
        // The whole input is the date, so an error rejects it as a unit
        let full = Span::new(0, clamp_u32(src.len()));
        let bytes = src.as_bytes();

        // A date is exactly ten bytes with the same separator in positions four
        // and seven. Length is checked first so the byte reads land in bounds
        if bytes.len() != DATE_LEN {
            return Err(ParseError::new(BAD_FORM, full));
        }
        let sep_byte = bytes.get(4).copied();
        let separator = match sep_byte.and_then(Separator::from_byte) {
            Some(sep) if bytes.get(7).copied() == sep_byte => sep,
            _ => return Err(ParseError::new(BAD_FORM, full)),
        };

        // The fields are fixed-width digit runs flanking the separators, so each
        // parses straight into the width jiff wants. A non-digit in a field or a
        // value too wide for the width fails the form check
        let year: i16 = digits(src, 0..4).ok_or_else(|| ParseError::new(BAD_FORM, full))?;
        let month: i8 = digits(src, 5..7).ok_or_else(|| ParseError::new(BAD_FORM, full))?;
        let day: i8 = digits(src, 8..10).ok_or_else(|| ParseError::new(BAD_FORM, full))?;

        // ledger's boost calendar has no year before 1400, so a date this early
        // would diverge between the two tools. jiff could represent it, so it is
        // refused here rather than by the calendar check below
        if year < MIN_YEAR {
            return Err(ParseError::new(
                format!("{src} is before {MIN_YEAR}, the earliest supported year"),
                full,
            ));
        }

        // jiff owns the calendar: month in 1..=12 and day in range for the month
        // and year, so a bad leap day is caught here
        let civil = Civil::new(year, month, day)
            .map_err(|_| ParseError::new(format!("{src} is not a real calendar date"), full))?;

        Ok(Self { civil, separator })
    }

    /// The validated civil date
    pub fn civil(self) -> Civil {
        self.civil
    }

    /// The day count from 1970-01-01, negative for dates before it
    ///
    /// Derived from the civil date on each call, so later layers can order and
    /// diff dates as plain integers
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "date difference is bounded by the 1400-9999 year range, well inside i32"
    )]
    pub fn epoch_day(self) -> i32 {
        // The difference to the epoch balances entirely into days and stays
        // well inside i32 across the supported year range, so this subtraction
        // cannot overflow
        (self.civil - EPOCH).get_days()
    }

    /// The separator the date was written with
    pub fn separator(self) -> Separator {
        self.separator
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Zero-padded fields joined by the stored separator, the canonical form
        // the date had to be read in, so it round-trips byte for byte
        let sep = char::from(self.separator.byte());
        write!(
            f,
            "{:04}{sep}{:02}{sep}{:02}",
            self.civil.year(),
            self.civil.month(),
            self.civil.day()
        )
    }
}

/// Parse a fixed-width digit run at `range` into `T`, or `None` if the slice is
/// out of bounds, holds a non-digit, or does not fit `T`
fn digits<T: FromStr>(src: &str, range: Range<usize>) -> Option<T> {
    let text = src.get(range)?;
    if text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::{Date, Separator};
    use crate::span::Span;

    // Parse a date, check its civil fields, epoch day, and separator, then prove
    // the value survives a format then re-parse. The inputs are canonical
    // zero-padded dates, so the format is byte-identical to the input
    fn check(input: &str, year: i16, month: i8, day: i8, epoch: i32, separator: Separator) {
        let date = Date::parse(input).unwrap();
        assert_eq!(date.civil().year(), year, "year of {input:?}");
        assert_eq!(date.civil().month(), month, "month of {input:?}");
        assert_eq!(date.civil().day(), day, "day of {input:?}");
        assert_eq!(date.epoch_day(), epoch, "epoch day of {input:?}");
        assert_eq!(date.separator(), separator, "separator of {input:?}");
        assert_eq!(date.to_string(), input, "round-trip of {input:?}");
        assert_eq!(
            Date::parse(&date.to_string()).unwrap(),
            date,
            "reparse of {input:?}"
        );
    }

    #[test]
    fn the_three_separator_forms_parse_to_the_same_day() {
        // Same day, different separator: the value matches but the separator is
        // kept, so the three are distinct dates
        check("2020-01-02", 2020, 1, 2, 18263, Separator::Dash);
        check("2020/01/02", 2020, 1, 2, 18263, Separator::Slash);
        check("2020.01.02", 2020, 1, 2, 18263, Separator::Dot);
    }

    #[test]
    fn the_epoch_day_counts_from_1970() {
        check("1970-01-01", 1970, 1, 1, 0, Separator::Dash);
        check("1970-01-02", 1970, 1, 2, 1, Separator::Dash);
        // A date before the epoch counts negative
        check("1969-12-31", 1969, 12, 31, -1, Separator::Dash);
        check("2000-01-01", 2000, 1, 1, 10957, Separator::Dash);
    }

    #[test]
    fn the_supported_year_range_parses_at_both_ends() {
        // ledger's boost calendar floor and the largest four-digit year
        check("1400-01-01", 1400, 1, 1, -208_188, Separator::Dash);
        check("9999-12-31", 9999, 12, 31, 2_932_896, Separator::Dash);
    }

    #[test]
    fn a_year_before_1400_is_rejected() {
        // jiff represents these years, but ledger's boost calendar does not,
        // and the floor keeps the two tools refusing the same dates
        for input in ["1399-12-31", "0001-01-01", "0000-01-01"] {
            let err = Date::parse(input).unwrap_err();
            assert_eq!(
                err.message,
                format!("{input} is before 1400, the earliest supported year"),
                "floor error expected for {input:?}"
            );
            assert_eq!(err.span, Span::new(0, 10), "span of {input:?}");
        }
    }

    #[test]
    fn a_leap_day_in_a_leap_year_parses() {
        check("2020-02-29", 2020, 2, 29, 18321, Separator::Dash);
        // The century rule: a year divisible by 400 is a leap year
        check("2000-02-29", 2000, 2, 29, 11016, Separator::Dash);
    }

    #[test]
    fn a_leap_day_outside_a_leap_year_is_rejected() {
        // The shape is well-formed, so this is a calendar error, not a form one
        let err = Date::parse("2021-02-29").unwrap_err();
        assert_eq!(err.message, "2021-02-29 is not a real calendar date");
        assert_eq!(err.span, Span::new(0, 10));
        // The century rule: divisible by 100 but not 400 is not a leap year
        let err = Date::parse("1900-02-29").unwrap_err();
        assert_eq!(err.message, "1900-02-29 is not a real calendar date");
    }

    #[test]
    fn out_of_range_months_and_days_are_rejected() {
        for input in ["2020-13-01", "2020-00-05", "2020-01-32", "2020-01-00"] {
            let err = Date::parse(input).unwrap_err();
            assert_eq!(
                err.message,
                format!("{input} is not a real calendar date"),
                "calendar error expected for {input:?}"
            );
        }
    }

    #[test]
    fn shapes_that_are_not_a_date_form_are_rejected() {
        for input in [
            "",            // empty
            "2020-1-2",    // single-digit month and day
            "2020-01-2",   // single-digit day
            "2020-1-02",   // single-digit month
            "20-01-02",    // two-digit year
            "02020-01-02", // five-digit year, too long
            " 2020-01-02", // leading whitespace
            "2020-01-02 ", // trailing whitespace
            "2020/01-02",  // separators disagree
            "2020_01_02",  // separator not one of the three
            "20a0-01-02",  // non-digit year field
            "2020-ab-02",  // non-digit month field
            "2020-01-0z",  // non-digit day field
            "hello world", // not a date at all
        ] {
            let err = Date::parse(input).unwrap_err();
            assert_eq!(
                err.message, "expected a date in YYYY-MM-DD, YYYY/MM/DD, or YYYY.MM.DD form",
                "form error expected for {input:?}"
            );
        }
    }

    #[test]
    fn the_digits_helper_refuses_out_of_bounds_and_overflow() {
        // Its None paths cannot be reached through parse, whose length check
        // keeps every range in bounds and every field inside its width, so the
        // helper's own contract is pinned directly
        assert_eq!(super::digits::<i8>("12", 0..3), None);
        assert_eq!(super::digits::<i8>("999", 0..3), None);
    }

    #[test]
    fn the_same_day_with_different_separators_is_not_equal() {
        let dash = Date::parse("2020-01-02").unwrap();
        let slash = Date::parse("2020/01/02").unwrap();
        assert_ne!(dash, slash);
        // The day they name is the same, which the epoch day and civil expose
        assert_eq!(dash.epoch_day(), slash.epoch_day());
        assert_eq!(dash.civil(), slash.civil());
    }
}
