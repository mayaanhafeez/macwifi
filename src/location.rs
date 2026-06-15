//! Trigger and keep alive the macOS Location authorization for this bundle.
//!
//! CoreWLAN scanning consults TCC silently and redacts SSIDs if our bundle
//! isn't authorized. For the prompt to actually fire we need:
//!   1. Info.plist with NSLocationUsageDescription (we ship one).
//!   2. A signed bundle (ad-hoc OK).
//!   3. A `CLLocationManager` that stays alive (leak it).
//!   4. `startUpdatingLocation()` — `requestWhenInUseAuthorization` alone is
//!      silently no-op'd in CLI tooling.
//!   5. A CFRunLoop that runs *continuously*, not just briefly during auth.
//!      On Sequoia/Tahoe, CoreWLAN's `scanForNetworks` silently redacts SSIDs
//!      in any process that doesn't run a normal GUI app-style run loop, even
//!      with Location authorized. Apple DTS confirmed this on the dev forum
//!      (thread 769950). We spin up a dedicated background thread that
//!      creates the CLLocationManager and then pumps the default run loop
//!      forever to satisfy that check.

use std::sync::Once;
use std::thread;

use objc2_core_foundation::CFRunLoop;
use objc2_core_location::{CLAuthorizationStatus, CLLocationManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationAuth {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    Unknown,
}

impl LocationAuth {
    pub fn is_authorized(self) -> bool {
        matches!(self, LocationAuth::Authorized)
    }
}

static START: Once = Once::new();

pub fn request_when_in_use() {
    START.call_once(|| {
        thread::Builder::new()
            .name("cl-runloop".into())
            .spawn(|| unsafe {
                let mgr = CLLocationManager::new();
                mgr.requestWhenInUseAuthorization();
                mgr.startUpdatingLocation();
                // Hold the manager for the lifetime of the thread, then pump
                // the default run loop forever. CoreWLAN treats this process
                // as a normal GUI app and stops redacting SSIDs.
                let _keep_alive = mgr;
                CFRunLoop::run();
            })
            .expect("spawn cl-runloop thread");
    });

    // Give tccd a chance to deliver the prompt and the user a chance to
    // respond, but only block here on the very first prompt. Once authorized
    // (or denied), return immediately.
    let max_wait = if matches!(auth_status(), LocationAuth::NotDetermined) {
        30.0
    } else {
        0.1
    };
    let slice = 0.5;
    let mut elapsed = 0.0;
    while elapsed < max_wait {
        thread::sleep(std::time::Duration::from_millis((slice * 1000.0) as u64));
        elapsed += slice;
        if !matches!(auth_status(), LocationAuth::NotDetermined) {
            break;
        }
    }
}

pub fn auth_status() -> LocationAuth {
    let raw = unsafe {
        let mgr = CLLocationManager::new();
        mgr.authorizationStatus()
    };
    if raw == CLAuthorizationStatus::AuthorizedAlways
        || raw == CLAuthorizationStatus::AuthorizedWhenInUse
    {
        LocationAuth::Authorized
    } else if raw == CLAuthorizationStatus::NotDetermined {
        LocationAuth::NotDetermined
    } else if raw == CLAuthorizationStatus::Restricted {
        LocationAuth::Restricted
    } else if raw == CLAuthorizationStatus::Denied {
        LocationAuth::Denied
    } else {
        LocationAuth::Unknown
    }
}

/// Diagnostic string for the user when Location is the reason SSIDs are
/// being redacted. `None` means everything is in order.
pub fn redaction_hint() -> Option<&'static str> {
    match auth_status() {
        LocationAuth::Authorized => None,
        LocationAuth::Denied => Some(
            "Location denied — grant in System Settings → Privacy & Security → Location Services",
        ),
        LocationAuth::Restricted => Some("Location restricted by policy — SSIDs will be blank"),
        LocationAuth::NotDetermined => Some(
            "Location not granted — run the bundled .app (scripts/bundle.sh) so the prompt fires",
        ),
        LocationAuth::Unknown => Some("Location status unknown — SSIDs may be redacted"),
    }
}
