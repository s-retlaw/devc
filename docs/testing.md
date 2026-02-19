# Testing Roadmap

## Purpose
Make the test suite faster, more reliable across macOS/Linux/Windows, and clearer about what is expected in restricted environments (sandboxed CI, containerized dev shells, etc.).

## Current State
- `just test` runs a broad nextest lane across `devc-config`, `devc-core`, `devc-cli`, `devc-tui`, `devc-provider`.
- `just test-e2e*` lanes run ignored runtime-dependent tests.
- Recent fragility classes were:
  - non-hermetic state/config paths in tests
  - parser brittleness on runtime outputs
  - localhost bind restrictions in sandboxed environments

## Test Taxonomy
Use explicit categories for every new test:

1. `unit-hermetic`
- No network, no runtime daemon, no fixed ports, no host global paths.
- Must pass in restricted sandboxes.

2. `integration-local`
- May use localhost sockets, temp filesystem, subprocesses.
- Must tolerate permission restrictions by skipping with explicit reason.

3. `e2e-runtime`
- Requires Docker/Podman and possibly registries/network.
- Keep under ignored/e2e profile and run in dedicated lanes.

## Proposed `just` Targets
Keep existing targets, add explicit lanes for clarity:

- `test-hermetic`
  - only hermetic unit/integration tests expected to pass everywhere.
- `test-local`
  - includes localhost/socket tests.
- `test-runtime`
  - existing ignored/runtime suite.
- `test-all`
  - orchestrates the three lanes with clear section summaries.

Suggested mapping:
- `test` should become alias of `test-hermetic`.
- `test-e2e*` remain as runtime-specific lanes.

## Guard Patterns (Required for Env-Sensitive Tests)
For tests that bind localhost, require privileged ops, or depend on host capabilities:

1. Add a capability probe helper.
2. Early-return (skip) on `PermissionDenied` or unsupported capability.
3. Keep a short comment that explains why skipping is valid.

Example pattern:

```rust
fn can_bind_localhost() -> bool {
    std::net::TcpListener::bind("127.0.0.1:0").is_ok()
}

#[tokio::test]
async fn test_socket_behavior() {
    if !can_bind_localhost() {
        return; // sandbox disallows bind
    }
    // normal assertions
}
```

## Reliability Standards
1. No fixed ports in new tests
- Use ephemeral ports (`127.0.0.1:0`) and discover assigned port.

2. No host-global mutable paths
- Use per-test temp dirs for config/state.
- Avoid `$HOME` mutation in unit/integration tests.

3. Deterministic assertions
- Assert structured error categories and stable message fragments.
- Avoid timing-sensitive assumptions without bounded retries.

4. Multi-process safety tests
- Add dedicated tests for concurrent state/config updates and tombstone behavior.

## CI Recommendations
Minimum matrix:

1. OS:
- Linux
- macOS
- Windows

2. Lanes:
- Hermetic lane on all OSes
- Runtime lane on Linux (Docker + Podman where feasible)

3. Restricted environment lane:
- One job with limited permissions to detect bind/path assumptions early.

## Migration Plan
### Phase 1: Classification and hygiene
1. Tag existing tests by category (`unit-hermetic`, `integration-local`, `e2e-runtime`).
2. Add guard helpers where localhost bind may fail.
3. Remove remaining host-global path dependencies from tests.

### Phase 2: Command and CI alignment
1. Add `test-hermetic`, `test-local`, `test-runtime` targets to `justfile`.
2. Make `just test` call `test-hermetic`.
3. Update CI workflows to run category-appropriate lanes.

### Phase 3: Concurrency and regression depth
1. Expand concurrent write/read tests for state/config.
2. Add parser fuzz-ish fixtures for malformed/mixed runtime outputs.
3. Track flaky tests and enforce zero flaky budget.

## Acceptance Criteria
- `just test` passes in restricted sandboxed environments without manual env setup.
- Runtime-only failures occur only in runtime lanes, not default lane.
- New env-sensitive tests include explicit capability guards.
- CI consistently exercises hermetic tests on all supported OSes.
