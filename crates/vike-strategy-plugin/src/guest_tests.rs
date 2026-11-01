use super::*;
use std::cell::RefCell;

thread_local! {
    static SUBMITS: RefCell<Vec<(String, i32, f64)>> = const { RefCell::new(Vec::new()) };
    static BAR_AT_CALLS: RefCell<usize> = const { RefCell::new(0) };
}

const STUB_BAR_COUNT: usize = 4;

fn read_sym(s: *const u8, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    // SAFETY: the test's own stubs are called only through the vtable this test builds, with
    // `(s, n)` always the `(ptr, len)` of a live `&str` for the duration of this call — the
    // same contract `abi.rs`'s module doc states for every string crossing the boundary.
    let bytes = unsafe { std::slice::from_raw_parts(s, n) };
    std::str::from_utf8(bytes).expect("test harness only ever sends valid UTF-8").to_string()
}

/// One inert host slot past the erased context every slot opens with: its remaining parameter
/// types (each slot's own — that difference is the ABI) and the constant it answers.
macro_rules! host_stub {
    ($name:ident($($ty:ty),*)) => {
        extern "C" fn $name(_c: *mut core::ffi::c_void, $(_: $ty),*) {}
    };
    ($name:ident($($ty:ty),*) -> $ret:ty => $answer:expr) => {
        extern "C" fn $name(_c: *mut core::ffi::c_void, $(_: $ty),*) -> $ret {
            $answer
        }
    };
}

extern "C" fn rec_submit_market(
    _c: *mut core::ffi::c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) {
    let sym = read_sym(s, n);
    SUBMITS.with(|v| v.borrow_mut().push((sym, side, qty)));
}
host_stub!(stub_submit_limit(*const u8, usize, i32, f64, f64));
host_stub!(stub_position(*const u8, usize) -> f64 => 3.5);
host_stub!(stub_price(*const u8, usize) -> f64 => 101.25);
host_stub!(stub_equity() -> f64 => 10_000.0);
host_stub!(stub_index() -> usize => 7);
host_stub!(stub_now() -> i64 => 1_700_000_000_000);
host_stub!(stub_bars_len(*const u8, usize) -> usize => STUB_BAR_COUNT);
host_stub!(stub_quote_vwap(*const u8, usize, i32, f64) -> crate::abi::COptF64 => Some(42.0).into());
host_stub!(stub_depth_within_price(*const u8, usize, i32, f64) -> f64 => 99.0);
extern "C" fn stub_bar_at(
    _c: *mut core::ffi::c_void,
    _s: *const u8,
    _n: usize,
    i: usize,
    out: *mut CBar,
) -> bool {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() += 1);
    if i >= STUB_BAR_COUNT {
        return false;
    }
    let bar = CBar::from_bar(&Bar {
        ts: i as i64,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    });
    // SAFETY: `out` is the live, aligned `*mut CBar` `HostBroker::bars` passed for exactly
    // this call — the same contract `BrokerVTable::bar_at`'s doc states.
    unsafe { *out = bar };
    true
}

fn bar_at_calls() -> usize {
    BAR_AT_CALLS.with(|v| *v.borrow())
}

// ---- HftBroker stubs ----

host_stub!(stub_hft_position() -> f64 => -1.25);
extern "C" fn rec_submit_limit_tagged(
    _c: *mut core::ffi::c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    price: f64,
) {
    let tag = read_sym(s, n);
    TAGGED_SUBMITS.with(|v| v.borrow_mut().push((tag, side, qty, price)));
}
extern "C" fn rec_modify_tagged(
    _c: *mut core::ffi::c_void,
    s: *const u8,
    n: usize,
    new_qty: crate::abi::COptF64,
    new_price: crate::abi::COptF64,
) {
    let tag = read_sym(s, n);
    TAGGED_MODIFIES.with(|v| v.borrow_mut().push((tag, new_qty.into(), new_price.into())));
}
extern "C" fn rec_cancel_tagged(_c: *mut core::ffi::c_void, s: *const u8, n: usize) {
    let tag = read_sym(s, n);
    TAGGED_CANCELS.with(|v| v.borrow_mut().push(tag));
}

/// `(tag, new_qty, new_price)`, as recorded by `rec_modify_tagged` — named to keep the
/// `thread_local!` declaration under clippy's `type_complexity` threshold.
type TaggedModify = (String, Option<f64>, Option<f64>);

thread_local! {
    static TAGGED_SUBMITS: RefCell<Vec<(String, i32, f64, f64)>> = const { RefCell::new(Vec::new()) };
    static TAGGED_MODIFIES: RefCell<Vec<TaggedModify>> = const { RefCell::new(Vec::new()) };
    static TAGGED_CANCELS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The host side every test here drives: the recording and stub slots above. A `static` because
/// [`HostBroker::new`] holds the vtable for `'static`, as a loaded plugin's is.
static TEST_VTABLE: BrokerVTable = BrokerVTable {
    submit_market: rec_submit_market,
    submit_limit: stub_submit_limit,
    position: stub_position,
    price: stub_price,
    equity: stub_equity,
    index: stub_index,
    now: stub_now,
    bars_len: stub_bars_len,
    bar_at: stub_bar_at,
    quote_vwap: stub_quote_vwap,
    depth_within_price: stub_depth_within_price,
    hft_position: stub_hft_position,
    submit_limit_tagged: rec_submit_limit_tagged,
    modify_tagged: rec_modify_tagged,
    cancel_tagged: rec_cancel_tagged,
};

/// A guest-side broker over [`TEST_VTABLE`], with no host context behind it.
fn broker() -> HostBroker {
    HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &TEST_VTABLE })
}

