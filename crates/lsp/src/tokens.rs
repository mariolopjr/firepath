//! Semantic tokens
//!
//! A token is a source span plus a [`Kind`]: a transaction's dates and payee, a
//! posting's account and amount, the commodity inside that amount, and a comment
//! line. A header's code is scanned with a span but has no kind yet, its status
//! marker is scanned without one, and costs, assertions, trailing comments, and
//! tags get their kinds when their grammar lands.
//!
//! The legend names are firepath's custom names. A client links the names to
//! highlight groups itself. `.nvim.lua` is used in this repo for linking, matching
//! what the ledger tree-sitter grammar uses for the same constructs.
//!
//! The buffer is grouped into blocks again here rather than carried over from
//! the diagnostics pass: [`parse`](firepath_ledger::parse) returns errors, not
//! items, so there is nothing to reuse yet.
//!
//! A block that does not scan contributes no tokens. Its diagnostic already
//! says what is wrong, and a half-scanned line has no spans to color. The one
//! construct that colors wrong rather than not at all is an indented comment
//! line under a transaction, which the parser still routes to
//! [`Posting::parse`] and reads as an account name

use std::ops::Range;

use firepath_ledger::{
    Block, BlockKind, Commodity, Placement, Posting, Span, TransactionHeader, blocks, clamp_u32,
};
use lsp_types::{Position, SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend};

use crate::Document;
use crate::docs::SOLE_FILE;

/// The exact byte length of a date, which the date scanner accepts in one
/// zero-padded ten-byte form only
///
/// An auxiliary date carries no span of its own, so its span is derived: it
/// always sits in the ten bytes past the `=` that follows the actual date
const DATE_LEN: u32 = 10;

/// What a token marks
///
/// The discriminant is the token's index into the legend, which is how a client
/// reads its type back
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A transaction's actual or auxiliary date
    Date = 0,
    /// A transaction's payee
    Payee = 1,
    /// A posting's account name, without the brackets of a virtual posting
    Account = 2,
    /// The quantity of a posting's amount, its sign included
    Amount = 3,
    /// The commodity symbol inside an amount, its quotes included
    Commodity = 4,
    /// A whole comment line, one per line of a comment region
    Comment = 5,
}

impl Kind {
    /// Every kind, in legend order
    const ALL: [Self; 6] = [
        Self::Date,
        Self::Payee,
        Self::Account,
        Self::Amount,
        Self::Commodity,
        Self::Comment,
    ];

    /// The name the legend publishes this kind under
    fn name(self) -> SemanticTokenType {
        match self {
            Self::Date => SemanticTokenType::new("date"),
            Self::Payee => SemanticTokenType::new("payee"),
            Self::Account => SemanticTokenType::new("account"),
            Self::Amount => SemanticTokenType::new("amount"),
            Self::Commodity => SemanticTokenType::new("commodity"),
            Self::Comment => SemanticTokenType::COMMENT,
        }
    }

    /// The index a token names this kind by
    fn index(self) -> u32 {
        self as u32
    }
}

/// The token types the server publishes, in the order tokens index into
///
/// No modifiers: nothing in the grammar varies a construct the way a modifier
/// describes
pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: Kind::ALL.iter().map(|&kind| kind.name()).collect(),
        token_modifiers: Vec::new(),
    }
}

/// One token before it is encoded
#[derive(Debug, Clone, Copy)]
struct Token {
    span: Span,
    kind: Kind,
}

impl Token {
    fn new(span: Span, kind: Kind) -> Self {
        Self { span, kind }
    }
}

/// Every token in a document, in the encoding the client decodes
pub(crate) fn tokens(document: &Document) -> SemanticTokens {
    let source = document.text();
    let mut found = Vec::new();
    for block in &blocks(SOLE_FILE, source).items {
        match block.kind {
            // A one-line comment is its header; a comment region carries each
            // of its raw lines, closer included, as a child
            BlockKind::Comment => {
                found.push(Token::new(block.header, Kind::Comment));
                found.extend(
                    block
                        .children
                        .iter()
                        .map(|&line| Token::new(line, Kind::Comment)),
                );
            }
            BlockKind::Transaction => transaction(source, block, &mut found),
            // A directive holds a keyword and a path, and a periodic or
            // automated header is refused by the parser, so none of the three
            // has a kind yet
            BlockKind::Directive | BlockKind::Periodic | BlockKind::Automated => {}
        }
    }
    // The encoding is a chain of deltas, so it has to be walked in ascending
    // order. Blocks arrive in source order and a block's parts are pushed in
    // the order its scanner returns them, which is already ascending; the sort
    // is what keeps that true as additional kinds are added
    found.sort_by_key(|token| token.span.start());
    SemanticTokens {
        result_id: None,
        data: encode(document, &found),
    }
}

