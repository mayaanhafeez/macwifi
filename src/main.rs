use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use macwifi::app::App;
use macwifi::client::{RemoteWifiHandle, cli_one_shot};
use macwifi::config::Config;
use macwifi::corewlan::Security;
use macwifi::event::{Event, UiEvent, UiEventHandler};
use macwifi::handler;
use macwifi::speedtest::{SpeedtestEvent, SpeedtestOptions, SpeedtestProvider, SpeedtestResult};
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
    /// Produce a standard Wi-Fi sharing URI for QR-code generators and apps.
    Share {
        ssid: String,
        /// Network security type.
        #[arg(long, value_enum, default_value_t = ShareSecurityArg::Wpa)]
        security: ShareSecurityArg,
        /// Machine-readable output containing the schema version and URI.
        #[arg(long)]
        json: bool,
    },
    Forget {
        ssid: String,
    },
    Themes,
    Diagnose,
    /// Measure download speed, upload speed, and latency.
    Speedtest {
        /// Test backend. Apple requires no additional installation.
        #[arg(long, value_enum)]
        provider: Option<SpeedtestProvider>,
        /// Output format. JSONL streams progress and the final result.
        #[arg(long, value_enum, default_value_t = SpeedtestOutput::Text)]
        format: SpeedtestOutput,
        /// Maximum test duration in seconds.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// Select a specific Ookla server.
        #[arg(long)]
        server_id: Option<u64>,
        /// Executable that emits macwifi speed-test JSON (custom provider only).
        #[arg(long)]
        custom_command: Option<PathBuf>,
        /// Argument passed to the custom executable. May be repeated.
        #[arg(long)]
        custom_arg: Vec<String>,
    },
    /// Run the daemon (invoked by the LaunchAgent — not for end users).
    Daemon,
    /// Install the LaunchAgent so the daemon starts at login.
    InstallDaemon,
    /// Remove the LaunchAgent and stop the daemon.
    UninstallDaemon,
}

#[derive(ValueEnum, Clone, Copy)]
enum PowerState {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum ShareSecurityArg {
    Wpa,
    Wep,
    Open,
}

impl From<ShareSecurityArg> for ShareSecurity {
    fn from(value: ShareSecurityArg) -> Self {
        match value {
            ShareSecurityArg::Wpa => Self::Wpa,
            ShareSecurityArg::Wep => Self::Wep,
            ShareSecurityArg::Open => Self::Nopass,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SpeedtestOutput {
    Text,
    Json,
    Jsonl,
}

impl std::fmt::Display for SpeedtestOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        })
    }
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
    // Scope the `Tui` so its `Drop` restores the terminal exactly once, before
    // we print any error. Calling `Tui::restore()` here *and* letting `Drop`
    // run would emit `LeaveAlternateScreen` twice, which corrupts some
    // terminal emulators.
    let result = {
        let mut tui = Tui::init()?;
        drive(&mut tui, theme_name.as_deref()).await
    };
    if let Err(e) = &result {
        eprintln!("error: {e:?}");
    }
    result
}

async fn drive(tui: &mut Tui, theme_name: Option<&str>) -> Result<()> {
    let mut ui_events = UiEventHandler::new(250);
    let (wire_tx, mut wire_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    // The remote handle requests an initial snapshot on every (re)connect, so
    // the TUI doesn't need to prime it here.
    let remote = RemoteWifiHandle::connect(wire_tx.clone()).await?;
    let wifi = WifiHandle::Remote(remote);
    let mut app = App::new(wifi, theme_name);

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
        Cmd::Speedtest {
            provider,
            format,
            timeout,
            server_id,
            custom_command,
            custom_arg,
        } => {
            let cfg = Config::load()?.speedtest;
            let provider = provider.unwrap_or(cfg.provider);
            let timeout = timeout
                .or(cfg.timeout_seconds)
                .map(Duration::from_secs)
                .unwrap_or_else(|| provider.default_timeout());
            let options = SpeedtestOptions {
                provider,
                timeout,
                server_id,
                custom_command: custom_command.or(cfg.custom_command),
                custom_args: if custom_arg.is_empty() {
                    cfg.custom_args
                } else {
                    custom_arg
                },
            };
            let live_terminal = std::io::stderr().is_terminal();
            let result = macwifi::speedtest::run_with_progress(&options, |event| match format {
                SpeedtestOutput::Text => render_speedtest_event(&event, live_terminal),
                SpeedtestOutput::Json => {}
                SpeedtestOutput::Jsonl => {
                    if let Ok(line) = serde_json::to_string(&event) {
                        println!("{line}");
                        let _ = std::io::stdout().flush();
                    }
                }
            })
            .await?;
            print_speedtest(&result, format)?;
            Ok(())
        }
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
                        s.channel
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "-".into()),
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
                Some(p) => macwifi::worker::join_request(ssid, p),
                None => Request::JoinSaved(ssid),
            };
            let evs = cli_one_shot(req, is_connect_terminal).await?;
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
        Cmd::Share {
            ssid,
            security,
            json,
        } => {
            let evs = cli_one_shot(
                Request::Share {
                    ssid,
                    security: security.into(),
                },
                |e| matches!(e, Event::ShareReady(_) | Event::Error(_)),
            )
            .await?;
            match evs.last() {
                Some(Event::ShareReady(payload)) if json => {
                    println!("{}", serde_json::to_string(payload)?);
                    Ok(())
                }
                Some(Event::ShareReady(payload)) => {
                    println!("{}", payload.uri);
                    Ok(())
                }
                Some(Event::Error(message)) => Err(anyhow::anyhow!(message.clone())),
                _ => Err(anyhow::anyhow!("daemon returned no share payload")),
            }
        }
        Cmd::Forget { ssid } => {
            let evs = cli_one_shot(Request::Forget(ssid), is_notice_or_error).await?;
            print_terminal_event(&evs);
            Ok(())
        }
        Cmd::Themes | Cmd::InstallDaemon | Cmd::UninstallDaemon => unreachable!(),
        Cmd::Diagnose => run_diagnose().await,
    }
}

