#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  echo "cargo-llvm-cov is required. Install it with: cargo install cargo-llvm-cov" >&2
  exit 127
fi

MODE="${1:-core}"
FAIL_UNDER_LINES="${FAIL_UNDER_LINES:-90}"
TEST_THREADS="${TEST_THREADS:-1}"

# Deterministic unit-test coverage gate.
#
# The ignored files are runtime adapters or platform/integration surfaces that
# require a GUI event loop, QUIC/TCP peers, D-Bus/session state, Parallels
# tooling, SSH, or macOS launcher behavior to exercise honestly. They are still
# compiled and tested by the workspace run; they are excluded only from the 90%
# line-coverage denominator.
CORE_IGNORE_REGEX='(^|/)crates/client/src/(app_icon|cli|clipboard|launch|main|net|volume)\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/client/src/data_plane/'
CORE_IGNORE_REGEX+='|(^|/)crates/client/src/ctl/(call|mod|print|tls|token)\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/client/src/viewer/(app|dump_signal|fullscreen|input|window_debug)\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/client/src/viewer/ime/hangul\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/server/src/(brand|main)\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/server/src/data_plane/'
CORE_IGNORE_REGEX+='|(^|/)crates/controld/src/(main|state|tls)\.rs$'
CORE_IGNORE_REGEX+='|(^|/)crates/vbox-cli/src/(distro_icons|machines|main|runtime)\.rs$'

cargo llvm-cov clean --workspace

case "$MODE" in
  core)
    echo "Running deterministic core coverage gate: line coverage >= ${FAIL_UNDER_LINES}%"
    cargo llvm-cov \
      --workspace \
      --summary-only \
      --fail-under-lines "$FAIL_UNDER_LINES" \
      --ignore-filename-regex "$CORE_IGNORE_REGEX" \
      -- \
      --test-threads="$TEST_THREADS"
    ;;
  raw)
    echo "Running raw workspace coverage without exclusions"
    cargo llvm-cov \
      --workspace \
      --summary-only \
      -- \
      --test-threads="$TEST_THREADS"
    ;;
  *)
    echo "usage: $0 [core|raw]" >&2
    exit 2
    ;;
esac
