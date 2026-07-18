//! `gen-fixtures`: the command wrapper over the `firepath-fixtures` library

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::process::ExitCode;

// The entry point is exercised through `just check` in CI
#[cfg_attr(coverage_nightly, coverage(off))]
fn main() -> ExitCode {
    firepath_fixtures::cli()
}