/// The tokens of a transaction block: its header line, then each posting under it
fn transaction(source: &[u8], block: &Block, found: &mut Vec<Token>) {
    if let Ok(header) = TransactionHeader::parse(slice(source, block.header), block.header.start())
    {
        found.push(Token::new(header.date_span, Kind::Date));
        if header.aux_date.is_some() {
            let start = header.date_span.end().saturating_add(1);
            found.push(Token::new(
                Span::new(start, start.saturating_add(DATE_LEN)),
                Kind::Date,
            ));
        }
        if let Some(payee) = header.payee {
            found.push(Token::new(payee, Kind::Payee));
        }
    }
    for &child in &block.children {
        if let Ok(posting) = Posting::parse(slice(source, child), child.start()) {
            found.push(Token::new(posting.account, Kind::Account));
            if let (Some(amount), Some(span)) = (&posting.amount, posting.amount_span) {
                amount_tokens(source, span, &amount.commodity, found);
            }
        }
    }
}

/// The tokens of one amount: the commodity symbol, and the quantity on either
/// side of it
///
/// The quantity is what the symbol leaves behind, so a sign written before a
/// prefix commodity (`-$20`) is part of the quantity it belongs to rather than
/// left uncolored
fn amount_tokens(source: &[u8], span: Span, commodity: &Commodity, found: &mut Vec<Token>) {
    let text = slice(source, span);
    let symbol = commodity_range(text, commodity);
    push(found, text, span, 0..symbol.start, Kind::Amount);
    push(found, text, span, symbol.clone(), Kind::Commodity);
    push(found, text, span, symbol.end..text.len(), Kind::Amount);
}

/// Push the `range` of an amount's `text` as a token, with the whitespace that
/// separates the quantity from a suffix commodity trimmed off
///
/// `base` is the amount's own span, so the token lands file-absolute
fn push(found: &mut Vec<Token>, text: &[u8], base: Span, range: Range<usize>, kind: Kind) {
    let part = text.get(range.clone()).unwrap_or_default();
    let lead = part.len().saturating_sub(part.trim_ascii_start().len());
    let trimmed = part.trim_ascii();
    let start = base
        .start()
        .saturating_add(clamp_u32(range.start))
        .saturating_add(clamp_u32(lead));
    found.push(Token::new(
        Span::new(start, start.saturating_add(clamp_u32(trimmed.len()))),
        kind,
    ));
}

/// The commodity symbol's byte range inside an amount's text
///
/// The symbol is anchored to the end of the amount it sits on rather than
/// searched for: a prefix commodity opens the amount, behind at most the one
/// sign the scanner takes before it and with no space allowed between the two,
/// and a suffix commodity closes it, since the scanner refuses anything written
/// after the amount. Searching would instead rest on the symbol's bytes
/// appearing nowhere in the quantity, which is true of the grammar today but is
/// not the scanner's contract
fn commodity_range(text: &[u8], commodity: &Commodity) -> Range<usize> {
    let symbol = commodity.symbol();
    match commodity.placement() {
        Placement::Prefix => {
            let start = usize::from(matches!(text.first(), Some(b'-' | b'+')));
            start..start.saturating_add(width(symbol, text.get(start) == Some(&b'"')))
        }
        Placement::Suffix => {
            let end = text.len();
            end.saturating_sub(width(symbol, text.last() == Some(&b'"')))..end
        }
    }
}

/// The bytes a symbol takes up in the source
///
/// The scanner reports a quoted symbol without its quotes, and the quotes color
/// with it, so a quoted symbol is written two bytes wider than it is reported
fn width(symbol: &[u8], quoted: bool) -> usize {
    symbol.len().saturating_add(if quoted { 2 } else { 0 })
}

/// Encode tokens into the relative form the protocol carries
///
/// Each token is written as a delta from the one before it: the line delta, the
/// start delta within the line when both sit on the same line, and the length.
/// The three all count UTF-16 code units, which is what the position mapping
/// returns.
///
/// A token never crosses a line, since every span comes from one line of one
/// block, so a length is the distance between two characters on one line
fn encode(document: &Document, found: &[Token]) -> Vec<SemanticToken> {
    let mut data: Vec<SemanticToken> = Vec::with_capacity(found.len());
    let mut previous = Position::new(0, 0);
    for token in found {
        let start = document.position(token.span.start());
        let length = document
            .position(token.span.end())
            .character
            .saturating_sub(start.character);
        // A zero-width token marks nothing and would leave the client decoding
        // a run of empty highlights. The empty account name of `[]`, which the
        // parser accepts, is the one the grammar produces
        if length == 0 {
            continue;
        }
        let delta_line = start.line.saturating_sub(previous.line);
        let delta_start = if delta_line == 0 {
            start.character.saturating_sub(previous.character)
        } else {
            start.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: token.kind.index(),
            token_modifiers_bitset: 0,
        });
        previous = start;
    }
    data
}

