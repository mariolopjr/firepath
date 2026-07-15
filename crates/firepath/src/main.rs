//! firepath CLI
//!
//! For now the only thing supported is `--help` and `--version`

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use clap::Parser;

/// FIRE budgeting, planning, and retirement tool driven by ledger journals
///
/// `version` reads the package version which is inherited from the workspace
/// `about` uses the first line of this comment
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {}

fn main() {
    // `parse` handles `--help` and `--version` by printing and exiting
    Cli::parse();
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
