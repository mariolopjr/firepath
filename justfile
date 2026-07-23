alias b := build
alias c := check
alias C := coverage
alias l := lint
alias t := test

default: check

# Compile the whole workspace
build:
  cargo build --workspace --all-targets

# Run the test suite, the ignored upstream harness tests included
test:
  cargo test --workspace --all-targets -- --include-ignored

# Format all sources in place
fmt:
  cargo fmt --all

# Clippy across the workspace making warnings block as errors
lint:
  cargo clippy --workspace --all-targets -- -D warnings

# Generate the ledger fixtures and verify them against the pinned hashes
# Pass --pin to record new hashes after an output change
gen-fixtures *args:
  cargo run --quiet --package firepath-fixtures --bin gen-fixtures -- {{ args }}

# Test if the `ledger` binary accepts the generated fixtures
# Skips if ledger is not installed
accept-ledger: gen-fixtures
  #!/usr/bin/env bash
  set -euo pipefail
  if ! command -v ledger >/dev/null 2>&1; then
    echo "ledger not installed, skipping acceptance test"
    exit 0
  fi
  # Plain balance, not --pedantic: pedantic errors on undeclared accounts and
  # commodities, and the account and commodity directives are not supported yet
  ledger -f data/fixtures/main.ledger balance >/dev/null
  echo "ledger accepts the generated fixtures"

# Checks out upstream ledger's tests, shallowly cloning as we only need the tests
fetch-upstream:
  git submodule update --init --depth 1 upstream/ledger

# Measure firepath against ledger's own tests, with the conformance values outputted
# to JSON
# Needs the submodule, so run `just fetch-upstream` first
# Depends on build: the harness runs the firepath binary next to its own, and
# `cargo run` on this bin alone would not produce one
conformance-upstream: build
  cargo run --quiet --package firepath-fixtures --bin conformance-upstream

# Audit dependencies for advisories, licenses, duplicates, and sources
deny:
  cargo deny check

# Write an JSON report to coverage/ for editor gutters with
# --include-ignored or the upstream harness runner counts as dead code
coverage:
  cargo llvm-cov --branch --workspace --json --output-path coverage/coverage.json -- --include-ignored

# Browse the coverage report as HTML
coverage-html:
  cargo llvm-cov --branch --workspace --html --output-dir coverage --open -- --include-ignored

# Everything CI runs
check:
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace --all-targets -- --include-ignored
  cargo build --workspace --all-targets
  cargo run --quiet --package firepath-fixtures --bin gen-fixtures

# `check` without the upstream harness tests
check-fast:
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace --all-targets
  cargo build --workspace --all-targets
  cargo run --quiet --package firepath-fixtures --bin gen-fixtures
