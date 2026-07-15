//! End-to-end tests that run the compiled `firepath` binary

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

// Cargo exposes the built binary's path to integration tests through this env
// var
fn firepath() -> Command {
    Command::new(env!("CARGO_BIN_EXE_firepath"))
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
fn help_flag_lists_no_subcommands() {
    let output = firepath().arg("--help").output().expect("run firepath");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    // The skeleton ships no subcommands, so help must not advertise a Commands
    // section
    assert!(!stdout.contains("Commands:"));
}
