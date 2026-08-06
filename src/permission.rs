//! System-audio-recording permission status.
//!
//! Windows and Linux need no permission for loopback and report `NotRequired`.
//!
//! macOS gates CoreAudio process taps behind the System Audio Recording grant,
//! with no public API to request or check it. The prompt appears on its own the
//! first time an aggregate device containing a tap is started, and a denial
//! shows up as silence rather than an error, so the usable signal is "no frames
//! arrived shortly after start".
//!
//! The `tcc-preflight` feature reads the grant up front via `TCCAccessPreflight`
//! from the private TCC framework. Off by default. Every failure path returns
//! `Unknown`, so enabling it cannot break anything.
//!
//! Measured on macOS 26.5 (2026-07-30): preflight returns `2` (not determined)
//! for `kTCCServiceAudioCapture` on a machine where capture works, while
//! returning `0` for `kTCCServiceMicrophone` and `kTCCServiceScreenCapture`. The
//! symbol resolves; it just does not reflect the grant taps consult. Use the
//! capture probe as the real answer.

/// Reachable variants depend on target and feature: macOS without preflight
/// answers only `Unknown`, other platforms only `NotRequired`. All variants stay
/// in the contract every platform compiles against.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Granted,
    Denied,
    Unknown,
    NotRequired,
}

#[cfg(not(target_os = "macos"))]
pub fn status() -> Status {
    Status::NotRequired
}

#[cfg(all(target_os = "macos", not(feature = "tcc-preflight")))]
pub fn status() -> Status {
    // Knowable only by trying. See the module docs.
    Status::Unknown
}

#[cfg(all(target_os = "macos", feature = "tcc-preflight"))]
pub fn status() -> Status {
    macos_tcc::preflight()
}

#[cfg(all(target_os = "macos", feature = "tcc-preflight"))]
mod macos_tcc {
    use super::Status;
    use objc2_core_foundation::{CFRetained, CFString};
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::ptr;
    use std::sync::OnceLock;

    const TCC_PATH: &str = "/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC";
    const SERVICE: &str = "kTCCServiceAudioCapture";

    /// `TCCAccessPreflight(CFStringRef service, CFDictionaryRef options)`
    ///
    /// Returns `c_int`. Widening to 64 bits could read junk from the high half
    /// of the return register if the symbol really returns `int`; narrowing is
    /// safe for the 0/1/2 results it produces.
    type PreflightFn = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;

    extern "C" {
        fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    const RTLD_NOW: c_int = 0x2;

    /// Resolved once. `None` if the framework or symbol is unavailable.
    fn preflight_fn() -> Option<PreflightFn> {
        static RESOLVED: OnceLock<Option<usize>> = OnceLock::new();

        // Parenthesised because `?` binds tighter than `*`: we want to unwrap
        // the Option behind the reference, not deref the result of `?`.
        let addr = (*RESOLVED.get_or_init(|| unsafe {
            let path = CString::new(TCC_PATH).ok()?;
            let handle = dlopen(path.as_ptr(), RTLD_NOW);
            if handle.is_null() {
                return None;
            }

            let symbol = CString::new("TCCAccessPreflight").ok()?;
            let sym = dlsym(handle, symbol.as_ptr());
            if sym.is_null() {
                return None;
            }
            Some(sym as usize)
        }))?;

        // SAFETY: address came from dlsym for a symbol with a known signature.
        // If it is ever removed, dlsym returns null above and we never get here.
        Some(unsafe { std::mem::transmute::<usize, PreflightFn>(addr) })
    }

    pub fn preflight() -> Status {
        let Some(f) = preflight_fn() else {
            return Status::Unknown;
        };

        let service: CFRetained<CFString> = CFString::from_str(SERVICE);
        let service_ptr = CFRetained::as_ptr(&service).as_ptr() as *const c_void;

        // SAFETY: `service_ptr` is a live CFStringRef held by `service` for the
        // duration of the call; a null options dictionary is accepted.
        let result = unsafe { f(service_ptr, ptr::null()) };

        match result {
            0 => Status::Granted,
            1 => Status::Denied,
            // 2 is "not determined"; anything else is undocumented. Both mean
            // "find out by trying".
            _ => Status::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn no_permission_off_macos() {
        assert_eq!(status(), Status::NotRequired);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_never_notrequired() {
        // NotRequired would tell the UI to skip a prompt that does exist.
        assert_ne!(status(), Status::NotRequired);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(feature = "tcc-preflight")))]
    fn without_preflight_unknown() {
        assert_eq!(status(), Status::Unknown);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "tcc-preflight"))]
    fn preflight_returns_a_defined_status() {
        // Value depends on the machine's TCC database; this checks the call
        // survives either way.
        assert!(matches!(
            status(),
            Status::Granted | Status::Denied | Status::Unknown
        ));
    }
}
