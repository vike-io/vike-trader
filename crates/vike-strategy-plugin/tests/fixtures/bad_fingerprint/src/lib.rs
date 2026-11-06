//! Correct ABI version, WRONG fingerprint — `crates/vike-strategy-plugin/tests/
//! load_refusals.rs`'s `a_fingerprint_mismatch_is_refused_naming_both_toolchains` dlopens this and
//! expects `loader::LoadError::FingerprintMismatch`. `loader::load` returns right after that
//! check, so this fixture needs no lifecycle/dispatch exports.

use std::ffi::c_char;

#[no_mangle]
pub extern "C" fn vike_plugin_abi_version() -> u32 {
    env!("VIKE_TEST_ABI_VERSION").parse().expect("VIKE_TEST_ABI_VERSION must be a valid u32")
}

#[no_mangle]
pub extern "C" fn vike_plugin_fingerprint() -> *const c_char {
    c"doctored-wrong-fingerprint-for-testing".as_ptr()
}
