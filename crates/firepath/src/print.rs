//! The `print` subcommand: read one journal and write it back in canonical form
//!
//! This is firepath's first conformance point against ledger's own tests. The
//! upstream harness runs `firepath --args-only --columns=80 -f <file> print` and
//! compares the output byte for byte against what `ledger print` produced, so the
//! output has to match the canonical writer's shape exactly.
//!
//! The journal comes from the global `-f` option: a path, or standard input when
//! it is `-` or the option was omitted. A parse error aborts the print the way
//! ledger's does, reported to stderr with a non-zero exit and nothing on stdout,
//! so a half-formatted journal is never emitted as if it were whole.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use firepath_ledger::{FileId, LineIndex, parse, write_transactions};

/// Exit code when the journal had parse errors
const FOUND_ERRORS: u8 = 1;

/// Exit code when the input could not be read
const UNREADABLE: u8 = 3;

/// Read the journal named by `file`, parse it, and write every transaction back
/// in `print` form, returning the process exit code
///
/// `file` is the global `-f` option: `Some(path)` reads that file, and either
/// `None` or `Some("-")` reads standard input, the way ledger's `-f -` does. A
/// clean parse writes the reformatted journal to stdout and exits zero. Any parse
/// error is reported to stderr as `label:line:col: message` and exits non-zero
/// with nothing on stdout, matching ledger aborting the whole read on a bad
/// entry. A file that cannot be read is a distinct exit code from a parse failure
pub(crate) fn run(file: Option<&Path>) -> ExitCode {
    // The label is what a read failure or a parse error is reported against: the
    // file path, or "-" for standard input. Computed once, before the read, so
    // the read-failure arm can name what it could not open
    let label = label(file);

    let source = match read_input(file) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("firepath: cannot read {label}: {err}");
            return ExitCode::from(UNREADABLE);
        }
    };

    let parsed = parse(FileId::new(0), &source);
    if !parsed.errors.is_empty() {
        // A parse error aborts the print, so nothing reaches stdout. The index is
        // built only now that there is a location to resolve
        let index = LineIndex::new(&source);
        let mut stderr = io::stderr().lock();
        for error in &parsed.errors {
            // A write failure here has nowhere left to be reported
            let _ = writeln!(stderr, "{}", error.render(&label, &index));
        }
        return ExitCode::from(FOUND_ERRORS);
    }

    // Locked once so the whole journal is one contiguous write rather than a
    // reacquire per line. A write failure has nowhere to be reported and only
    // trips on a downstream that closed the pipe early, which the harness and a
    // real terminal never do
    let mut stdout = io::stdout().lock();
    let _ = write_transactions(&mut stdout, &source, &parsed.items);
    ExitCode::SUCCESS
}

/// The bytes of the input: the file at `path`, or standard input when `path` is
/// `None` or `-`
fn read_input(file: Option<&Path>) -> io::Result<Vec<u8>> {
    match file {
        Some(path) if !is_stdin(path) => fs::read(path),
        _ => {
            let mut buf = Vec::new();
            io::stdin().lock().read_to_end(&mut buf)?;
            Ok(buf)
        }
    }
}

/// How the input is named in errors: the path, or `-` for standard input
///
/// Rendered lossily for a path that is not valid UTF-8, the same as `check`, so
/// the label always prints even when it is not a path the caller could feed back
fn label(file: Option<&Path>) -> Cow<'static, str> {
    match file {
        Some(path) if !is_stdin(path) => Cow::Owned(path.to_string_lossy().into_owned()),
        _ => Cow::Borrowed("-"),
    }
}

/// Whether this `-f` argument means standard input, which ledger spells `-`
fn is_stdin(path: &Path) -> bool {
    path.as_os_str() == "-"
}
