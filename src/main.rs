use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use macwifi::app::App;
use macwifi::config::Config;
use macwifi::corewlan::{Security, WifiClient};
use macwifi::event::{Event, EventHandler};
use macwifi::handler;
use macwifi::networksetup;
use macwifi::terminal::Tui;
use macwifi::theme;
use macwifi::ui;
use macwifi::worker::WifiHandle;

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
        Some(c) => run_cli(c),
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
    macwifi::location::request_when_in_use();
    let mut tui = Tui::init()?;
    let result = drive(&mut tui, theme_name.as_deref()).await;
    let _ = Tui::restore();
    if let Err(e) = &result {
        eprintln!("error: {e:?}");
    }
    result
}

async fn drive(tui: &mut Tui, theme_name: Option<&str>) -> Result<()> {
    let mut events = EventHandler::new(250);
    let wifi = WifiHandle::spawn(events.tx.clone());
    let mut app = App::new(wifi, theme_name);

    if let Some(hint) = macwifi::location::redaction_hint() {
        let _ = events.tx.send(Event::Error(hint.to_string()));
    }

    while app.running {
        tui.terminal.draw(|f| ui::draw(f, &mut app))?;
        let ev = events.next().await?;
        match ev {
            Event::Tick => app.tick(),
            Event::Key(k) => handler::handle_key(&mut app, k),
            Event::Resize(_, _) => {}
            other => app.handle_event(other),
        }
    }
    Ok(())
}

fn run_cli(cmd: Cmd) -> Result<()> {
    let client = WifiClient::shared()?;
    let iface = client.default_interface()?;

    match cmd {
        Cmd::Status => {
            let s = iface.state()?;
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
        Cmd::Scan => {
            macwifi::location::request_when_in_use();
            let mut nets = iface.scan()?;
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
            let all_blank = !nets.is_empty()
                && nets
                    .iter()
                    .all(|n| n.ssid.as_deref().map_or(true, str::is_empty));
            if all_blank {
                eprintln!();
                eprintln!(
                    "! All SSIDs are blank. Grant Location Services to your terminal app"
                );
                eprintln!("! (System Settings → Privacy & Security → Location Services).");
            }
        }
        Cmd::Power { state } => {
            let on = matches!(state, PowerState::On);
            if let Err(e) = iface.set_power(on) {
                eprintln!("CoreWLAN setPower failed ({e}); falling back to networksetup");
                networksetup::set_power(&iface.name(), on)?;
            }
        }
        Cmd::Connect { ssid, password } => {
            match password {
                Some(p) => iface.associate_psk(&ssid, &p)?,
                None => iface.associate_open(&ssid)?,
            }
            println!("connected to {ssid}");
        }
        Cmd::ConnectHidden { ssid, password } => {
            iface.scan_for_ssid(&ssid)?;
            match password {
                Some(p) => iface.associate_psk(&ssid, &p)?,
                None => iface.associate_open(&ssid)?,
            }
            println!("connected to {ssid} (hidden)");
        }
        Cmd::ConnectPeap {
            ssid,
            username,
            password,
        } => {
            iface.associate_peap(&ssid, &username, &password)?;
            println!("connected to {ssid} (PEAP)");
        }
        Cmd::Disconnect => {
            iface.disassociate();
            println!("disassociated");
        }
        Cmd::Preferred => {
            let name = iface.name();
            for ssid in networksetup::list_preferred(&name)? {
                println!("{ssid}");
            }
        }
        Cmd::Forget { ssid } => {
            networksetup::remove_preferred(&iface.name(), &ssid)?;
            println!("removed {ssid} from preferred networks");
        }
        Cmd::Themes => unreachable!(),
        Cmd::Diagnose => {
            use objc2_core_location::CLLocationManager;
            use std::io::Write;
            // When stdout is not a tty (e.g. launched via `open .app`), also
            // append to /tmp/macwifi-diagnose.log so we can read the result.
            let log_path = "/tmp/macwifi-diagnose.log";
            let mut log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
                .ok();
            macro_rules! line {
                ($($a:tt)*) => {{
                    let s = format!($($a)*);
                    println!("{s}");
                    if let Some(f) = log_file.as_mut() { let _ = writeln!(f, "{s}"); }
                }};
            }
            let exe = std::env::current_exe().ok();
            line!("== macwifi diagnose {} ==", chrono_now());
            if let Some(e) = &exe {
                line!("executable        : {}", e.display());
                let bundled = e.to_string_lossy().contains(".app/Contents/MacOS/");
                line!("bundled           : {bundled}");
            }
            line!("parent pid        : {}", unsafe { libc_getppid() });
            // Fire the Location prompt from the terminal session so it has
            // time to actually render and the user can click Allow.
            macwifi::location::request_when_in_use();
            unsafe {
                let mgr = CLLocationManager::new();
                let status = mgr.authorizationStatus();
                line!("location auth     : {status:?}  (0=notDet 1=restr 2=denied 3=always 4=whenInUse)");
            }
            let s = iface.state()?;
            line!("interface         : {}", s.name);
            line!("hw addr           : {}", s.hw_address.as_deref().unwrap_or("-"));
            line!("current SSID      : {}", s.ssid.as_deref().unwrap_or("-"));
            line!("current BSSID     : {}", s.bssid.as_deref().unwrap_or("-"));
            line!("RSSI              : {} dBm", s.rssi);
            let nets = iface.scan()?;
            let blank = nets
                .iter()
                .filter(|n| n.ssid.as_deref().map_or(true, str::is_empty))
                .count();
            line!(
                "scan              : {} networks, {} blank",
                nets.len(),
                blank
            );
            for n in nets.iter().take(5) {
                line!(
                    "  ssid={:?}  rssi={}  ch={:?}",
                    n.ssid, n.rssi, n.channel
                );
            }
            line!("");
        }
    }
    Ok(())
}

unsafe extern "C" {
    fn getppid() -> i32;
}
unsafe fn libc_getppid() -> i32 {
    unsafe { getppid() }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("epoch={}", d.as_secs()))
        .unwrap_or_else(|_| "epoch=?".into())
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