fn is_notice_or_error(ev: &Event) -> bool {
    matches!(ev, Event::Notice(_) | Event::Error(_))
}

fn is_connect_terminal(ev: &Event) -> bool {
    is_notice_or_error(ev) || matches!(ev, Event::JoinSavedFailed { .. })
}

fn print_terminal_event(evs: &[Event]) {
    if let Some(ev) = evs.last() {
        match ev {
            Event::Notice(s) => println!("{s}"),
            Event::Error(s) => eprintln!("error: {s}"),
            Event::JoinSavedFailed { ssid, detail, .. } => {
                eprintln!("error: connect to {ssid} failed: {detail}")
            }
            _ => {}
        }
    }
}

fn print_speedtest(result: &SpeedtestResult, format: SpeedtestOutput) -> Result<()> {
    match format {
        SpeedtestOutput::Json => {
            println!("{}", serde_json::to_string(result)?);
            return Ok(());
        }
        SpeedtestOutput::Jsonl => return Ok(()),
        SpeedtestOutput::Text => {}
    }

    println!("provider : {}", result.provider);
    println!("download : {}", metric(result.download_mbps, "Mbps"));
    println!("upload   : {}", metric(result.upload_mbps, "Mbps"));
    println!("ping     : {}", metric(result.ping_ms, "ms"));
    if let Some(value) = result.jitter_ms {
        println!("jitter   : {value:.2} ms");
    }
    if let Some(value) = result.packet_loss_percent {
        println!("loss     : {value:.2}%");
    }
    if let Some(server) = &result.server {
        let label = server
            .name
            .as_deref()
            .or(server.host.as_deref())
            .unwrap_or("-");
        println!("server   : {label}");
    }
    Ok(())
}

fn render_speedtest_event(event: &SpeedtestEvent, live_terminal: bool) {
    match event {
        SpeedtestEvent::Started { provider, .. } if live_terminal => {
            eprint!("\rTesting with {provider}... 0.0s");
            let _ = std::io::stderr().flush();
        }
        SpeedtestEvent::Started { provider, .. } => eprintln!("Testing with {provider}..."),
        SpeedtestEvent::Progress {
            elapsed_ms,
            download_mbps,
            upload_mbps,
            ping_ms,
            ..
        } if live_terminal => {
            eprint!(
                "\r\x1b[2KTesting... down {}  up {}  ping {}  {:.1}s",
                live_metric(*download_mbps, "Mbps"),
                live_metric(*upload_mbps, "Mbps"),
                live_metric(*ping_ms, "ms"),
                *elapsed_ms as f64 / 1000.0,
            );
            let _ = std::io::stderr().flush();
        }
        SpeedtestEvent::Complete { .. } | SpeedtestEvent::Failed { .. } if live_terminal => {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
        _ => {}
    }
}

fn live_metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.1} {unit}"))
        .unwrap_or_else(|| format!("- {unit}"))
}

fn metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.2} {unit}"))
        .unwrap_or_else(|| "-".into())
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
    println!(
        "socket path       : {}",
        macwifi::ipc::socket_path().display()
    );
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
