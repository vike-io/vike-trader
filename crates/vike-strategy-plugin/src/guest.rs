//! The GUEST side: a [`BrokerRef`] wearing the ordinary [`vike_model::Broker`] trait, so a user
//! strategy compiled into a plugin calls `broker.submit_market(..)` and never learns FFI exists.
//!
//! ⚠ Every method of `Broker` as it stood on 2026-09-21 (`crates/vike-model/src/strategy/mod.rs`)
//! is wired below to a real vtable slot — `submit_market`, `submit_limit`, `position`, `price`,
//! `equity`, `bars`, `index`, `now`, plus the two DEFAULTED reads `quote_vwap` and
//! `depth_within_price`. None is left on the trait's own default: a defaulted read here would
//! silently answer "no liquidity data" / "I don't know" instead of asking the host, which is
//! exactly the silent-divergence class this crate exists to avoid.
//!
//! ⚠ **`HftBroker` is ALSO implemented, as of `ABI_VERSION` 2 — a correction, not scope creep.**
//! The real user-strategy entry contract is `pub fn build<B: vike_model::HftBroker +
//! 'static>(..)`, not `Broker` alone (`HftBroker: Broker`, four more REQUIRED methods with no
//! trait-level default), so a `HostBroker` implementing only `Broker` cannot satisfy the bound the
//! cdylib template instantiates a real user strategy against. `MultiHftBroker` — the multi-symbol
//! extension one layer above `HftBroker` — is still NOT implemented: the entry contract names
//! `HftBroker`, not `MultiHftBroker`, and [`BrokerVTable`] has no slots for it.
//!
//! ⚠ **`ABI_VERSION` 3 adds two guest-side DECODERS beside the broker wrapper**, because the
//! remaining `Strategy` hooks carry payloads a `#[repr(C)]` mirror cannot express:
//! [`book_from_ref`] rebuilds an `L2Book` through the cursor `abi.rs` declares, and
//! [`strategy_params_from_json`] decodes the one payload whose only faithful text form is JSON.
//! Both live HERE rather than in the cdylib template so the template's own dependency table stays
//! `vike-model` + `vike-strategy-plugin` + `toml` — the user-facing surface the design's open
//! question 3 is about.

use std::cell::UnsafeCell;
use std::collections::HashMap;

use vike_marketdata::{Bar, BookLevel, L2Book};
use vike_model::{Broker, HftBroker};

use crate::abi::{BookRef, BrokerRef, BrokerVTable, CBar, CBookLevel, SIDE_ASK, SIDE_BID};

/// One symbol's cached bar mirror, plus the host `index` it was last filled at.
struct Bucket {
    at_index: Option<usize>,
    bars: Vec<Bar>,
}

/// A [`BrokerRef`] wearing [`Broker`]. Holds the host's vtable already dereferenced once (at
/// construction) rather than re-deref'ing `r.vtable` on every call.
pub struct HostBroker {
    ctx: *mut core::ffi::c_void,
    vtable: &'static BrokerVTable,
    /// `Broker::bars` takes `&self`, so refilling it lazily needs interior mutability — see the
    /// `// SAFETY:` note at the one call site in [`HostBroker::bars`] for why a raw cell is sound
    /// here rather than a `RefCell` (which cannot hand back a `&'a [Bar]` tied to `&self` without
    /// holding its guard alive). Keyed by symbol, deliberately NOT a single `Vec`: refilling one
    /// symbol's bars must never reallocate — and so never invalidate — a slice already returned
    /// for a DIFFERENT symbol earlier in the same call.
    bars_cache: UnsafeCell<HashMap<String, Bucket>>,
}

impl HostBroker {
    pub fn new(r: BrokerRef) -> Self {
        // SAFETY: `r.vtable` is host-owned and `'static` for the whole lifetime of a loaded
        // plugin — the loader refuses to construct a `BrokerRef` otherwise (see `loader.rs`), and
        // this crate's ABI contract (`abi.rs`'s module doc) is the authority a caller across the
        // boundary must uphold. Dereferenced exactly once, here, rather than on every vtable call.
        let vtable = unsafe { &*r.vtable };
        Self { ctx: r.ctx, vtable, bars_cache: UnsafeCell::new(HashMap::new()) }
    }
}

