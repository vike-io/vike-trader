//! The toolchain fingerprint — catches ACCIDENTAL divergence between the box that built a plugin
//! and the box running the host, where [`crate::abi::ABI_VERSION`] catches only a DELIBERATE
//! vtable change (see that constant's own doc).
//!
//! Baked in at compile time by `build.rs` (`cargo:rustc-env=VIKE_PLUGIN_FINGERPRINT=...`) from the
//! rustc release + commit hash, the target triple, and Cargo's own `PROFILE`/`OPT_LEVEL` — the
//! same shape on both sides of the boundary, because both the host binary and a real compiled
//! plugin depend on this crate and therefore run the identical `build.rs` logic.

/// The fingerprint this build was compiled with.
///
/// `env!` rather than `option_env!`: a `build.rs` that failed to run at all is a build worth
/// failing loudly over at COMPILE time, not one that silently ships an empty fingerprint nothing
/// can ever match (which would make every fingerprint check vacuously agree on emptiness).
pub const FINGERPRINT: &str = env!("VIKE_PLUGIN_FINGERPRINT");

/// A NUL-terminated, `'static` C-string view of [`FINGERPRINT`], for a plugin's
/// `vike_plugin_fingerprint() -> *const c_char` export — the ONE handshake value that crosses
/// NUL-terminated rather than as `(ptr, len)` (`loader.rs`'s `read_c_str` is the read side, and
/// argues why staying lossy there is fine for a diagnostic string in a way `CBar::to_bar`'s
/// checked conversion is not for a trading symbol). Built once and leaked once: the fingerprint is
/// fixed for the life of the process, so there is exactly one C string to ever need.
pub fn fingerprint_c_str() -> *const std::os::raw::c_char {
    static LEAKED: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    LEAKED
        .get_or_init(|| {
            std::ffi::CString::new(FINGERPRINT)
                .expect("FINGERPRINT must not contain an interior NUL")
        })
        .as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_non_empty_and_names_the_target_and_profile() {
        assert!(!FINGERPRINT.is_empty());
        assert!(
            FINGERPRINT.contains("target="),
            "fingerprint should name the target: {FINGERPRINT}"
        );
        assert!(
            FINGERPRINT.contains("profile="),
            "fingerprint should name the profile: {FINGERPRINT}"
        );
    }

    #[test]
    fn the_c_str_view_reads_back_as_the_same_text() {
        let ptr = fingerprint_c_str();
        assert!(!ptr.is_null());
        // SAFETY: `fingerprint_c_str` guarantees a valid, NUL-terminated, `'static` pointer.
        let back = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().expect("ASCII fingerprint");
        assert_eq!(back, FINGERPRINT);
    }

    #[test]
    fn fingerprint_c_str_is_stable_across_calls() {
        assert_eq!(fingerprint_c_str(), fingerprint_c_str(), "must return the same leaked pointer");
    }
}
