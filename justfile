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
test-e2e: _fix-docker-creds
    cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only

# Run e2e tests against Docker
test-e2e-docker: _fix-docker-creds
    DEVC_TEST_PROVIDER=docker cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only

# Run e2e tests against Podman
test-e2e-podman: _fix-docker-creds
    DEVC_TEST_PROVIDER=podman cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only

# Run e2e tests via simulated toolbox (Podman through flatpak-spawn shim)
test-e2e-toolbox: _toolbox-setup
    #!/usr/bin/env bash
    set -euo pipefail
    # Start podman API service for the shim to connect to
    podman system service --time=0 "unix:///tmp/devc-podman.sock" &
    PODMAN_PID=$!
    trap "kill $PODMAN_PID 2>/dev/null || true" EXIT
    sleep 1

    podman run --rm \
        -v "{{justfile_directory()}}:/workspace:Z" \
        -v "/tmp/devc-podman.sock:/run/podman/podman.sock" \
        -v "${CARGO_HOME:-$HOME/.cargo}:/cargo:ro" \
        -v "${RUSTUP_HOME:-$HOME/.rustup}:/rustup:ro" \
        -e DEVC_TEST_PROVIDER=toolbox \
        -e CARGO_HOME=/cargo \
        -e RUSTUP_HOME=/rustup \
        -e PATH="/cargo/bin:/rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:/usr/bin:/bin" \
        -w /workspace \
        devc-toolbox-env \
        cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only

# Run all tests including e2e against all providers, with combined summary
test-all: _fix-docker-creds
    #!/usr/bin/env bash
    source "{{justfile_directory()}}/.devcontainer/test-runner.sh"
    run_section "Unit Tests" \
        cargo nextest run -p devc-config -p devc-core -p devc-tui -p devc-provider
    run_section "E2E: Docker" \
        env DEVC_TEST_PROVIDER=docker cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only
    run_section "E2E: Podman" \
        env DEVC_TEST_PROVIDER=podman cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only
    # TODO: add E2E: Toolbox section once toolbox sim container is validated
    print_summary

# Quick check: unit tests + Docker e2e
test-quick: _fix-docker-creds
    #!/usr/bin/env bash
    source "{{justfile_directory()}}/.devcontainer/test-runner.sh"
    run_section "Unit Tests" \
        cargo nextest run -p devc-config -p devc-core -p devc-tui -p devc-provider
    run_section "E2E: Docker" \
        env DEVC_TEST_PROVIDER=docker cargo nextest run --profile e2e -p devc-core -p devc-tui -p devc-provider --run-ignored ignored-only
    print_summary

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

# Install git hooks (auto-formats on commit)
setup-hooks:
    git config core.hooksPath .githooks
    @echo "Git hooks installed (.githooks/pre-commit)"

# --- Internal targets ---

# Strip devc's credential helper from Docker config so e2e tests can pull public images.
# devc re-injects credsStore:"devc" on every shell/exec, so this must run right before tests.
_fix-docker-creds:
    #!/usr/bin/env bash
    if [ -f "$HOME/.docker/config.json" ] && command -v jq &>/dev/null; then
        if jq -e '.credsStore' "$HOME/.docker/config.json" &>/dev/null; then
            tmp=$(jq 'del(.credsStore)' "$HOME/.docker/config.json")
            echo "$tmp" > "$HOME/.docker/config.json"
        fi
    fi

# Build the simulated toolbox container image
_toolbox-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! podman image exists devc-toolbox-env 2>/dev/null; then
        podman build -t devc-toolbox-env tests/toolbox-env/
    fi
