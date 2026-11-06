//! Loads cleanly (correct ABI version + fingerprint), but `vike_plugin_on_bar` panics internally
//! and catches its OWN panic — `crates/vike-strategy-plugin/tests/load_refusals.rs`'s
//! `a_panicking_plugin_surfaces_an_error_and_does_not_abort_the_process` calls it directly and
//! asserts the returned status is `Panicked`, then keeps running: reaching that assertion at all
//! IS the proof, since a panic that escaped this `extern "C"` function uncaught would abort the
//! whole test process per Rust's unwind-across-`extern "C"` rule, per the design doc's own "a
//! panic across a C-ABI boundary is UB" note. The `catch_unwind` therefore has to live HERE, on
//! the plugin's own side — nothing on the host's side of a real `dlopen` boundary could ever catch
//! it after the fact.
//!
//! ⚠ **What this fixture does NOT prove, stated here because the design once read it as proving
//! it.** The `catch_unwind` below is hand-written in THIS file. The one that actually protects the
//! backtest server lives in `crates/vike-strategy-plugin/template/lib.rs.in`, which this fixture
//! never uses — so "a fixture that panics in `on_bar` surfaces an error, not a crash" was true of
//! this `.so` and unproven of every real plugin.
//! `a_panicking_user_strategy_built_from_the_template_is_reported_not_fatal`
//! (`crates/vike-strategy-plugin/tests/load_refusals.rs`) builds a panicking USER STRATEGY through
//! the real `build_plugin` and covers that. This fixture stays as the cheap loader-side witness.
//!
//! See `../good/src/lib.rs`'s module doc for why this fixture depends on nothing but `std`/`core`
//! and uses generic pointer types for the parameters it never dereferences.

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
    match std::panic::catch_unwind(|| {
        panic!("deliberate panic for load_refusals's panic test");
    }) {
        Ok(()) => 0, // vike_strategy_plugin::abi::PluginStatus::Ok
        Err(_) => 1, // vike_strategy_plugin::abi::PluginStatus::Panicked
    }
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
