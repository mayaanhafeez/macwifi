use ratatui::widgets::ListState;
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
    pub available_state: ListState,
    pub preferred_state: ListState,
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
        let mut available_state = ListState::default();
        available_state.select(Some(0));
        let mut preferred_state = ListState::default();
        preferred_state.select(Some(0));
        Self {
            running: true,
            focus: Focus::Available,
            state: None,
            networks: Vec::new(),
            preferred: Vec::new(),
            scanning: true,
            show_all: false,
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

    pub fn visible_networks(&self) -> Vec<&ScannedNetwork> {
        if self.show_all {
            self.networks.iter().collect()
        } else {
            self.networks
                .iter()
                .filter(|n| {
                    n.ssid.as_deref().map_or(false, |s| !s.is_empty()) && n.rssi > -85
                })
                .collect()
        }
    }

    pub fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::State(s) => self.state = Some(s),
            Event::ScanStarted => self.scanning = true,
            Event::ScanResult(n) => {
                self.networks = n;
                self.scanning = false;
                let len = self.visible_networks().len();
                if len == 0 {
                    self.available_state.select(None);
                } else if self.available_state.selected().map_or(true, |i| i >= len) {
                    self.available_state.select(Some(0));
                }
            }
            Event::PreferredResult(v) => {
                self.preferred = v;
                if self.preferred.is_empty() {
                    self.preferred_state.select(None);
                } else if self
                    .preferred_state
                    .selected()
                    .map_or(true, |i| i >= self.preferred.len())
                {
                    self.preferred_state.select(Some(0));
                }
            }
            Event::Notice(s) => self.notifications.push(Notification::info(s)),
            Event::Error(s) => self.notifications.push(Notification::error(s)),
            Event::ShareReady(p) => self.overlay = Overlay::Share(p),
            Event::JoinSavedFailed { ssid, reason } => {
                self.notifications.push(Notification::error(format!(
                    "Keychain join failed — enter password ({reason})"
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

    pub fn selected_preferred(&self) -> Option<&String> {
        let i = self.preferred_state.selected()?;
        self.preferred.get(i)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let visible_len = self.visible_networks().len();
        let (state, len) = match self.focus {
            Focus::Available => (&mut self.available_state, visible_len),
            Focus::Preferred => (&mut self.preferred_state, self.preferred.len()),
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
            self.notifications
                .push(Notification::error("network has no SSID — grant Location permission"));
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
        let Some(ssid) = self.selected_preferred().cloned() else {
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
        let Some(ssid) = self.selected_preferred().cloned() else {
            return;
        };
        let security = self.security_for(&ssid);
        self.wifi.send(Request::Share { ssid, security });
    }

    fn security_for(&self, ssid: &str) -> ShareSecurity {
        match self
            .networks
            .iter()
            .find(|n| n.ssid.as_deref() == Some(ssid))
            .map(|n| n.security)
        {
            Some(Security::Open) => ShareSecurity::Nopass,
            Some(Security::Wep) => ShareSecurity::Wep,
            _ => ShareSecurity::Wpa,
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
