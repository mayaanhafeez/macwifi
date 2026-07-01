use ratatui::widgets::TableState;
use tui_input::Input;

use crate::corewlan::{InterfaceState, ScannedNetwork, Security};
use crate::event::{Event, SharePayload};
use crate::notification::Notification;
use crate::theme::{self, Theme};
use crate::worker::{Associate, AssociateKind, Request, ShareSecurity, WifiHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Preferred,
    Available,
}

pub enum Overlay {
    None,
    Password(PasswordPrompt),
    EnterpriseUser(EnterprisePrompt),
    EnterprisePass(EnterprisePrompt),
    HiddenSsid(HiddenPrompt),
    HiddenPass(HiddenPrompt),
    Info,
    Share(SharePayload),
}

pub struct PasswordPrompt {
    pub ssid: String,
    pub input: Input,
}

pub struct EnterprisePrompt {
    pub ssid: String,
    pub username: String,
    pub input: Input,
}

pub struct HiddenPrompt {
    pub ssid: String,
    pub input: Input,
}

pub struct App {
    pub running: bool,
    pub focus: Focus,
    pub state: Option<InterfaceState>,
    pub networks: Vec<ScannedNetwork>,
    pub preferred: Vec<String>,
    pub scanning: bool,
    pub show_all: bool,
    pub show_all_preferred: bool,
    pub available_state: TableState,
    pub preferred_state: TableState,
    pub notifications: Vec<Notification>,
    pub overlay: Overlay,
    pub wifi: WifiHandle,
    pub theme: Theme,
    pub theme_index: usize,
}

impl App {
    pub fn new(wifi: WifiHandle, theme_name: Option<&str>) -> Self {
        let theme = theme_name
            .and_then(theme::by_name)
            .unwrap_or(theme::DEFAULT);
        let theme_index = theme::index_of(theme.name);
        let mut available_state = TableState::default();
        available_state.select(Some(0));
        let mut preferred_state = TableState::default();
        preferred_state.select(Some(0));
        Self {
            running: true,
            focus: Focus::Available,
            state: None,
            networks: Vec::new(),
            preferred: Vec::new(),
            scanning: true,
            show_all: false,
            show_all_preferred: false,
            available_state,
            preferred_state,
            notifications: Vec::new(),
            overlay: Overlay::None,
            wifi,
            theme,
            theme_index,
        }
    }

    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn tick(&mut self) {
        self.notifications.retain(|n| n.ttl > 0);
        for n in self.notifications.iter_mut() {
            n.ttl = n.ttl.saturating_sub(1);
        }
    }

    pub fn cycle_theme(&mut self, delta: isize) {
        let len = theme::ALL.len() as isize;
        self.theme_index = ((self.theme_index as isize + delta).rem_euclid(len)) as usize;
        self.theme = theme::ALL[self.theme_index];
        let _ = crate::config::Config::save_theme(self.theme.name);
        self.notifications
            .push(Notification::info(format!("theme: {}", self.theme.name)));
    }

    pub fn toggle_show_all(&mut self) {
        self.show_all = !self.show_all;
        self.notifications.push(Notification::info(format!(
            "show all: {}",
            if self.show_all { "on" } else { "off" }
        )));
        if !self.networks.is_empty() {
            self.available_state.select(Some(0));
        }
    }

    pub fn toggle_show_all_preferred(&mut self) {
        self.show_all_preferred = !self.show_all_preferred;
        self.notifications.push(Notification::info(format!(
            "show unavailable known: {}",
            if self.show_all_preferred { "on" } else { "off" }
        )));
        let len = self.visible_preferred().len();
        if len == 0 {
            self.preferred_state.select(None);
        } else if self.preferred_state.selected().map_or(true, |i| i >= len) {
            self.preferred_state.select(Some(0));
        }
    }

    /// Order known networks strongest-signal-first using the latest scan
    /// results. Networks not currently in range (no scan match) have no RSSI,
    /// so they sink below every in-range one. Must be re-run whenever either
    /// the preferred list or the scan results change.
    fn sort_preferred_by_signal(&mut self) {
        let networks = &self.networks;
        self.preferred.sort_by(|a, b| {
            let rssi = |ssid: &str| {
                networks
                    .iter()
                    .find(|n| n.ssid.as_deref() == Some(ssid))
                    .map(|n| n.rssi)
            };
            // Some(strong) > Some(weak) > None; reverse for strongest first.
            rssi(b).cmp(&rssi(a))
        });
    }

