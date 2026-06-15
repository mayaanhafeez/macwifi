#!/usr/bin/env bash
# Build a .app bundle around the macwifi binary so macOS will honor
# NSLocationUsageDescription (gates SSID visibility) and so Keychain ACLs
# have a stable code-signing identity to bind to.
#
# Signing:
#   By default we sign ad-hoc (`-`). Ad-hoc signatures are *not* stable
#   across rebuilds — every build gets a fresh CDHash, which invalidates
#   any per-item Keychain "Always Allow" grants the user has clicked.
#
#   To stop being re-prompted for Wi-Fi passwords after every `cargo build`,
#   create a self-signed code-signing cert in Keychain Access:
#       Keychain Access → Certificate Assistant → Create a Certificate
#         Name:      macwifi-dev
#         Identity:  Self Signed Root
#         Type:      Code Signing
#   Then run:
#       CODESIGN_IDENTITY=macwifi-dev scripts/bundle.sh
#
#   For distribution, set CODESIGN_IDENTITY="Developer ID Application: …".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
PROFILE="${PROFILE:-release}"
BIN="$TARGET_DIR/$PROFILE/macwifi"
APP="$TARGET_DIR/$PROFILE/macwifi.app"
PLIST="$ROOT/bundle/Info.plist"
IDENTITY="${CODESIGN_IDENTITY:--}"

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

# Hardened runtime stays off: hardened + ad-hoc on Sequoia/Tahoe interacts
# badly with TCC (tccd silently refuses prompts). With a real cert you can
# add `--options runtime` here if you also notarize.
codesign --force --deep \
    --sign "$IDENTITY" \
    --identifier dev.macwifi.macwifi \
    "$APP"

if [[ "$IDENTITY" == "-" ]]; then
    echo "bundled: $APP  (ad-hoc signed — Keychain grants will not persist across rebuilds)"
    echo "  → set CODESIGN_IDENTITY=<cert-name> to fix"
else
    echo "bundled: $APP  (signed: $IDENTITY)"
fi
echo
echo "next steps (daemon split: TUI talks to a backgrounded daemon over a socket):"
echo "  1. cp -R \"$APP\" /Applications/"
echo "  2. /Applications/macwifi.app/Contents/MacOS/macwifi install-daemon"
echo "  3. ln -sf /Applications/macwifi.app/Contents/MacOS/macwifi /usr/local/bin/macwifi"
echo
echo "Step 2 fires the one-time Location prompt — click Allow. The daemon is"
echo "loaded via launchctl so its parent is launchd, which is required for"
echo "CoreWLAN to return un-redacted SSIDs on Sequoia/Tahoe."
echo
echo "If you rebuild with a different CODESIGN_IDENTITY the keychain + TCC"
echo "grants are invalidated; re-run install-daemon to re-bootstrap."
