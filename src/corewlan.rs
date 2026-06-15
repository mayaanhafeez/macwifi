//! Thin Rust wrapper over CoreWLAN.framework.
//!
//! Covers the operations impala drives over iwd: enumerate the default
//! Wi-Fi interface, read live state, scan, associate (open / PSK / PEAP),
//! and disassociate. Power on/off is exposed here for parity, but the
//! binary also falls back to `networksetup(8)` when CoreWLAN's setPower
//! returns an authorization error.

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2_core_wlan::{CWInterface, CWNetwork, CWSecurity, CWWiFiClient};
use objc2_foundation::{NSData, NSError, NSString};
use serde::{Deserialize, Serialize};

pub struct WifiClient {
    client: Retained<CWWiFiClient>,
}

pub struct WifiInterface {
    iface: Retained<CWInterface>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedNetwork {
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub rssi: isize,
    pub channel: Option<u32>,
    pub security: Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    Open,
    Wep,
    WpaPersonal,
    Wpa2Personal,
    Wpa3Personal,
    WpaEnterprise,
    Wpa2Enterprise,
    Wpa3Enterprise,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceState {
    pub name: String,
    pub powered: bool,
    pub hw_address: Option<String>,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub rssi: isize,
    pub noise: isize,
    pub tx_rate: f64,
    pub channel: Option<u32>,
}

impl WifiClient {
    pub fn shared() -> Result<Self> {
        let client = unsafe { CWWiFiClient::sharedWiFiClient() };
        Ok(Self { client })
    }

    pub fn default_interface(&self) -> Result<WifiInterface> {
        let iface = unsafe { self.client.interface() }
            .ok_or_else(|| anyhow!("no Wi-Fi interface available"))?;
        Ok(WifiInterface { iface })
    }
}

impl WifiInterface {
    pub fn name(&self) -> String {
        unsafe { self.iface.interfaceName() }
            .map(ns_string)
            .unwrap_or_else(|| "?".into())
    }

    pub fn state(&self) -> Result<InterfaceState> {
        unsafe {
            let ssid = self
                .iface
                .ssid()
                .map(ns_string)
                .or_else(|| self.iface.ssidData().and_then(ns_data_to_string));
            Ok(InterfaceState {
                name: self.iface.interfaceName().map(ns_string).unwrap_or_default(),
                powered: self.iface.powerOn(),
                hw_address: self.iface.hardwareAddress().map(ns_string),
                ssid,
                bssid: self.iface.bssid().map(ns_string),
                rssi: self.iface.rssiValue(),
                noise: self.iface.noiseMeasurement(),
                tx_rate: self.iface.transmitRate(),
                channel: self
                    .iface
                    .wlanChannel()
                    .map(|c| c.channelNumber() as u32),
            })
        }
    }

    pub fn set_power(&self, on: bool) -> Result<()> {
        unsafe {
            self.iface
                .setPower_error(on)
                .map_err(|e| anyhow!("setPower failed: {}", ns_error_message(&e)))
        }
    }

    pub fn scan(&self) -> Result<Vec<ScannedNetwork>> {
        let set = unsafe {
            self.iface
                .scanForNetworksWithName_error(None)
                .map_err(|e| anyhow!("scan failed: {}", ns_error_message(&e)))?
        };
        let mut out = Vec::new();
        for net in set.iter() {
            out.push(network_to_scanned(&net));
        }
        Ok(out)
    }

    pub fn scan_for_ssid(&self, ssid: &str) -> Result<Vec<ScannedNetwork>> {
        let ssid_ns = NSString::from_str(ssid);
        let set = unsafe {
            self.iface
                .scanForNetworksWithName_error(Some(&ssid_ns))
                .map_err(|e| anyhow!("directed scan failed: {}", ns_error_message(&e)))?
        };
        let mut out = Vec::new();
        for net in set.iter() {
            out.push(network_to_scanned(&net));
        }
        Ok(out)
    }

    pub fn associate_open(&self, ssid: &str) -> Result<()> {
        let net = self.find_network(ssid)?;
        unsafe {
            self.iface
                .associateToNetwork_password_error(&net, None)
                .map_err(|e| anyhow!("associate failed: {}", ns_error_message(&e)))
        }
    }

    pub fn associate_psk(&self, ssid: &str, password: &str) -> Result<()> {
        let net = self.find_network(ssid)?;
        let pw = NSString::from_str(password);
        unsafe {
            self.iface
                .associateToNetwork_password_error(&net, Some(&pw))
                .map_err(|e| anyhow!("associate failed: {}", ns_error_message(&e)))
        }
    }

    pub fn associate_peap(&self, ssid: &str, username: &str, password: &str) -> Result<()> {
        let net = self.find_network(ssid)?;
        let u = NSString::from_str(username);
        let p = NSString::from_str(password);
        unsafe {
            self.iface
                .associateToEnterpriseNetwork_identity_username_password_error(
                    &net,
                    None,
                    Some(&u),
                    Some(&p),
                )
                .map_err(|e| anyhow!("enterprise associate failed: {}", ns_error_message(&e)))
        }
    }

    pub fn disassociate(&self) {
        unsafe { self.iface.disassociate() };
    }

    fn find_network(&self, ssid: &str) -> Result<Retained<CWNetwork>> {
        let ssid_ns = NSString::from_str(ssid);
        let set = unsafe {
            self.iface
                .scanForNetworksWithName_error(Some(&ssid_ns))
                .map_err(|e| anyhow!("directed scan failed: {}", ns_error_message(&e)))?
        };
        // scanForNetworksWithName: already filters server-side; trust the
        // result set even if our ssid() readback is redacted by Location.
        if let Some(net) = set.iter().next() {
            return Ok(net.clone());
        }
        Err(anyhow!(
            "network '{ssid}' not visible — too far, wrong band, or radio off"
        ))
    }
}

fn network_to_scanned(net: &CWNetwork) -> ScannedNetwork {
    unsafe {
        // `ssid()` returns nil when CoreWLAN considers the caller untrusted,
        // even with Location authorized — Sequoia/Tahoe behavior. The raw
        // `ssidData()` bytes are often still populated; decode and use them
        // as a fallback before declaring the network hidden.
        let ssid = net
            .ssid()
            .map(ns_string)
            .or_else(|| net.ssidData().and_then(ns_data_to_string));
        ScannedNetwork {
            ssid,
            bssid: net.bssid().map(ns_string),
            rssi: net.rssiValue(),
            channel: net.wlanChannel().map(|c| c.channelNumber() as u32),
            security: detect_security(net),
        }
    }
}

fn detect_security(net: &CWNetwork) -> Security {
    let probes: &[(CWSecurity, Security)] = &[
        (CWSecurity::None, Security::Open),
        (CWSecurity::WPA3Enterprise, Security::Wpa3Enterprise),
        (CWSecurity::WPA2Enterprise, Security::Wpa2Enterprise),
        (CWSecurity::WPAEnterprise, Security::WpaEnterprise),
        (CWSecurity::WPA3Personal, Security::Wpa3Personal),
        (CWSecurity::WPA2Personal, Security::Wpa2Personal),
        (CWSecurity::WPAPersonal, Security::WpaPersonal),
        (CWSecurity::WEP, Security::Wep),
    ];
    for (probe, mapped) in probes {
        if unsafe { net.supportsSecurity(*probe) } {
            return *mapped;
        }
    }
    Security::Unknown
}

fn ns_string(s: Retained<NSString>) -> String {
    s.to_string()
}

fn ns_data_to_string(d: Retained<NSData>) -> Option<String> {
    let bytes = d.to_vec();
    if bytes.is_empty() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn ns_error_message(err: &NSError) -> String {
    err.localizedDescription().to_string()
}