/// The bytes a span covers, empty for a span that does not land in `source`
fn slice(source: &[u8], span: Span) -> &[u8] {
    source
        .get(span.start() as usize..span.end() as usize)
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "unwrap keeps the fixtures terse, and a table row names itself on the way out"
)]
mod tests {
    use std::fmt::Write as _;

    use firepath_fixtures::{Manifest, generate};
    use firepath_ledger::Amount;
    use lsp_types::Position;

    use super::{Kind, commodity_range, legend, tokens};
    use crate::Document;

    // Ten lines of `transactions/2015.ledger` as the generator emits them,
    // covering every kind a generated file holds: the header comments, a
    // transaction's date and payee, and postings with and without an amount.
    // `the_excerpt_is_what_the_generator_still_emits` keeps this in step with
    // the generator
    const EXCERPT: &str = "\
; firepath transactions for 2015
; generated by `just gen-fixtures`, do not edit by hand

2015-01-01 * Meridian Systems
    Assets:Checking                         $7083.33
    Income:Salary:Meridian Systems

2015-01-01 * Bramble & Co
    Assets:Checking                         $5166.66
    Income:Salary:Bramble & Co
";

    // Every token the excerpt encodes to, decoded back to absolute positions
    // and resolved to the text it covers
    const SNAPSHOT: &str = r#"0:0-32 comment "; firepath transactions for 2015"
1:0-55 comment "; generated by `just gen-fixtures`, do not edit by hand"
3:0-10 date "2015-01-01"
3:13-29 payee "Meridian Systems"
4:4-19 account "Assets:Checking"
4:44-45 commodity "$"
4:45-52 amount "7083.33"
5:4-34 account "Income:Salary:Meridian Systems"
7:0-10 date "2015-01-01"
7:13-25 payee "Bramble & Co"
8:4-19 account "Assets:Checking"
8:44-45 commodity "$"
8:45-52 amount "5166.66"
9:4-30 account "Income:Salary:Bramble & Co"
"#;

    fn doc(text: &str) -> Document {
        Document::new(1, text.to_owned())
    }

    // Decode the relative encoding back into `line:start-end kind "text"` lines,
    // which reads as the source the tokens came from. Walking the deltas back
    // and resolving each position to bytes is what proves the encoding is the
    // one a client would decode
    fn render(document: &Document) -> String {
        let mut out = String::new();
        let mut line = 0u32;
        let mut character = 0u32;
        for token in tokens(document).data {
            line = line.saturating_add(token.delta_line);
            character = if token.delta_line == 0 {
                character.saturating_add(token.delta_start)
            } else {
                token.delta_start
            };
            let end = character.saturating_add(token.length);
            let bytes = document.text();
            let text = String::from_utf8_lossy(
                bytes
                    .get(
                        document.offset(Position::new(line, character)) as usize
                            ..document.offset(Position::new(line, end)) as usize,
                    )
                    .expect("a token covers bytes of the buffer"),
            );
            let kind = Kind::ALL
                .get(token.token_type as usize)
                .expect("a token names a kind in the legend");
            let _ = writeln!(
                out,
                "{line}:{character}-{end} {} {text:?}",
                kind.name().as_str()
            );
        }
        out
    }

    #[test]
    fn a_generated_file_tokenizes_to_its_snapshot() {
        assert_eq!(render(&doc(EXCERPT)), SNAPSHOT);
    }

    #[test]
    fn the_excerpt_is_what_the_generator_still_emits() {
        // The snapshot is only worth having while it is the real corpus, so the
        // excerpt is checked against the generator rather than trusted
        let files = generate(&Manifest::default()).expect("the fixtures generate");
        let body = files
            .get("transactions/2015.ledger")
            .expect("the 2015 journal is generated");
        assert!(
            body.starts_with(EXCERPT),
            "the generator no longer opens 2015.ledger with the snapshot excerpt"
        );
    }