impl Broker for HostBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        (self.vtable.submit_market)(self.ctx, symbol.as_ptr(), symbol.len(), side, qty);
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        (self.vtable.submit_limit)(self.ctx, symbol.as_ptr(), symbol.len(), side, qty, price);
    }
    fn position(&self, symbol: &str) -> f64 {
        (self.vtable.position)(self.ctx, symbol.as_ptr(), symbol.len())
    }
    fn price(&self, symbol: &str) -> f64 {
        (self.vtable.price)(self.ctx, symbol.as_ptr(), symbol.len())
    }
    fn equity(&self) -> f64 {
        (self.vtable.equity)(self.ctx)
    }
    fn index(&self) -> usize {
        (self.vtable.index)(self.ctx)
    }
    fn now(&self) -> i64 {
        (self.vtable.now)(self.ctx)
    }
    /// Lazily refills `symbol`'s bar mirror from the host's `bars_len`/`bar_at` cursor, but only
    /// when the host's `index` has advanced past the last fill for THIS symbol — a second call
    /// for the same symbol at the same step returns the cached mirror with no cursor walk.
    fn bars(&self, symbol: &str) -> &[Bar] {
        let idx = (self.vtable.index)(self.ctx);
        // SAFETY: `HostBroker` is used exactly the way every FFI dispatch entry point in this
        // design uses it — synchronously, single-threaded, for the duration of one call — and is
        // neither `Send` nor `Sync`, so no other reference to `self` can be alive concurrently
        // with this one. The remaining hazard interior mutability usually carries — a mutation
        // through the cell invalidating a `&[Bar]` this same method already handed back — cannot
        // reach a DIFFERENT symbol's entry (each `Bucket`'s `Vec` is its own heap allocation, and
        // only the entry keyed by `symbol` is ever touched). It cannot reach the SAME symbol's
        // entry either, and the reason is NOT that a fresh `HostBroker` is built per dispatch
        // (this type has no way to enforce that, and the cache would be pointless if it were
        // true: an always-fresh cache could never observe an `index` advance in the first place —
        // the LOAD-BEARING invariant is narrower and IS something this ABI guarantees: the host's
        // `index()` is constant for the duration of one synchronous dispatch call, because nothing
        // advances the engine's step between a plugin entry point being called and it returning
        // (`abi.rs`'s module doc: the whole exchange is one borrowed, synchronous call). So two
        // `bars(symbol)` calls that read the SAME `idx` are necessarily within the SAME dispatch
        // and see the cache untouched (no refill runs, nothing is invalidated); a call that reads
        // an ADVANCED `idx` is necessarily a NEW dispatch, and the refill it triggers cannot
        // invalidate a slice from the PREVIOUS dispatch because nothing can still be holding one —
        // a `&[Bar]` borrowed from `&self` cannot outlive the call that produced it once that
        // call's own stack frame is gone, dispatch-fresh `HostBroker` or not.
        let cache = unsafe { &mut *self.bars_cache.get() };
        let bucket = cache
            .entry(symbol.to_string())
            .or_insert_with(|| Bucket { at_index: None, bars: Vec::new() });
        if bucket.at_index != Some(idx) {
            let n = (self.vtable.bars_len)(self.ctx, symbol.as_ptr(), symbol.len());
            bucket.bars.clear();
            bucket.bars.reserve(n);
            for i in 0..n {
                let mut out = CBar::empty();
                // SAFETY: `out` is a live, aligned, owned `CBar` on this stack frame; the host
                // writes into it through the pointer and returns `false` without writing when `i`
                // is out of range, so `out` is never read uninitialised.
                let wrote =
                    (self.vtable.bar_at)(self.ctx, symbol.as_ptr(), symbol.len(), i, &mut out);
                if wrote {
                    bucket.bars.push(out.to_bar());
                }
            }
            bucket.at_index = Some(idx);
        }
        &bucket.bars
    }
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        (self.vtable.quote_vwap)(self.ctx, symbol.as_ptr(), symbol.len(), side, qty).into()
    }
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        (self.vtable.depth_within_price)(self.ctx, symbol.as_ptr(), symbol.len(), side, limit_px)
    }
}

