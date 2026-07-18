//! Directive scanning: supports capturing `include`
//!
//! A directive is a column-0 line whose first word is a keyword, optionally
//! behind a leading `!` or `@` that ledger allows and this strips. Only
//! `include` is read here: its argument, the file or glob to pull in, is
//! captured raw for the include resolver to expand later

use crate::error::ParseError;
use crate::scan::{skip_ws, span_at, trim_end, widen_empty};
use crate::span::Span;

/// A parsed directive
///
/// Spans are file-absolute through the `base` handed to
/// [`parse`](Directive::parse)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    /// An `include` line, its raw argument captured for the resolver
    Include {
        /// The file or glob after `include`, trimmed, a span into the file
        argument: Span,
    },
}

impl Directive {
    /// Scan one directive from `line`, a directive block's column-0 header line
    /// with no trailing newline or carriage return. `base` is the line's byte
    /// offset into its file, so every span and error lands file-absolute
    ///
    /// # Errors
    /// An `include` with no argument errors naming the missing path. Every
    /// directive that has no scanner yet, which today is every keyword but
    /// `include`, is refused as a hard error naming the keyword
    pub fn parse(line: &str, base: u32) -> Result<Self, ParseError> {
        let bytes = line.as_bytes();
        // A directive keyword may carry a leading `!` or `@`: ledger accepts
        // the prefixed form, so skip it before reading the keyword, the same
        // way the block grouper recognizes a prefixed `comment` or `test`
        // region. A directive line is never indented, so the keyword or its
        // prefix opens it
        let kw_start = usize::from(matches!(bytes.first(), Some(b'!' | b'@')));
        // The keyword is the first whitespace-delimited word past any prefix
        let kw_end = memchr::memchr2(b' ', b'\t', bytes).unwrap_or(bytes.len());
        let keyword = line.get(kw_start..kw_end).unwrap_or_default();

        if keyword == "include" {
            // The argument is the rest of the line, trimmed. Internal spaces
            // stay, so a path may hold them, but a leading or trailing run is
            // not part of the name
            let arg_lo = skip_ws(bytes, kw_end);
            let arg_hi = trim_end(bytes, arg_lo, bytes.len());
            if arg_lo >= arg_hi {
                return Err(ParseError::new(
                    "include directive is missing a file path",
                    span_at(base, 0, line.len()),
                ));
            }
            return Ok(Self::Include {
                argument: span_at(base, arg_lo, arg_hi),
            });
        }

        // Naming the keyword keeps the refusal precise, and the span points at
        // the keyword rather than the whole line so an editor underlines the
        // word. An empty keyword, a bare `!` or `@`, widens so the refusal
        // stays visible
        Err(ParseError::new(
            format!("the {keyword:?} directive is not supported yet"),
            widen_empty(span_at(base, kw_start, kw_end), base, line.len()),
        ))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::Directive;
    use crate::span::Span;

    // Resolve a span back to its text so expectations read as source text
    // instead of byte offsets
    fn text(line: &str, span: Span) -> &str {
        let range = usize::try_from(span.start()).unwrap()..usize::try_from(span.end()).unwrap();
        line.get(range).unwrap()
    }

    // The argument span of an include line parsed at base 0
    fn include_arg(line: &str) -> Span {
        let Directive::Include { argument } = Directive::parse(line, 0).unwrap();
        argument
    }

    #[test]
    fn an_include_captures_its_raw_argument() {
        // A plain path, surrounding whitespace trimmed, and a glob all come
        // through verbatim between the trimmed bounds
        for (line, argument) in [
            ("include txns/2020.ledger", "txns/2020.ledger"),
            ("include  txns/*.ledger", "txns/*.ledger"),
            (
                "include ../shared/prices.ledger   ",
                "../shared/prices.ledger",
            ),
            ("include\tpath/with/tab.ledger", "path/with/tab.ledger"),
            // A path may hold internal spaces, which the trim keeps
            ("include my ledgers/2020.ledger", "my ledgers/2020.ledger"),
        ] {
            let span = include_arg(line);
            assert_eq!(text(line, span), argument, "argument of {line:?}");
        }
    }

    #[test]
    fn an_include_argument_span_is_exact() {
        // `include` is seven bytes, the space is byte seven, so the argument
        // starts at byte eight and runs to the end of the line
        let span = include_arg("include txns/2020.ledger");
        assert_eq!(span, Span::new(8, 24));
    }

    #[test]
    fn an_include_with_no_argument_is_an_error() {
        // A bare keyword and a keyword with only trailing whitespace both have
        // no path to capture, so both error over the whole line
        for line in ["include", "include   ", "include\t"] {
            let err = Directive::parse(line, 0).unwrap_err();
            assert_eq!(
                err.message, "include directive is missing a file path",
                "message for {line:?}"
            );
            assert_eq!(
                err.span,
                Span::new(0, u32::try_from(line.len()).unwrap()),
                "span for {line:?}"
            );
        }
    }

    #[test]
    fn an_unsupported_directive_is_refused_by_name() {
        // Every directive that has no scanner yet is refused, the keyword named
        // and its span pointing at the keyword, not the whole line
        for (line, keyword, kw_len) in [
            ("account Assets:Cash", "account", 7),
            ("commodity $", "commodity", 9),
            ("P 2020-01-01 VTI $1.00", "P", 1),
            ("apply account Personal", "apply", 5),
            ("tag Trip", "tag", 3),
            // A keyword alone, no argument, is still refused by name
            ("bucket", "bucket", 6),
        ] {
            let err = Directive::parse(line, 0).unwrap_err();
            assert_eq!(
                err.message,
                format!("the {keyword:?} directive is not supported yet"),
                "message for {line:?}"
            );
            assert_eq!(err.span, Span::new(0, kw_len), "span for {line:?}");
        }
    }

    #[test]
    fn a_bang_or_at_prefix_is_stripped_before_the_keyword() {
        // Ledger accepts a directive behind a `!` or `@`, so a prefixed include
        // captures its argument like a bare one and a prefixed unsupported
        // directive is refused by the keyword with the prefix dropped
        for prefix in ["!", "@"] {
            let line = format!("{prefix}include txns/2020.ledger");
            assert_eq!(
                text(&line, include_arg(&line)),
                "txns/2020.ledger",
                "argument of {line:?}"
            );

            let line = format!("{prefix}account Assets:Cash");
            let err = Directive::parse(&line, 0).unwrap_err();
            assert_eq!(
                err.message, "the \"account\" directive is not supported yet",
                "message for {line:?}"
            );
            // The span underlines just the keyword, the prefix left out
            assert_eq!(err.span, Span::new(1, 8), "span for {line:?}");
        }
    }

    #[test]
    fn a_bare_prefix_widens_its_empty_refusal_span() {
        // A lone `!` or `@` has an empty keyword, so its refusal span widens
        // back over the prefix byte instead of rendering as nothing
        for line in ["!", "@"] {
            let err = Directive::parse(line, 0).unwrap_err();
            assert_eq!(err.span, Span::new(0, 1), "span for {line:?}");
        }
    }

    #[test]
    fn spans_are_file_absolute_through_the_base() {
        // The include argument and the refusal keyword both shift by the base
        let Directive::Include { argument } =
            Directive::parse("include txns/2020.ledger", 1000).unwrap();
        assert_eq!(argument, Span::new(1008, 1024));

        let err = Directive::parse("account Assets", 1000).unwrap_err();
        assert_eq!(err.span, Span::new(1000, 1007));

        let err = Directive::parse("include", 1000).unwrap_err();
        assert_eq!(err.span, Span::new(1000, 1007));
    }
}
