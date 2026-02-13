# Credential & Port Forwarding Research for devc

## Problem Statement

When running `devc` inside a dev container on Mac with "manual Docker-in-Docker", users get authentication errors when trying to build/pull images. This works in VS Code and DevPod but fails in `devc`.

### Root Cause

VS Code injects a credential helper (`docker-credential-dev-containers-<UUID>`) that:
1. Communicates with the host via Unix socket (path in `REMOTE_CONTAINERS_IPC` env var)
2. Proxies credential requests to the host's Docker credential store (macOS Keychain)

When running outside VS Code's terminal, `REMOTE_CONTAINERS_IPC` is not set, causing:
```
error getting credentials - err: exit status 255
```

---

## How Major Players Implement This

### VS Code Dev Containers

**Port Forwarding:**
- Reads `/proc/net/tcp` to detect listening ports (state `0x0A` = LISTEN)
- Three modes: `process` (scan /proc), `output` (parse terminal), `hybrid`
- Uses SSH port forwarding tunnels

**Credential Forwarding:**
- Injects `docker-credential-dev-containers-<UUID>` binary
- Sets `REMOTE_CONTAINERS_IPC` env var pointing to Unix socket
- VS Code process listens on socket, proxies to host keychain
- Modifies container's `~/.docker/config.json` with `"credsStore": "dev-containers-<UUID>"`

**Limitation**: Only works in VS Code terminal (requires IPC socket)

### DevPod

**Architecture:**
- Client-Agent model: deploys agent binary into containers
- Tunnel via SSH over STDIO

**Credential Forwarding:**
- Config flags: `injectGitCredentials`, `injectDockerCredentials`
- Git SSH: SSH agent forwarding
- Docker: credential helper injection
- GPG: agent forwarding (opt-in)

### GitHub Codespaces

- Auto-detects localhost URLs in terminal output
- TCP port forwarding with visibility controls
- Integrated GitHub authentication

### Coder

- Dev containers as "sub-agents" within workspaces
- Each container gets independent SSH access
- Standard SSH port forwarding

---

## Credential Helper Protocols

### Docker Credential Helper Protocol

Commands: `get`, `store`, `erase`, `list`

**get** (stdin: server URL, stdout: JSON):
```bash
echo "ghcr.io" | docker-credential-osxkeychain get
# Output: {"ServerURL":"ghcr.io","Username":"user","Secret":"token"}
```

**store** (stdin: JSON with ServerURL, Username, Secret)

**erase** (stdin: server URL)

### Git Credential Helper Protocol

Commands: `get`, `store`, `erase`

**get** (stdin: key=value pairs, stdout: key=value pairs):
```bash
git credential fill <<EOF
protocol=https
host=github.com

EOF
# Output:
# protocol=https
# host=github.com
# username=user
# password=token
```

---

## Proposed Architecture for devc

```
┌─────────────────────────────────────────────────────────────────┐
│                     Mac Host                                     │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                    devc (host process)                     │  │
│  │  - Listens on Unix socket                                  │  │
│  │  - Receives credential requests from container             │  │
│  │  - Calls real osxkeychain helpers                          │  │
│  │  - Returns credentials over socket                         │  │
│  └───────────────────────────────────────────────────────────┘  │
│                             │ Unix Socket (mounted)              │
├─────────────────────────────┼───────────────────────────────────┤
│         Container           │                                    │
│  ┌──────────────────────────▼────────────────────────────────┐  │
│  │  docker-credential-devc / git-credential-devc              │  │
│  │  - Forwards requests to host via socket                    │  │
│  └───────────────────────────────────────────────────────────┘  │
│           ▲                                      ▲               │
│  ┌────────┴────────┐                  ┌─────────┴─────────┐     │
│  │   Docker CLI    │                  │     Git CLI       │     │
│  └─────────────────┘                  └───────────────────┘     │
└─────────────────────────────────────────────────────────────────┘
```

---

## Implementation Options

### Option 1: One-Time Credential Resolution (Simplest)

