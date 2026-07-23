//! Acceptance test: the parser keeps up with the fixtures generator
//!
//! Generates the fixtures in memory and parses every file with the real parser,
//! asserting zero errors. If a future emitter adds a construct the parser does
//! not handle, this fails, so the emitter can never outrun the parser.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "an acceptance test reads clearest with expect on the invariants it assumes"
)]

use firepath_fixtures::{Manifest, generate};
use firepath_ledger::{FileId, parse};

#[test]
fn every_generated_file_parses_without_error() {
    let files = generate(&Manifest::default()).expect("the fixture generates");
    assert!(!files.is_empty(), "the generator produced no files");

    // Every transaction the fixture emits has to come back as an item, not only
    // parse without error, so a regression that drops retained transactions
    // fails here rather than passing on a clean error list
    let mut transactions = 0;
    for (index, (name, body)) in files.iter().enumerate() {
        let file = FileId::new(u32::try_from(index).expect("few enough files for a u32 id"));
        let parsed = parse(file, body.as_bytes());
        let messages: Vec<&String> = parsed.errors.iter().map(|e| &e.message).collect();
        assert!(
            parsed.errors.is_empty(),
            "{name} did not parse cleanly: {messages:?}"
        );
        // Every kept transaction holds at least one posting: the emitter never
        // writes a bare header, so a transaction that came back empty would mean
        // its postings were dropped on the way through the parse
        for transaction in &parsed.items {
            assert!(
                !transaction.postings.is_empty(),
                "{name} yielded a transaction with no postings"
            );
        }
        transactions += parsed.items.len();
    }
    assert!(
        transactions > 0,
        "the corpus emitted transactions but the parse kept none"
    );
}

#[test]
fn a_bad_emitted_line_fails() {
    // Prove the acceptance test actually fails: splice an unsupported directive onto the
    // generated fixture and the parse must report it rather than pass silently
    let files = generate(&Manifest::default()).expect("the fixture generates");
    let mut body = files
        .get("main.ledger")
        .expect("main.ledger is generated")
        .clone();
    body.push_str("account Assets:Cash\n");

    let errors = parse(FileId::new(0), body.as_bytes()).errors;
    assert!(
        !errors.is_empty(),
        "a bad line slipped through the acceptance test silently"
    );
}
