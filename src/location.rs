//! Trigger and keep alive the macOS Location authorization for this bundle.
//!
//! CoreWLAN scanning consults TCC silently and redacts SSIDs if our bundle
//! isn't authorized. For the prompt to actually fire we need:
//!   1. Info.plist with NSLocationUsageDescription (we ship one).
//!   2. A signed bundle (ad-hoc OK).
//!   3. A `CLLocationManager` that stays alive (leak it).
//!   4. `startUpdatingLocation()` — `requestWhenInUseAuthorization` alone is
//!      silently no-op'd in CLI tooling.
//!   5. A CFRunLoop pump so the system can deliver the prompt to our process
//!      before we hand control to the TUI.

use std::mem::ManuallyDrop;

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_location::CLLocationManager;

pub fn request_when_in_use() {
    unsafe {
        let mgr = CLLocationManager::new();
        mgr.requestWhenInUseAuthorization();
        mgr.startUpdatingLocation();
        // Leak the manager so the Retained never drops — without this, the
        // CL object releases and TCC cancels the pending prompt.
        let _leaked: &'static mut ManuallyDrop<_> =
            Box::leak(Box::new(ManuallyDrop::new(mgr)));

        // Pump the run loop for ~1.2s so TCC can deliver and the dialog can
        // paint. Returns early if the system handles the event sooner.
        CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 1.2, false);
    }
}
