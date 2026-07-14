firepath
========

Building
--------

* Requires rustup and just. The toolchain is pinned by rust-toolchain.toml (nightly, required for branch coverage) and rustup installs it on first cargo run. `ledger` on PATH enables the conformance suite
* Build: `just build`
* Everything: `just check` (fmt + clippy + test + build) must pass before any commit
* Dependency audit (enforced via CI): `just deny` (advisories, licenses, bans, sources)
* The rest: `just --list`

Code Conventions
----------------

* Edition 2024, rustfmt defaults, clippy with -D warnings
* Money is Decimal in the ledger layer and f64 only inside the projection engines. Amounts cross
  that boundary once on the way in only
* The web dashboard is read-only. The journal is written only through the LSP and the CLI
* Sum money in deterministic order
* The journal states what a transaction is
* Comment heavily. Explain why, not just what

