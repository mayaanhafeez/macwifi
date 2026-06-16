# macwifi

A TUI for managing Wi-Fi on macOS. A clean-room macOS port of
[impala](https://github.com/pythops/impala) (Linux/iwd) — same idea, same
keybindings where they map, but built on CoreWLAN and `networksetup(8)`.

Station mode only. Theming, hidden networks, QR sharing, adapter info, and a
`.app` bundle that gets the Location permission story right.

## Features

- Live scan & associate (open / WPA-PSK / WPA-Enterprise PEAP/MSCHAPv2 / hidden)
- Manage saved networks (list, forget) and the current connection (disconnect, toggle power)
- QR-code sharing of saved networks (pulls password from Keychain)
- Adapter info popup: SSID, BSSID, RSSI, noise, channel, TX rate, MAC
- **14 themes**: `default`, Catppuccin (latte/frappe/macchiato/mocha), Rose Pine (main/moon/dawn), Tokyo Night (night/storm), Gruvbox (dark/light), Nord, Dracula
- Cycle themes live with `T` / `Shift-Tab` — choice is persisted across launches
- TOML config at `~/.config/macwifi/config.toml`
- Self-contained `.app` bundle with `NSLocationUsageDescription` so scan results aren't redacted

## Demo

```
┌ macwifi ─────────────────────────────────────────────────────┐
│ en0 │ ON │ SSID: MyNetwork  RSSI: -43 dBm  CH: 157  TX: 286 Mbps │
└──────────────────────────────────────────────────────────────┘
┌ Preferred (24) ──────────────────────────────────────────────┐
│ ▶ MB_Lounge                                                  │
│   AndroidAP_8011                                             │
└──────────────────────────────────────────────────────────────┘
┌ Available (9/12) ────────────────────────────────────────────┐
│ ▶ MyNetwork                  -36 dBm  ch157   WPA2          │
│   guest-wifi                 -42 dBm  ch5     open          │
└──────────────────────────────────────────────────────────────┘
 Tab focus │ j/k │ Enter connect │ s scan │ o power │ d forget │ x off │ p share │ h hidden │ i info │ a all │ T theme │ q quit
```

---

## Setup

### 1. Prerequisites

**macOS 13 Ventura or newer.** CoreWLAN's scan API requires Location Services,
which works correctly only on Ventura+.

**Rust toolchain.** If you don't have `cargo`:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

---

### 2. Create a code-signing certificate (recommended, do once)

macwifi's `.app` bundle must be code-signed so macOS lets it hold a stable
Keychain identity. Without a stable identity, every rebuild invalidates any
"Always Allow" Keychain grants you've clicked for your saved Wi-Fi networks,
and macOS will re-prompt for each password on the next connect.

A free self-signed certificate is enough — you don't need an Apple Developer
account for personal use.

**Steps:**

1. Open **Keychain Access** (Spotlight → "Keychain Access").
2. From the menu bar: **Keychain Access → Certificate Assistant → Create a Certificate…**
3. Fill in:
   - **Name:** `macwifi-dev`
   - **Identity Type:** Self Signed Root
   - **Certificate Type:** Code Signing
   - Leave everything else at defaults.
4. Click **Create**, then **Done**.

The certificate is stored in your login keychain and is ready to use immediately.

> **Skip this step?** You can still use macwifi — just leave `CODESIGN_IDENTITY`
> unset and the bundle will be ad-hoc signed. It will work, but you'll get a
> Keychain password prompt on every connect after a rebuild.

---

### 3. Build and bundle

Clone the repo and build the release binary, then wrap it in a `.app` bundle:

```sh
git clone https://github.com/<you>/macwifi
cd macwifi

cargo build --release

# If you created the certificate in step 2:
CODESIGN_IDENTITY=macwifi-dev ./scripts/bundle.sh

# If you skipped step 2 (ad-hoc signing):
./scripts/bundle.sh
```

This produces `target/release/macwifi.app`.

---

### 4. Install the app

Copy the bundle to `/Applications` so the daemon LaunchAgent can find it at a
stable path (it is hardcoded to `/Applications/macwifi.app`):

```sh
cp -R target/release/macwifi.app /Applications/
```

Symlink the binary so you can run `macwifi` from any terminal:

```sh
ln -sf /Applications/macwifi.app/Contents/MacOS/macwifi /usr/local/bin/macwifi
```

---

### 5. Install and start the daemon

macwifi uses a split architecture: a background daemon owns the CoreWLAN
interface and a LaunchAgent keeps it running at login. The TUI is a thin client
that connects to the daemon over a local Unix socket.

```sh
macwifi install-daemon
```

This will:
- Write a LaunchAgent plist to `~/Library/LaunchAgents/dev.macwifi.daemon.plist`
- Load it with `launchctl bootstrap` so the daemon starts immediately
- Wait up to 15 seconds for the daemon socket to appear

**Why a daemon?** CoreWLAN returns blank/redacted SSIDs unless the calling
process is a properly launched Aqua app session. The daemon is started via
`/usr/bin/open -a /Applications/macwifi.app` which satisfies this requirement.
A plain `launchctl` exec or `cargo run` does not.

---

### 6. Grant Location permission (first launch)

On the very first scan, macOS will show a system dialog:

> **"macwifi" would like to use your current location.**

Click **Allow While Using App** (or **Allow**). This is required for CoreWLAN
to return real SSIDs instead of blank strings. The permission is bound to the
app's bundle identifier (`dev.macwifi.macwifi`) and persists across relaunches.

If you accidentally clicked **Don't Allow**, reset the grant and relaunch:

```sh
tccutil reset Location dev.macwifi.macwifi
macwifi
```

---

### 7. Launch the TUI

```sh
macwifi
```

The first connect to a saved WPA network will trigger a Keychain prompt asking
whether to allow macwifi to read the saved password. Click **Always Allow** so
it doesn't re-prompt after every session.

---

## Rebuilding after changes

If you edit source code and rebuild, you must re-bundle and re-copy to
`/Applications` so the running binary and the daemon's app path stay in sync:

```sh
cargo build --release
CODESIGN_IDENTITY=macwifi-dev ./scripts/bundle.sh   # or without CODESIGN_IDENTITY
cp -R target/release/macwifi.app /Applications/
```

The daemon will be restarted automatically by the LaunchAgent's `KeepAlive`
policy once the old process exits. Or restart it manually:

```sh
launchctl kickstart -k gui/$(id -u)/dev.macwifi.daemon
```

> **Note on Keychain grants and rebuilding:** If you used a stable
> `CODESIGN_IDENTITY`, the CDHash doesn't change between builds signed with the
> same certificate, so Keychain "Always Allow" grants survive rebuilds. With
> ad-hoc signing (`-`), every build gets a new hash and grants are lost.

---

## Uninstalling

Stop the daemon and remove the LaunchAgent:

```sh
macwifi uninstall-daemon
```

Remove the app, symlink, and config:

```sh
rm -rf /Applications/macwifi.app
rm -f /usr/local/bin/macwifi
rm -rf ~/.config/macwifi
```

---

## Usage

| Key | Action |
|-----|--------|
| `Tab` | Toggle focus between Preferred / Available lists |
| `j` / `k` / `↓` / `↑` | Move selection |
| `Enter` | Connect (password / enterprise overlays appear as needed) |
| `s` | Rescan |
| `o` | Toggle radio power on/off |
| `d` | Forget the selected saved network |
| `x` | Disconnect |
| `p` | Share selected saved network as a QR code |
| `h` | Connect to a hidden network |
| `i` | Adapter info popup |
| `a` | Show all networks (disable RSSI/SSID filter) |
| `A` | Show full preferred list (default: top 10) |
| `T` | Cycle theme forward |
| `Shift-Tab` | Cycle theme backward |
| `q` / `Esc` / `Ctrl-C` | Quit (or dismiss overlay) |

Non-interactive subcommands for scripting:

```sh
macwifi status
macwifi scan
macwifi power on|off
macwifi connect <SSID> [PASSWORD]
macwifi connect-hidden <SSID> [PASSWORD]
macwifi connect-peap <SSID> <USERNAME> <PASSWORD>
macwifi disconnect
macwifi preferred
macwifi forget <SSID>
macwifi themes
macwifi diagnose
```

---

## Configuration

Optional file at `~/.config/macwifi/config.toml`:

```toml
theme = "catppuccin-mocha"
```

The CLI flag `--theme <name>` overrides the config file for that session.
Cycling themes with `T` / `Shift-Tab` inside the TUI automatically writes the
chosen theme back to the config file so it persists across launches.

Run `macwifi themes` for the full list of available theme names.

---

## Architecture

```
┌─────────────────────┐        Unix socket        ┌──────────────────────────┐
│   macwifi (TUI)     │  ←── newline-delimited ──→ │   macwifi daemon         │
│   (thin client)     │        JSON (IPC)           │   (CoreWLAN + launchd)   │
└─────────────────────┘                             └──────────────────────────┘
```

- **Daemon** (`macwifi daemon`): launched by a LaunchAgent via
  `/usr/bin/open -a /Applications/macwifi.app`. This gives it a proper Aqua
  app session, which is required for CoreWLAN to return real SSIDs and for
  Location TCC grants to apply.
- **TUI client** (`macwifi` with no subcommand): connects to the daemon socket,
  receives scan/state events, and sends commands.
- **Socket path**: `~/Library/Application Support/macwifi/daemon.sock`
- **Daemon logs**: `/tmp/macwifi-daemon.out.log` and `/tmp/macwifi-daemon.err.log`

---

## Troubleshooting

**SSIDs show as blank or `<hidden>`**

The daemon doesn't have Location permission. Run `macwifi diagnose` to check
the authorization status. If it shows `2` (denied):

```sh
tccutil reset Location dev.macwifi.macwifi
# Then restart the daemon:
launchctl kickstart -k gui/$(id -u)/dev.macwifi.daemon
```

**"connection refused" or "no such file" on launch**

The daemon isn't running. Check if the socket exists:

```sh
ls ~/Library/Application\ Support/macwifi/daemon.sock
```

If not, check the logs and reinstall:

```sh
cat /tmp/macwifi-daemon.err.log
macwifi uninstall-daemon && macwifi install-daemon
```

**Keychain password prompt on every connect**

Either you haven't clicked "Always Allow" yet, or the app's code-signing
identity changed (ad-hoc rebuild). To fix permanently, create the self-signed
certificate (step 2 above) and rebuild with `CODESIGN_IDENTITY=macwifi-dev`.

**Full diagnostics**

```sh
macwifi diagnose
```

Prints the binary path, bundle status, Location auth status (client and
daemon), socket path, current SSID, and scan stats.

---

## Limitations

- **No AP / hotspot mode.** macOS doesn't expose a clean public API for this.
- **WPA Enterprise**: PEAP/MSCHAPv2 only. EAP-TLS (client certificate auth) is not implemented.
- **No preferred-network reorder / autojoin toggle** — power, list, and forget work; reorder and autojoin are TODO.
- `wifid` / `airportd` continue to own the radio; macwifi cooperates with the system stack rather than replacing it.

---

## Credits

macwifi is a macOS reimagining of [**impala**](https://github.com/pythops/impala)
by [Badr Badri / pythops](https://github.com/pythops), licensed under GPL-3.0.
No impala source code is copied, but the structure, keybindings, and behavior
closely follow impala's station-mode design — this is therefore a derivative
work under GPL-3.0 and is released under the same license.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
