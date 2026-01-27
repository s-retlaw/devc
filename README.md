# devc

A fast, Rust-based dev container manager with both TUI and CLI interfaces. Supports Docker and Podman.

![License](https://img.shields.io/badge/license-MIT-blue.svg)

## Features

- **TUI Dashboard** - Interactive terminal UI for managing containers
- **CLI Commands** - Full command-line interface for scripting and quick actions
- **Docker & Podman** - Works with both container runtimes
- **Dev Container Spec** - Compatible with VS Code's devcontainer.json format
- **Interactive Selection** - Arrow-key navigation when container name is omitted
- **SSH Integration** - Native SSH connections with automatic key management
- **Vim-style Navigation** - j/k, g/G, Ctrl+d/u throughout the interface

## Installation

### From Source

```bash
git clone https://github.com/s-retlaw/devc.git
cd devc
cargo build --release
cp target/release/devc ~/.local/bin/
```

### Requirements

- Rust 1.70+
- Docker or Podman
- SSH client (for `devc ssh`)

## Quick Start

```bash
# Initialize a dev container from a directory with devcontainer.json
cd your-project
devc init

# Build and start the container
devc up

# Connect to the container
devc ssh

# Or launch the TUI
devc
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `devc` | Launch the TUI dashboard |
| `devc init` | Initialize a container from current directory |
| `devc up [name]` | Build, create, and start a container |
| `devc down [name]` | Stop and remove a container |
| `devc ssh [name]` | Open an interactive shell |
| `devc run [name] <cmd>` | Run a command in a container |
| `devc build [name]` | Build the container image |
| `devc start [name]` | Start a stopped container |
| `devc stop [name]` | Stop a running container |
| `devc rm [name]` | Remove a container |
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
| `b` | Build container |
| `s` | Start/Stop container |
| `u` | Up (full lifecycle) |
| `d` | Delete container |
| `r` | Refresh list |
| `?` | Help |
| `q` | Quit |

### Container Detail
| Key | Action |
|-----|--------|
| `l` | View logs |
| `b` | Build |
| `s` | Start/Stop |
| `u` | Up |
| `q` | Back |

### Logs Viewer
| Key | Action |
|-----|--------|
| `j` / `k` | Scroll line |
| `g` / `G` | Top/Bottom |
| `Ctrl+d` / `Ctrl+u` | Half page |
| `Ctrl+f` / `Ctrl+b` | Full page |
| `r` | Refresh |
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

Supported features:
- `image` - Use a pre-built image
- `build.dockerfile` - Build from Dockerfile
- `remoteUser` - Set the container user
- `mounts` - Additional volume mounts
- `forwardPorts` - Port forwarding
- `postCreateCommand` - Run after container creation
- `postStartCommand` - Run after container start

## License

MIT
