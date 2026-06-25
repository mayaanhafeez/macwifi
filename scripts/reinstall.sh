#!/usr/bin/env bash
# Rebuild macwifi, refresh the /Applications bundle, and re-bootstrap the daemon
# — correctly. This exists because the obvious `cp -R … /Applications` corrupts
# the app's code signature, after which macOS SIGKILLs every launch (exit 137)
# and the daemon never binds its socket. We re-sign after copying to avoid that.
#
# Usage:
#   CODESIGN_IDENTITY=macwifi-dev scripts/reinstall.sh   # stable cert (recommended)
#   scripts/reinstall.sh                                  # ad-hoc (grants won't persist)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IDENTITY="${CODESIGN_IDENTITY:--}"
APP_SRC="$ROOT/target/release/macwifi.app"
APP_DST="/Applications/macwifi.app"
LABEL="dev.macwifi.daemon"
UID_="$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SOCK="$HOME/Library/Application Support/macwifi/daemon.sock"
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

echo "==> building release"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> bundling + signing (identity: $IDENTITY)"
CODESIGN_IDENTITY="$IDENTITY" "$ROOT/scripts/bundle.sh" >/dev/null

echo "==> stopping daemon"
launchctl bootout "gui/$UID_/$LABEL" 2>/dev/null || true
pkill -f "macwifi.app/Contents/MacOS/macwifi daemon" 2>/dev/null || true
pkill -f "open -W -a $APP_DST" 2>/dev/null || true
sleep 1

echo "==> refreshing $APP_DST (ditto preserves the signature better than cp -R)"
rm -rf "$APP_DST"
ditto "$APP_SRC" "$APP_DST"
# Re-sign in place regardless — the kernel SIGKILLs an invalid signature.
codesign --force --deep --sign "$IDENTITY" --identifier dev.macwifi.macwifi "$APP_DST"
# Sanity: a corrupt signature shows up as exit 137 on any subcommand.
if ! "$APP_DST/Contents/MacOS/macwifi" themes >/dev/null 2>&1; then
    echo "ERROR: $APP_DST is SIGKILL'd on launch — signature still invalid" >&2
    exit 1
fi
"$LSREG" -f "$APP_DST"

echo "==> bootstrapping daemon"
rm -f "$SOCK"
launchctl bootstrap "gui/$UID_" "$PLIST"
for i in $(seq 1 40); do
    [ -S "$SOCK" ] && break
    sleep 0.5
done
if [ ! -S "$SOCK" ]; then
    echo "ERROR: daemon did not bind $SOCK within 20s — check /tmp/macwifi-daemon.err.log" >&2
    exit 1
fi

echo "==> verifying"
"$APP_DST/Contents/MacOS/macwifi" status | sed -n '1,4p'
echo "OK — daemon up, socket at $SOCK"
