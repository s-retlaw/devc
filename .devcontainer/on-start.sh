#!/usr/bin/env bash
# Devcontainer startup script — ensures Docker and Podman are ready for e2e tests.
# Called via postStartCommand in devcontainer.json.
set -euo pipefail

# ── Docker credential fix ────────────────────────────────────────────────
# devc's credential forwarding sets credsStore=devc in ~/.docker/config.json,
# but the docker-credential-devc helper may not work inside the devcontainer.
# Clear it so Docker can pull public images without a credential helper.
if [ -f "$HOME/.docker/config.json" ] && grep -q '"credsStore"' "$HOME/.docker/config.json" 2>/dev/null; then
    echo "[on-start] Clearing Docker credsStore (was set by devc credential forwarding)"
    tmp=$(jq 'del(.credsStore)' "$HOME/.docker/config.json")
    echo "$tmp" > "$HOME/.docker/config.json"
fi

# ── Docker ──────────────────────────────────────────────────────────────
# The docker-in-docker feature installs Docker but its entrypoint may not
# run under all devcontainer CLIs. We start dockerd ourselves if needed.

# Fix iptables: the feature defaults to iptables-legacy, but modern kernels
# (Fedora 43+, etc.) only support nftables. Switch to the nft backend.
if command -v update-alternatives &>/dev/null; then
    sudo update-alternatives --set iptables /usr/sbin/iptables-nft 2>/dev/null || true
    sudo update-alternatives --set ip6tables /usr/sbin/ip6tables-nft 2>/dev/null || true
fi

if ! docker info &>/dev/null; then
    echo "[on-start] Starting Docker daemon..."
    sudo rm -f /var/run/docker.pid /var/run/docker.sock /tmp/dockerd.log
    sudo bash -c 'dockerd --log-level=warn &>/tmp/dockerd.log &'

    for i in $(seq 1 30); do
        if docker info &>/dev/null; then
            echo "[on-start] Docker is ready (took ${i}s)"
            break
        fi
        if [ "$i" -eq 30 ]; then
            echo "[on-start] WARNING: Docker failed to start within 30s. Check /tmp/dockerd.log"
        fi
        sleep 1
    done
else
    echo "[on-start] Docker is already running"
fi

# ── Podman ──────────────────────────────────────────────────────────────
echo "[on-start] Configuring Podman..."

# Reset Podman storage in case overlay state is stale from a rebuild
podman system reset --force 2>/dev/null || true

if podman info &>/dev/null; then
    echo "[on-start] Podman is ready"
else
    echo "[on-start] WARNING: Podman is not working. Check 'podman info' for details."
fi

echo "[on-start] Done."
