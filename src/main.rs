use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use macwifi::app::App;
use macwifi::client::{RemoteWifiHandle, cli_one_shot};
use macwifi::config::Config;
use macwifi::corewlan::Security;
use macwifi::event::{Event, UiEvent, UiEventHandler};
use macwifi::handler;
use macwifi::terminal::Tui;
use macwifi::theme;
use macwifi::ui;
use macwifi::worker::{Request, ShareSecurity, WifiHandle};

#[derive(Parser)]
#[command(version, about = "macOS port of impala — Wi-Fi from the terminal")]
struct Cli {
    /// Override the theme. See `macwifi themes` for the full list.
    #[arg(long, global = true)]
    theme: Option<String>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    Scan,
    Power {
        state: PowerState,
    },
    Connect {
        ssid: String,
        password: Option<String>,
    },
    ConnectHidden {
        ssid: String,
        password: Option<String>,
    },
    ConnectPeap {
        ssid: String,
        username: String,
        password: String,
    },
    Disconnect,
    Preferred,
    Forget {
        ssid: String,
    },
    Themes,
    Diagnose,
    /// Run the daemon (invoked by the LaunchAgent — not for end users).
    Daemon,
    /// Install the LaunchAgent so the daemon starts at login.
    InstallDaemon,
    /// Remove the LaunchAgent and stop the daemon.
    UninstallDaemon,
    /// Add the daemon to the keychain ACL of every saved Wi-Fi network so
    /// joins are silent (no per-connect admin prompt). Prompts admin once.
    PreGrant,
    /// Internal helper invoked by `pre-grant` after re-execing under sudo.
    #[command(hide = true)]
    PreGrantInternal { ssids: Vec<String> },
}

#[derive(ValueEnum, Clone, Copy)]
enum PowerState {
    On,
    Off,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Themes) => {
            for t in theme::ALL {
                println!("{}", t.name);
            }
            Ok(())
        }
        Some(Cmd::InstallDaemon) => macwifi::install::install(),
        Some(Cmd::UninstallDaemon) => macwifi::install::uninstall(),
        Some(Cmd::PreGrant) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_pre_grant())
        }
        Some(Cmd::PreGrantInternal { ssids }) => run_pre_grant_internal(ssids),
        Some(c) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_cli(c))
        }
        None => {
            let cfg = Config::load().unwrap_or_default();
            let theme_name = cli.theme.or(cfg.theme);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_tui(theme_name))
        }
    }
}

async fn run_tui(theme_name: Option<String>) -> Result<()> {
    let mut tui = Tui::init()?;
    let result = drive(&mut tui, theme_name.as_deref()).await;
    let _ = Tui::restore();
    if let Err(e) = &result {
        eprintln!("error: {e:?}");
    }
    result
}

