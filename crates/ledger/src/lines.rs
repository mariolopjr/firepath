//! Line classification and block grouping: the journal's top-level shape
//!
//! A journal file is a sequence of blocks. A column-0 line opens one, and its
//! first byte says which kind: `;`, `#`, `%`, `|`, or `*` a comment, a digit a
//! transaction, `~` a periodic transaction, `=` an automated transaction,
//! anything else a directive. Indented lines belong to the open block, and a
//! blank line or the next column-0 line ends it. One-line comments take no
//! children, so an indented line under one is an error
//!
//! `comment` and `test` open a region that takes every following line raw,
//! blank and column-0 lines included, until a line starting with `end comment`
//! or `end test`. The region groups as one [`BlockKind::Comment`] block holding
//! its lines as children, closer last, and it ends the way upstream ledger ends
//! it: either closer ends either region, an indented closer does not count, and
//! a region left unclosed runs to end of input without an error
//!
//! Grouping only addresses lines. What a header or child line means is the
//! per-block parsers' job, so a block carries spans, not parsed content

use crate::Parsed;
use crate::error::ParseError;
use crate::span::{FileId, Span, clamp_u32};

/// Shown on an indented line no block is open to receive
const ORPHAN: &str = "indented line has no transaction or directive to attach to";

/// What a block's opening line is, decided by its first byte, or by its
/// first word for a `comment` or `test` region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// `;`, `#`, `%`, `|`, or `*`: a full-line comment, without children, or a
    /// `comment`/`test` region carrying its raw lines as children
    Comment,
    /// A digit: a transaction header, which starts with its date
    Transaction,
    /// `~`: a periodic transaction header
    Periodic,
    /// `=`: an automated transaction header
    Automated,
    /// Any other first byte: a directive, recognized by name later
    Directive,
}

/// One block: a column-0 opening line plus the lines under it
///
/// Spans cover a line's content with the trailing newline and any trailing
/// carriage returns excluded, so a CRLF file reads like an LF one. The fields
/// are public like [`Parsed`]'s: consumers read them directly
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// What the opening line classified the block as
    pub kind: BlockKind,
    /// The column-0 line that opened the block
    pub header: Span,
    /// The lines under the header, in source order: the indented lines for a
    /// normal block, every raw line with the closer last for a comment region
    pub children: Vec<Span>,
}

/// How one line participates in grouping
enum Class {
    /// Empty or whitespace only: ends the open block, belongs to none
    Blank,
    /// Starts with a space or tab: belongs to the open block
    Indented,
    /// A column-0 line: ends the open block and opens one of this kind
    Opener(BlockKind),
}

/// Classify one line by its first byte
fn classify(line: &[u8]) -> Class {
    // A whitespace-only line is blank, not indented, so it is ruled out
    // before the first byte is read
    if line.iter().all(|&b| b == b' ' || b == b'\t') {
        return Class::Blank;
    }
    match line.first() {
        Some(b' ' | b'\t') => Class::Indented,
        Some(b';' | b'#' | b'%' | b'|' | b'*') => Class::Opener(BlockKind::Comment),
        Some(b'0'..=b'9') => Class::Opener(BlockKind::Transaction),
        Some(b'~') => Class::Opener(BlockKind::Periodic),
        Some(b'=') => Class::Opener(BlockKind::Automated),
        _ => Class::Opener(BlockKind::Directive),
    }
}

/// Whether a directive line opens a comment region
///
/// The first word must be exactly `comment` or `test`, so `comments` stays an
/// ordinary directive, and an optional leading `!` or `@` is skipped because
/// ledger accepts prefixed directives
fn opens_region(line: &[u8]) -> bool {
    let word = match line.first() {
        Some(b'!' | b'@') => line.get(1..).unwrap_or_default(),
        _ => line,
    };
    let len = word
        .iter()
        .position(|&b| b == b' ' || b == b'\t')
        .unwrap_or(word.len());
    let word = word.get(..len).unwrap_or_default();
    word == b"comment" || word == b"test"
}

