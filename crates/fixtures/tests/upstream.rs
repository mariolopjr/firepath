//! The upstream harness run against the tests it reads
//!
//! Every test here is `#[ignore]`d, because a run spawns firepath once per
//! upstream case and takes seconds rather than milliseconds. `just check` and
//! `just test` pass `--include-ignored` and run them; `just check-fast` is the
//! one that leaves them out, and a run that does reports them as ignored rather
//! than saying nothing.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a harness test reads clearest with expect on the invariants it assumes"
)]

use std::sync::OnceLock;

use firepath_fixtures::upstream::{Report, firepath_binary, measure, tests_dir};

/// Two runs over the checked-out tests, or `None` when they are not checked out
///
/// A run spawns firepath once per case, so the whole suite shares one pair
/// rather than paying for it per test. Two, because proving a run repeatable
/// takes two of them and every other assertion here can read the first
fn runs(test: &str) -> Option<&'static [Report; 2]> {
    static RUNS: OnceLock<Option<[Report; 2]>> = OnceLock::new();

    let cached = RUNS.get_or_init(|| {
        let dir = tests_dir();
        if !dir.join("baseline").is_dir() {
            assert!(
                std::env::var_os("CI").is_none(),
                "{} is not checked out: CI must run `just fetch-upstream` before the tests",
                dir.display()
            );
            eprintln!("the upstream tests are not checked out, run `just fetch-upstream`");
            return None;
        }
        let Some(firepath) = firepath_binary() else {
            assert!(
                std::env::var_os("CI").is_none(),
                "no firepath binary next to this one: CI builds the whole workspace"
            );
            eprintln!("no firepath binary next to this one, run `just build`");
            return None;
        };
        let once = || measure(&dir, &firepath).expect("the checked-out tests are readable");
        Some([once(), once()])
    });

    if cached.is_none() {
        eprintln!("SKIP {test}");
    }
    cached.as_ref()
}

/// The first of the two runs, which every assertion but determinism reads
fn measured(test: &str) -> Option<&'static Report> {
    runs(test).map(|both| &both[0])
}

#[test]
#[ignore = "spawns firepath once per upstream case, run by `just check`"]
fn the_harness_runs_every_case_upstream_runs() {
    let Some(report) = measured("the_harness_runs_every_case_upstream_runs") else {
        return;
    };

    // ledger v3.4.1: 445 files declaring 785 cases, less the ten `_py.test`
    // files, which upstream registers only when it was built with Boost.Python
    // and this harness leaves out for good. 775 is what an upstream build
    // without Python runs, and it is the whole denominator
    assert_eq!(report.cases, 775);

    // Every case is accounted for by exactly one category, and every file that
    // declared none is named
    let categorized: usize = report.failures.values().sum();
    assert_eq!(categorized.saturating_add(report.passing), report.cases);
    assert!(
        report.malformed.is_empty(),
        "the checked-out tests all declare a case: {:?}",
        report.malformed
    );
}

#[test]
#[ignore = "spawns firepath once per upstream case, run by `just check`"]
fn the_grammar_breakdown_is_reported_without_being_scored() {
    let Some(report) = measured("the_grammar_breakdown_is_reported_without_being_scored") else {
        return;
    };

    // How many parse errors the grammar firepath supports today raises over the
    // upstream journals. A ratchet, not a pin: every grammar wave lowers it, and
    // the number here comes down with it. Raising it means firepath got stricter
    // on purpose, which is a decision to document rather than a chore
    let errors: usize = report.grammar.values().sum();
    assert!(
        errors <= 1298,
        "grammar coverage regressed: {errors} parse errors over the upstream journals"
    );
}

#[test]
#[ignore = "spawns firepath once per upstream case, run by `just check`"]
fn two_runs_over_the_same_tests_report_the_same_thing() {
    let Some([first, second]) = runs("two_runs_over_the_same_tests_report_the_same_thing") else {
        return;
    };

    // Same counts and same categories in the same order, so the badge a CI run
    // writes does not depend on the order the filesystem handed the files back
    assert_eq!(first, second);
    assert_eq!(
        first.to_json().expect("the report serializes"),
        second.to_json().expect("the report serializes")
    );
}