#[test]
fn submit_market_crosses_the_vtable_with_its_symbol_intact() {
    SUBMITS.with(|v| v.borrow_mut().clear());
    let mut b = broker();
    b.submit_market("BTCUSDT", 1, 2.0);
    SUBMITS.with(|v| {
        assert_eq!(v.borrow().as_slice(), &[("BTCUSDT".to_string(), 1, 2.0)]);
    });
}

#[test]
fn position_reads_through_the_vtable() {
    let b = broker();
    // UFCS: `Broker::position(&self, symbol)` and `HftBroker::position(&self)` share a name,
    // and both traits are in scope here — `.position(..)` is ambiguous (E0034) regardless of
    // argument count, per this crate's own `host.rs` module doc.
    assert_eq!(Broker::position(&b, "BTCUSDT"), 3.5);
}

#[test]
fn price_equity_index_now_and_the_defaulted_reads_cross_the_vtable() {
    let b = broker();
    assert_eq!(b.price("BTCUSDT"), 101.25);
    assert_eq!(b.equity(), 10_000.0);
    assert_eq!(b.index(), 7);
    assert_eq!(b.now(), 1_700_000_000_000);
    assert_eq!(b.quote_vwap("BTCUSDT", 1, 1.0), Some(42.0));
    assert_eq!(b.depth_within_price("BTCUSDT", 1, 100.0), 99.0);
}

/// `bars()` returns a slice, so the guest must materialise its own. Refilling on every call
/// would be O(n) per access; this pins that it refills only when the host's `index` advanced.
#[test]
fn bars_are_cached_until_the_index_advances() {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    let first = b.bars("BTCUSDT").len();
    let calls_after_first = bar_at_calls();
    let second = b.bars("BTCUSDT").len();
    assert_eq!(first, second);
    assert_eq!(first, STUB_BAR_COUNT);
    assert_eq!(calls_after_first, STUB_BAR_COUNT, "the first fill walks the whole cursor");
    assert_eq!(bar_at_calls(), calls_after_first, "a second bars() call must not re-fetch");
}

/// Two DIFFERENT symbols cache independently — refilling one must not disturb the other's
/// already-materialised mirror (the property that makes the `UnsafeCell` in `bars_cache`
/// sound: see the `// SAFETY:` note at its one call site).
#[test]
fn bars_cache_independently_per_symbol() {
    BAR_AT_CALLS.with(|v| *v.borrow_mut() = 0);
    let b = broker();
    let a = b.bars("AAA").len();
    let calls_after_a = bar_at_calls();
    let c = b.bars("CCC").len();
    assert_eq!(a, STUB_BAR_COUNT);
    assert_eq!(c, STUB_BAR_COUNT);
    assert_eq!(bar_at_calls(), calls_after_a * 2, "a new symbol must still walk its own cursor");
    // Re-reading the FIRST symbol must still be cached (not disturbed by filling the second).
    let a_again = b.bars("AAA").len();
    assert_eq!(a_again, STUB_BAR_COUNT);
    assert_eq!(bar_at_calls(), calls_after_a * 2, "the first symbol must not be re-fetched");
}

// ---- HftBroker: every one of its four REQUIRED methods (no trait-level default to fall
// back on, unlike Broker::quote_vwap/depth_within_price) reaches a real vtable slot. ----

#[test]
fn hft_position_reads_through_the_vtable_with_no_symbol_argument() {
    let b = broker();
    assert_eq!(HftBroker::position(&b), -1.25);
}

#[test]
fn submit_limit_tagged_crosses_the_vtable_with_its_tag_intact() {
    TAGGED_SUBMITS.with(|v| v.borrow_mut().clear());
    let mut b = broker();
    b.submit_limit_tagged("bid-1", 1, 2.0, 100.5);
    TAGGED_SUBMITS.with(|v| {
        assert_eq!(v.borrow().as_slice(), &[("bid-1".to_string(), 1, 2.0, 100.5)]);
    });
}

#[test]
fn modify_tagged_flattens_its_optionals_through_coptf64() {
    TAGGED_MODIFIES.with(|v| v.borrow_mut().clear());
    let mut b = broker();
    b.modify_tagged("bid-1", Some(3.0), None);
    TAGGED_MODIFIES.with(|v| {
        assert_eq!(v.borrow().as_slice(), &[("bid-1".to_string(), Some(3.0), None)]);
    });
}

#[test]
fn cancel_tagged_crosses_the_vtable_with_its_tag_intact() {
    TAGGED_CANCELS.with(|v| v.borrow_mut().clear());
    let mut b = broker();
    b.cancel_tagged("bid-1");
    TAGGED_CANCELS.with(|v| assert_eq!(v.borrow().as_slice(), &["bid-1".to_string()]));
}
