//! `conformance-upstream`: the command wrapper over the upstream harness

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::process::ExitCode;

// The entry point is exercised through `just conformance-upstream`
#[cfg_attr(coverage_nightly, coverage(off))]
fn main() -> ExitCode {
    firepath_fixtures::upstream::cli()
}