/// Whether a line closes an open comment region
///
/// Upstream matches the raw line against both closers by prefix, so either
/// closer ends either region, trailing text is allowed, and an indented
/// closer does not close
fn closes_region(line: &[u8]) -> bool {
    line.starts_with(b"end comment") || line.starts_with(b"end test")
}

/// Group a source file's lines into blocks
///
/// Errors do not stop the grouping. An indented line with no open block above
/// it is reported as an error and skipped, so one stray line costs one error
/// and the blocks around it still group
pub fn blocks(file: FileId, source: &[u8]) -> Parsed<Block> {
    let mut parsed = Parsed::new(file);
    // The block still accepting indented children, if any
    let mut open: Option<Block> = None;
    // The comment region taking lines raw, if any. A region only opens after
    // the open block is flushed, so at most one of `open` and `region` is Some
    let mut region: Option<Block> = None;

    let bytes = source;
    let mut pos = 0usize;
    while pos < bytes.len() {
        // The line runs to the next newline or the end of input. Trailing
        // carriage returns are stripped from the span, not just tolerated,
        // so no later layer ever sees a `\r`
        let rest = bytes.get(pos..).unwrap_or_default();
        let len = memchr::memchr(b'\n', rest).unwrap_or(rest.len());
        let mut end = pos.saturating_add(len);
        while end > pos && bytes.get(end.saturating_sub(1)) == Some(&b'\r') {
            end = end.saturating_sub(1);
        }
        let line = bytes.get(pos..end).unwrap_or_default();
        let span = Span::new(clamp_u32(pos), clamp_u32(end));

        if let Some(mut block) = region.take() {
            // Every line is a raw child, the closer included, so the block
            // covers the whole region
            block.children.push(span);
            if closes_region(line) {
                parsed.items.push(block);
            } else {
                region = Some(block);
            }
        } else {
            match classify(line) {
                Class::Blank => {
                    if let Some(block) = open.take() {
                        parsed.items.push(block);
                    }
                }
                Class::Indented => {
                    if let Some(block) = open.as_mut() {
                        block.children.push(span);
                    } else {
                        parsed.errors.push(ParseError::new(ORPHAN, span));
                    }
                }
                Class::Opener(kind) => {
                    if let Some(block) = open.take() {
                        parsed.items.push(block);
                    }
                    if kind == BlockKind::Directive && opens_region(line) {
                        // A `comment` or `test` directive opens a region,
                        // grouped as a comment rather than a directive, held
                        // open for its raw lines
                        region = Some(Block {
                            kind: BlockKind::Comment,
                            header: span,
                            children: Vec::new(),
                        });
                    } else if kind == BlockKind::Comment {
                        // A one-line comment closes at once so a line indented
                        // under it is an orphan rather than silently swallowed
                        // as its child
                        parsed.items.push(Block {
                            kind,
                            header: span,
                            children: Vec::new(),
                        });
                    } else {
                        open = Some(Block {
                            kind,
                            header: span,
                            children: Vec::new(),
                        });
                    }
                }
            }
        }

        // Step past the newline
        pos = pos.saturating_add(len).saturating_add(1);
    }
    // End of input closes whichever of the two is open, an unfinished region
    // without its closer: upstream reads an unclosed region to the end of the
    // file and reports nothing
    if let Some(block) = open.or(region) {
        parsed.items.push(block);
    }
    parsed
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, reason = "unwrap keeps the table tests terse")]
mod tests {
    use super::{Block, BlockKind, blocks};
    use crate::span::{FileId, Span};

    // Resolve a span back to its text so expectations read as source lines
    // instead of byte offsets
    fn text(src: &str, span: Span) -> &str {
        let range = usize::try_from(span.start()).unwrap()..usize::try_from(span.end()).unwrap();
        src.get(range).unwrap()
    }

