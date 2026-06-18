//! Grant macwifi access to a saved network's System-keychain item by adding our
//! bundled binary to the item's ACL, via the (deprecated but still functional)
//! `SecKeychain` ACL APIs.
//!
//! Why this instead of just reading the password: System-keychain Wi-Fi items
//! don't offer the login-keychain style "Allow / Always Allow" dialog. Touching
//! one shows only an admin-authentication prompt, which authorizes that single
//! operation and never adds the app to the item's ACL — so a read can't make
//! future reads silent. The only ways to persist trust are Keychain Access
//! (manual drag onto the Access Control list) or `SecKeychainItemSetAccess`
//! (this module).
//!
//! Split across privilege: [`probe`] runs as the normal user to decide whether
//! an item already trusts us (root could read everything and would wrongly
//! report "already granted"); [`add_to_acl`] performs the actual ACL write and
//! must run as **root** — `SecKeychainItemSetAccess` on the root-owned System
//! keychain returns `errSecWrPerm` (-61) for an unprivileged caller.
//!
//! These APIs are deprecated since macOS 10.10 but remain present and working;
//! there is no non-deprecated replacement for editing a legacy keychain item's
//! trusted-application list.

use std::ffi::{CString, c_char, c_void};
use std::ptr;

use anyhow::{Result, bail};
use core_foundation_sys::array::{
    CFArrayAppendValue, CFArrayCreateMutableCopy, CFArrayGetCount, CFArrayGetValueAtIndex,
    CFArrayRef, CFMutableArrayRef,
};
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::string::CFStringRef;

const WIFI_SERVICE: &str = "AirPort";
const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";
/// The binary whose identity the daemon runs under; that's what must be on the
/// ACL for connect-time reads to be silent.
const APP_BIN: &str = "/Applications/macwifi.app/Contents/MacOS/macwifi";

const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
// A no-interaction probe of an item we're not yet trusted for comes back as
// either errSecInteractionNotAllowed (can't show the dialog) or errSecAuthFailed
// (the admin auth it would have shown is treated as failed). Both mean the same
// thing: there's an item, we're just not on its ACL yet.
const ERR_SEC_INTERACTION_NOT_ALLOWED: OSStatus = -25308;
const ERR_SEC_AUTH_FAILED: OSStatus = -25293;

type OSStatus = i32;
type SecKeychainRef = *mut c_void;
type SecKeychainItemRef = *mut c_void;
type SecAccessRef = *mut c_void;
type SecTrustedApplicationRef = *mut c_void;
type SecACLRef = *const c_void;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainOpen(path_name: *const c_char, keychain: *mut SecKeychainRef) -> OSStatus;
    fn SecKeychainSetUserInteractionAllowed(state: u8) -> OSStatus;
    fn SecKeychainItemFreeContent(attr_list: *mut c_void, data: *mut c_void) -> OSStatus;
    fn SecKeychainFindGenericPassword(
        keychain_or_array: CFTypeRef,
        service_name_length: u32,
        service_name: *const c_char,
        account_name_length: u32,
        account_name: *const c_char,
        password_length: *mut u32,
        password_data: *mut *mut c_void,
        item_ref: *mut SecKeychainItemRef,
    ) -> OSStatus;
    fn SecKeychainItemCopyAccess(item: SecKeychainItemRef, access: *mut SecAccessRef) -> OSStatus;
    fn SecKeychainItemSetAccess(item: SecKeychainItemRef, access: SecAccessRef) -> OSStatus;
    fn SecTrustedApplicationCreateFromPath(
        path: *const c_char,
        app: *mut SecTrustedApplicationRef,
    ) -> OSStatus;
    fn SecAccessCopyACLList(access: SecAccessRef, acl_list: *mut CFArrayRef) -> OSStatus;
    fn SecACLCopyContents(
        acl: SecACLRef,
        application_list: *mut CFArrayRef,
        description: *mut CFStringRef,
        prompt_selector: *mut u16,
    ) -> OSStatus;
    fn SecACLSetContents(
        acl: SecACLRef,
        application_list: CFArrayRef,
        description: CFStringRef,
        prompt_selector: u16,
    ) -> OSStatus;
}

/// Result of the unprivileged trust check for one SSID.
pub enum ProbeResult {
    /// macwifi can already read the item silently — nothing to do.
    AlreadyGranted,
    /// An item exists but macwifi isn't on its ACL yet — needs [`add_to_acl`].
    NeedsGrant,
    /// No saved keychain item for this SSID (open network, or never saved).
    NotFound,
}

