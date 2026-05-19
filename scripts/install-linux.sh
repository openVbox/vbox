#!/usr/bin/env bash
#
# vbox Linux guest installer.
#
# One-liner:
#   curl -fsSL https://raw.githubusercontent.com/openVbox/vbox/main/scripts/install-linux.sh | sh
#
# Optional env overrides:
#   VBOX_VERSION   pin to a specific release tag       (default: latest)
#   VBOX_PREFIX    binary install prefix               (default: /usr/local)
#   VBOX_DIR       guest work directory (per-instance) (default: $HOME/vbox)
#   VBOX_PORT      controld listen port                (default: 5711)
#
# What it does, in order:
#   1. detect Linux + arch (x86_64 / aarch64)
#   2. install libxkbcommon via apt / dnf / pacman / zypper
#   3. download the matching prebuilt tarball from the latest GitHub Release
#   4. install vbox-server + vbox-controld to $VBOX_PREFIX/bin
#   5. write ~/.config/systemd/user/vbox-controld.service pointing at them
#   6. systemctl --user enable --now + loginctl enable-linger
#   7. print the exact macOS-side command to connect

set -euo pipefail

REPO="openVbox/vbox"
PREFIX="${VBOX_PREFIX:-/usr/local}"
VERSION="${VBOX_VERSION:-latest}"
DIR="${VBOX_DIR:-$HOME/vbox}"
PORT="${VBOX_PORT:-5711}"

SERVICE="vbox-controld.service"
WORK_DIR="$DIR/.vbox-controld"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_PATH="$UNIT_DIR/$SERVICE"
TOKEN_FILE="$WORK_DIR/token"

log()  { printf '[vbox] %s\n' "$*"; }
warn() { printf '[vbox] warning: %s\n' "$*" >&2; }
die()  { printf '[vbox] error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

# ---------- 1. environment ----------
[[ "$(uname -s)" = Linux ]] || die "this installer only supports Linux"
need curl
need tar

case "$(uname -m)" in
    x86_64|amd64)  ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) die "unsupported architecture: $(uname -m); build from source instead" ;;
esac

# ---------- 2. runtime deps ----------
install_runtime_deps() {
    if command -v apt-get >/dev/null 2>&1; then
        sudo apt-get update -qq
        sudo apt-get install -y --no-install-recommends libxkbcommon0
    elif command -v dnf >/dev/null 2>&1; then
        sudo dnf install -y libxkbcommon
    elif command -v pacman >/dev/null 2>&1; then
        sudo pacman -Sy --noconfirm libxkbcommon
    elif command -v zypper >/dev/null 2>&1; then
        sudo zypper --non-interactive install libxkbcommon0
    else
        warn "no supported package manager found; ensure libxkbcommon is installed"
    fi
}

# ---------- 3. download tarball ----------
download_and_extract() {
    if [[ "$VERSION" = latest ]]; then
        url="https://github.com/$REPO/releases/latest/download/vbox-${ARCH}-linux-gnu.tar.gz"
    else
        ver="${VERSION#v}"
        url="https://github.com/$REPO/releases/download/v${ver}/vbox-${ARCH}-linux-gnu.tar.gz"
    fi
    EXTRACT_DIR="$(mktemp -d)"
    trap 'rm -rf "$EXTRACT_DIR"' EXIT
    log "downloading $url"
    curl -fsSL "$url" -o "$EXTRACT_DIR/vbox.tar.gz" \
        || die "download failed: $url"
    tar -xzf "$EXTRACT_DIR/vbox.tar.gz" -C "$EXTRACT_DIR"
    [[ -x "$EXTRACT_DIR/bin/vbox-server" && -x "$EXTRACT_DIR/bin/vbox-controld" ]] \
        || die "tarball did not contain expected bin/vbox-{server,controld}"
}

# ---------- 4. install binaries ----------
install_binaries() {
    log "installing binaries to $PREFIX/bin"
    sudo install -m 0755 -d "$PREFIX/bin"
    sudo install -m 0755 "$EXTRACT_DIR/bin/vbox-server"   "$PREFIX/bin/vbox-server"
    sudo install -m 0755 "$EXTRACT_DIR/bin/vbox-controld" "$PREFIX/bin/vbox-controld"
}

# ---------- 5. systemd user unit ----------
write_unit() {
    mkdir -p "$UNIT_DIR" "$WORK_DIR"
    cat > "$UNIT_PATH" <<UNIT
[Unit]
Description=vbox control daemon
After=default.target

[Service]
Type=simple
WorkingDirectory=$DIR
ExecStart=$PREFIX/bin/vbox-controld --bind 127.0.0.1 --port $PORT --work-dir $WORK_DIR --server-bin $PREFIX/bin/vbox-server --token-file $TOKEN_FILE
Restart=always
RestartSec=2
KillMode=process
StandardOutput=append:$WORK_DIR/daemon.log
StandardError=append:$WORK_DIR/daemon.log

[Install]
WantedBy=default.target
UNIT
    log "wrote $UNIT_PATH"
}

enable_unit() {
    command -v systemctl >/dev/null 2>&1 \
        || { warn "systemctl not found; unit installed but not activated"; return; }
    if [[ -z "${XDG_RUNTIME_DIR:-}" && -d "/run/user/$(id -u)" ]]; then
        export XDG_RUNTIME_DIR="/run/user/$(id -u)"
    fi
    if ! systemctl --user show-environment >/dev/null 2>&1; then
        warn "systemd --user is not reachable; log in as $(id -un) once and the daemon will start"
        return
    fi
    systemctl --user daemon-reload
    systemctl --user reset-failed "$SERVICE" 2>/dev/null || true
    systemctl --user enable --now "$SERVICE"
    for _ in $(seq 1 40); do
        [[ -s "$TOKEN_FILE" ]] && break
        sleep 0.1
    done
}

enable_linger() {
    command -v loginctl >/dev/null 2>&1 || return
    local user; user="$(id -un)"
    [[ "$(loginctl show-user "$user" -p Linger --value 2>/dev/null || true)" = yes ]] && return
    if loginctl enable-linger "$user" 2>/dev/null \
        || (command -v sudo >/dev/null && sudo -n loginctl enable-linger "$user" 2>/dev/null); then
        log "enabled linger (daemon survives logout)"
    else
        warn "could not enable linger; daemon will stop when you log out"
    fi
}

main() {
    download_and_extract
    install_runtime_deps
    install_binaries
    write_unit
    enable_unit
    enable_linger

    cat <<MSG

vbox guest daemon is up.

  binaries : $PREFIX/bin/vbox-{server,controld}
  work dir : $WORK_DIR
  service  : $SERVICE  (port $PORT)
  token    : $TOKEN_FILE

On your Mac (after \`brew install --cask vbox\`):

  vbox --guest $(id -un)@$(hostname) --guest-dir $DIR run gnome-calculator

Or set the defaults once:

  export VBOX_GUEST=$(id -un)@$(hostname)
  export VBOX_GUEST_DIR=$DIR

MSG
}

main "$@"