    #[test]
    fn every_generated_file_encodes_a_usable_token_stream() {
        // The whole fixture rather than the excerpt: a token that crosses a line
        // or overlaps the one before it corrupts how the client reads the rest
        // of the stream, and nothing in the encoding itself would catch it. The
        // deltas are unsigned, so a token emitted out of order encodes as one
        // starting inside its predecessor rather than as anything malformed
        let files = generate(&Manifest::default()).expect("the fixtures generate");
        for (name, body) in &files {
            let document = doc(body);
            let mut line = 0u32;
            let mut character = 0u32;
            let mut previous_end = 0u32;
            let mut count = 0u32;
            for token in tokens(&document).data {
                line = line.saturating_add(token.delta_line);
                if token.delta_line == 0 {
                    character = character.saturating_add(token.delta_start);
                } else {
                    character = token.delta_start;
                    previous_end = 0;
                }
                assert!(token.length > 0, "empty token in {name}");
                assert!(
                    character >= previous_end,
                    "token at {line}:{character} of {name} overlaps the one before it"
                );
                let start = document.offset(Position::new(line, character));
                let end =
                    document.offset(Position::new(line, character.saturating_add(token.length)));
                let text = document
                    .text()
                    .get(start as usize..end as usize)
                    .expect("a token covers bytes of the buffer");
                assert!(
                    !text.contains(&b'\n'),
                    "token at {line}:{character} of {name} crosses a line"
                );
                previous_end = character.saturating_add(token.length);
                count = count.saturating_add(1);
            }
            assert!(count > 0, "{name} produced no tokens");
        }
    }

    #[test]
    fn a_kind_indexes_the_legend_entry_that_names_it() {
        // The discriminant is what a client reads the type back by, so a kind
        // added out of order would rename every kind after it
        let published = legend().token_types;
        for kind in Kind::ALL {
            assert_eq!(
                published.get(kind.index() as usize),
                Some(&kind.name()),
                "{kind:?}"
            );
        }
        assert_eq!(published.len(), Kind::ALL.len());
        assert!(legend().token_modifiers.is_empty());
    }

