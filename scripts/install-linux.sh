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
#   VBOX_LIBC      release libc: gnu or musl           (default: auto)
#
# What it does, in order:
#   1. detect Linux + arch (x86_64 / aarch64) + libc (gnu / musl)
#   2. download the matching prebuilt tarball from the latest GitHub Release
#   3. install runtime deps via apt / dnf / pacman / zypper / apk
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
as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    else
        need sudo
        sudo "$@"
    fi
}

# ---------- 1. environment ----------
[[ "$(uname -s)" = Linux ]] || die "this installer only supports Linux"
need curl
need tar

case "$(uname -m)" in
    x86_64|amd64)  ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) die "unsupported architecture: $(uname -m); build from source instead" ;;
esac

detect_libc() {
    case "${VBOX_LIBC:-auto}" in
        gnu|musl) LIBC="$VBOX_LIBC"; return ;;
        auto|"") ;;
        *) die "unsupported VBOX_LIBC=${VBOX_LIBC}; expected gnu, musl, or auto" ;;
    esac

    if getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
        LIBC=gnu
    elif { ldd --version 2>&1 || true; } | grep -qi musl; then
        LIBC=musl
    else
        LIBC=gnu
        warn "could not detect libc; defaulting to linux-gnu release"
    fi
}

detect_libc

# ---------- 2. runtime deps ----------
install_runtime_deps() {
    if command -v apt-get >/dev/null 2>&1; then
        as_root apt-get update -qq
        as_root apt-get install -y --no-install-recommends libxkbcommon0 libwayland-server0
    elif command -v dnf >/dev/null 2>&1; then
        as_root dnf install -y libxkbcommon libwayland-server
    elif command -v pacman >/dev/null 2>&1; then
        as_root pacman -Sy --noconfirm libxkbcommon wayland
    elif command -v zypper >/dev/null 2>&1; then
        as_root zypper --non-interactive install libxkbcommon0 libwayland-server0
    elif command -v apk >/dev/null 2>&1; then
        as_root apk add --no-cache libxkbcommon wayland-libs-server
    else
        warn "no supported package manager found; ensure libxkbcommon and libwayland-server are installed"
    fi
}

# ---------- 3. download tarball ----------
download_and_extract() {
    if [[ "$VERSION" = latest ]]; then
        url="https://github.com/$REPO/releases/latest/download/vbox-${ARCH}-linux-${LIBC}.tar.gz"
    else
        ver="${VERSION#v}"
        url="https://github.com/$REPO/releases/download/v${ver}/vbox-${ARCH}-linux-${LIBC}.tar.gz"
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
    as_root install -m 0755 -d "$PREFIX/bin"
    as_root install -m 0755 "$EXTRACT_DIR/bin/vbox-server"   "$PREFIX/bin/vbox-server"
    as_root install -m 0755 "$EXTRACT_DIR/bin/vbox-controld" "$PREFIX/bin/vbox-controld"
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
  release  : linux-$LIBC ($ARCH)
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
