//! Correct ABI version, WRONG fingerprint — `crates/vike-rust-strategy/tests/
//! load_refusals.rs`'s `a_fingerprint_mismatch_is_refused_naming_both_toolchains` loads this and
//! expects `loader::LoadError::FingerprintMismatch`. Its compatibility record (`ABI_VERSION` 15) states
//! the same wrong fingerprint, so the loader refuses it from the FILE, before `dlopen`; the exports stay
//! the record's twins.

use std::ffi::c_char;

include!(concat!(env!("OUT_DIR"), "/identity.rs"));

#[no_mangle]
pub extern "C" fn vike_strategy_abi_version() -> u32 {
    env!("VIKE_TEST_ABI_VERSION").parse().expect("VIKE_TEST_ABI_VERSION must be a valid u32")
}

#[no_mangle]
pub extern "C" fn vike_strategy_fingerprint() -> *const c_char {
    c"doctored-wrong-fingerprint-for-testing".as_ptr()
}
