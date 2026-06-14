#!/usr/bin/env bash
# Build a .app bundle around the macwifi binary so macOS will honor
# NSLocationUsageDescription and stop redacting Wi-Fi SSIDs in scan results.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
PROFILE="${PROFILE:-release}"
BIN="$TARGET_DIR/$PROFILE/macwifi"
APP="$TARGET_DIR/$PROFILE/macwifi.app"
PLIST="$ROOT/bundle/Info.plist"

if [[ ! -f "$BIN" ]]; then
    echo "binary not found at $BIN" >&2
    echo "build it first:  cargo build --$PROFILE" >&2
    exit 1
fi

if [[ ! -f "$PLIST" ]]; then
    echo "Info.plist template missing at $PLIST" >&2
    exit 1
fi

plutil -lint "$PLIST" >/dev/null

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/macwifi"
cp "$PLIST" "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc sign without hardened runtime. Hardened runtime + ad-hoc on
# Sequoia/Tahoe interacts badly with TCC: tccd silently refuses prompts.
# For distribution, replace "-" with "Developer ID Application: …".
codesign --force --deep \
    --sign - \
    --identifier dev.macwifi.macwifi \
    "$APP"

echo "bundled: $APP"
echo
echo "next steps:"
echo "  open $APP                                   # one-off launch (no terminal)"
echo "  $APP/Contents/MacOS/macwifi                 # run with TUI in current terminal"
echo "  ln -sf $APP/Contents/MacOS/macwifi /usr/local/bin/macwifi"
echo
echo "on first scan, macOS will ask for Location — click Allow."
