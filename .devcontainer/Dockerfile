FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# ── System packages ──────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y \
    # Build essentials for Rust native deps (openssl-sys, nix, etc.)
    gcc g++ make pkg-config libssl-dev perl \
    # Version control
    git \
    # Podman + container tools (rootless-ready)
    podman buildah skopeo fuse-overlayfs slirp4netns crun uidmap podman-compose \
    # Test utilities used by e2e tests
    socat ncat jq procps \
    # Node.js for Claude Code
    nodejs npm \
    # Misc
    curl wget sudo zsh ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# ── Claude Code (needs root for global install) ─────────────────────────
RUN npm install -g @anthropic-ai/claude-code

# ── Configure Podman for container-in-container ──────────────────────────
RUN mkdir -p /etc/containers && \
    printf '[storage]\ndriver = "overlay"\n\n[storage.options.overlay]\nmount_program = "/usr/bin/fuse-overlayfs"\n' \
        > /etc/containers/storage.conf && \
    printf 'unqualified-search-registries = ["docker.io"]\n' \
        > /etc/containers/registries.conf

# ── Non-root user ────────────────────────────────────────────────────────
ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=$USER_UID

# Remove default ubuntu user (occupies UID/GID 1000 on Ubuntu 24.04)
RUN userdel -r ubuntu 2>/dev/null || true && \
    groupdel ubuntu 2>/dev/null || true && \
    groupadd --gid $USER_GID $USERNAME && \
    useradd --uid $USER_UID --gid $USER_GID -m $USERNAME -s /bin/zsh && \
    echo "$USERNAME ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/$USERNAME && \
    chmod 0440 /etc/sudoers.d/$USERNAME && \
    echo "$USERNAME:10000:55536" > /etc/subuid && \
    echo "$USERNAME:10000:55536" > /etc/subgid

# ── Switch to non-root user for Rust + cargo installs ───────────────────
USER $USERNAME

ENV RUSTUP_HOME=/home/$USERNAME/.rustup \
    CARGO_HOME=/home/$USERNAME/.cargo \
    PATH=/home/$USERNAME/.cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable --profile default

RUN cargo install --locked cargo-nextest cargo-insta just
