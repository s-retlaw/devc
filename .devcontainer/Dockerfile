FROM fedora:43

# ── System packages ──────────────────────────────────────────────────────
RUN dnf install -y \
    # Build essentials for Rust native deps (openssl-sys, nix, etc.)
    gcc gcc-c++ make pkg-config openssl-devel perl-FindBin \
    # Version control
    git \
    # Podman + container tools (rootless-ready)
    podman podman-compose buildah skopeo fuse-overlayfs slirp4netns crun \
    # Flatpak & Toolbox (for testing toolbox detection / flatpak-spawn paths)
    flatpak toolbox \
    # Test utilities used by e2e tests
    socat nmap-ncat jq procps-ng \
    # Node.js for Claude Code
    nodejs npm \
    # Misc
    curl wget which findutils sudo zsh \
    && dnf clean all

# ── Claude Code (needs root for global install) ─────────────────────────
RUN npm install -g @anthropic-ai/claude-code

# ── Docker Engine (dnf5-compatible — replaces docker-in-docker feature) ──
RUN dnf config-manager addrepo --from-repofile=https://download.docker.com/linux/fedora/docker-ce.repo && \
    dnf install -y docker-ce docker-ce-cli containerd.io docker-compose-plugin && \
    dnf clean all

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

RUN groupadd --gid $USER_GID $USERNAME && \
    useradd --uid $USER_UID --gid $USER_GID -m $USERNAME -s /bin/zsh && \
    echo "$USERNAME ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/$USERNAME && \
    chmod 0440 /etc/sudoers.d/$USERNAME && \
    # Use subuid/subgid ranges that fit inside the devcontainer's user namespace
    echo "$USERNAME:10000:55536" > /etc/subuid && \
    echo "$USERNAME:10000:55536" > /etc/subgid && \
    # Allow non-root user to use Docker
    usermod -aG docker $USERNAME

# ── Switch to non-root user for Rust + cargo installs ───────────────────
USER $USERNAME

ENV RUSTUP_HOME=/home/$USERNAME/.rustup \
    CARGO_HOME=/home/$USERNAME/.cargo \
    PATH=/home/$USERNAME/.cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable --profile default

RUN cargo install --locked cargo-nextest cargo-insta just