impl HftBroker for HostBroker {
    fn position(&self) -> f64 {
        (self.vtable.hft_position)(self.ctx)
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        (self.vtable.submit_limit_tagged)(self.ctx, tag.as_ptr(), tag.len(), side, qty, price);
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        (self.vtable.modify_tagged)(
            self.ctx,
            tag.as_ptr(),
            tag.len(),
            new_qty.into(),
            new_price.into(),
        );
    }
    fn cancel_tagged(&mut self, tag: &str) {
        (self.vtable.cancel_tagged)(self.ctx, tag.as_ptr(), tag.len());
    }
}

/// Materialise the host's L2 book, level by level, through the [`BookRef`] cursor — so a user
/// strategy's `on_order_book(&mut B, book: &L2Book)` receives an ordinary `&L2Book` and never
/// learns the real one lives in another `.so`.
///
/// ⚠ **The materialisation is unavoidable and the cursor does not avoid it** — that is worth
/// stating plainly rather than letting the shape imply otherwise. `Strategy::on_order_book`'s
/// parameter is a concrete `&L2Book`, so SOMETHING has to build one on this side of the boundary
/// whatever the payload looks like. What the cursor buys is that the HOST never clones its own
/// book, no second serialisation format exists to drift from `L2Book`'s own semantics, and every
/// level crosses as two `f64`s. `abi.rs`'s cursor section states the per-call cost in full.
///
/// **Faithfulness.** `tick_size` and `last_seq` cross explicitly and the levels are re-folded
/// through `L2Book::apply_snapshot`, so every public read — `best_bid`/`best_ask`/`mid`/`spread`/
/// `imbalance`/`bid_qty_at`/`top_n`/`avg_px_for_quantity`/`quantity_for_price`/`simulate_fill` —
/// answers exactly what the host's own book answers. The one place a float could bite is the tick
/// grid: a level's price crosses as `tick * tick_size` and is re-quantised by
/// `(price / tick_size).round_ties_even()`, whose error is many orders of magnitude below the
/// half-tick that rounding decides, so the tick index comes back identical.
///
/// ⚠ **Caller contract: only from inside a `catch_unwind`.** The vtable calls below can panic
/// through `guarded`'s poison latch on the host's side; the template's `vike_plugin_on_order_book`
/// export is the wrapper that makes that safe.
pub fn book_from_ref(r: BookRef) -> L2Book {
    // SAFETY: `r.vtable` is host-owned and `'static` for the whole lifetime of a loaded plugin —
    // the same contract `HostBroker::new` relies on for `BrokerVTable`, stated in `abi.rs`'s
    // module doc. Dereferenced exactly once, here, rather than on every cursor call.
    let vt = unsafe { &*r.vtable };
    let side = |which: i32| -> Vec<BookLevel> {
        let n = (vt.side_len)(r.ctx, which);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // `out_level` is an OWNED local, so reading it back after the call needs no `unsafe`
            // on this side — only the host's write through the pointer does, exactly as
            // `HostBroker::bars` splits the same act with `bar_at`.
            let mut out_level = CBookLevel::empty();
            if (vt.level_at)(r.ctx, which, i, &mut out_level) {
                out.push(BookLevel::new(out_level.price, out_level.qty));
            }
        }
        out
    };
    let bids = side(SIDE_BID);
    let asks = side(SIDE_ASK);
    let mut book = L2Book::new((vt.tick_size)(r.ctx));
    book.apply_snapshot((vt.last_seq)(r.ctx), &bids, &asks);
    book
}