    /// True when `ssid` shows up in the latest scan, i.e. it's in range.
    fn is_in_range(&self, ssid: &str) -> bool {
        self.networks
            .iter()
            .any(|n| n.ssid.as_deref() == Some(ssid))
    }

    /// impala parity: the Known Networks table lists only known networks that
    /// are currently in range. `show_all_preferred` (the `A` toggle) appends
    /// the out-of-range remembered networks after them. `preferred` is already
    /// sorted strongest-signal-first, so the in-range ones come out ordered.
    pub fn visible_preferred(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .preferred
            .iter()
            .filter(|ssid| self.is_in_range(ssid))
            .cloned()
            .collect();
        if self.show_all_preferred {
            out.extend(
                self.preferred
                    .iter()
                    .filter(|ssid| !self.is_in_range(ssid))
                    .cloned(),
            );
        }
        out
    }

    pub fn visible_networks(&self) -> Vec<&ScannedNetwork> {
        let mut out = self.networks
            .iter()
            .filter(|n| {
                // impala parity: a scanned network we already have a saved
                // profile for belongs in the Known/Preferred table, never in
                // New Networks. Drop it here regardless of `show_all`.
                let known = n
                    .ssid
                    .as_deref()
                    .map_or(false, |s| self.preferred.iter().any(|p| p == s));
                if known {
                    return false;
                }
                // `show_all` additionally reveals weak-signal and
                // hidden/redacted networks that are otherwise filtered out.
                self.show_all
                    || (n.ssid.as_deref().map_or(false, |s| !s.is_empty()) && n.rssi > -85)
            })
            .collect::<Vec<_>>();
        // Sort strongest signal first (RSSI is negative; higher = stronger).
        out.sort_by(|a, b| b.rssi.cmp(&a.rssi));
        out
    }

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::State(s) => self.state = Some(s),
            Event::ScanStarted => self.scanning = true,
            Event::ScanResult(n) => {
                self.networks = n;
                self.scanning = false;
                self.sort_preferred_by_signal();
                let len = self.visible_networks().len();
                if len == 0 {
                    self.available_state.select(None);
                } else if self.available_state.selected().map_or(true, |i| i >= len) {
                    self.available_state.select(Some(0));
                }
                // The Known Networks list is filtered by what's in range, so it
                // shifts with each scan — re-clamp its selection too.
                let plen = self.visible_preferred().len();
                if plen == 0 {
                    self.preferred_state.select(None);
                } else if self.preferred_state.selected().map_or(true, |i| i >= plen) {
                    self.preferred_state.select(Some(0));
                }
            }
            Event::PreferredResult(v) => {
                self.preferred = v;
                self.sort_preferred_by_signal();
                let len = self.visible_preferred().len();
                if len == 0 {
                    self.preferred_state.select(None);
                } else if self.preferred_state.selected().map_or(true, |i| i >= len) {
                    self.preferred_state.select(Some(0));
                }
            }
            Event::Notice(s) => self.notifications.push(Notification::info(s)),
            Event::Error(s) => self.notifications.push(Notification::error(s)),
            Event::ShareReady(p) => self.overlay = Overlay::Share(p),
            Event::JoinSavedFailed { ssid, reason } => {
                self.notifications.push(Notification::info(format!(
                    "{ssid}: enter password to connect ({reason})"
                )));
                self.overlay = Overlay::Password(PasswordPrompt {
                    ssid,
                    input: tui_input::Input::default(),
                });
            }
            _ => {}
        }
    }

    pub fn selected_network(&self) -> Option<&ScannedNetwork> {
        let i = self.available_state.selected()?;
        self.visible_networks().get(i).copied()
    }

    pub fn selected_preferred(&self) -> Option<String> {
        let i = self.preferred_state.selected()?;
        self.visible_preferred().into_iter().nth(i)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let visible_len = self.visible_networks().len();
        let preferred_len = self.visible_preferred().len();
        let (state, len) = match self.focus {
            Focus::Available => (&mut self.available_state, visible_len),
            Focus::Preferred => (&mut self.preferred_state, preferred_len),
        };
        if len == 0 {
            state.select(None);
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len as isize) as usize;
        state.select(Some(next));
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Available => Focus::Preferred,
            Focus::Preferred => Focus::Available,
        };
    }

    pub fn connect_selected_available(&mut self) {
        let Some(net) = self.selected_network().cloned() else {
            return;
        };
        let Some(ssid) = net.ssid.clone() else {
            // A blank SSID means either Location is denied (CoreWLAN redacts
            // every name) or this is a genuinely hidden network (no broadcast
            // name). Distinguish the two so the hint is actionable.
            let msg = if crate::location::auth_status().is_authorized() {
                "hidden network — use the hidden-SSID join (h) to connect by name"
            } else {
                "network names hidden — grant Location permission to macwifi.app"
            };
            self.notifications.push(Notification::error(msg));
            return;
        };
        match net.security {
            Security::Open => {
                self.wifi.send(Request::Associate(Associate {
                    ssid: ssid.clone(),
                    kind: AssociateKind::Open,
                }));
            }
            Security::WpaEnterprise | Security::Wpa2Enterprise | Security::Wpa3Enterprise => {
                self.overlay = Overlay::EnterpriseUser(EnterprisePrompt {
                    ssid,
                    username: String::new(),
                    input: Input::default(),
                });
            }
            _ => {
                self.overlay = Overlay::Password(PasswordPrompt {
                    ssid,
                    input: Input::default(),
                });
            }
        }
    }

    pub fn connect_selected_preferred(&mut self) {
        let Some(ssid) = self.selected_preferred() else {
            return;
        };
        // Saved networks already have their credentials in Keychain; route
        // through `networksetup -setairportnetwork` instead of doing a
        // directed scan + associate (which fails when SSIDs are redacted).
        self.wifi.send(Request::JoinSaved(ssid));
    }

    pub fn start_hidden(&mut self) {
        self.overlay = Overlay::HiddenSsid(HiddenPrompt {
            ssid: String::new(),
            input: Input::default(),
        });
    }

    pub fn show_info(&mut self) {
        self.overlay = Overlay::Info;
    }

    pub fn share_selected_preferred(&mut self) {
        let Some(ssid) = self.selected_preferred() else {
            return;
        };
        let Some(security) = self.security_for(&ssid) else {
            self.notifications.push(Notification::error(
                "can't share enterprise networks — no shareable password",
            ));
            return;
        };
        self.wifi.send(Request::Share { ssid, security });
    }

    /// Map a network's security to a shareable QR type. Returns `None` for
    /// enterprise networks: their credentials aren't stored as a generic
    /// keychain password, so a `WIFI:` URI would carry no usable key.
    /// Networks not present in the latest scan default to WPA (the common case
    /// for a saved-but-out-of-range network).
    fn security_for(&self, ssid: &str) -> Option<ShareSecurity> {
        match self
            .networks
            .iter()
            .find(|n| n.ssid.as_deref() == Some(ssid))
            .map(|n| n.security)
        {
            Some(Security::Open) => Some(ShareSecurity::Nopass),
            Some(Security::Wep) => Some(ShareSecurity::Wep),
            Some(
                Security::WpaEnterprise | Security::Wpa2Enterprise | Security::Wpa3Enterprise,
            ) => None,
            _ => Some(ShareSecurity::Wpa),
        }
    }

    pub fn submit_overlay(&mut self) {
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        match overlay {
            Overlay::Password(p) => {
                let pw = p.input.value().to_string();
                // Route via networksetup so Location-denied bundles still
                // associate (CoreWLAN's associateToNetwork: is TCC-gated).
                self.wifi.send(Request::JoinWithPassword {
                    ssid: p.ssid,
                    password: pw,
                });
            }
            Overlay::EnterpriseUser(p) => {
                let username = p.input.value().to_string();
                self.overlay = Overlay::EnterprisePass(EnterprisePrompt {
                    ssid: p.ssid,
                    username,
                    input: Input::default(),
                });
            }
            Overlay::EnterprisePass(p) => {
                let password = p.input.value().to_string();
                self.wifi.send(Request::Associate(Associate {
                    ssid: p.ssid,
                    kind: AssociateKind::Peap {
                        username: p.username,
                        password,
                    },
                }));
            }
            Overlay::HiddenSsid(p) => {
                let ssid = p.input.value().to_string();
                if ssid.is_empty() {
                    return;
                }
                self.overlay = Overlay::HiddenPass(HiddenPrompt {
                    ssid,
                    input: Input::default(),
                });
            }
            Overlay::HiddenPass(p) => {
                let pw = p.input.value().to_string();
                if pw.is_empty() {
                    self.wifi.send(Request::Associate(Associate {
                        ssid: p.ssid,
                        kind: AssociateKind::Hidden(None),
                    }));
                } else {
                    self.wifi.send(Request::JoinWithPassword {
                        ssid: p.ssid,
                        password: pw,
                    });
                }
            }
            Overlay::Info | Overlay::Share(_) | Overlay::None => {}
        }
    }
}
