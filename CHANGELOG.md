# Changelog

## 1.1.0

### Podman Improvements
- Install `podman-compose` in devcontainer for `podman compose` support
- Skip feature install tests that fail under rootless Podman (kernel setegid/setgroups limitation)
- Fix hardcoded `docker` in test cleanup to use provider runtime type

### E2E Testing
- Add Toolbox provider support across all e2e test suites
- Improve credential injection for multi-user container environments
- Fix test summary to show passed/failed counts only (drop misleading nextest skip count)
- Add `just test-e2e-docker`, `test-e2e-podman`, and `test-all` targets

### Docker Compose
- Fix compose lifecycle for Podman runtime

## 1.0.0

Initial stable release.

### Container Lifecycle
- Full devcontainer.json support: `image`, `build.dockerfile`, `dockerComposeFile`
- Container lifecycle management: init, up, down, start, stop, rebuild, adopt
- Docker and Podman runtime support
- Docker Compose multi-container projects

### Dev Container Features
- OCI-based feature installation from registries (ghcr.io, etc.)
- Variable substitution in devcontainer.json (`${localWorkspaceFolder}`, `${devcontainerId}`, etc.)
- All lifecycle commands: `initializeCommand`, `onCreateCommand`, `updateContentCommand`, `postCreateCommand`, `postStartCommand`, `postAttachCommand`
- `remoteEnv` and `containerEnv` environment variable support
- `runArgs`, `privileged`, `capAdd`, `securityOpt` container options
- `mounts`, `forwardPorts`, `appPort`, `portsAttributes` configuration

### TUI Dashboard
- Interactive terminal UI with vim-style navigation (j/k, g/G, Ctrl+d/u)
- Container detail view with logs viewer
- Port forwarding management panel
- Docker Compose services view
- Shell session management

### CLI
- Full command-line interface: `init`, `up`, `down`, `shell`, `run`, `build`, `start`, `stop`, `rm`, `rebuild`, `adopt`, `resize`, `list`, `config`
- Interactive container selection when name is omitted
- Global configuration via `~/.config/devc/config.toml`

### Networking & Security
- Port forwarding via socat tunnels with auto-forward detection
- SSH agent forwarding into containers
- Docker credential forwarding (credsStore, credHelpers, auths)
- Git credential forwarding (GitHub, GitLab, Bitbucket, Azure DevOps)
- Credential cache on tmpfs (RAM only, never written to disk)

### Other
- Dotfiles repository support (automatic clone and install)
- Persistent shell sessions with host-side PTY relay
- Container state persistence across restarts
- Cross-platform: Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), Windows
