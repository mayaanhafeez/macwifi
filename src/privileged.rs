//! Run a copy of macwifi as root after a single graphical admin-password
//! prompt, using `AuthorizationExecuteWithPrivileges`.
//!
//! Used by `pre-grant`: the System-keychain ACL writes in `keychain_grant`
//! require root, and this gives us root via one Security-Agent prompt (no
//! Terminal `sudo`). The API is deprecated but is still the only in-process way
//! to obtain a privileged child without shipping a separate setuid/SMJobBless
//! helper. The child it launches is our own signed binary, by absolute path.

use std::ffi::{CString, c_char, c_void};
use std::io::Write;

use anyhow::{Context, Result, bail};

type AuthorizationRef = *mut c_void;
type OSStatus = i32;
const KAUTH_FLAG_DEFAULTS: u32 = 0;
const ERR_AUTHORIZATION_CANCELED: OSStatus = -60006;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(
        rights: *const c_void,
        environment: *const c_void,
        flags: u32,
        authorization: *mut AuthorizationRef,
    ) -> OSStatus;
    fn AuthorizationFree(authorization: AuthorizationRef, flags: u32) -> OSStatus;
    fn AuthorizationExecuteWithPrivileges(
        authorization: AuthorizationRef,
        path_to_tool: *const c_char,
        options: u32,
        arguments: *const *mut c_char,
        communications_pipe: *mut *mut libc::FILE,
    ) -> OSStatus;
}

/// Launch `tool` (absolute path to an executable) as root with `args`, after a
/// one-time admin-password prompt. The child's stdout/stderr is relayed to ours
/// line by line; returns once the child exits. The admin prompt being dismissed
/// surfaces as an error.
pub fn exec_as_root(tool: &str, args: &[String]) -> Result<()> {
    unsafe {
        let mut auth: AuthorizationRef = std::ptr::null_mut();
        let st = AuthorizationCreate(
            std::ptr::null(),
            std::ptr::null(),
            KAUTH_FLAG_DEFAULTS,
            &mut auth,
        );
        if st != 0 {
            bail!("AuthorizationCreate failed ({st})");
        }

        let tool_c = CString::new(tool).context("tool path contains NUL")?;
        let arg_cstrings: Vec<CString> = args
            .iter()
            .map(|a| CString::new(a.as_str()))
            .collect::<std::result::Result<_, _>>()
            .context("argument contains NUL")?;
        // NULL-terminated argv, excluding the tool path itself.
        let mut argv: Vec<*mut c_char> =
            arg_cstrings.iter().map(|c| c.as_ptr() as *mut c_char).collect();
        argv.push(std::ptr::null_mut());

        let mut pipe: *mut libc::FILE = std::ptr::null_mut();
        let st = AuthorizationExecuteWithPrivileges(
            auth,
            tool_c.as_ptr(),
            KAUTH_FLAG_DEFAULTS,
            argv.as_ptr(),
            &mut pipe,
        );
        if st != 0 {
            AuthorizationFree(auth, KAUTH_FLAG_DEFAULTS);
            if st == ERR_AUTHORIZATION_CANCELED {
                bail!("admin authorization was canceled");
            }
            bail!("AuthorizationExecuteWithPrivileges failed ({st})");
        }

        // Relay the privileged child's output until it closes the pipe (exits).
        if !pipe.is_null() {
            let mut buf = [0u8; 1024];
            let stdout = std::io::stdout();
            loop {
                let n = libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), pipe);
                if n == 0 {
                    break;
                }
                let _ = stdout.lock().write_all(&buf[..n]);
            }
            libc::fclose(pipe);
        }

        AuthorizationFree(auth, KAUTH_FLAG_DEFAULTS);
    }
    Ok(())
}
