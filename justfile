alias b := build
alias c := check
alias C := coverage
alias l := lint
alias t := test

default: check

# Compile the whole workspace
build:
  cargo build --workspace --all-targets

# Run the test suite
test:
  cargo test --workspace --all-targets

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
  ledger --pedantic -f data/fixtures/main.ledger balance >/dev/null
  echo "ledger accepts the generated fixtures"

# Audit dependencies for advisories, licenses, duplicates, and sources
deny:
  cargo deny check

# Write an lcov report to coverage/ for editor gutters
coverage:
  cargo llvm-cov --branch --workspace --lcov --output-path coverage/lcov.info

# Browse the coverage report as HTML
coverage-html:
  cargo llvm-cov --branch --workspace --html --output-dir coverage --open

# Everything CI runs
check:
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace --all-targets
  cargo build --workspace --all-targets
  cargo run --quiet --package firepath-fixtures --bin gen-fixtures
