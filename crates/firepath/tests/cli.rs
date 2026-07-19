//! End-to-end tests that run the compiled `firepath` binary

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::process::Command;

use tempfile::{Builder, NamedTempFile, TempDir};

// Cargo exposes the built binary's path to integration tests through this env
// var
fn firepath() -> Command {
    Command::new(env!("CARGO_BIN_EXE_firepath"))
}

// A journal in a fresh temp file, created with a random name and O_EXCL so a
// pre-planted symlink at a guessable path cannot redirect the write. The
// handle deletes the file when it drops, including when a test panics, so the
// caller holds it for as long as the path is needed
fn temp_journal(contents: &[u8]) -> NamedTempFile {
    let mut file = Builder::new().suffix(".ledger").tempfile().unwrap();
    file.write_all(contents).unwrap();
    file.flush().unwrap();
    file
}

#[test]
fn version_flag_prints_package_version() {
    let output = firepath().arg("--version").output().expect("run firepath");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        concat!("firepath ", env!("CARGO_PKG_VERSION")),
    );
}

#[test]
fn help_flag_lists_the_check_subcommand() {
    let output = firepath().arg("--help").output().expect("run firepath");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    // The one subcommand so far is advertised under a Commands section
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("check"));
}

#[test]
fn check_help_says_include_is_not_followed() {
    let output = firepath()
        .args(["check", "--help"])
        .output()
        .expect("run firepath");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    // The single-file scope is the trap a caller hits without it
    assert!(stdout.contains("not followed"));
}

#[test]
fn check_on_a_clean_file_exits_zero_silently() {
    let journal =
        temp_journal(b"2020-01-02 * Grocery\n    Expenses:Food    $50.00\n    Assets:Checking\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "a clean file prints nothing");
    assert!(output.stderr.is_empty(), "a clean file prints nothing");
}

#[test]
fn check_on_a_seeded_error_prints_location_and_exits_one() {
    // A bad month in the header is one error at the very first byte, line 1
    // column 1
    let journal =
        temp_journal(b"2020-13-01 Grocery\n    Expenses:Food    $50.00\n    Assets:Checking\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        format!(
            "{}:1:1: 2020-13-01 is not a real calendar date",
            journal.path().display()
        )
    );
}

#[test]
fn every_error_in_a_file_is_reported_on_its_own_line() {
    // Two errors on different lines, so the loop over them is exercised and
    // each location is resolved independently
    let journal = temp_journal(b"2020-13-01 Grocery\n    Expenses:Food    $\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "one line per error, got {stderr:?}");
    // The header error sits at line 1, the posting error on line 2
    assert!(lines.iter().any(|l| l.contains(":1:1: 2020-13-01")));
    assert!(lines.iter().any(|l| l.contains(":2:")));
}

#[test]
fn check_on_an_unreadable_file_exits_three() {
    // A path inside a fresh temp dir that was never created, so the read fails
    // before any parse
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.ledger");

    let output = firepath()
        .arg("check")
        .arg(&missing)
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("cannot read"), "reports the read failure");
}

#[test]
fn a_usage_error_exits_two_and_is_distinct_from_an_unreadable_file() {
    // Guards the split: clap's usage code must not collide with the codes
    // `check` returns for its own failures
    let output = firepath().arg("foobar").output().expect("run firepath");

    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_non_utf8_journal_parses_like_any_other() {
    // A lone 0xe9 is a Latin-1 e-acute and not valid UTF-8
    let journal =
        temp_journal(b"2020-01-02 * Caf\xe9\n    Expenses:Food    $50.00\n    Assets:Checking\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
}

#[test]
fn a_non_utf8_commodity_symbol_parses() {
    // The one place the parser stores text rather than a span. A Latin-1 symbol
    // has to survive being scanned into a commodity
    let journal = temp_journal(b"2020-01-02 * Coffee\n    Expenses:Food    3 caf\xe9\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
}

#[test]
fn an_error_in_a_non_utf8_journal_still_reports_its_location() {
    // The high byte sits on line 1, the bad date on line 2. The column count is
    // bytes, so the two-byte payee does not shift the reported line
    let journal = temp_journal(b"2020-01-02 * Caf\xe9\n2020-13-01 Grocery\n");
    let output = firepath()
        .arg("check")
        .arg(journal.path())
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(":2:1: 2020-13-01 is not a real calendar date"),
        "got {stderr:?}"
    );
}
