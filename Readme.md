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
- Cycle themes live with `T`
- Optional TOML config at `~/.config/macwifi/config.toml`
- Self-contained `.app` bundle with `NSLocationUsageDescription` so scan results aren't redacted

## Demo

```text
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

## Prerequisites

- macOS 13 (Ventura) or newer — CoreWLAN scanning requires Location Services.
- Rust toolchain. If you don't have `cargo`:
  ```sh
  brew install rustup-init && rustup-init -y
  # or
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

## Install

```sh
git clone https://github.com/<you>/macwifi
cd macwifi
just install                    # builds + bundles + symlinks to /usr/local/bin/macwifi
```

Then:

```sh
macwifi                         # launch the TUI
```

If you don't have `just`, the manual recipe is:

```sh
cargo build --release
./scripts/bundle.sh
ln -sf "$PWD/target/release/macwifi.app/Contents/MacOS/macwifi" /usr/local/bin/macwifi
```

## Usage

| key | action |
|-----|--------|
| `Tab` / `Shift-Tab` | toggle list focus / cycle theme back |
| `j` / `k` / `↓` / `↑` | move selection |
| `Enter` | connect (PSK/open/PEAP overlays appear as needed) |
| `s` | rescan |
| `o` | toggle radio power |
| `d` | forget the selected saved network |
| `x` | disconnect |
| `p` | share the selected saved network as a QR code |
| `h` | connect to a hidden network |
| `i` | adapter info popup |
| `a` | show all (un-filter blank/weak networks) |
| `T` | cycle theme forward |
| `q` / `Esc` / `Ctrl-C` | quit (or cancel overlay) |

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
```

## Configuration

Optional file at `~/.config/macwifi/config.toml`:

```toml
theme = "catppuccin-mocha"
```

CLI flag `--theme <name>` overrides config.

## Location permission

CoreWLAN's `scanForNetworks*` API silently consults TCC and **returns blank
SSIDs** unless the calling binary has Location authorization.

The bundled `.app` declares `NSLocationUsageDescription` and explicitly calls
`CLLocationManager.requestWhenInUseAuthorization()` at startup, so on first
launch macOS pops the system dialog. Click **Allow**.

If you ever denied and want to redo the prompt:

```sh
tccutil reset Location dev.macwifi.macwifi
```

If you run an unbundled `cargo run`-style binary, grant Location to your
terminal app itself (System Settings → Privacy & Security → Location
Services → Terminal/iTerm). The grant propagates to child processes.

## Limitations

- **No AP / hotspot mode.** macOS doesn't expose a clean public API; Internet
  Sharing lives behind private frameworks and System Settings.
- **WPA Enterprise**: PEAP/MSCHAPv2 only. EAP-TLS (Keychain client identities)
  is not implemented.
- **No preferred-network reorder / autojoin toggle** yet — power, list, and
  forget work; reorder/autojoin TODO.
- `wifid`/`airportd` continue to own the radio; macwifi cooperates with the
  system stack rather than replacing it (the way impala replaces NetworkManager).

## Building from source

```sh
cargo build --release            # debug builds work but the .app expects release
./scripts/bundle.sh              # creates target/release/macwifi.app
```

The bundle is ad-hoc signed with `codesign -s -`. For distribution beyond your
own Mac, swap the identity in `scripts/bundle.sh` for a Developer ID and
notarize with `xcrun notarytool`.

## Credits

macwifi is a macOS reimagining of [**impala**](https://github.com/pythops/impala)
by [Badr Badri / pythops](https://github.com/pythops), licensed under GPL-3.0.
No impala source code is copied, but the structure, keybindings, and
behavior closely follow impala's station-mode design — this is therefore a
derivative work under GPL-3.0 and is released under the same license.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
