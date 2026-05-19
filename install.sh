#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

SERVICE_NAME="vbox-controld.service"

script_dir() {
    local source="$1" dir target
    while [[ -L "$source" ]]; do
        dir="$(cd -P "$(dirname "$source")" && pwd)"
        target="$(readlink "$source")"
        if [[ "$target" == /* ]]; then
            source="$target"
        else
            source="$dir/$target"
        fi
    done
    cd -P "$(dirname "$source")" && pwd
}

ROOT="$(script_dir "${BASH_SOURCE[0]}")"
BIND="127.0.0.1"
PORT="5711"
WORK_DIR="$ROOT/.vbox-controld"
SERVER_BIN="$ROOT/target/release/vbox-server"
CONTROLD_BIN="$ROOT/target/release/vbox-controld"
TOKEN_FILE="$WORK_DIR/token"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_PATH="$UNIT_DIR/$SERVICE_NAME"

log() {
    printf '[install] %s\n' "$*" >&2
}

warn() {
    printf '[install] warning: %s\n' "$*" >&2
}

die() {
    printf '[install] error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat >&2 <<'EOF'
Usage: ./install.sh

Builds vbox-server and vbox-controld, then installs a plain-token
systemd --user service on the Linux guest.

For mTLS setup, run from the macOS host instead:
  vbox controld-install --with-tls
EOF
}

parse_args() {
    case "$#" in
        0) ;;
        1)
            case "$1" in
                -h|--help)
                    usage
                    exit 0
                    ;;
                --with-tls)
                    die "install.sh is plain-token only. Run from macOS: vbox controld-install --with-tls"
                    ;;
                *)
                    usage
                    die "unknown option: $1"
                    ;;
            esac
            ;;
        *)
            usage
            die "install.sh takes no options"
            ;;
    esac
}

require_linux() {
    [[ "$(uname -s)" == "Linux" ]] \
        || die "install.sh must run on the Linux server. On macOS, run target/release/vbox controld-install instead."
}

require_systemd_user() {
    command -v systemctl >/dev/null 2>&1 || die "systemctl not found"
    if [[ -z "${XDG_RUNTIME_DIR:-}" && -d "/run/user/$(id -u)" ]]; then
        export XDG_RUNTIME_DIR="/run/user/$(id -u)"
    fi
    systemctl --user show-environment >/dev/null 2>&1 \
        || die "systemd --user is not reachable. Log in as the target user first."
}

validate_simple_unit_arg() {
    local name="$1" value="$2"
    [[ -n "$value" ]] || die "$name cannot be empty"
    case "$value" in
        *[$' \t\n\r\"\'\\%']*)
            die "$name contains whitespace, quoting, backslash, or systemd specifier characters unsupported by this installer: $value"
            ;;
    esac
}

build_binaries() {
    [[ -f "$ROOT/Cargo.toml" ]] || die "Cargo.toml not found in $ROOT"
    command -v cargo >/dev/null 2>&1 || die "cargo not found"
    log "build Linux release binaries"
    (cd "$ROOT" && cargo build --release -p vbox-server -p vbox-controld)
}

validate_binaries() {
    [[ -x "$CONTROLD_BIN" ]] || die "vbox-controld is not executable: $CONTROLD_BIN"
    [[ -x "$SERVER_BIN" ]] || die "vbox-server is not executable: $SERVER_BIN"
}

write_unit() {
    mkdir -p "$UNIT_DIR" "$WORK_DIR"
    chmod 700 "$WORK_DIR"
    cat > "$UNIT_PATH" <<UNIT
[Unit]
Description=vbox control daemon
After=default.target

[Service]
Type=simple
WorkingDirectory=$ROOT
Environment=RUST_BACKTRACE=1
ExecStart=$CONTROLD_BIN --bind $BIND --port $PORT --work-dir $WORK_DIR --server-bin $SERVER_BIN --token-file $TOKEN_FILE
Restart=always
RestartSec=2
KillMode=process
StandardOutput=append:$WORK_DIR/daemon.log
StandardError=append:$WORK_DIR/daemon.log

[Install]
WantedBy=default.target
UNIT
}

enable_linger() {
    command -v loginctl >/dev/null 2>&1 || {
        warn "loginctl not found; skipping linger"
        return
    }
    local user
    user="$(id -un)"
    if [[ "$(loginctl show-user "$user" -p Linger --value 2>/dev/null || true)" == "yes" ]]; then
        log "linger already enabled for $user"
        return
    fi
    if loginctl enable-linger "$user" >/dev/null 2>&1; then
        log "enabled linger for $user"
    elif command -v sudo >/dev/null 2>&1 && sudo -n loginctl enable-linger "$user" >/dev/null 2>&1; then
        log "enabled linger for $user via sudo"
    else
        warn "could not enable linger for $user; service works while the user manager is active"
    fi
}

install_service() {
    require_linux
    require_systemd_user
    validate_simple_unit_arg "ROOT" "$ROOT"
    validate_simple_unit_arg "WORK_DIR" "$WORK_DIR"
    validate_simple_unit_arg "SERVER_BIN" "$SERVER_BIN"
    validate_simple_unit_arg "CONTROLD_BIN" "$CONTROLD_BIN"
    validate_simple_unit_arg "TOKEN_FILE" "$TOKEN_FILE"

    build_binaries
    validate_binaries
    write_unit
    log "installed $UNIT_PATH"

    systemctl --user daemon-reload
    systemctl --user reset-failed "$SERVICE_NAME" >/dev/null 2>&1 || true
    systemctl --user enable "$SERVICE_NAME"
    enable_linger

    pkill -f "$CONTROLD_BIN" >/dev/null 2>&1 || true
    systemctl --user restart "$SERVICE_NAME"
    for _ in {1..40}; do
        if systemctl --user is-active --quiet "$SERVICE_NAME"; then
            break
        fi
        sleep 0.1
    done
    systemctl --user is-active --quiet "$SERVICE_NAME" \
        || die "$SERVICE_NAME did not become active"

    for _ in {1..40}; do
        [[ -s "$TOKEN_FILE" ]] && break
        sleep 0.1
    done
    [[ -s "$TOKEN_FILE" ]] || warn "token file was not created yet: $TOKEN_FILE"
    log "$SERVICE_NAME active on $BIND:$PORT"
    log "plain-token auth file on guest: $TOKEN_FILE"
}

parse_args "$@"
install_service
