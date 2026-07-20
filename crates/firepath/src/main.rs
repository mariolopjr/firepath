//! firepath CLI
//!
//! [`Command::Check`] parses one journal file and reports its errors. [`Command::Lsp`] serves the
//! language server over stdio. `--help` and `--version` come from clap

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod check;

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

    /// Serve the language server over stdin and stdout
    Lsp,
}

/// Exit code when the language server failed
const SERVER_FAILED: u8 = 4;

fn main() -> ExitCode {
    // `parse` handles `--help` and `--version` by printing and exiting, and
    // rejects a missing or unknown subcommand before returning here
    match Cli::parse().command {
        Command::Check { file } => check::run(&file),
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
