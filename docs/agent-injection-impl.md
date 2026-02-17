# Agent Injection Implementation Note

## Data flow
- `devc-config` adds per-agent blocks (`codex`, `claude`, `cursor`, `gemini`) with defaults.
- `devc-core::agents` resolves enabled agents from preset defaults plus config overrides.
- Claude preset includes an extra default sync target for `~/.claude.json` in addition to `~/.claude`.
- Host validation runs first: check host config path readability and required/allowlisted env key presence.
- Injection for each enabled agent then runs in order:
  1. resolve target container path
  2. ensure target parent directory exists
  3. copy host config/auth material into container
  4. probe binary in container
  5. install only if probe fails

## Warning semantics
- Agent sync is best-effort by default.
- Any per-agent failure is converted to a warning and does not fail `up`, `start`, or `rebuild`.
- Lifecycle summary logs warning count and points users to `devc agents doctor`.
- `devc agents sync` returns per-agent status and warnings for immediate inspection.

## Security boundaries
- Env forwarding is allowlist-only (`env_forward` + preset-required keys).
- Secret values are never logged; warnings include key names only.
- Host material remains source-of-truth in v1 (re-copy model), with no persistent volume dependency.
- Existing Docker/Git credential injection behavior remains unchanged and isolated in `credentials/*`.
