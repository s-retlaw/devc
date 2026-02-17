# Agent Auth Sync Notes (Codex + Claude)

## Problem Observed
Inside a devcontainer, `claude auth status` reported logged in, but interactive `claude` still showed full OAuth onboarding/login.

## Root Cause
Syncing only `~/.claude/.credentials.json` (and `settings.json`) is insufficient for Claude's interactive UX.
Claude also uses `~/.claude.json` (session/UI/account state). If that file is missing or stale, interactive startup may re-enter onboarding despite valid token files.

## Required Files To Sync
For Claude:
- `~/.claude/.credentials.json`
- `~/.claude/settings.json`
- `~/.claude.json`

For Codex:
- `~/.codex/auth.json`
- `~/.codex/config.toml`

## Validation Commands
Run inside container after sync:

```bash
claude auth status
claude -p "reply with exactly OK"
codex --version
```

Expected:
- Claude auth status shows logged in
- Claude `-p` call returns output (no 401)
- Codex binary is available

## Minimal One-Off Sync Commands
From host to running devc container (`devc`):

```bash
cat ~/.claude/.credentials.json | devc exec devc -- sh -lc 'umask 077; mkdir -p ~/.claude; cat > ~/.claude/.credentials.json'
cat ~/.claude/settings.json      | devc exec devc -- sh -lc 'umask 022; mkdir -p ~/.claude; cat > ~/.claude/settings.json'
cat ~/.claude.json               | devc exec devc -- sh -lc 'umask 077; cat > ~/.claude.json'

cat ~/.codex/auth.json           | devc exec devc -- sh -lc 'umask 077; mkdir -p ~/.codex; cat > ~/.codex/auth.json'
cat ~/.codex/config.toml         | devc exec devc -- sh -lc 'umask 077; mkdir -p ~/.codex; cat > ~/.codex/config.toml'
```

## Recommended Behavior In devc (for implementation)
On each `up` / `rebuild` (and optionally `start`):
1. Validate selected agent is enabled.
2. Validate host files exist for that agent.
3. Copy required files into container paths with strict permissions.
4. Install binary only if missing.
5. Continue on failure with warning (do not fail container lifecycle).
6. Emit per-agent summary.

## Acceptance Criteria
- Rebuild a devcontainer.
- Run `claude` interactively: no full OAuth onboarding when host is already authenticated.
- Run `claude -p "reply with exactly OK"`: succeeds.
- Run `codex --version`: succeeds.
- If files missing on host: lifecycle continues and warning is shown.