async fn drive(tui: &mut Tui, theme_name: Option<&str>) -> Result<()> {
    let mut ui_events = UiEventHandler::new(250);
    let (wire_tx, mut wire_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let remote = RemoteWifiHandle::connect(wire_tx.clone()).await?;
    let wifi = WifiHandle::Remote(remote.clone());
    let mut app = App::new(wifi, theme_name);

    // Ask the daemon for an initial snapshot so the TUI isn't blank on open.
    remote.send(Request::RefreshState);
    remote.send(Request::RefreshPreferred);
    remote.send(Request::Scan);

    while app.running {
        tui.terminal.draw(|f| ui::draw(f, &mut app))?;
        tokio::select! {
            ui_ev = ui_events.next() => match ui_ev? {
                UiEvent::Tick => app.tick(),
                UiEvent::Key(k) => handler::handle_key(&mut app, k),
                UiEvent::Resize(_, _) => {}
            },
            Some(wire_ev) = wire_rx.recv() => {
                app.handle_event(wire_ev);
            }
        }
    }
    Ok(())
}

async fn run_cli(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Daemon => macwifi::daemon::run().await,
        Cmd::Status => {
            let evs = cli_one_shot(Request::RefreshState, |e| matches!(e, Event::State(_))).await?;
            for ev in evs {
                if let Event::State(s) = ev {
                    println!("interface : {}", s.name);
                    println!("powered   : {}", s.powered);
                    println!("hw addr   : {}", s.hw_address.as_deref().unwrap_or("-"));
                    println!("ssid      : {}", s.ssid.as_deref().unwrap_or("-"));
                    println!("bssid     : {}", s.bssid.as_deref().unwrap_or("-"));
                    println!("rssi      : {} dBm", s.rssi);
                    println!("noise     : {} dBm", s.noise);
                    println!("tx rate   : {} Mbps", s.tx_rate);
                    println!(
                        "channel   : {}",
                        s.channel.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                    );
                }
            }
            Ok(())
        }
        Cmd::Scan => {
            let evs = cli_one_shot(Request::Scan, |e| matches!(e, Event::ScanResult(_))).await?;
            for ev in evs {
                if let Event::ScanResult(mut nets) = ev {
                    nets.sort_by_key(|n| -n.rssi);
                    println!(
                        "{:<32}  {:>5}  {:>4}  {:<10}  {}",
                        "SSID", "RSSI", "CH", "SEC", "BSSID"
                    );
                    for n in &nets {
                        println!(
                            "{:<32}  {:>5}  {:>4}  {:<10}  {}",
                            n.ssid.as_deref().unwrap_or("<hidden>"),
                            n.rssi,
                            n.channel
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "?".into()),
                            sec_label(n.security),
                            n.bssid.as_deref().unwrap_or("-"),
                        );
                    }
                }
            }
            Ok(())
        }
        Cmd::Power { state } => {
            let on = matches!(state, PowerState::On);
            let evs = cli_one_shot(Request::SetPower(on), is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::Connect { ssid, password } => {
            let req = match password {
                Some(p) => Request::JoinWithPassword { ssid, password: p },
                None => Request::Associate(macwifi::worker::Associate {
                    ssid,
                    kind: macwifi::worker::AssociateKind::Open,
                }),
            };
            let evs = cli_one_shot(req, is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::ConnectHidden { ssid, password } => {
            let req = Request::Associate(macwifi::worker::Associate {
                ssid,
                kind: macwifi::worker::AssociateKind::Hidden(password),
            });
            let evs = cli_one_shot(req, is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::ConnectPeap {
            ssid,
            username,
            password,
        } => {
            let req = Request::Associate(macwifi::worker::Associate {
                ssid,
                kind: macwifi::worker::AssociateKind::Peap { username, password },
            });
            let evs = cli_one_shot(req, is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::Disconnect => {
            let evs = cli_one_shot(Request::Disconnect, is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::Preferred => {
            let evs = cli_one_shot(Request::RefreshPreferred, |e| {
                matches!(e, Event::PreferredResult(_))
            })
            .await?;
            for ev in evs {
                if let Event::PreferredResult(v) = ev {
                    for ssid in v {
                        println!("{ssid}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Forget { ssid } => {
            let evs = cli_one_shot(Request::Forget(ssid), is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::Themes
        | Cmd::InstallDaemon
        | Cmd::UninstallDaemon
        | Cmd::PreGrant
        | Cmd::PreGrantInternal { .. } => unreachable!(),
        Cmd::Diagnose => run_diagnose().await,
        // ShareReady never comes from the CLI; placeholder for completeness.
    }
}

async fn run_pre_grant() -> Result<()> {
    // macOS prompts admin auth on every System keychain ACL write and
    // sudo / AuthorizationCreate don't batch the rights — every
    // SecKeychainItemSetAccess hits SecurityAgent independently. So
    // automation can't make this a one-prompt flow. The least-bad UX is
    // to print the SSID list, open Keychain Access at the System keychain,
    // and tell the user how to drag the daemon onto each item.
    let evs = cli_one_shot(Request::RefreshPreferred, |e| {
        matches!(e, Event::PreferredResult(_))
    })
    .await?;
    let ssids = evs
        .into_iter()
        .find_map(|ev| match ev {
            Event::PreferredResult(v) => Some(v),
            _ => None,
        })
        .unwrap_or_default();
    if ssids.is_empty() {
        println!("no saved Wi-Fi networks found");
        return Ok(());
    }
    println!("Your saved Wi-Fi networks ({}):", ssids.len());
    for s in &ssids {
        println!("  - {s}");
    }
    println!();
    println!("macOS requires admin auth on every System-keychain ACL write");
    println!("and won't batch them, so do this manually (per SSID you use):");
    println!();
    println!("  1. The Keychain Access window will open at the System keychain");
    println!("  2. Search for the SSID");
    println!("  3. Double-click → Access Control tab → '+' button");
    println!("  4. Cmd-Shift-G, paste: /Applications/macwifi.app/Contents/MacOS/macwifi");
    println!("  5. Add → Save Changes → admin password");
    println!();
    println!("You only need to do this for SSIDs you actively use. Press Enter");
    println!("to open Keychain Access, or Ctrl-C to skip.");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    std::process::Command::new("open")
        .args([
            "-a",
            "Keychain Access",
            "/Library/Keychains/System.keychain",
        ])
        .status()
        .ok();
    Ok(())
}

fn run_pre_grant_internal(_ssids: Vec<String>) -> Result<()> {
    // Retained as a hidden subcommand only for backwards compatibility with
    // a possible sudo re-exec from older copies of the binary; not used in
    // the current flow.
    anyhow::bail!("pre-grant-internal is no longer used; run `macwifi pre-grant`")
}

fn is_notice_or_error(ev: &Event) -> bool {
    matches!(ev, Event::Notice(_) | Event::Error(_))
}

fn print_terminal_event(evs: &[Event]) {
    if let Some(ev) = evs.last() {
        match ev {
            Event::Notice(s) => println!("{s}"),
            Event::Error(s) => eprintln!("error: {s}"),
            _ => {}
        }
    }
}

async fn run_diagnose() -> Result<()> {
    use objc2_core_location::CLLocationManager;
    let exe = std::env::current_exe().ok();
    println!("== macwifi diagnose (client) ==");
    if let Some(e) = &exe {
        println!("executable        : {}", e.display());
        let bundled = e.to_string_lossy().contains(".app/Contents/MacOS/");
        println!("bundled           : {bundled}");
    }
    println!("parent pid        : {}", unsafe { libc::getppid() });
    unsafe {
        let mgr = CLLocationManager::new();
        let status = mgr.authorizationStatus();
        println!(
            "location auth     : {status:?}  (0=notDet 1=restr 2=denied 3=always 4=whenInUse)"
        );
    }
    println!("socket path       : {}", macwifi::ipc::socket_path().display());
    println!();
    println!("== macwifi diagnose (daemon) ==");
    match cli_one_shot(Request::Diagnose, |e| matches!(e, Event::DaemonDiagnose(_))).await {
        Ok(evs) => {
            for ev in evs {
                if let Event::DaemonDiagnose(d) = ev {
                    println!("daemon pid        : {}", d.pid);
                    println!("daemon parent pid : {}", d.parent_pid);
                    println!(
                        "daemon location   : {}  (0=notDet 1=restr 2=denied 3=always 4=whenInUse)",
                        d.location_auth_raw
                    );
                    println!("interface         : {}", d.interface);
                    println!(
                        "current SSID      : {}",
                        d.current_ssid.as_deref().unwrap_or("-")
                    );
                    println!(
                        "scan              : {} networks, {} blank",
                        d.scan_count, d.scan_blank
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("daemon section unavailable: {e:#}");
            eprintln!("(try `macwifi install-daemon`)");
        }
    }
    Ok(())
}

fn sec_label(s: Security) -> &'static str {
    match s {
        Security::Open => "open",
        Security::Wep => "WEP",
        Security::WpaPersonal => "WPA",
        Security::Wpa2Personal => "WPA2",
        Security::Wpa3Personal => "WPA3",
        Security::WpaEnterprise => "WPA-E",
        Security::Wpa2Enterprise => "WPA2-E",
        Security::Wpa3Enterprise => "WPA3-E",
        Security::Unknown => "?",
    }
}

#[allow(dead_code)]
fn _suppress_unused(_: ShareSecurity) {}
