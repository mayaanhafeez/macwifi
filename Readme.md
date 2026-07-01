# macwifi

A TUI for managing Wi-Fi on macOS. A clean-room macOS port of
[impala](https://github.com/pythops/impala) (Linux/iwd) — same idea, same
keybindings where they map, but built on CoreWLAN and `networksetup(8)`.

Station mode only. Theming, hidden networks, QR sharing, adapter info, and a
`.app` bundle that gets the Location permission story right.

## Features

- Live scan & associate (open / WPA-PSK / WPA-Enterprise PEAP/MSCHAPv2 / hidden)
- Both network lists sorted by signal strength (strongest first); out-of-range saved networks sink to the bottom
- Manage saved networks (list, remove) and the current connection (disconnect, toggle power)
- **Silent reconnect**: the password you type on first connect is cached in macwifi's own login-keychain item, so reconnecting to a saved network is promptless (see [Passwords & prompts](#passwords--prompts))
- QR-code sharing of saved networks (reads the password from the System keychain — triggers one macOS admin-auth prompt per share)
- Adapter info popup: SSID, BSSID, RSSI, noise, channel, TX rate, MAC
- **14 themes**: `default`, Catppuccin (latte/frappe/macchiato/mocha), Rose Pine (main/moon/dawn), Tokyo Night (night/storm), Gruvbox (dark/light), Nord, Dracula
- Cycle themes live with `T` / `Shift-Tab` — choice is persisted across launches
- TOML config at `~/.config/macwifi/config.toml`
- Self-contained `.app` bundle with `NSLocationUsageDescription` so scan results aren't redacted

## Demo

![macwifi TUI showing the Known Networks, New Networks, and Device tables over a desktop wallpaper](images/demo.png)

Three stacked tables — **Known Networks** (in-range saved profiles; press `A`
to also list out-of-range ones), **New Networks** (in-range scan results), and
**Device** (current interface) — both network lists sorted
strongest-signal-first.

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

macwifi's `.app` bundle should be code-signed with a **stable** identity. Two
things are bound to that identity and break if it changes between rebuilds:

- The **Location permission** (TCC) grant that un-redacts SSIDs.
- macwifi's **cached Wi-Fi passwords** in the login keychain — the ACL on those
  items is tied to the app's code signature, so a new signature means macwifi
  can no longer read its own cache silently and you re-enter passwords.

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
> unset and the bundle will be ad-hoc signed. It works, but every rebuild gets a
> new signature, so you'll have to re-grant Location and re-enter saved Wi-Fi
> passwords after each rebuild.

---

### 3. Build and bundle

Clone the repo and build the release binary, then wrap it in a `.app` bundle:

```sh
git clone https://github.com/mayaanhafeez/macwifi
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
stable path (it is hardcoded to `/Applications/macwifi.app`). Use `ditto`, not
`cp -R` — `cp -R` corrupts the code signature, after which macOS kills every
launch with `Killed: 9` (exit 137) and the daemon never starts:

```sh
ditto target/release/macwifi.app /Applications/macwifi.app
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

The first time you connect to a secured network you'll be asked for its
password. After that, macwifi caches it (in its own login-keychain item) and
reconnects silently — no further prompts. See
[Passwords & prompts](#passwords--prompts) for the full picture.

---

## Rebuilding after changes

Use the reinstall script — it rebuilds, re-bundles, refreshes
`/Applications/macwifi.app`, **re-signs** it (mandatory — see below), and
re-bootstraps the daemon, with a sanity check that catches a bad signature:

```sh
CODESIGN_IDENTITY=macwifi-dev ./scripts/reinstall.sh   # or without CODESIGN_IDENTITY for ad-hoc
```

> **Why the script instead of `cp`?** Copying a signed `.app` with `cp -R`
> corrupts its signature; macOS then SIGKILLs the binary on every launch
> (`Killed: 9` / exit 137) and the daemon silently fails to start. The script
> uses `ditto` and re-signs in place, then verifies the result launches before
> bootstrapping.

> **Keychain cache and rebuilding:** with a stable `CODESIGN_IDENTITY`, the
> signature is identical across builds, so macwifi's cached Wi-Fi passwords (and
> the Location grant) survive rebuilds. With ad-hoc signing (`-`), every build
> gets a new signature and you re-enter passwords + re-grant Location.

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

macwifi's cached Wi-Fi passwords live in your login keychain under the service
name `macwifi-wifi`. `Forget` removes them per-network; to clear them all at
once:

```sh
while security delete-generic-password -s macwifi-wifi >/dev/null 2>&1; do :; done
```

---

## Usage

| Key | Action |
|-----|--------|
| `Tab` | Toggle focus between Known Networks / New Networks lists |
| `j` / `k` / `↓` / `↑` | Move selection |
| `Enter` | Connect (password / enterprise overlays appear as needed) |
| `s` | Rescan |
| `o` | Toggle radio power on/off |
| `d` | Remove the selected saved network (Known Networks) |
| `x` | Disconnect |
| `p` | Share selected saved network as a QR code (Known Networks) |
| `h` | Connect to a hidden network (New Networks) |
| `i` | Adapter info popup |
| `a` | Show all networks — disable RSSI/SSID filter (New Networks) |
| `A` | Also show out-of-range saved networks — default: in-range only (Known Networks) |
| `T` | Cycle theme forward |
| `Shift-Tab` | Cycle theme backward |
| `q` / `Ctrl-C` | Quit |
| `Esc` | Dismiss the current overlay |

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

## Passwords & prompts

macOS stores Wi-Fi passwords in the root-owned **System keychain**, where each
item is guarded by a partition-list ACL that `securityd` enforces by the
caller's *code signature*, not its uid. A third-party app like macwifi can't
read those items silently — and neither can a root helper (verified: the read
fails with `errSecAuthFailed` even as root). So macwifi never relies on reading
the System keychain for everyday use:

- **Connecting / reconnecting / forgetting / power / scan** — all go through
  CoreWLAN and `networksetup`, where the system's own `wifid` handles any
  credential lookup. **Silent, no prompts.**
- **First connect to a secured network** — you type the password once. macwifi
  associates *and* caches that password in its **own** login-keychain item
  (`service=macwifi-wifi`), which it can read back silently because it owns the
  item under its stable signing identity.
- **Reconnecting to a saved network** — macwifi reads its cached password and
  reconnects with no prompt. If there's no cache (e.g. a network saved outside
  macwifi), it tries the system auto-join, and only asks for the password if
  that fails.
- **QR-share** — the one exception. It reads the password from the *System*
  keychain, which triggers **one admin-auth dialog per share**. Unavoidable for
  any third-party app.

This means macwifi keeps a second, app-scoped copy of each password you connect
with (in your login keychain). `Forget` deletes both the saved network and
macwifi's cached copy. The full rationale — and why the earlier "root keychain
helper" design was abandoned — is in `ARCHITECTURE_PASSWORDS.md`.

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

**Asked for the password every time you reconnect to a saved network**

macwifi's cache can't be read silently — usually because the code-signing
identity changed (an ad-hoc rebuild, or a different `CODESIGN_IDENTITY`), which
invalidates the ACL on the cached items. Fix it by always rebuilding with the
same cert: `CODESIGN_IDENTITY=macwifi-dev ./scripts/reinstall.sh`. You'll
re-enter each password once more, after which reconnects are silent again.

**Admin-password dialog when sharing a QR code**

Expected. QR-share reads the password from the *System* keychain, which macOS
guards with an admin-auth prompt for any third-party app — there is no silent
path (not even as root). Enter your login password to continue, or cancel to
share the SSID without the password. See `ARCHITECTURE_PASSWORDS.md` for why.

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
