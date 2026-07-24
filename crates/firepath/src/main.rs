//! firepath CLI
//!
//! [`Command::Check`] parses one journal file and reports its errors.
//! [`Command::Print`] reads a journal and writes it back in canonical form.
//! [`Command::Lsp`] serves the language server over stdio.
//! `--help` and `--version` come from clap.
//!
//! `-f`, `--columns`, and `--args-only` are global options because ledger accepts
//! them before the command word, and the conformance harness invokes firepath the
//! same way: `firepath --args-only --columns=80 -f <file> print`

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod check;
mod print;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// FIRE budgeting, planning, and retirement tool driven by ledger journals
///
/// `version` reads the package version which is inherited from the workspace
/// `about` uses the first line of this comment
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Read the journal from this file, or `-` for standard input
    ///
    /// A global option so it can precede the command the way ledger's `-f` does.
    /// `check` names its file positionally and ignores this. The arg id is
    /// `input` so it does not collide with `check`'s positional `file`
    #[arg(
        short = 'f',
        long = "file",
        global = true,
        id = "input",
        value_name = "FILE"
    )]
    input: Option<PathBuf>,

    /// Accepted for command-line compatibility with ledger and otherwise ignored
    ///
    /// ledger reads no init file or environment once this is set. firepath reads
    /// neither to begin with, so it accepts the flag and does nothing with it
    #[arg(long = "args-only", global = true)]
    args_only: bool,

    /// Accepted for compatibility and ignored by `print`
    ///
    /// The canonical writer's column widths are fixed by the format, so the
    /// terminal width does not change what `print` emits. The harness always
    /// passes `--columns=80`, so the option has to parse
    #[arg(long = "columns", global = true, value_name = "N")]
    columns: Option<u16>,

    #[command(subcommand)]
    command: Command,
}

/// The subcommands firepath dispatches to
#[derive(Debug, Subcommand)]
enum Command {
    /// Parse a single journal file and report any errors
    ///
    /// Only the named file is read. An `include` directive is checked for being
    /// well-formed but is not followed
    Check {
        /// The journal file to parse
        file: PathBuf,
    },

    /// Read a journal and write it back in canonical `print` form
    ///
    /// The journal comes from the global `-f` option, or standard input when
    /// that is `-` or absent. This is firepath's first conformance point: the
    /// output matches `ledger print` byte for byte
    Print,

    /// Serve the language server over stdin and stdout
    Lsp,
}

/// Exit code when the language server failed
const SERVER_FAILED: u8 = 4;

fn main() -> ExitCode {
    // `parse` handles `--help` and `--version` by printing and exiting, and
    // rejects a missing or unknown subcommand before returning here. `args_only`
    // and `columns` are accepted for ledger compatibility and read by neither
    // command, so they are dropped here
    let Cli { input, command, .. } = Cli::parse();
    match command {
        Command::Check { file } => check::run(&file),
        Command::Print => print::run(input.as_deref()),
        Command::Lsp => match firepath_lsp::run_stdio() {
            Ok(exit) => ExitCode::from(exit.code()),
            // stderr, because stdout belongs to the protocol even now that the
            // session is over
            Err(err) => {
                eprintln!("firepath: language server failed: {err}");
                ExitCode::from(SERVER_FAILED)
            }
        },
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    // Catches structural mistakes in the derived command (overlapping flags,
    // bad defaults)
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
