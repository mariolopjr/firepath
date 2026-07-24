//! End-to-end tests that run the compiled `firepath` binary

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

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
fn help_flag_lists_the_subcommands() {
    let output = firepath().arg("--help").output().expect("run firepath");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    // Every subcommand that works is advertised under a Commands section
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("check"));
    assert!(stdout.contains("print"));
    assert!(stdout.contains("lsp"));
}

// The A28CF697 regression journal and the exact bytes `ledger print` emits for
// it, the first upstream case firepath conforms to. The input dates are dashed
// and its amount over-indented. The output is dates normalized to slashes, the
// account left-justified to the 36-column floor, and the commodity re-quoted
// because `&` is not a bare-commodity byte
const MODEL_INPUT: &[u8] = b"2010-02-05 * Flight SN2094\n    Assets:Rewards:Airmiles                        125 \"M&M\"\n    Income:Rewards\n";
const MODEL_OUTPUT: &str = "2010/02/05 * Flight SN2094\n    Assets:Rewards:Airmiles                125 \"M&M\"\n    Income:Rewards\n";

// Run `print` with the journal fed on stdin, the way the harness drives `-f -`.
// Returns the finished output after stdin is closed
fn print_stdin(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = firepath()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run firepath");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write the journal to stdin");
    child.wait_with_output().expect("firepath exited")
}

#[test]
fn print_emits_the_canonical_form_of_the_model_case() {
    // The global flags are passed in the harness's own order, before the
    // command, so this locks that invocation shape as much as the output
    let journal = temp_journal(MODEL_INPUT);
    let output = firepath()
        .args(["--args-only", "--columns=80", "-f"])
        .arg(journal.path())
        .arg("print")
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), MODEL_OUTPUT);
}

#[test]
fn print_reads_the_journal_from_stdin_with_dash_f() {
    let output = print_stdin(&["-f", "-", "print"], MODEL_INPUT);

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), MODEL_OUTPUT);
}

#[test]
fn print_reads_stdin_when_no_file_is_given() {
    // No `-f` at all falls back to standard input, so a piped journal still
    // prints. This is the arm the harness never takes but a shell pipe does
    let output = print_stdin(&["print"], MODEL_INPUT);

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), MODEL_OUTPUT);
}

#[test]
fn print_output_is_idempotent() {
    // Canonical form is a fixed point: printing already-canonical output leaves
    // it byte for byte unchanged. Proves the round trip end to end without a
    // hand-counted expectation
    let once = print_stdin(&["print"], MODEL_INPUT);
    assert_eq!(once.status.code(), Some(0));
    let twice = print_stdin(&["print"], &once.stdout);
    assert_eq!(twice.stdout, once.stdout);
}

#[test]
fn print_on_a_parse_error_writes_stderr_and_exits_one_with_no_stdout() {
    // A parse error stops the whole print the way ledger does: the error goes
    // to stderr with its location, stdout stays empty, and the exit code is the
    // one `check` uses for a file that had errors, distinct from an unreadable
    // file
    let journal = temp_journal(b"2020-13-01 Grocery\n    Expenses:Food    $5.00\n");
    let output = firepath()
        .arg("-f")
        .arg(journal.path())
        .arg("print")
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "nothing is printed on a bad parse"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(":1:1: 2020-13-01 is not a real calendar date"),
        "got {stderr:?}"
    );
}

#[test]
fn print_on_an_unreadable_file_exits_three() {
    // A missing `-f` target is a read failure, exit 3, the same split `check`
    // draws between an unreadable file and a parse failure
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.ledger");
    let output = firepath()
        .arg("-f")
        .arg(&missing)
        .arg("print")
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("cannot read"), "reports the read failure");
}

#[test]
fn print_rejects_a_ledger_flag_it_does_not_model_without_crashing() {
    // A `print` flag firepath has no meaning for yet is a clap usage error, exit
    // 2, not a panic. The out-of-scope upstream cases fail this way rather than
    // producing wrong output
    let journal = temp_journal(MODEL_INPUT);
    let output = firepath()
        .arg("-f")
        .arg(journal.path())
        .args(["print", "--decimal-comma"])
        .output()
        .expect("run firepath");

    assert_eq!(output.status.code(), Some(2));
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

// LSP framing: a `Content-Length` header, a blank line, then exactly that many
// bytes of JSON
fn lsp_frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

// Reads one framed message off `reader`, returning its JSON body. Blocks until
// the whole body has arrived, so a server that writes a short frame hangs the
// test rather than passing it on a truncated read
fn read_lsp_frame(reader: &mut impl BufRead) -> String {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "stream ended early"
        );
        let line = line.trim_end_matches("\r\n");
        // The blank line closes the headers
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.parse::<usize>().unwrap());
        }
    }

    let mut body = vec![0; length.expect("a Content-Length header")];
    reader.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

#[test]
fn lsp_serves_the_initialize_handshake_over_stdio() {
    let mut child = firepath()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run firepath");

    let mut stdin = child.stdin.take().unwrap();
    // The whole session is scripted up front: the server answers initialize,
    // then shutdown, and exits on the notification
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#,
        r#"{"jsonrpc":"2.0","method":"exit","params":null}"#,
    ] {
        stdin.write_all(lsp_frame(message).as_bytes()).unwrap();
    }
    stdin.flush().unwrap();

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let initialize = read_lsp_frame(&mut stdout);
    assert!(initialize.contains(r#""id":1"#), "{initialize}");
    assert!(
        initialize.contains(r#""name":"firepath-lsp""#),
        "{initialize}"
    );
    assert!(initialize.contains(r#""capabilities""#), "{initialize}");

    let shutdown = read_lsp_frame(&mut stdout);
    assert!(shutdown.contains(r#""id":2"#), "{shutdown}");

    // Ensure server exits cleanly: a server that answered but never let
    // go of the pipes would leave the editor with a stuck process
    let status = child.wait().expect("the server exited");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn lsp_exits_on_a_bare_exit_notification() {
    let mut child = firepath()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run firepath");

    let mut stdin = child.stdin.take().unwrap();
    // An editor that goes down without shutting the server down first
    for message in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"exit","params":null}"#,
    ] {
        stdin.write_all(lsp_frame(message).as_bytes()).unwrap();
    }
    stdin.flush().unwrap();

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    read_lsp_frame(&mut stdout);

    // stdin is deliberately still open, so nothing but the notification itself
    // can be what brought the process down
    let status = child.wait().expect("the server exited");
    // The protocol's own code for an exit that skipped the shutdown
    assert_eq!(status.code(), Some(1));
    drop(stdin);
}

#[test]
fn lsp_exits_four_when_the_handshake_breaks() {
    let mut child = firepath()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run firepath");

    // The first message has to be initialize. A request that is not one is
    // refused, and the closed pipe then ends the handshake before it finished
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            lsp_frame(r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":{}}"#)
                .as_bytes(),
        )
        .unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let output = child.wait_with_output().expect("the server exited");
    // Distinct from the codes `check` uses
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("language server failed"), "got {stderr:?}");
}
