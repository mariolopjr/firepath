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

# Everything CI runs
check:
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace --all-targets
  cargo build --workspace --all-targets