    // A block as (kind, header text, child texts), the shape the fixture
    // expectations are written in
    fn shape<'s>(src: &'s str, block: &Block) -> (BlockKind, &'s str, Vec<&'s str>) {
        let children = block.children.iter().map(|&c| text(src, c)).collect();
        (block.kind, text(src, block.header), children)
    }

    #[test]
    fn a_mixed_fixture_groups_into_the_expected_blocks() {
        let src = "\
; opening note
2020-01-02 * grocery store
    Expenses:Food    $50.00
    Assets:Checking

~ monthly
    Budget:Food    $400.00

= /Expenses:Food/
    Budget:Food    -1

account Expenses:Food
    note weekly shopping

    orphan child
2020-01-03 coffee
    Expenses:Coffee    $4.00
";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![
                (BlockKind::Comment, "; opening note", vec![]),
                (
                    BlockKind::Transaction,
                    "2020-01-02 * grocery store",
                    vec!["    Expenses:Food    $50.00", "    Assets:Checking"],
                ),
                (
                    BlockKind::Periodic,
                    "~ monthly",
                    vec!["    Budget:Food    $400.00"]
                ),
                (
                    BlockKind::Automated,
                    "= /Expenses:Food/",
                    vec!["    Budget:Food    -1"]
                ),
                (
                    BlockKind::Directive,
                    "account Expenses:Food",
                    vec!["    note weekly shopping"],
                ),
                (
                    BlockKind::Transaction,
                    "2020-01-03 coffee",
                    vec!["    Expenses:Coffee    $4.00"],
                ),
            ]
        );
        // The blank line above the indented line closed the account block, so the
        // indented line has nothing to join: exactly one error
        assert_eq!(parsed.errors.len(), 1);
        let err = parsed.errors.first().unwrap();
        assert_eq!(
            err.message,
            "indented line has no transaction or directive to attach to"
        );
        assert_eq!(text(src, err.span), "    orphan child");
    }

    #[test]
    fn every_kind_is_dispatched_on_the_first_byte() {
        // One line per first byte, the digit range pinned at both ends
        for (src, kind) in [
            ("; c", BlockKind::Comment),
            ("# c", BlockKind::Comment),
            ("% c", BlockKind::Comment),
            ("| c", BlockKind::Comment),
            ("* c", BlockKind::Comment),
            ("0999-01-01 payee", BlockKind::Transaction),
            ("9999-01-01 payee", BlockKind::Transaction),
            ("~ monthly", BlockKind::Periodic),
            ("= /regex/", BlockKind::Automated),
            ("account Assets", BlockKind::Directive),
            ("include transactions/*.ledger", BlockKind::Directive),
            ("P 2020-01-02 VTI $150.00", BlockKind::Directive),
        ] {
            let parsed = blocks(FileId::new(0), src.as_bytes());
            let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
            assert_eq!(got, vec![(kind, src, vec![])], "dispatch of {src:?}");
            assert!(!parsed.has_errors(), "no errors for {src:?}");
        }
    }

    #[test]
    fn a_blank_line_closes_the_open_block() {
        let src = "2020-01-02 payee\n    Assets:Cash  $5\n\n    Expenses:Food  $5\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![(
                BlockKind::Transaction,
                "2020-01-02 payee",
                vec!["    Assets:Cash  $5"],
            )]
        );
        // The posting after the blank is stranded, not attached
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            text(src, parsed.errors.first().unwrap().span),
            "    Expenses:Food  $5"
        );
    }

    #[test]
    fn a_column_zero_line_closes_the_open_block() {
        // No blank between the transactions and no trailing newline, so the
        // second header closes the first block and the end of input closes
        // the second. The tab-indented child attaches like a spaced one
        let src = "2020-01-02 one\n    Assets:Cash  $5\n2020-01-03 two\n\tAssets:Cash  $6";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![
                (
                    BlockKind::Transaction,
                    "2020-01-02 one",
                    vec!["    Assets:Cash  $5"]
                ),
                (
                    BlockKind::Transaction,
                    "2020-01-03 two",
                    vec!["\tAssets:Cash  $6"]
                ),
            ]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn a_comment_takes_no_children() {
        let src = "; note\n    indented under a comment\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(got, vec![(BlockKind::Comment, "; note", vec![])]);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(
            text(src, parsed.errors.first().unwrap().span),
            "    indented under a comment"
        );
    }

    #[test]
    fn a_whitespace_only_line_is_blank_not_indented() {
        // The middle line holds only a space and a tab: it closes the block
        // instead of attaching to it, so the transaction has no children
        let src = "2020-01-02 payee\n \t \naccount Assets\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![
                (BlockKind::Transaction, "2020-01-02 payee", vec![]),
                (BlockKind::Directive, "account Assets", vec![]),
            ]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn crlf_lines_strip_carriage_returns_from_spans() {
        // Exact spans pin the offset arithmetic: every span stops before its
        // line's carriage returns, the doubled \r included, and the empty
        // CRLF line closes the transaction
        let src = "; c\r\n2020-01-02 p\r\n    a  $5\r\n\r\naccount A\r\r\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        assert_eq!(
            parsed.items,
            vec![
                Block {
                    kind: BlockKind::Comment,
                    header: Span::new(0, 3),
                    children: vec![],
                },
                Block {
                    kind: BlockKind::Transaction,
                    header: Span::new(5, 17),
                    children: vec![Span::new(19, 28)],
                },
                Block {
                    kind: BlockKind::Directive,
                    header: Span::new(32, 41),
                    children: vec![],
                },
            ]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn a_comment_region_takes_lines_raw_until_its_closer() {
        // The region swallows transaction-shaped, indented, and blank lines
        // alike, and a closer with trailing text still closes. Everything
        // lands as children with the closer last, and the transaction after
        // the region parses normally
        let src = "\
comment
2020-01-01 not a transaction
    not a posting

end comment extra
2020-01-02 real
    Expenses:Food    $5.00
";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![
                (
                    BlockKind::Comment,
                    "comment",
                    vec![
                        "2020-01-01 not a transaction",
                        "    not a posting",
                        "",
                        "end comment extra",
                    ],
                ),
                (
                    BlockKind::Transaction,
                    "2020-01-02 real",
                    vec!["    Expenses:Food    $5.00"],
                ),
            ]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn either_closer_ends_either_region() {
        // A `test` region closed by `end comment` and a bang-prefixed
        // `comment` region with trailing text closed by `end test`: the cross
        // pairings and prefixed opener upstream accepts
        let src = "test\nraw\nend comment\n!comment trailing\nraw\nend test\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![
                (BlockKind::Comment, "test", vec!["raw", "end comment"]),
                (
                    BlockKind::Comment,
                    "!comment trailing",
                    vec!["raw", "end test"]
                ),
            ]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn an_indented_closer_does_not_close_a_region() {
        let src = "comment\n    end comment\nend comment\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![(
                BlockKind::Comment,
                "comment",
                vec!["    end comment", "end comment"],
            )]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn an_unclosed_region_runs_to_end_of_input() {
        // No closer before end of input: the region keeps everything and no
        // error is reported, matching upstream
        let src = "comment\n2020-01-01 swallowed\n    also swallowed\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![(
                BlockKind::Comment,
                "comment",
                vec!["2020-01-01 swallowed", "    also swallowed"],
            )]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn a_region_opener_must_match_its_word_exactly() {
        // `comments` is an ordinary directive, so the indented line is its
        // child and no region opens
        let src = "comments\n    child\n";
        let parsed = blocks(FileId::new(0), src.as_bytes());
        let got: Vec<_> = parsed.items.iter().map(|b| shape(src, b)).collect();
        assert_eq!(
            got,
            vec![(BlockKind::Directive, "comments", vec!["    child"])]
        );
        assert!(!parsed.has_errors());
    }

    #[test]
    fn an_empty_source_yields_no_blocks() {
        for src in ["", "\n", "\n\n \n"] {
            let parsed = blocks(FileId::new(0), src.as_bytes());
            assert!(parsed.items.is_empty(), "no blocks for {src:?}");
            assert!(!parsed.has_errors(), "no errors for {src:?}");
        }
    }

    #[test]
    fn an_indented_first_line_is_an_orphan() {
        let parsed = blocks(FileId::new(0), "    Assets:Cash  $5\n".as_bytes());
        assert!(parsed.items.is_empty());
        assert_eq!(parsed.errors.len(), 1);
        let err = parsed.errors.first().unwrap();
        assert_eq!(
            err.message,
            "indented line has no transaction or directive to attach to"
        );
        assert_eq!(err.span, Span::new(0, 19));
    }
}
