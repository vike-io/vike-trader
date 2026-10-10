//! Exports a WRONG `vike_plugin_abi_version` on purpose — `crates/vike-strategy-plugin/tests/
//! load_refusals.rs`'s `an_abi_version_mismatch_is_refused` dlopens this and expects
//! `loader::LoadError::AbiMismatch`. `loader::load` checks this handshake FIRST, before resolving
//! any other symbol, so this fixture needs no other export.

#[no_mangle]
pub extern "C" fn vike_plugin_abi_version() -> u32 {
    999 // intentionally NOT crates/vike-strategy-plugin/src/abi.rs's ABI_VERSION
}