/// Decode a `vike_model::StrategyParams` from the JSON document `on_params_updated` carries.
///
/// ⚠ **JSON, not TOML, and the reason is MEASURED on the types rather than preferred.** The
/// obvious choice was TOML — it is the idiom `vike_plugin_create` already uses for the params
/// table, and reusing it would have added no dependency. It does not work: `StrategyParams`
/// reaches `TripleBarrier`'s four `Option<f64>`/`Option<i64>` fields, `SpreadMakerParams::
/// avellaneda_stoikov` and `XemmParams::refresh_tolerance`, none of which carries
/// `skip_serializing_if`, and `toml`'s serializer REFUSES a `None` outright
/// (`UnsupportedNone`). So a TOML hop would fail for the ordinary case — a controller bag with no
/// take-profit set — and a hook that silently stops being delivered when a knob is unset is the
/// exact silent-divergence class this crate exists to refuse.
///
/// The rejected alternative was a flat `#[repr(C)]` mirror of the three variants. That is a
/// hand-maintained second copy of `SpreadMakerParams` (five nested sub-bags),
/// `ControllerParams` and `XemmParams`, with nothing holding it complete — a field added
/// upstream would simply stop arriving, which is the same failure wearing a different hat, and
/// the "second name for one contract" the design rejected `abi_stable` over.
///
/// ⚠ **`serde_json` costs a plugin NOTHING it was not already linking**: `vike-model` — which
/// every plugin depends on directly, because that is where `Strategy` lives — has had a normal
/// `serde_json` dependency since `save_state` existed. Naming it in this crate's manifest adds an
/// edge, not a crate, and the cdylib TEMPLATE's own dependency table is untouched (the helper is
/// here, so a user file still never names anything but `vike-model` and `toml`) — which is what
/// keeps the design's open question 3, the plugin tier's narrower dependency SURFACE, closed.
pub fn strategy_params_from_json(json: &str) -> Option<vike_model::StrategyParams> {
    serde_json::from_str(json).ok()
}

// A `BrokerRef`'s `ctx` is an erased `&mut SimBroker` owned by the HOST for the duration of the
// call the plugin is currently inside; `HostBroker` never outlives that call (the plugin's own
// `extern "C"` entry point constructs one, uses it, and drops it before returning), so it never
// needs to be `Send`/`Sync` and deliberately is not (the default: it holds a raw pointer, which
// opts it out of both auto traits already).

#[cfg(test)]
mod tests {
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
    extern "C" fn stub_submit_limit(
        _c: *mut core::ffi::c_void,
        _s: *const u8,
        _n: usize,
        _side: i32,
        _qty: f64,
        _price: f64,
    ) {
    }
    extern "C" fn stub_position(_c: *mut core::ffi::c_void, _s: *const u8, _n: usize) -> f64 {
        3.5
    }
    extern "C" fn stub_price(_c: *mut core::ffi::c_void, _s: *const u8, _n: usize) -> f64 {
        101.25
    }
    extern "C" fn stub_equity(_c: *mut core::ffi::c_void) -> f64 {
        10_000.0
    }
    extern "C" fn stub_index(_c: *mut core::ffi::c_void) -> usize {
        7
    }
    extern "C" fn stub_now(_c: *mut core::ffi::c_void) -> i64 {
        1_700_000_000_000
    }
    extern "C" fn stub_bars_len(_c: *mut core::ffi::c_void, _s: *const u8, _n: usize) -> usize {
        STUB_BAR_COUNT
    }
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
    extern "C" fn stub_quote_vwap(
        _c: *mut core::ffi::c_void,
        _s: *const u8,
        _n: usize,
        _side: i32,
        _qty: f64,
    ) -> crate::abi::COptF64 {
        Some(42.0).into()
    }
    extern "C" fn stub_depth_within_price(
        _c: *mut core::ffi::c_void,
        _s: *const u8,
        _n: usize,
        _side: i32,
        _limit_px: f64,
    ) -> f64 {
        99.0
    }

    fn bar_at_calls() -> usize {
        BAR_AT_CALLS.with(|v| *v.borrow())
    }

