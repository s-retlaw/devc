# devc

A fast, Rust-based dev container manager with both TUI and CLI interfaces. Supports Docker and Podman.

![License](https://img.shields.io/badge/license-MIT-blue.svg)

## Features

- **TUI Dashboard** - Interactive terminal UI for managing containers
- **CLI Commands** - Full command-line interface for scripting and quick actions
- **Docker & Podman** - Works with both container runtimes
- **Docker Compose** - Manage multi-container projects via `dockerComposeFile`
- **Dev Container Spec** - Compatible with VS Code's devcontainer.json format
- **Dev Container Features** - OCI-based feature installation
- **Port Forwarding** - Automatic port forwarding with socat tunnels
- **Credential Forwarding** - Docker and Git credentials forwarded into containers
- **SSH Agent Forwarding** - Seamless SSH key access inside containers
- **Dotfiles** - Automatic dotfiles repository cloning and installation
- **Interactive Selection** - Arrow-key navigation when container name is omitted
- **Vim-style Navigation** - j/k, g/G, Ctrl+d/u throughout the interface

## Installation

### From GitHub Releases

Download pre-built binaries for Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), and Windows from the [Releases page](https://github.com/s-retlaw/devc/releases).

### From Source

```bash
git clone https://github.com/s-retlaw/devc.git
cd devc
cargo build --release
cp target/release/devc ~/.local/bin/
```

### Requirements

- Rust 1.70+ (building from source)
- Docker or Podman

## Quick Start

```bash
# Initialize a dev container from a directory with devcontainer.json
cd your-project
devc init

# Build and start the container
devc up

# Connect to the container
devc shell

# Or launch the TUI
devc
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `devc` | Launch the TUI dashboard |
| `devc init` | Initialize a container from current directory |
| `devc up [name]` | Build, create, and start a container |
| `devc down [name]` | Stop and remove a container (keeps state) |
| `devc shell [name]` | Open an interactive shell |
| `devc run [name] <cmd>` | Run a command in a container |
| `devc build [name]` | Build the container image |
| `devc start [name]` | Start a stopped container |
| `devc stop [name]` | Stop a running container |
| `devc rm [name]` | Remove a container |
| `devc rebuild [name]` | Rebuild a container from scratch |
| `devc adopt [name]` | Adopt an existing devcontainer into devc |
| `devc resize [name]` | Resize container PTY |
| `devc list` | List all containers |
| `devc config` | Show or edit configuration |

When `[name]` is omitted, an interactive selector is shown (if TTY).

## TUI Keybindings

### Dashboard
| Key | Action |
|-----|--------|
| `j` / `k` | Navigate up/down |
| `g` / `G` | Go to top/bottom |
| `Enter` | View container details |
| `s` | Start/Stop container |
| `u` | Up (full lifecycle) |
| `d` | Delete container |
| `R` | Rebuild container |
| `S` | Open shell |
| `p` | Port forwarding |
| `r` / `F5` | Refresh list |
| `q` | Quit |

### Container Detail
| Key | Action |
|-----|--------|
| `l` | View logs |
| `s` | Start/Stop |
| `u` | Up |
| `R` | Rebuild |
| `S` | Open shell |
| `q` | Back |

### Logs Viewer
| Key | Action |
|-----|--------|
| `j` / `k` | Scroll line |
| `g` / `G` | Top/Bottom |
| `Ctrl+d` / `Ctrl+u` | Half page |
| `PageDown` / `PageUp` | Full page |
| `r` | Refresh |
| `q` | Back |

### Port Forwarding
| Key | Action |
|-----|--------|
| `j` / `k` | Navigate ports |
| `f` | Forward selected port |
| `s` | Stop forwarding port |
| `a` | Forward all ports |
| `n` | Stop all forwards |
| `o` | Open in browser |
| `i` | Install socat in container |
| `q` | Back |

## Configuration

Configuration file location: `~/.config/devc/config.toml`

```bash
# View current config
devc config

# Edit config
devc config --edit
```

## Project Structure

```
crates/
├── devc-cli/      # CLI entry point and commands
├── devc-tui/      # Terminal user interface
├── devc-core/     # Core container management logic
├── devc-provider/ # Docker/Podman provider abstraction
└── devc-config/   # Configuration handling
```

## Dev Container Support

devc reads standard `devcontainer.json` files:

```
your-project/
├── .devcontainer/
│   └── devcontainer.json
└── ...
```

Supported fields:
- `image` - Use a pre-built image
- `build.dockerfile` - Build from Dockerfile
- `dockerComposeFile` / `service` - Docker Compose projects
- `remoteUser` - Set the container user
- `mounts` - Additional volume mounts
- `forwardPorts` - Port forwarding
- `appPort` - Always-forwarded application ports
- `portsAttributes` - Per-port labels, protocol, and auto-forward behavior
- `containerEnv` / `remoteEnv` - Environment variables
- `features` - Dev container features (OCI-based)
- `initializeCommand` - Run on host before container creation
- `onCreateCommand` - Run after first container creation
- `updateContentCommand` - Run after creating or starting container
- `postCreateCommand` - Run after container creation
- `postStartCommand` - Run after container start
- `postAttachCommand` - Run when attaching to container
- `runArgs` - Additional arguments passed to `docker run` / `podman run`
- `privileged` - Run container in privileged mode
- `capAdd` - Linux capabilities to add
- `securityOpt` - Security options for the container

## License

MIT - see [LICENSE](LICENSE)
