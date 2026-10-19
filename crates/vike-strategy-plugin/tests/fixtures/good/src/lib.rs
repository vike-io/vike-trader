//! A well-formed fixture "plugin" for `crates/vike-strategy-plugin/tests/load_refusals.rs`'s
//! `a_well_formed_plugin_loads_successfully` — every export correct, proving `loader::load`'s
//! happy path actually succeeds rather than every test in that file being a vacuously-true
//! refusal.
//!
//! ⚠ Deliberately depends on nothing but `std`/`core` — see `../../../build.rs`'s module doc for
//! why a fixture must never take a path dependency back on `vike-strategy-plugin`. Its `on_bar`'s
//! `broker`/`bar` parameters use GENERIC pointer types rather than the real `BrokerRef`/`CBar`: a
//! `#[repr(C)]` struct's ABI depends only on its fields' sizes/alignments, not their pointee
//! types, so two pointer-sized fields are two pointer-sized fields either way, and this fixture
//! never dereferences either parameter — the exact pointee type is immaterial to the calling
//! convention `loader.rs`'s `dlsym` + transmute crosses.

use std::ffi::{c_char, c_int, c_void};

#[no_mangle]
pub extern "C" fn vike_plugin_abi_version() -> u32 {
    env!("VIKE_TEST_ABI_VERSION").parse().expect("VIKE_TEST_ABI_VERSION must be a valid u32")
}

#[no_mangle]
pub extern "C" fn vike_plugin_fingerprint() -> *const c_char {
    static FP: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    FP.get_or_init(|| {
        std::ffi::CString::new(env!("VIKE_TEST_FINGERPRINT")).expect("no interior NUL")
    })
    .as_ptr()
}

#[no_mangle]
pub extern "C" fn vike_plugin_create(_params_ptr: *const u8, _params_len: usize) -> *mut c_void {
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn vike_plugin_destroy(_handle: *mut c_void) {}

#[no_mangle]
pub extern "C" fn vike_plugin_warmup(_handle: *mut c_void) -> usize {
    0
}

/// Layout-identical to `vike_strategy_plugin::abi::BrokerRef` (two pointer-sized fields, same
/// order, `#[repr(C)]`) without importing it — see this file's own module doc.
#[repr(C)]
pub struct FakeBrokerRef {
    pub ctx: *mut c_void,
    pub vtable: *const c_void,
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_bar(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _bar: *const c_void,
) -> c_int {
    0 // vike_strategy_plugin::abi::PluginStatus::Ok's discriminant
}

// ---- the thirteen ABI_VERSION 3 dispatch exports ---------------------------------------------
//
// `loader::load` resolves EVERY dispatch symbol (`RTLD_NOW` + a `bind!` per name) and refuses a
// plugin missing one, so a fixture that wants to be LOADED has to export all of them. Each is
// inert: this fixture is a witness for the loader, not for dispatch, and it dereferences no
// payload — see this file's module doc for why the pointer types may be generic.

#[no_mangle]
pub extern "C" fn vike_plugin_on_start(_handle: *mut c_void, _broker: FakeBrokerRef) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_stop(_handle: *mut c_void, _broker: FakeBrokerRef) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_quote_tick(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _quote: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_trade_tick(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _trade: *const c_void,
) -> c_int {
    0
}

/// `BookRef` is two pointer-sized fields, exactly like `BrokerRef` — see the module doc.
#[no_mangle]
pub extern "C" fn vike_plugin_on_order_book(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _book: FakeBrokerRef,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_schedule(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _tag_ptr: *const u8,
    _tag_len: usize,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_fill(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _fill: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_feed_status(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _status: u32,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_mark(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _mark: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_reference_quote(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _venue_ptr: *const u8,
    _venue_len: usize,
    _quote: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_flow(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _flow: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_order_event(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _event: *const c_void,
) -> c_int {
    0
}

#[no_mangle]
pub extern "C" fn vike_plugin_on_params_updated(
    _handle: *mut c_void,
    _broker: FakeBrokerRef,
    _params_ptr: *const u8,
    _params_len: usize,
) -> c_int {
    0
}
