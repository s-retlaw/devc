# devc task runner
# Install: cargo install just
# Usage: just --list

# List available recipes
default:
    @just --list

# Build debug binary
build:
    cargo build

# Build release binary
build-release:
    cargo build --release

# Run all unit tests (skips e2e tests that need a container runtime)
test:
    cargo nextest run -p devc-config -p devc-core -p devc-tui -p devc-provider

# Run e2e tests (requires Docker or Podman)
test-e2e:
    cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only

# Run all tests including e2e
test-all:
    cargo nextest run --profile e2e --workspace --run-ignored all

# Check formatting and run clippy
lint:
    cargo fmt -- --check
    cargo clippy --workspace -- -D warnings

# Auto-fix formatting
fmt:
    cargo fmt

# Check that everything compiles without building
check:
    cargo check --workspace

# Clean build artifacts
clean:
    cargo clean

# Run the TUI app
run:
    cargo run -p devc-tui

# Run the CLI
cli *ARGS:
    cargo run -p devc-cli -- {{ARGS}}

# Install devc locally
install:
    cargo install --path crates/devc-cli

# Update snapshot tests (requires cargo-insta)
snap-review:
    cargo insta review

# Run a specific test by name pattern
test-filter PATTERN:
    cargo nextest run --workspace -E 'test({{PATTERN}})'