    #[test]
    fn a_prefix_commodity_splits_from_its_quantity() {
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  $50.00\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 commodity \"$\"\n1:8-13 amount \"50.00\"\n"
        );
    }

    #[test]
    fn a_sign_before_a_prefix_commodity_is_part_of_the_quantity() {
        // The `-` binds to the number, so it colors with the number rather than
        // being left as the one uncolored byte in the amount
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  -$20\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 amount \"-\"\n1:8-9 commodity \"$\"\n1:9-11 amount \"20\"\n"
        );
    }

    #[test]
    fn a_suffix_commodity_splits_from_its_quantity() {
        // The space between them belongs to neither token
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  5 VTI\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 amount \"5\"\n1:9-12 commodity \"VTI\"\n"
        );
    }

    #[test]
    fn a_quoted_commodity_token_covers_its_quotes() {
        // The quotes are how the symbol reads as one token, so they color with
        // it. The symbol itself holds a space and would not be found by its
        // bytes alone
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  5 \"MUTF: VFIAX\"\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 amount \"5\"\n1:9-22 commodity \"\\\"MUTF: VFIAX\\\"\"\n"
        );
    }

    #[test]
    fn a_space_between_a_prefix_commodity_and_its_number_belongs_to_neither() {
        // The quantity starts past the space, the way a suffix commodity starts
        // past the one before it. Keeping the space in the token would color it
        // and shift the quantity a column left of where it is
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  $ 50.00\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 commodity \"$\"\n1:9-14 amount \"50.00\"\n"
        );
    }

    #[test]
    fn a_commodity_range_covers_the_symbol_in_every_shape_an_amount_scans_in() {
        // The range is anchored to the side the commodity sits on rather than
        // searched for, so it has to hold across the whole amount grammar: both
        // placements, a sign on either side of a prefix symbol, whitespace
        // between the parts, and a quoted symbol whether or not it needed the
        // quotes. Each row is the amount as written and the bytes the commodity
        // token covers inside it
        for (written, expected) in [
            ("$5", "$"),
            ("-$20", "$"),
            ("+$20", "$"),
            ("$-5", "$"),
            ("$ 5", "$"),
            (".5$", "$"),
            ("5 VTI", "VTI"),
            ("5VTI", "VTI"),
            ("-5\tVTI", "VTI"),
            ("1,000.50 USD", "USD"),
            ("\"MUTF: VFIAX\"5", "\"MUTF: VFIAX\""),
            ("-\"MUTF: VFIAX\"5", "\"MUTF: VFIAX\""),
            ("5 \"MUTF: VFIAX\"", "\"MUTF: VFIAX\""),
            // Quoted where the bare form would have scanned just as well
            ("\"USD\"5", "\"USD\""),
            ("5 \"USD\"", "\"USD\""),
        ] {
            let amount = Amount::parse(written.as_bytes())
                .unwrap_or_else(|err| panic!("{written:?} scans as an amount: {}", err.message));
            let range = commodity_range(written.as_bytes(), &amount.commodity);
            assert_eq!(
                written.get(range.clone()),
                Some(expected),
                "commodity range {range:?} of {written:?}"
            );
        }
    }

    #[test]
    fn a_quoted_prefix_commodity_token_covers_its_quotes() {
        // The quoted form on the prefix side, where the quotes sit between the
        // sign and the number rather than at the end of the amount
        assert_eq!(
            render(&doc("2020-01-02 P\n    A  -\"US $\"20\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:4-5 account \"A\"\n1:7-8 amount \"-\"\n1:8-14 commodity \"\\\"US $\\\"\"\n\
             1:14-16 amount \"20\"\n"
        );
    }

    #[test]
    fn an_auxiliary_date_is_a_date_token_of_its_own() {
        assert_eq!(
            render(&doc("2020-01-02=2020-01-05 P\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-21 date \"2020-01-05\"\n0:22-23 payee \"P\"\n"
        );
    }

    #[test]
    fn a_header_that_names_no_payee_colors_only_its_dates() {
        // ledger accepts a payee-less header, so the parse succeeds and the
        // header colors as far as it goes rather than not at all
        assert_eq!(render(&doc("2020-01-02\n")), "0:0-10 date \"2020-01-02\"\n");
        assert_eq!(
            render(&doc("2020-01-02=2020-01-05 *\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-21 date \"2020-01-05\"\n"
        );
    }

    #[test]
    fn a_virtual_posting_colors_the_account_inside_its_brackets() {
        assert_eq!(
            render(&doc("2020-01-02 P\n    [Assets:Cash]  $1\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:5-16 account \"Assets:Cash\"\n1:19-20 commodity \"$\"\n1:20-21 amount \"1\"\n"
        );
    }

    #[test]
    fn an_empty_account_name_produces_no_token() {
        // `[]` is an account the parser accepts and a span that covers nothing.
        // Encoding it would put a zero-length token in the stream
        assert_eq!(
            render(&doc("2020-01-02 P\n    []  $1\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n\
             1:8-9 commodity \"$\"\n1:9-10 amount \"1\"\n"
        );
    }

    #[test]
    fn a_comment_region_colors_every_line_it_holds() {
        // The opening line, the raw lines inside, and the closer are all
        // comment lines, so the region colors whole
        assert_eq!(
            render(&doc("comment\n2020-13-01 not parsed\nend comment\n")),
            "0:0-7 comment \"comment\"\n1:0-21 comment \"2020-13-01 not parsed\"\n\
             2:0-11 comment \"end comment\"\n"
        );
    }

    #[test]
    fn a_block_that_does_not_scan_contributes_no_tokens() {
        // The header's date is malformed, so the header has no spans to color,
        // but the postings under it are scanned on their own and still do
        assert_eq!(
            render(&doc("2020-13-01 P\n    Assets:Cash  $1\n")),
            "1:4-15 account \"Assets:Cash\"\n1:17-18 commodity \"$\"\n1:18-19 amount \"1\"\n"
        );
        // A posting that does not scan is dropped the same way, its account
        // included: the split it names is the one the scanner failed on
        assert_eq!(
            render(&doc("2020-01-02 P\n    Assets:Cash  $\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-12 payee \"P\"\n"
        );
    }

    #[test]
    fn a_directive_carries_no_tokens_yet() {
        // Neither the keyword nor the path has a kind, and the refused
        // constructs have none either
        assert_eq!(render(&doc("include transactions/2020.ledger\n")), "");
        assert_eq!(render(&doc("~ monthly\n    Budget:Food  $1\n")), "");
        assert_eq!(render(&doc("= /Food/\n    Budget:Food  $1\n")), "");
    }

    #[test]
    fn a_character_counts_utf16_code_units_not_bytes() {
        // The é in the account is two bytes and one unit, so the amount that
        // follows starts one column earlier than its byte offset. Handing bytes
        // over unmapped would shift every token after it
        assert_eq!(
            render(&doc("2020-01-02 Café\n    Assets:Café  $1\n")),
            "0:0-10 date \"2020-01-02\"\n0:11-15 payee \"Café\"\n\
             1:4-15 account \"Assets:Café\"\n1:17-18 commodity \"$\"\n1:18-19 amount \"1\"\n"
        );
    }

    #[test]
    fn an_empty_buffer_has_no_tokens() {
        assert!(tokens(&doc("")).data.is_empty());
        assert!(tokens(&doc("")).result_id.is_none());
    }
}