/// Check, **as the current user**, whether macwifi is already trusted for
/// `ssid`'s System-keychain item. Never prompts: it disables user interaction
/// and interprets the resulting status. Must NOT be run as root — root can read
/// every item and would always report `AlreadyGranted`.
pub fn probe(ssid: &str) -> Result<ProbeResult> {
    unsafe {
        let keychain = open_system_keychain()?;
        let _kc = Releaser(keychain as CFTypeRef);

        let service = WIFI_SERVICE.as_bytes();
        let account = ssid.as_bytes();

        SecKeychainSetUserInteractionAllowed(0);
        let mut pw_len: u32 = 0;
        let mut pw_data: *mut c_void = ptr::null_mut();
        let mut item: SecKeychainItemRef = ptr::null_mut();
        let st = SecKeychainFindGenericPassword(
            keychain as CFTypeRef,
            service.len() as u32,
            service.as_ptr() as *const c_char,
            account.len() as u32,
            account.as_ptr() as *const c_char,
            &mut pw_len,
            &mut pw_data,
            &mut item,
        );
        SecKeychainSetUserInteractionAllowed(1);
        if !item.is_null() {
            CFRelease(item as CFTypeRef);
        }
        match st {
            0 => {
                SecKeychainItemFreeContent(ptr::null_mut(), pw_data);
                Ok(ProbeResult::AlreadyGranted)
            }
            ERR_SEC_ITEM_NOT_FOUND => Ok(ProbeResult::NotFound),
            ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_AUTH_FAILED => Ok(ProbeResult::NeedsGrant),
            other => bail!("keychain probe failed ({other})"),
        }
    }
}

/// Add macwifi to `ssid`'s System-keychain item ACL. **Must run as root** —
/// `SecKeychainItemSetAccess` on the root-owned System keychain fails with
/// `errSecWrPerm` (-61) otherwise. Extends the existing ACL rather than
/// replacing it, so airportd/wifid keep their access.
pub fn add_to_acl(ssid: &str) -> Result<()> {
    unsafe {
        let keychain = open_system_keychain()?;
        let _kc = Releaser(keychain as CFTypeRef);

        let service = WIFI_SERVICE.as_bytes();
        let account = ssid.as_bytes();

        // Locate the item WITHOUT requesting the secret (null password
        // out-params) — that lookup doesn't prompt.
        let mut item: SecKeychainItemRef = ptr::null_mut();
        let st = SecKeychainFindGenericPassword(
            keychain as CFTypeRef,
            service.len() as u32,
            service.as_ptr() as *const c_char,
            account.len() as u32,
            account.as_ptr() as *const c_char,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut item,
        );
        if st == ERR_SEC_ITEM_NOT_FOUND {
            bail!("no saved password");
        }
        if st != 0 {
            bail!("locate item failed ({st})");
        }
        let _item = Releaser(item as CFTypeRef);

        // Trusted-application reference for our bundled binary.
        let app_path = CString::new(APP_BIN)?;
        let mut app: SecTrustedApplicationRef = ptr::null_mut();
        let st = SecTrustedApplicationCreateFromPath(app_path.as_ptr(), &mut app);
        if st != 0 {
            bail!("SecTrustedApplicationCreateFromPath failed ({st})");
        }
        let _app = Releaser(app as CFTypeRef);

        // Copy the item's existing ACL set so we extend rather than replace it.
        let mut access: SecAccessRef = ptr::null_mut();
        let st = SecKeychainItemCopyAccess(item, &mut access);
        if st != 0 {
            bail!("SecKeychainItemCopyAccess failed ({st})");
        }
        let _access = Releaser(access as CFTypeRef);

        let mut acl_list: CFArrayRef = ptr::null();
        let st = SecAccessCopyACLList(access, &mut acl_list);
        if st != 0 || acl_list.is_null() {
            bail!("SecAccessCopyACLList failed ({st})");
        }
        let _acls = Releaser(acl_list as CFTypeRef);

        // Append our app to every ACL that has an explicit application list. A
        // null list means "any application is allowed", which needs no change.
        let count = CFArrayGetCount(acl_list);
        let mut modified = false;
        for i in 0..count {
            let acl = CFArrayGetValueAtIndex(acl_list, i) as SecACLRef;
            let mut app_list: CFArrayRef = ptr::null();
            let mut desc: CFStringRef = ptr::null();
            let mut prompt: u16 = 0;
            if SecACLCopyContents(acl, &mut app_list, &mut desc, &mut prompt) != 0 {
                continue;
            }
            if app_list.is_null() {
                if !desc.is_null() {
                    CFRelease(desc as CFTypeRef);
                }
                continue;
            }
            let new_list: CFMutableArrayRef = CFArrayCreateMutableCopy(ptr::null(), 0, app_list);
            CFArrayAppendValue(new_list, app as *const c_void);
            let set = SecACLSetContents(acl, new_list as CFArrayRef, desc, prompt);
            CFRelease(new_list as CFTypeRef);
            CFRelease(app_list as CFTypeRef);
            if !desc.is_null() {
                CFRelease(desc as CFTypeRef);
            }
            if set == 0 {
                modified = true;
            }
        }
        if !modified {
            bail!("no editable ACL entry found for this item");
        }

        let st = SecKeychainItemSetAccess(item, access);
        if st != 0 {
            bail!("SecKeychainItemSetAccess failed ({st})");
        }
        Ok(())
    }
}

unsafe fn open_system_keychain() -> Result<SecKeychainRef> {
    unsafe {
        let mut keychain: SecKeychainRef = ptr::null_mut();
        let kc_path = CString::new(SYSTEM_KEYCHAIN)?;
        let st = SecKeychainOpen(kc_path.as_ptr(), &mut keychain);
        if st != 0 {
            bail!("SecKeychainOpen failed ({st})");
        }
        Ok(keychain)
    }
}

struct Releaser(CFTypeRef);
impl Drop for Releaser {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
