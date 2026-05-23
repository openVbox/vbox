#!/usr/bin/env bash
#
# Build the prebuilt vbox.app (SwiftUI launcher) for the Homebrew cask.
#
# Used by .github/workflows/release.yml after the CLI tarball is staged:
# the resulting vbox.app is dropped into the universal tarball next to bin/vbox
# so a single `brew install --cask vbox` puts vbox in Launchpad without
# requiring the user to run `vbox install-apps` first.
#
# Required env:
#   VERSION   semver string baked into Info.plist (e.g. 0.1.1)
#
# Optional env:
#   OUT       output dir; .app is written to $OUT/vbox.app
#             (default: <repo-root>/dist/stage)
#   SRC       SwiftUI source path. Accepts either a single .swift file or a
#             directory; a directory compiles every *.swift inside it.
#             (default: <repo-root>/vbox-swift/Sources/VBoxLibrary)
#   ICON      optional path to AppIcon.icns; if missing, app gets no custom icon
#             (default: <repo-root>/assets/AppIcon.icns)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${SRC:-$ROOT/vbox-swift/Sources/VBoxLibrary}"
OUT="${OUT:-$ROOT/dist/stage}"
ICON="${ICON:-$ROOT/assets/AppIcon.icns}"
VERSION="${VERSION:?VERSION env is required (e.g. VERSION=0.1.1)}"

APP="$OUT/vbox.app"
CONTENTS="$APP/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

if [[ -f "$SRC" ]]; then
    SRCS=("$SRC")
elif [[ -d "$SRC" ]]; then
    shopt -s nullglob
    SRCS=("$SRC"/*.swift)
    shopt -u nullglob
else
    echo "[build-mac-app] swift source not found: $SRC" >&2
    exit 1
fi
[[ ${#SRCS[@]} -gt 0 ]] || {
    echo "[build-mac-app] no .swift sources under $SRC" >&2
    exit 1
}
command -v swiftc >/dev/null || { echo "[build-mac-app] swiftc not in PATH" >&2; exit 1; }
command -v lipo   >/dev/null || { echo "[build-mac-app] lipo not in PATH"   >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$MACOS_DIR" "$RESOURCES"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Compile arch-specific Swift binaries, then lipo into a universal one.
for triple in x86_64-apple-macos13.0 arm64-apple-macos13.0; do
    arch="${triple%%-*}"
    out="$tmp/$arch/VBoxLibrary"
    mkdir -p "$(dirname "$out")"
    echo "[build-mac-app] swiftc -> $arch (${#SRCS[@]} files)"
    swiftc \
        -target "$triple" \
        -parse-as-library \
        -O \
        "${SRCS[@]}" \
        -o "$out"
done
lipo -create \
    "$tmp/x86_64/VBoxLibrary" \
    "$tmp/arm64/VBoxLibrary"  \
    -output "$MACOS_DIR/VBoxLibrary"
chmod +x "$MACOS_DIR/VBoxLibrary"

# Optional custom icon.
icon_key=""
if [[ -f "$ICON" ]]; then
    cp "$ICON" "$RESOURCES/AppIcon.icns"
    icon_key="AppIcon"
fi

# Empty Resources/*.txt — the SwiftUI shell already has built-in fallbacks
# (empty guest / cli path) and `vbox install-apps` will overwrite these
# on the user's machine with the real values from their AppContext.
for name in Root CliPath StateDir LauncherDir IconCacheDir DistroIconDir Guest GuestDir Instance Socket Suffix; do
    : > "$RESOURCES/$name.txt"
done
printf '5710\n' > "$RESOURCES/Port.txt"
printf '1024\n' > "$RESOURCES/Width.txt"
printf '768\n'  > "$RESOURCES/Height.txt"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>VBoxLibrary</string>
  <key>CFBundleIdentifier</key><string>local.vbox.native.library</string>
  <key>CFBundleName</key><string>vbox</string>
  <key>CFBundleDisplayName</key><string>vbox</string>
  <key>CFBundleIconFile</key><string>$icon_key</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

/usr/bin/plutil -lint "$CONTENTS/Info.plist" >/dev/null

# Ad-hoc sign — Gatekeeper still flags the first run because we don't have a
# Developer ID, but at least the bundle is sealed and reproducible.
/usr/bin/codesign --force --sign - --timestamp=none "$MACOS_DIR/VBoxLibrary"
/usr/bin/codesign --force --sign - --timestamp=none "$APP"

echo "[build-mac-app] wrote $APP"
