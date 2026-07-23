firepath
========

Building
--------

* Requires rustup and just. The toolchain is pinned by rust-toolchain.toml (nightly, required for branch coverage) and rustup installs it on first cargo run. `ledger` on PATH enables the conformance suite
* `upstream/ledger` is a git submodule pinned to a ledger release, holding ledger's own tests for the upstream harness. `just fetch-upstream` checks it out and the tests reading it skip while it is absent, except under `CI` where they fail instead, so a workflow that forgets the checkout does not pass by proving nothing. Nothing under it is firepath's code, and the `CLAUDE.md` in it is ledger's, not firepath's instructions
* Build: `just build`
* Everything: `just check` (fmt + clippy + test + build + fixtures) must pass before any commit. It runs the upstream harness tests, which spawn firepath once per upstream case. `just check-fast` is the same without them
* Conformance against ledger's own tests: `just conformance-upstream` prints the report as JSON
* Dependency audit (enforced via CI): `just deny` (advisories, licenses, bans, sources)
* The rest: `just --list`

Code Conventions
----------------

* Edition 2024, rustfmt defaults, clippy with -D warnings
* Money is Decimal in the ledger layer and f64 only inside the projection engines. Amounts cross
  that boundary once on the way in only
* Coverage (`just coverage`, cargo-llvm-cov on nightly) instruments every crate, so test code counts unless excluded. A crate root with in-file `#[cfg(test)]` modules declares `#![cfg_attr(coverage_nightly, feature(coverage_attribute))]` once and tags each test module `#[cfg_attr(coverage_nightly, coverage(off))]`, which cascades to every test in it
* The web dashboard is read-only. The journal is written only through the LSP and the CLI
* Sum money in deterministic order
* The journal states what a transaction is
* Comment heavily. Explain why, not just what

