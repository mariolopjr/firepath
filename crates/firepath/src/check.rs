//! The `check` subcommand: parse one journal file and report its errors
//!
//! It reads a single file, parses it, and prints every error as
//! `file:line:col: message`, the location resolved through the parser's
//! own line index so the CLI and the editor agree on where an error exists.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use firepath_ledger::{FileId, LineIndex, parse};

/// Exit code when the file had parse errors
const FOUND_ERRORS: u8 = 1;

/// Exit code when the file could not be read
const UNREADABLE: u8 = 3;

/// Parse the file at `path`, printing each error and returning the process exit
/// code
///
/// No output if no errors were encountered. Every error goes to stderr as
/// `path:line:col: message`, using the path as given on the command line. Note
/// the path prints through [`Path::display`], so one that is not valid UTF-8
/// renders lossily and is not a path the caller can feed back in. A file that
/// cannot be read is separated from a parse failure by its exit code
pub(crate) fn run(path: &Path) -> ExitCode {
    let source = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("firepath: cannot read {}: {err}", path.display());
            return ExitCode::from(UNREADABLE);
        }
    };

    // A single file, so it needs only the one handle. Only the errors are read
    // here: `check` reports problems and says nothing about a clean file, so the
    // transactions the parse also returns go unused
    let errors = parse(FileId::new(0), &source).errors;
    if errors.is_empty() {
        return ExitCode::SUCCESS;
    }

    // The index is built only when there is something to locate
    let index = LineIndex::new(&source);
    // Locked once rather than per line: `eprintln!` reacquires and flushes on
    // every call, which a file with thousands of errors pays for each one
    let mut stderr = io::stderr().lock();
    for error in &errors {
        // A write failure here has nowhere left to be reported
        let _ = writeln!(stderr, "{}", error.render(path.display(), &index));
    }
    ExitCode::from(FOUND_ERRORS)
}