During container setup:
1. Read host's `~/.docker/config.json`
2. Call `docker-credential-osxkeychain get` for each registry
3. Write resolved credentials to container's `~/.docker/config.json`

**Pros**: Simple, no agent needed
**Cons**: Credentials are static, stored in container

### Option 2: Socket Proxy (Like VS Code)

1. `devc` creates Unix socket on host
2. Mount socket into container
3. Inject credential helper that talks to socket
4. Host proxies requests to real keychain

**Pros**: Dynamic, credentials stay on host
**Cons**: More complex, needs background process

### Option 3: Exec-Based Proxy (No Socket Mount)

Use `docker exec` to proxy requests instead of socket mount.

**Pros**: Works without socket mounting capability
**Cons**: Higher latency per request

---

## Credential Security Options

### tmpfs (Memory-Only Storage)

```bash
mkdir -p ~/.docker
mount -t tmpfs -o size=1M,mode=700 tmpfs ~/.docker
# Write credentials here - never touches disk
```

**Pros**: Credentials never written to disk, disappear on container stop
**Cons**: Requires mount capability, still visible via `docker exec`

### Encrypted Storage

Store encrypted credentials, use helper to decrypt:

```
~/.docker/
├── config.json          # Points to helper: {"credsStore": "devc-encrypted"}
└── .credentials.enc     # AES-256 encrypted credentials
```

Key delivery options:
- Environment variable (visible in inspect)
- Mounted secret file
- Agent provides key at runtime
- User passphrase (interactive)

### GPG + pass

Use standard `pass` credential store with GPG encryption.
Requires GPG key management.

---

## Recommended Implementation Path

### Phase 1: Credential Forwarding (MVP)
- Inject simple credential helper script
- Use `docker exec` to proxy requests
- No persistent agent needed
- Use tmpfs for credential storage

### Phase 2: Agent Binary
- Create `devc-agent` Rust binary
- Add to container during build
- IPC via mounted Unix socket

### Phase 3: Auto Port Forwarding
- Agent scans `/proc/net/tcp` for listening ports
- Reports new ports to host
- Host sets up SSH/socat tunnels

### Phase 4: Advanced Features
- GPG forwarding
- Environment variable sync
- File sync / hot reload

---

## Configuration

Proposed config in `~/.config/devc/config.toml`:

```toml
[credentials]
forward_docker = true
forward_git = true
method = "proxy"  # "proxy", "copy", "tmpfs", "encrypted"

[ports]
auto_forward = true
detection_method = "process"  # "process", "output", "hybrid"
```

---

## Key Files in devc Codebase

- `/home/user/devc/crates/devc-provider/src/cli_provider.rs` - Docker/Podman CLI commands
- `/home/user/devc/crates/devc-core/src/manager.rs` - Container lifecycle
- `/home/user/devc/crates/devc-core/src/container.rs` - Exec and lifecycle commands
- `/home/user/devc/crates/devc-config/src/global.rs` - Global configuration
- `/home/user/devc/crates/devc-config/src/devcontainer.rs` - devcontainer.json parsing

---

## References

- [Docker Credential Helpers](https://github.com/docker/docker-credential-helpers)
- [Git Credential Storage](https://git-scm.com/book/en/v2/Git-Tools-Credential-Storage)
- [VS Code Dev Containers](https://code.visualstudio.com/docs/devcontainers/containers)
- [DevPod Agent Docs](https://devpod.sh/docs/developing-providers/agent)
- [Docker Secrets](https://docs.docker.com/engine/swarm/secrets/)
- [devcontainers/features#453](https://github.com/devcontainers/features/issues/453) - Docker-in-docker login issue
- [devcontainers/features#376](https://github.com/devcontainers/features/issues/376) - Non-root user credentials issue

---

## Session Info

This research was conducted in Claude Code session: `01UfpGsFFkJAfnVHdSBn4CvX`
Branch: `claude/debug-docker-auth-error-B5Nbd`

To continue on desktop, read this file and provide context to a new Claude Code session.
