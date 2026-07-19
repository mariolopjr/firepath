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

/// Exit code when the file was read but is not valid UTF-8
const NOT_UTF8: u8 = 4;

/// Parse the file at `path`, printing each error and returning the process exit
/// code
///
/// No output if no errors were encountered. Every error goes to stderr as
/// `path:line:col: message`, using the path as given on the command line. Note
/// the path prints through [`Path::display`], so one that is not valid UTF-8
/// renders lossily and is not a path the caller can feed back in. Reading and
/// decoding fail with their own exit codes, each distinct from a parse failure
pub(crate) fn run(path: &Path) -> ExitCode {
    // Read bytes rather than text so a decode failure is reported as itself
    // instead of arriving disguised as an I/O error
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("firepath: cannot read {}: {err}", path.display());
            return ExitCode::from(UNREADABLE);
        }
    };
    let source = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            // The offset locates the first bad byte, which is what a caller
            // needs to fix the encoding
            eprintln!(
                "firepath: {} is not valid UTF-8: byte {} is not part of a valid sequence",
                path.display(),
                err.utf8_error().valid_up_to()
            );
            return ExitCode::from(NOT_UTF8);
        }
    };

    // A single file, so it needs only the one handle
    let errors = parse(FileId::new(0), &source);
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
