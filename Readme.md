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
- Process API for Wi-Fi share URIs so other apps can render or transmit QR codes
- Adapter info popup: SSID, BSSID, RSSI, noise, channel, TX rate, MAC
- Download, upload, and latency tests with Apple, Ookla, Netflix, or custom providers
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

### Option A: Homebrew

```sh
brew install mayaanhafeez/tap/macwifi
```

This builds macwifi from source (cargo + the project's own bundling script),
ad-hoc signs it, installs `macwifi.app` to `/Applications`, and registers the
LaunchAgent daemon automatically. On first launch, macOS will prompt for
Location permission — click **Allow While Using App**.

Ad-hoc signing means the signature isn't stable across rebuilds: if you
reinstall/upgrade later, you'll be asked to re-grant Location and re-enter any
saved Wi-Fi passwords once (see [Passwords & prompts](#passwords--prompts)).
If that matters to you, use the manual setup below instead, which walks
through creating a stable self-signed cert first.

To uninstall: `brew uninstall --cask mayaanhafeez/tap/macwifi` (add `--zap` to
also remove `~/.config/macwifi` and `~/Library/Application Support/macwifi`).

### Option B: Manual setup

Gives you full control over code signing (recommended if you want a stable
identity so Keychain grants survive rebuilds) and matches how the project is
developed day to day.

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
macwifi speedtest
macwifi speedtest --provider ookla --format json
macwifi speedtest --provider netflix
macwifi install-daemon
macwifi uninstall-daemon
```

### Speed tests and structured output

`macwifi speedtest` uses macOS's built-in `networkQuality` by default, so it has
no extra dependencies. Other providers use their official or commonly used
CLI:

| Provider | Requirement | Command |
|---|---|---|
| `apple` | Included with macOS | `networkQuality` |
| `ookla` | [Official Speedtest CLI](https://www.speedtest.net/apps/cli) | `speedtest` |
| `netflix` | [fast-cli](https://github.com/sindresorhus/fast-cli) | `fast` |
| `custom` | Any executable that implements the JSON contract below | Configurable |

Install the optional providers:

```sh
# Official Ookla CLI. Current Homebrew requires explicitly trusting its tap.
brew tap teamookla/speedtest
brew trust teamookla/speedtest
brew install teamookla/speedtest/speedtest

# Netflix Fast.com CLI and its Puppeteer browser dependency.
npm install --global --allow-scripts=puppeteer fast-cli
```

The Netflix adapter uses `PUPPETEER_EXECUTABLE_PATH` when it is set. Otherwise,
it automatically uses Google Chrome from `/Applications` when available, then
falls back to fast-cli's Puppeteer-managed browser.

#### Speed-test command list

```text
macwifi speedtest [OPTIONS]

--provider <apple|ookla|netflix|custom>  Backend; default comes from config
--format <text|json|jsonl>               Live terminal, final JSON, or event stream
--timeout <SECONDS>                      Override the provider-aware time limit
--server-id <ID>                         Specific Ookla server
--custom-command <PATH>                  Custom provider executable
--custom-arg <VALUE>                     Custom argument; repeat for multiple values
```

Examples:

```sh
macwifi speedtest --provider apple --timeout 45
macwifi speedtest --provider ookla --server-id 12345
macwifi speedtest --provider netflix --format json
macwifi speedtest --provider apple --format jsonl
macwifi speedtest --provider custom --custom-command ./my-speedtest --custom-arg value
```

All providers default to a five-minute measurement window so tests on slow
connections can finish. Apple allows up to five additional seconds for
`networkQuality` to serialize its result. Use `--timeout` to override the limit
for one run.

#### Process API

`--format json` writes one JSON object to stdout and diagnostics to stderr. The
normalized schema is versioned, making this the recommended interface for web,
desktop, and mobile front ends that launch macwifi as a subprocess:

```json
{"schema_version":1,"provider":"apple","download_mbps":102.4,"upload_mbps":21.8,"ping_ms":15.2,"responsiveness_rpm":540.0,"bytes_downloaded":48123904,"bytes_uploaded":10485760,"interface":"en0","server":{"host":"example.apple.com"},"duration_ms":15342}
```

The process exits with status `0` after a valid result and non-zero on provider,
timeout, or parsing errors. Final JSON is the only stdout line in `json` mode,
so callers can decode it directly as `SpeedtestResult`.

For live integrations, `--format jsonl` writes newline-delimited `started`,
`progress`, `complete`, and `failed` events. Progress includes live download
and upload throughput sampled from the active network interface every 500 ms,
plus an updated RTT probe and elapsed time. Human-readable terminal mode shows
the same changing values on one line. Live throughput is interface-wide and
can include unrelated traffic; the provider's `complete` event remains the
authoritative result.

Example JSON Lines stream:

```jsonl
{"type":"started","schema_version":1,"provider":"ookla"}
{"type":"progress","schema_version":1,"provider":"ookla","elapsed_ms":2502,"download_mbps":34.2,"upload_mbps":1.1,"ping_ms":24.8}
{"type":"complete","schema_version":1,"result":{"schema_version":1,"provider":"ookla","download_mbps":38.9,"upload_mbps":11.6,"ping_ms":18.6,"duration_ms":28798}}
```

Consumers should switch on `type` and ignore unknown fields for forward
compatibility. A failed stream ends with a `failed` event containing `error`
and then exits non-zero.

#### Rust API

Rust clients can call the same implementation directly through the public
`macwifi::speedtest::run(&SpeedtestOptions)` async API and receive a
serializable `SpeedtestResult`. This avoids running an HTTP server or parsing
terminal text while still allowing an API service to wrap the library later.
Use `run_with_progress` to receive typed `SpeedtestEvent` callbacks while the
test is running.

```rust,no_run
use std::time::Duration;
use macwifi::speedtest::{run_with_progress, SpeedtestOptions, SpeedtestProvider};

# async fn example() -> anyhow::Result<()> {
let options = SpeedtestOptions {
    provider: SpeedtestProvider::Ookla,
    timeout: Duration::from_secs(90),
    ..SpeedtestOptions::default()
};

let result = run_with_progress(&options, |event| {
    // Forward this serializable event to a WebSocket, SSE stream, or UI state.
    println!("{}", serde_json::to_string(&event).unwrap());
}).await?;

println!("final download: {:?} Mbps", result.download_mbps);
# Ok(())
# }
```

macwifi does not currently open an HTTP port. Front ends can use the stable
JSON/JSONL subprocess protocol, link the Rust library directly, or expose the
Rust events through their own HTTP, Server-Sent Events, or WebSocket service
without changing the measurement implementation.

#### Custom provider API

A custom provider is invoked directly without a shell. It must print a JSON
object containing at least one of `download_mbps`, `upload_mbps`, or `ping_ms`.
It may also provide `loaded_latency_ms`, `jitter_ms`, `packet_loss_percent`,
`bytes_downloaded`, `bytes_uploaded`, `interface`, `result_url`, `timestamp`,
and `server` (`id`, `name`, `location`, `host`). macwifi adds the provider,
schema version, and measured duration.

---

## Configuration

Optional file at `~/.config/macwifi/config.toml`:

```toml
theme = "catppuccin-mocha"

[speedtest]
provider = "apple"
# timeout_seconds = 600 # Optional override; defaults depend on provider
# custom_command = "/path/to/my-speedtest"
# custom_args = ["--some-option"]
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
- **QR-share** — it first reads macwifi's app-owned cache silently. For a
  network saved outside macwifi, the first share reads the *System* keychain,
  triggers an admin-auth dialog, and caches the result. Later shares are silent.

### Wi-Fi share process API

Other apps can request the standard `WIFI:` payload through the existing
same-user daemon API:

```sh
# WPA/WPA2/WPA3 Personal (default)
macwifi share "Home WiFi"

# Versioned JSON for applications
macwifi share "Home WiFi" --json

# WEP or open networks
macwifi share "Legacy WiFi" --security wep --json
macwifi share "Cafe WiFi" --security open --json
```

The plain command writes only the URI to stdout:

```text
WIFI:T:WPA;S:Home WiFi;P:correct horse battery staple;;
```

JSON output is a single object suitable for subprocess integrations:

```json
{"schema_version":1,"ssid":"Home WiFi","uri":"WIFI:T:WPA;S:Home WiFi;P:correct horse battery staple;;","has_password":true}
```

The URI contains the password and must be treated as a secret. The daemon only
accepts Unix-socket clients running as the same macOS user. macwifi does not
open an HTTP port or expose this API to the network.

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

Expected on the first share when the network was saved outside macwifi. The
System keychain read requires admin authentication; after a successful read,
macwifi stores an app-owned copy and later shares are silent. Cancelling shares
the SSID without a password. See `ARCHITECTURE_PASSWORDS.md` for why the first
protected read cannot be avoided.

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