    // ---- HftBroker stubs ----

    extern "C" fn stub_hft_position(_c: *mut core::ffi::c_void) -> f64 {
        -1.25
    }
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

    fn test_vtable() -> BrokerVTable {
        BrokerVTable {
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
        }
    }

    #[test]
    fn submit_market_crosses_the_vtable_with_its_symbol_intact() {
        SUBMITS.with(|v| v.borrow_mut().clear());
        let vt = test_vtable();
        let mut b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        b.submit_market("BTCUSDT", 1, 2.0);
        SUBMITS.with(|v| {
            assert_eq!(v.borrow().as_slice(), &[("BTCUSDT".to_string(), 1, 2.0)]);
        });
    }

    #[test]
    fn position_reads_through_the_vtable() {
        let vt = test_vtable();
        let b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        // UFCS: `Broker::position(&self, symbol)` and `HftBroker::position(&self)` share a name,
        // and both traits are in scope here — `.position(..)` is ambiguous (E0034) regardless of
        // argument count, per this crate's own `host.rs` module doc.
        assert_eq!(Broker::position(&b, "BTCUSDT"), 3.5);
    }

    #[test]
    fn price_equity_index_now_and_the_defaulted_reads_cross_the_vtable() {
        let vt = test_vtable();
        let b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
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
        let vt = test_vtable();
        let b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
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
        let vt = test_vtable();
        let b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        let a = b.bars("AAA").len();
        let calls_after_a = bar_at_calls();
        let c = b.bars("CCC").len();
        assert_eq!(a, STUB_BAR_COUNT);
        assert_eq!(c, STUB_BAR_COUNT);
        assert_eq!(
            bar_at_calls(),
            calls_after_a * 2,
            "a new symbol must still walk its own cursor"
        );
        // Re-reading the FIRST symbol must still be cached (not disturbed by filling the second).
        let a_again = b.bars("AAA").len();
        assert_eq!(a_again, STUB_BAR_COUNT);
        assert_eq!(bar_at_calls(), calls_after_a * 2, "the first symbol must not be re-fetched");
    }

    // ---- HftBroker: every one of its four REQUIRED methods (no trait-level default to fall
    // back on, unlike Broker::quote_vwap/depth_within_price) reaches a real vtable slot. ----

    #[test]
    fn hft_position_reads_through_the_vtable_with_no_symbol_argument() {
        let vt = test_vtable();
        let b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        assert_eq!(HftBroker::position(&b), -1.25);
    }

    #[test]
    fn submit_limit_tagged_crosses_the_vtable_with_its_tag_intact() {
        TAGGED_SUBMITS.with(|v| v.borrow_mut().clear());
        let vt = test_vtable();
        let mut b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        b.submit_limit_tagged("bid-1", 1, 2.0, 100.5);
        TAGGED_SUBMITS.with(|v| {
            assert_eq!(v.borrow().as_slice(), &[("bid-1".to_string(), 1, 2.0, 100.5)]);
        });
    }

    #[test]
    fn modify_tagged_flattens_its_optionals_through_coptf64() {
        TAGGED_MODIFIES.with(|v| v.borrow_mut().clear());
        let vt = test_vtable();
        let mut b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        b.modify_tagged("bid-1", Some(3.0), None);
        TAGGED_MODIFIES.with(|v| {
            assert_eq!(v.borrow().as_slice(), &[("bid-1".to_string(), Some(3.0), None)]);
        });
    }

    #[test]
    fn cancel_tagged_crosses_the_vtable_with_its_tag_intact() {
        TAGGED_CANCELS.with(|v| v.borrow_mut().clear());
        let vt = test_vtable();
        let mut b = HostBroker::new(BrokerRef { ctx: std::ptr::null_mut(), vtable: &vt });
        b.cancel_tagged("bid-1");
        TAGGED_CANCELS.with(|v| assert_eq!(v.borrow().as_slice(), &["bid-1".to_string()]));
    }
}
