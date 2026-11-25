//! Exports a WRONG `vike_strategy_abi_version` on purpose — `crates/vike-rust-strategy/tests/
//! load_refusals.rs`'s `an_abi_version_mismatch_is_refused` loads this and expects
//! `loader::LoadError::AbiMismatch`. Its compatibility record (`ABI_VERSION` 15) states the same wrong
//! version, so the loader refuses it from the FILE, before `dlopen`; the export stays the record's twin,
//! and nothing else is needed.

include!(concat!(env!("OUT_DIR"), "/identity.rs"));

#[no_mangle]
pub extern "C" fn vike_strategy_abi_version() -> u32 {
    999 // intentionally NOT crates/vike-rust-strategy/src/abi.rs's ABI_VERSION
}
