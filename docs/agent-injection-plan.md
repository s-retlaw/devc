# devc Agent Injection (Host Re-copy Model)

## Summary
Implement an opt-in agent injection framework in `devc` that supports `codex`, `claude`, `cursor`, and `gemini`, with behavior aligned to existing credential injection patterns:
- Agents are off by default.
- Users enable selected agents in config.
- On container lifecycle events, devc validates host prerequisites, re-copies agent config/auth into container, and installs agent CLI only if missing.
- Injection failures do not fail lifecycle operations; they emit warnings and continue.

## Finalized Defaults
1. Failure behavior: continue with warning.
2. Install behavior: install-if-missing.
3. Source of truth: host config/auth material; re-copy on lifecycle events.
4. No persistent volumes in v1.

## Public Interfaces / Config Changes

### Global config additions (`crates/devc-config/src/global.rs`)
Add:
- `[agents]`
  - `enabled = false` (global gate)
  - `on_start = true` (run injection on `start`/`up` paths)
  - `on_rebuild = true`
- `[agents.codex]`, `[agents.claude]`, `[agents.cursor]`, `[agents.gemini]`
  - `enabled = false`
  - `install = true`
  - `host_config_path = "<optional override>"`
  - `container_config_path = "<optional override>"`
  - `env_forward = []` (explicit allowlist)
  - `install_command = "<optional override>"`

Preset defaults are baked in code (path/env/install probe per agent), user-overridable via config.

### CLI additions (`crates/devc-cli/src/commands/manage.rs` + wiring)
Add:
- `devc agents doctor [container]`
  - Shows enabled agents, host validation status, and planned actions.
- `devc agents sync [container]`
  - Forces injection now for running container (useful after host config changes).

## Architecture & Code Structure

### New core module
Create:
- `crates/devc-core/src/agents/mod.rs`
- `crates/devc-core/src/agents/host.rs` (host checks + path/env resolution)
- `crates/devc-core/src/agents/inject.rs` (copy/install execution)
- `crates/devc-core/src/agents/presets.rs` (agent definitions)

Expose from `crates/devc-core/src/lib.rs`.

### Core types
Define:
- `AgentKind` enum: `Codex | Claude | Cursor | Gemini`
- `AgentPreset`:
  - default host path
  - default container path
  - required env keys
  - binary probe command
  - default install command
- `AgentSyncResult`:
  - `agent`
  - `validated`
  - `copied`
  - `installed`
  - `warnings: Vec<String>`

## Lifecycle Integration

### Hook points in manager
Integrate in `crates/devc-core/src/manager/mod.rs`:
1. After container is running in `up_with_progress`.
2. After `start` successfully transitions/runs post-start.
3. After `rebuild` path completes and container is running.

Call:
- `setup_agents_for_container(provider, container_id, container_state, global_config)`

### Execution flow per enabled agent
1. Resolve effective config (preset + overrides).
2. Validate host prerequisites:
   - host config path exists/readable if required
   - required env vars present
3. Copy host files into container target path.
4. Forward allowlisted env vars for install/runtime setup.
5. Probe binary in container.
6. If missing and install enabled, run install command.
7. Record warning on any failure; continue to next agent.

## Security & Safety Rules
1. Env forwarding is allowlist-only.
2. Never log secret values; only key names and redacted paths.
3. Reuse shell escaping hardening patterns from `credentials/inject.rs`.
4. Ensure copied files are owned/readable by target container user where possible.
5. No destructive behavior on existing agent config in container unless explicit overwrite policy says so (default overwrite target files copied by devc).

## UX / Logging
1. During lifecycle:
   - `Injecting agents: codex, cursor`
   - Per-agent results: `ok`, `skipped`, `warning: <reason>`
2. End summary:
   - `Agent injection completed with N warning(s). Run 'devc agents doctor' for details.`

## Test Plan

### Unit tests
1. Config defaults and serde for all new `[agents.*]` fields.
2. Preset resolution and override precedence.
3. Host validation (missing path/env).
4. Install-if-missing probe logic.
5. Warning aggregation and non-fatal behavior.

### Integration tests (mock provider, non-runtime)
1. Enabled agents trigger copy/install calls in expected order.
2. Disabled agents produce no provider calls.
3. Missing host prerequisite yields warning, not error.
4. Binary present skips install.
5. `agents sync` returns detailed result set.

### E2E tests (ignored/runtime)
1. Enable one agent in temp config and verify:
   - config copied into container
   - binary installed if absent
2. Rebuild reruns copy; no manual re-auth.
3. Missing host config yields warning while container lifecycle still succeeds.

## justfile / Test Lanes
1. Add `-p devc-cli` to unit lane in `test` and `test-all`.
2. Add `test-agents` lane for fast mock-provider agent tests.
3. Keep runtime agent e2e under existing `--profile e2e --run-ignored ignored-only`.

## Assumptions
1. Host has necessary agent config/auth material when lifecycle runs.
2. Container has network/package manager access for installer commands (if needed).
3. Agent binary names and install commands are stable enough for preset defaults; overrides handle drift.
4. Best-effort warning behavior is acceptable for teams preferring uninterrupted `up/rebuild`.
