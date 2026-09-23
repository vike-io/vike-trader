//! The HOST side: wraps a loaded plugin handle as an ordinary [`vike_model::Strategy`], so
//! `vike-studio-core`/`vike-backend` drive it exactly like any built-in strategy.
//!
//! ⚠ **Layering forces this to be GENERIC over `Broker`, not hard-wired to `SimBroker`.** The
//! design names this crate's job as producing "`Strategy<SimBroker>`", but `SimBroker`
//! (`crates/vike-backtest/src/engine/sim_broker.rs`) lives in `vike-backtest`, declared
//! `layer = 50` — ABOVE this crate's own `layer = 41`
//! (`crates/vike-ops/tests/layer_gate.rs`: every normal dependency must be STRICTLY lower). So
//! `vike-strategy-plugin` cannot name `SimBroker` at all without inverting the dependency graph.
//! Every thunk and [`PluginStrategy`] below are instead generic over any
//! `B: vike_model::HftBroker + 'static`; the module doc this crate opened with already
//! anticipated exactly this shape ("`vike-studio-core`, `vike-backend` and every consumer receive
//! an ordinary `Box<dyn Strategy<SimBroker>>`") — it is a HIGHER-layer crate that instantiates
//! `B = SimBroker` by naming `PluginStrategy<SimBroker>`, never this one.
//!
//! ⚠ **`HftBroker`, not `Broker` — a correction, not a widening for its own sake.** The real
//! user-strategy entry contract is `pub fn build<B: HftBroker + 'static>(..)`
//! (`HftBroker: Broker`), so `PluginStrategy` must be usable wherever THAT bound is required, not
//! merely wherever `Broker` is. Every thunk generic bound stays the NARROWEST trait it actually
//! needs (`Broker` for the original eleven, `HftBroker` only for the four new ones) — widening
//! `broker_vtable`'s own bound to `HftBroker` costs nothing extra for the `Broker`-only thunks,
//! since `HftBroker: Broker` already provides everything they use.
//!
//! ⚠ **`Broker::position(&self, symbol)` and `HftBroker::position(&self)` share a name.** Both
//! traits are in scope together everywhere this file calls into a `B: HftBroker`, so a plain
//! `.position(..)` dot-call is AMBIGUOUS (E0034) regardless of argument count — Rust resolves
//! trait methods reached via `.method()` by NAME first, not by trying each candidate's arity
//! against the call site. Every call to either is therefore fully-qualified UFCS
//! (`Broker::position(..)` / `HftBroker::position(..)`) rather than `broker.position(..)`, the
//! same shape `vike-model`'s own module doc names for `SimBroker`'s inherent `submit_limit`.
//!
//! ⚠ **Review Critical 1: every `t_*` thunk below wraps its body in `catch_unwind`.** These are
//! `extern "C"` frames CALLED BY PLUGIN CODE — the OTHER direction from the ones the design's
//! global constraint named ("every extern "C" entry point in the plugin wraps catch_unwind"),
//! which is why that rule alone did not cover them. A panic in a host-side `Broker` impl (a
//! `SimBroker` unwrap, an out-of-range index, an allocation failure) or in `read_str` (this file,
//! deliberately panicking rather than lossy on a contract violation) would otherwise unwind out
//! of a C-ABI frame and hit rustc's abort shim — "it destroys the backtest server", the exact
//! outcome the design exists to prevent. See [`guarded`].

use std::any::TypeId;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::{Mutex, OnceLock};

use vike_marketdata::Bar;
use vike_model::{Broker, HftBroker, Strategy};

use crate::abi::{
    BookRef, BookVTable, BrokerRef, BrokerVTable, CBar, CFill, CFlowToxicity, CMarkTick,
    COrderLifecycle, CQuoteTick, CTradeTick, PluginStatus,
};

/// Recover the concrete `&mut B` a generic thunk's erased `ctx` was built from.
///
/// A macro rather than a generic helper FUNCTION deliberately: a safe fn of shape
/// `fn f<'a, B>(ctx: *mut c_void) -> &'a mut B` would let its caller pick ANY `'a` it likes
/// (lifetime laundering) — the classic unsound-helper shape. Expanding inline at each call site
/// keeps the borrow tied to that call's own stack frame, the same reason `crates/bridges/fxcm`'s
/// `bind!` macro (`src/loader.rs`) exists rather than a same-shaped function.
macro_rules! broker_mut {
    ($ctx:expr, $B:ty) => {{
        // SAFETY: every caller of a generic thunk below builds `$ctx` as `broker as *mut $B as
        // *mut c_void` in the SAME call frame that then invokes the plugin (`PluginStrategy`'s
        // trait methods, further down) — a live, exclusively-borrowed `&mut $B` for exactly the
        // duration of the plugin call currently unwinding back through this thunk. The plugin
        // ABI is synchronous, so no other reference to it can be alive concurrently.
        unsafe { &mut *($ctx as *mut $B) }
    }};
}

/// Read a borrowed `(ptr, len)` pair into an owned `String`, checked rather than lossy — the same
/// argument `CBar::to_bar`'s doc comment makes: bytes crossing this ABI are supposed to be an
/// exact copy of a Rust `&str`'s bytes, so a decode failure is a CONTRACT VIOLATION worth a loud
/// panic rather than a quietly-corrupted symbol. That panic is only safe to take because every
/// call site below runs inside [`guarded`] — never call this outside one.
fn read_str(ptr: *const u8, len: usize) -> String {
    if len == 0 {
        return String::new();
    }
    // SAFETY: `(ptr, len)` is the pair the ABI contract requires — borrowed for the duration of
    // this call only, from a valid `&str`'s bytes (`abi.rs`'s module doc states the contract).
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8(bytes.to_vec())
        .expect("plugin ABI contract violated: symbol bytes are not valid UTF-8")
}

thread_local! {
    /// Set by [`guarded`] when a host-side thunk panics during the CURRENT dispatch.
    /// `PluginStrategy::on_bar` resets it before dispatching into the plugin and reads it after —
    /// see `guarded`'s own doc for why swallowing the panic without this would be worse than not
    /// catching it at all.
    static POISONED: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, catching any panic and returning `neutral` instead, so a panic inside a `t_*` thunk
/// (below) — an `extern "C"` frame CALLED BY PLUGIN CODE — never unwinds across that C-ABI frame.
/// See this module's header (Critical 1) for why the design's own "the plugin wraps catch_unwind"
/// rule does not already cover this direction.
///
/// `AssertUnwindSafe`: `&mut B` is not `UnwindSafe` by default (a panic mid-mutation could leave
/// the referent inconsistent), and this crate accepts that deliberately — a panicking `Broker`
/// impl is a HOST-SIDE BUG, and the alternative (an uncaught unwind aborting the whole backtest
/// server) is strictly worse than a broker left in a possibly-inconsistent state whose CALLER is
/// about to be told, loudly, to stop trusting this dispatch (see [`POISONED`] below).
///
/// Latches [`POISONED`] rather than merely swallowing the panic: a thunk quietly returning `0.0`
/// for `equity` after a panic would read as a plausible, WRONG NUMBER with no signal anything was
/// skipped — silently wrong is worse than loudly failed. `PluginStrategy::on_bar` reads the flag
/// once the plugin's call returns and re-raises as a genuine Rust panic, which is safe to do
/// there: that frame is back on the host's own stack, no C-ABI boundary in the way.
///
/// ⚠ **Known limit of the poison design, recorded rather than fixed (round-2 review): the
/// re-panic in `on_bar` stops the CALLER trusting the REST of this dispatch, but it does not undo
/// side effects a plugin already took on an EARLIER neutral value.** A plugin whose `on_bar` reads
/// `equity()` (returns `0.0`, poisoned), sizes an order off that `0.0`, submits it, and THEN calls
/// `position()` (which also panics) will have already placed that order by the time `on_bar`
/// re-panics — the poison flag makes the DISPATCH untrustworthy in hindsight, it does not roll
/// back what the plugin already did with the bad number. Track A's scope is "does not abort the
/// process"; "does not act on a bad number" would need the host to refuse `submit_*` calls once
/// poisoned, which is follow-up work, not built here.
///
/// ⚠ **The recovery path itself must not be able to escape this frame.** `eprintln!` panics if
/// stderr is closed — reporting the ORIGINAL panic must never itself become a SECOND, uncaught one
/// unwinding across the very `extern "C"` frame this function exists to protect. [`POISONED`] is
/// therefore set FIRST, unconditionally, and the message formatting + `eprintln!` run inside their
/// OWN nested `catch_unwind` — best-effort reporting, never load-bearing.
fn guarded<F: FnOnce() -> R, R>(neutral: R, f: F) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            POISONED.with(|p| p.set(true));
            // Best-effort: if even reporting the panic panics (a closed stderr), swallow that
            // too rather than let it unwind across this extern "C" frame in its place.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let msg = panic_payload_message(&payload);
                eprintln!(
                    "vike-strategy-plugin: a host-side Broker/HftBroker call panicked while a \
                     plugin was calling back into the host: {msg}. Returning a neutral \
                     placeholder for this one call; the dispatch that triggered it is poisoned — \
                     see PluginStrategy::on_bar."
                );
            }));
            neutral
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

// ---- the eleven BrokerVTable thunks, one instantiation per concrete B a caller monomorphizes ----

extern "C" fn t_submit_market<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_market(&read_str(s, n), side, qty));
}
extern "C" fn t_submit_limit<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    price: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_limit(&read_str(s, n), side, qty, price));
}
extern "C" fn t_position<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).position(&read_str(s, n)))
}
extern "C" fn t_price<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).price(&read_str(s, n)))
}
extern "C" fn t_equity<B: Broker>(ctx: *mut c_void) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).equity())
}
extern "C" fn t_index<B: Broker>(ctx: *mut c_void) -> usize {
    guarded(0, || broker_mut!(ctx, B).index())
}
extern "C" fn t_now<B: Broker>(ctx: *mut c_void) -> i64 {
    guarded(0, || broker_mut!(ctx, B).now())
}
extern "C" fn t_bars_len<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> usize {
    guarded(0, || broker_mut!(ctx, B).bars(&read_str(s, n)).len())
}
extern "C" fn t_bar_at<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    i: usize,
    out: *mut CBar,
) -> bool {
    guarded(false, || {
        let symbol = read_str(s, n);
        match broker_mut!(ctx, B).bars(&symbol).get(i) {
            Some(bar) => {
                // SAFETY: `out` is the live, aligned `*mut CBar` the guest passed for exactly this
                // call — the contract `BrokerVTable::bar_at`'s own doc states.
                unsafe { *out = CBar::from_bar(bar) };
                true
            }
            None => false,
        }
    })
}
extern "C" fn t_quote_vwap<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) -> crate::abi::COptF64 {
    guarded(crate::abi::COptF64::from(None), || {
        broker_mut!(ctx, B).quote_vwap(&read_str(s, n), side, qty).into()
    })
}
extern "C" fn t_depth_within_price<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    limit_px: f64,
) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).depth_within_price(&read_str(s, n), side, limit_px))
}

// ---- the four HftBroker thunks (ABI_VERSION 2) — bound `B: HftBroker`, unlike the eleven above ----

extern "C" fn t_hft_position<B: HftBroker>(ctx: *mut c_void) -> f64 {
    // UFCS: `Broker::position` and `HftBroker::position` share a name (see this module's header).
    guarded(0.0, || HftBroker::position(broker_mut!(ctx, B)))
}
extern "C" fn t_submit_limit_tagged<B: HftBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    price: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_limit_tagged(&read_str(s, n), side, qty, price));
}
extern "C" fn t_modify_tagged<B: HftBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    new_qty: crate::abi::COptF64,
    new_price: crate::abi::COptF64,
) {
    guarded((), || {
        broker_mut!(ctx, B).modify_tagged(&read_str(s, n), new_qty.into(), new_price.into());
    });
}
extern "C" fn t_cancel_tagged<B: HftBroker>(ctx: *mut c_void, s: *const u8, n: usize) {
    guarded((), || broker_mut!(ctx, B).cancel_tagged(&read_str(s, n)));
}

// ---- the BOOK cursor (ABI_VERSION 3) ----------------------------------------------------------
//
// Not generic: the payload type is the concrete `L2Book`, so one `static` vtable serves every `B`
// and none of `broker_vtable`'s `TypeId` registry is needed here.

/// The host's flattened view of ONE `L2Book`, built once per `on_order_book` dispatch and lent to
/// the plugin through [`BookRef::ctx`].
///
/// ⚠ **Why `ctx` is this and not the `&L2Book` itself.** `L2Book`'s sides are `BTreeMap`s, which
/// have no index — a literal per-index cursor over one would be `iter().nth(i)`, O(i), so a full
/// walk would be O(levels²) node steps to deliver O(levels) values. Flattening once here makes
/// [`BookVTable::level_at`] O(1) and the whole dispatch O(levels). `abi.rs`'s cursor section
/// states the resulting per-call cost end to end.
struct BookCursor {
    tick_size: f64,
    last_seq: u64,
    /// Best first: highest bid first.
    bids: Vec<crate::abi::CBookLevel>,
    /// Best first: lowest ask first.
    asks: Vec<crate::abi::CBookLevel>,
}

impl BookCursor {
    fn of(book: &vike_marketdata::L2Book) -> Self {
        // `top_n` is the ONE public read that yields both sides ordered best-first; asking for the
        // deeper side's length gives every level of both, since it takes at most `n` per side.
        let depth = book.bid_levels().max(book.ask_levels());
        let (bids, asks) = book.top_n(depth);
        let flatten = |v: Vec<vike_marketdata::BookLevel>| {
            v.into_iter()
                .map(|l| crate::abi::CBookLevel { price: l.price, qty: l.qty })
                .collect::<Vec<_>>()
        };
        BookCursor {
            tick_size: book.tick_size,
            last_seq: book.last_seq,
            bids: flatten(bids),
            asks: flatten(asks),
        }
    }

    fn side(&self, which: i32) -> &[crate::abi::CBookLevel] {
        if which >= 0 { &self.bids } else { &self.asks }
    }
}

/// Recover the `&BookCursor` a book thunk's erased `ctx` was built from — the `broker_mut!`
/// argument, one rung down: a safe `fn(*mut c_void) -> &'a BookCursor` would let its caller pick
/// any `'a`, so the act is expanded inline at each call site instead.
macro_rules! book_cursor {
    ($ctx:expr) => {{
        // SAFETY: every caller of a book thunk builds `$ctx` as `&cursor as *const BookCursor as
        // *mut c_void` in the SAME call frame that then invokes the plugin
        // (`PluginStrategy::on_order_book`, further down) — a live, immovable `BookCursor` for
        // exactly the duration of the plugin call currently unwinding back through this thunk.
        // The plugin ABI is synchronous, so nothing can drop it concurrently.
        unsafe { &*($ctx as *const BookCursor) }
    }};
}

extern "C" fn t_book_tick_size(ctx: *mut c_void) -> f64 {
    guarded(0.0, || book_cursor!(ctx).tick_size)
}
extern "C" fn t_book_last_seq(ctx: *mut c_void) -> u64 {
    guarded(0, || book_cursor!(ctx).last_seq)
}
extern "C" fn t_book_side_len(ctx: *mut c_void, side: i32) -> usize {
    guarded(0, || book_cursor!(ctx).side(side).len())
}
extern "C" fn t_book_level_at(
    ctx: *mut c_void,
    side: i32,
    i: usize,
    out: *mut crate::abi::CBookLevel,
) -> bool {
    guarded(false, || match book_cursor!(ctx).side(side).get(i) {
        Some(level) => {
            // SAFETY: `out` is the live, aligned `*mut CBookLevel` the guest passed for exactly
            // this call — the contract `BookVTable::level_at`'s own doc states.
            unsafe { *out = *level };
            true
        }
        None => false,
    })
}

/// The one book vtable, shared by every `B`. A plain `static` rather than
/// [`broker_vtable`]'s leaked registry because none of these thunks is generic — the footgun that
/// registry exists for (a function-local `static` in a generic fn being ONE instance across every
/// monomorphisation) cannot arise where there is nothing to monomorphise.
static BOOK_VTABLE: BookVTable = BookVTable {
    tick_size: t_book_tick_size,
    last_seq: t_book_last_seq,
    side_len: t_book_side_len,
    level_at: t_book_level_at,
};

/// The `BrokerVTable` a plugin calls back into, monomorphized for `B` and cached for the life of
/// the process — one leaked `BrokerVTable` per DISTINCT `B` ever asked for (in the shipped
/// binaries, exactly one: `SimBroker`).
///
/// Generic rather than the design's literal "`broker_vtable() -> &'static BrokerVTable`" —
/// necessarily so; see this module's header. The cache is keyed by [`TypeId`] rather than a plain
/// function-local `static`, because a `static` declared inside a generic function whose TYPE does
/// not itself mention the generic parameter is exactly ONE instance shared across every
/// monomorphization (a well-known Rust footgun): `BrokerVTable`'s fields are already type-erased
/// `extern "C" fn(*mut c_void, ..)` pointers, so its type never mentions `B`, and a naive
/// `static VT: OnceLock<BrokerVTable>` inside this function would hand every `B` the FIRST
/// caller's thunks. The registry sidesteps it with zero `unsafe`: `Box::leak` gives a genuinely
/// `'static` reference per key, and the map is read back by value (`&'static BrokerVTable` is
/// `Copy`), so nothing here borrows from the `MutexGuard`.
///
/// ⚠ Called ONCE per [`PluginStrategy`] (from `PluginStrategy::new`), not once per dispatch — a
/// review flagged the earlier version's per-`on_bar` call as a `Mutex` lock + `TypeId` hash +
/// `HashMap` lookup paid on every bar in the backtest hot loop, for a value that never changes for
/// a given `B`.
pub fn broker_vtable<B: HftBroker + 'static>() -> &'static BrokerVTable {
    static REGISTRY: OnceLock<Mutex<HashMap<TypeId, &'static BrokerVTable>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(TypeId::of::<B>()).or_insert_with(|| {
        Box::leak(Box::new(BrokerVTable {
            submit_market: t_submit_market::<B>,
            submit_limit: t_submit_limit::<B>,
            position: t_position::<B>,
            price: t_price::<B>,
            equity: t_equity::<B>,
            index: t_index::<B>,
            now: t_now::<B>,
            bars_len: t_bars_len::<B>,
            bar_at: t_bar_at::<B>,
            quote_vwap: t_quote_vwap::<B>,
            depth_within_price: t_depth_within_price::<B>,
            hft_position: t_hft_position::<B>,
            submit_limit_tagged: t_submit_limit_tagged::<B>,
            modify_tagged: t_modify_tagged::<B>,
            cancel_tagged: t_cancel_tagged::<B>,
        }))
    })
}

/// A loaded plugin's exported dispatch symbols this file gives meaning to. `loader.rs` (Task 4)
/// is what NAMES and RESOLVES these via `dlsym`; this struct is defined here because
/// [`PluginStrategy`] is what the pointers mean, and the loader only produces one.
///
/// ⚠ **It used to be deliberately NARROW — `create`/`destroy` plus `warmup`/`on_bar` — and it is
/// not any more.** At `ABI_VERSION` 3 it carries the WHOLE `vike_model::Strategy` seam except the
/// three members named in [`UNWIRED_HOOKS`], so a plugin receives what a compiled-in strategy
/// receives. The narrow version was safe only because of the build-time refusal that reads
/// [`UNWIRED_HOOKS`]; every hook moved from that list to [`WIRED_HOOKS`] is a refusal lifted.
///
/// Field order mirrors the declaration order of `vike_model::Strategy`'s own methods, so a reader
/// comparing the two reads them in one pass.
#[derive(Clone, Copy, Debug)]
pub struct PluginVTable {
    pub create: extern "C" fn(*const u8, usize) -> *mut c_void,
    pub destroy: extern "C" fn(*mut c_void),
    pub warmup: extern "C" fn(*mut c_void) -> usize,
    pub on_start: extern "C" fn(*mut c_void, BrokerRef) -> PluginStatus,
    pub on_bar: extern "C" fn(*mut c_void, BrokerRef, *const CBar) -> PluginStatus,
    pub on_quote_tick: extern "C" fn(*mut c_void, BrokerRef, *const CQuoteTick) -> PluginStatus,
    pub on_trade_tick: extern "C" fn(*mut c_void, BrokerRef, *const CTradeTick) -> PluginStatus,
    pub on_order_book: extern "C" fn(*mut c_void, BrokerRef, BookRef) -> PluginStatus,
    /// `tag` crosses as the borrowed `(ptr, len)` every string in this ABI uses.
    pub on_schedule: extern "C" fn(*mut c_void, BrokerRef, *const u8, usize) -> PluginStatus,
    pub on_fill: extern "C" fn(*mut c_void, BrokerRef, *const CFill) -> PluginStatus,
    /// One of `abi`'s `FEED_STATUS_*` codes — see `abi::feed_status_code`.
    pub on_feed_status: extern "C" fn(*mut c_void, BrokerRef, u32) -> PluginStatus,
    pub on_mark: extern "C" fn(*mut c_void, BrokerRef, *const CMarkTick) -> PluginStatus,
    /// `venue` is a borrowed `(ptr, len)`, carried as an ARGUMENT because no tick type carries a
    /// venue — `vike_model::Strategy::on_reference_quote`'s own doc argues why.
    pub on_reference_quote:
        extern "C" fn(*mut c_void, BrokerRef, *const u8, usize, *const CQuoteTick) -> PluginStatus,
    pub on_flow: extern "C" fn(*mut c_void, BrokerRef, *const CFlowToxicity) -> PluginStatus,
    pub on_order_event:
        extern "C" fn(*mut c_void, BrokerRef, *const COrderLifecycle) -> PluginStatus,
    /// The params bag as a JSON document, borrowed `(ptr, len)` — `guest::strategy_params_from_json`
    /// carries the argument for JSON over TOML and over a flat mirror.
    pub on_params_updated: extern "C" fn(*mut c_void, BrokerRef, *const u8, usize) -> PluginStatus,
    pub on_stop: extern "C" fn(*mut c_void, BrokerRef) -> PluginStatus,
}

/// A plugin instance wearing [`Strategy<B>`] for the SAME `B` it was constructed with.
///
/// Generic over `B` (rather than the earlier non-generic struct implementing `Strategy<B>` for
/// EVERY `B` via a blanket impl) for two reasons a review surfaced together: it lets `new` resolve
/// and cache [`broker_vtable`] ONCE at construction instead of once per dispatch, and it makes the
/// type system — not a runtime check — refuse a `PluginStrategy<SimBroker>` ever being asked to
/// route a DIFFERENT broker's `ctx` through thunks built for `SimBroker`.
///
/// RAII over the plugin's own lifecycle: [`PluginStrategy::new`] calls `create` once, [`Drop`]
/// calls `destroy` once — the caller never needs an explicit teardown call.
pub struct PluginStrategy<B> {
    vt: PluginVTable,
    handle: *mut c_void,
    /// Resolved once in `new`, not on every `on_bar` — see [`broker_vtable`]'s own doc for the
    /// cost this replaces.
    broker_vtable: &'static BrokerVTable,
    _broker: PhantomData<B>,
}

impl<B: HftBroker + 'static> PluginStrategy<B> {
    /// `params_toml` crosses as `(ptr, len)` text, never a `toml::Value` — no Rust type crosses
    /// this ABI by value (this module's header). The plugin parses it on its own side.
    ///
    /// Does NOT refuse a null `create` result with a `Result` return: `create`/`destroy` are the
    /// ONE lifecycle pair this ABI gives no separate status channel for (unlike `on_bar`, which
    /// returns a real [`PluginStatus`]) — null is the only signal a failed creation has, and a
    /// fallible constructor a caller can `.unwrap()` without reading is no safer than what this
    /// does instead: carry the null handle, and have EVERY dispatch method check it before
    /// calling into the plugin at all (`warmup`/`on_bar` below), reporting
    /// `PluginStatus::BadHandle` — the enum variant this ABI already declares for exactly this.
    pub fn new(vt: PluginVTable, params_toml: &str) -> Self {
        let handle = (vt.create)(params_toml.as_ptr(), params_toml.len());
        Self { vt, handle, broker_vtable: broker_vtable::<B>(), _broker: PhantomData }
    }

    /// The ONE body every dispatch method below shares: refuse a null handle, arm the poison
    /// latch, hand the plugin a `BrokerRef` built on THIS call's `&mut B`, then judge what came
    /// back.
    ///
    /// ⚠ **It is a shared helper because it was a shared BODY, and the widening from two hooks to
    /// fifteen is what made that matter.** Each of the four acts below is easy to leave out of one
    /// hook and impossible to notice afterwards: a missing `POISONED` reset makes a LATER hook
    /// inherit an earlier one's panic, a missing poison READ lets a neutral placeholder pass for
    /// an answer, and a missing status check drops a `Panicked` dispatch in silence. Fifteen
    /// hand-written copies would have been fifteen chances to omit one, and the omission reads as
    /// working code.
    ///
    /// `hook` is named in both diagnostics so a failure says WHICH seam method failed — with two
    /// hooks the message could hard-code `on_bar`; with fifteen a generic message would send a
    /// reader hunting.
    fn dispatch(
        &self,
        hook: &'static str,
        broker: &mut B,
        call: impl FnOnce(*mut c_void, BrokerRef) -> PluginStatus,
    ) {
        let status = if self.handle.is_null() {
            // Produce the REAL enum value rather than merely naming it in a message: a null
            // handle is exactly what `PluginStatus::BadHandle` models, so it is reported through
            // the same `status != PluginStatus::Ok` path below instead of a parallel one.
            PluginStatus::BadHandle
        } else {
            POISONED.with(|p| p.set(false));
            let r = BrokerRef { ctx: (broker as *mut B).cast(), vtable: self.broker_vtable };
            let s = call(self.handle, r);
            if POISONED.with(|p| p.get()) {
                // Safe to raise a genuine panic HERE: this frame is back on the host's own stack,
                // with no C-ABI boundary between it and the panic site — unlike the thunks
                // `guarded` protects, propagating here does not abort the process. Correct to
                // raise one: continuing would let the CALLER (this method's own caller) believe
                // whatever neutral placeholder the panicking thunk returned was a real answer.
                panic!(
                    "vike-strategy-plugin: a host-side Broker/HftBroker thunk panicked while \
                     this plugin's {hook} was dispatching (see the panic message printed above \
                     for the real cause)."
                );
            }
            s
        };
        if status != PluginStatus::Ok {
            // Every `Strategy` hook returns `()` — there is no Result to propagate through the
            // trait, so a non-Ok status is reported rather than silently dropped. `eprintln!`
            // rather than `tracing`: this crate takes no logging dependency, and a plugin
            // dispatch failure is exactly the kind of "this needs an operator's eyes" event
            // stderr already serves for the composition roots that wrap this call.
            eprintln!(
                "vike-strategy-plugin: plugin {hook} returned {status:?} — this dispatch was \
                 SKIPPED, so the strategy did not see the event. A Panicked status means the \
                 plugin's own catch_unwind caught a panic in user strategy code (\"a panic across \
                 a C-ABI boundary is UB\" — the design doc); BadHandle means create() returned a \
                 null handle; BadParams means the payload carried a code or a document this \
                 plugin could not decode."
            );
        }
    }
}

impl<B> Drop for PluginStrategy<B> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            (self.vt.destroy)(self.handle);
        }
    }
}

// `PluginStrategy` holds a raw `*mut c_void` a single plugin instance owns exclusively; it is
// neither `Send` nor `Sync` by default (a raw pointer field opts both out), which is correct for
// Track A's backtest-only scope — nothing here claims the plugin's own code is thread-safe.

impl<B: HftBroker + 'static> Strategy<B> for PluginStrategy<B> {
    fn warmup(&self) -> usize {
        if self.handle.is_null() {
            eprintln!(
                "vike-strategy-plugin: plugin handle is null (PluginStatus::BadHandle — create() \
                 failed) — warmup defaults to 0 rather than calling into the plugin"
            );
            return 0;
        }
        (self.vt.warmup)(self.handle)
    }

    fn on_start(&mut self, broker: &mut B) {
        self.dispatch("on_start", broker, |h, r| (self.vt.on_start)(h, r));
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let c = CBar::from_bar(bar);
        self.dispatch("on_bar", broker, |h, r| (self.vt.on_bar)(h, r, &c as *const CBar));
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &vike_marketdata::QuoteTick) {
        let c = CQuoteTick::of(q);
        self.dispatch("on_quote_tick", broker, |h, r| {
            (self.vt.on_quote_tick)(h, r, &c as *const CQuoteTick)
        });
    }

    fn on_trade_tick(&mut self, broker: &mut B, t: &vike_marketdata::TradeTick) {
        let c = CTradeTick::of(t);
        self.dispatch("on_trade_tick", broker, |h, r| {
            (self.vt.on_trade_tick)(h, r, &c as *const CTradeTick)
        });
    }

    fn on_order_book(&mut self, broker: &mut B, book: &vike_marketdata::L2Book) {
        // Flattened ONCE per dispatch, here, and lent to the plugin for exactly this call — see
        // `BookCursor`'s own doc for why the cursor's `ctx` is this and not the `&L2Book`.
        let cursor = BookCursor::of(book);
        let book_ref =
            BookRef { ctx: (&raw const cursor).cast::<c_void>().cast_mut(), vtable: &BOOK_VTABLE };
        self.dispatch("on_order_book", broker, |h, r| (self.vt.on_order_book)(h, r, book_ref));
    }

    fn on_schedule(&mut self, broker: &mut B, tag: &str) {
        self.dispatch("on_schedule", broker, |h, r| {
            (self.vt.on_schedule)(h, r, tag.as_ptr(), tag.len())
        });
    }

    fn on_fill(&mut self, broker: &mut B, fill: &vike_model::Fill) {
        let c = CFill::of(fill);
        self.dispatch("on_fill", broker, |h, r| (self.vt.on_fill)(h, r, &c as *const CFill));
    }

    fn on_feed_status(&mut self, broker: &mut B, status: vike_model::FeedStatus) {
        let code = crate::abi::feed_status_code(status);
        self.dispatch("on_feed_status", broker, |h, r| (self.vt.on_feed_status)(h, r, code));
    }

    fn on_mark(&mut self, broker: &mut B, mark: &vike_model::MarkTick) {
        let c = CMarkTick::of(mark);
        self.dispatch("on_mark", broker, |h, r| (self.vt.on_mark)(h, r, &c as *const CMarkTick));
    }

    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &vike_marketdata::QuoteTick) {
        let c = CQuoteTick::of(q);
        self.dispatch("on_reference_quote", broker, |h, r| {
            (self.vt.on_reference_quote)(h, r, venue.as_ptr(), venue.len(), &c as *const CQuoteTick)
        });
    }

    fn on_flow(&mut self, broker: &mut B, flow: vike_model::FlowToxicity) {
        let c = CFlowToxicity::of(flow);
        self.dispatch("on_flow", broker, |h, r| {
            (self.vt.on_flow)(h, r, &c as *const CFlowToxicity)
        });
    }

    fn on_order_event(&mut self, broker: &mut B, event: &vike_model::OrderLifecycle) {
        let c = COrderLifecycle::of(event);
        self.dispatch("on_order_event", broker, |h, r| {
            (self.vt.on_order_event)(h, r, &c as *const COrderLifecycle)
        });
    }

    fn on_params_updated(&mut self, broker: &mut B, params: &vike_model::StrategyParams) {
        // ⚠ A serialisation failure must NOT be silently skipped — that is exactly the
        // hook-arrived-empty shape this whole widening exists to end. `serde_json` cannot fail on
        // these types (no map with non-string keys, no NaN outside `f64`'s own allowance), but
        // "cannot fail" is a claim about today's `StrategyParams`, so the impossible branch
        // reports itself rather than returning.
        let json = match serde_json::to_string(params) {
            Ok(j) => j,
            Err(e) => {
                eprintln!(
                    "vike-strategy-plugin: could not serialise StrategyParams for this plugin's \
                     on_params_updated: {e}. The update was NOT delivered — the strategy is still \
                     running its previous tunables. This is a defect in the params type, not in \
                     the operator's input."
                );
                return;
            }
        };
        self.dispatch("on_params_updated", broker, |h, r| {
            (self.vt.on_params_updated)(h, r, json.as_ptr(), json.len())
        });
    }

    fn on_stop(&mut self, broker: &mut B) {
        self.dispatch("on_stop", broker, |h, r| (self.vt.on_stop)(h, r));
    }

    // ⚠ `params`, `save_state` and `load_state` are the three `Strategy` members still left on the
    // trait's own default — [`UNWIRED_HOOKS`], and the build-time refusal that reads it is what
    // keeps that safe. See that constant's doc for the argument; it is a DIFFERENT argument from
    // the one the thirteen wired above were left out on, which was simply "not written yet".
}

/// The `Strategy` seam methods [`PluginVTable`] actually carries, so a plugin genuinely receives
/// them.
///
/// ⚠ **Adding a field to [`PluginVTable`] means adding its name here.** Forgetting leaves
/// `build_plugin` refusing a hook that now works — loud and immediate, which is the correct
/// direction for this list to rot in.
pub const WIRED_HOOKS: &[&str] = &[
    "warmup",
    "on_start",
    "on_bar",
    "on_quote_tick",
    "on_trade_tick",
    "on_order_book",
    "on_schedule",
    "on_fill",
    "on_feed_status",
    "on_mark",
    "on_reference_quote",
    "on_flow",
    "on_order_event",
    "on_params_updated",
    "on_stop",
];

/// Every OTHER `vike_model::Strategy` method: declared by the trait, overridable by a user
/// strategy, and NOT carried by [`PluginVTable`] — so an override of one is dead code under the
/// plugin mechanism and live code under the build-time tier.
///
/// `vike_strategy_builder::render::build_plugin` refuses a source that overrides one of these,
/// which is the only signal that exists; see the comment at the end of [`Strategy`]'s impl above
/// for what happened when there was none.
///
/// ⚠ **Held complete against the real trait** by
/// `crates/vike-strategy-plugin/tests/hook_roster.rs`, which reads
/// `crates/vike-model/src/strategy/mod.rs` and fails if the trait grows a method neither list
/// names — otherwise a new hook would join the silent set the day it was declared, which is the
/// failure this whole pair exists to end.
/// ⚠ **It was SIXTEEN and is now THREE, and the three that remain are a different kind of
/// omission from the thirteen that left.** The thirteen were unwired because nobody had written
/// the slots yet; these three are unwired because wiring them means a decision this crate does not
/// own:
///
/// * **`save_state` and `params` RETURN owned data** (`Option<serde_json::Value>`,
///   `Option<StrategyParams>`). Every slot in [`PluginVTable`] today is fire-and-forget — the
///   plugin borrows, acts, and answers with a `PluginStatus`. Returning a document means the
///   PLUGIN allocates and the HOST frees across the `.so` boundary, which needs an ownership
///   contract (a `vike_plugin_free_string` the host must call on exactly the pointers the plugin
///   minted, never on any other) that this ABI has never had and that nothing here would
///   machine-check. That is a new hazard class, not a fourteenth copy of an existing pattern.
/// * **`load_state` takes a `&serde_json::Value`.** Its payload could cross as JSON text like
///   `on_params_updated`'s does — the mechanism exists. What it lacks is a caller: nothing in this
///   tree calls `Strategy::load_state` on a backtest path, so wiring it would ship a slot whose
///   only witness could ever be a test calling it directly. It is grouped with its READ half
///   deliberately; a save/load pair half-delivered is worse than one plainly absent, because a
///   strategy that restores state it never saved starts from a lie.
///
/// So the refusal mechanism is unchanged and still bites — it now refuses exactly these three.
/// Anything above stays true of them: a user file overriding one compiles, renders, links, loads
/// and handshakes, then never receives the hook.
pub const UNWIRED_HOOKS: &[&str] = &["params", "save_state", "load_state"];

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBroker {
        pos: f64,
        submits: Vec<(String, i32, f64)>,
    }

    impl Broker for FakeBroker {
        fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
            self.submits.push((symbol.to_string(), side, qty));
        }
        fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, _symbol: &str) -> f64 {
            self.pos
        }
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }

    impl HftBroker for FakeBroker {
        fn position(&self) -> f64 {
            self.pos
        }
        fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
        fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
        fn cancel_tagged(&mut self, _tag: &str) {}
    }

    /// A non-null, never-dereferenced sentinel handle for fakes that carry no real state — using
    /// a genuine null here (as an earlier version of this test module did) would now trip the
    /// `PluginStatus::BadHandle` guard `PluginStrategy` checks before every dispatch, since that
    /// guard cannot distinguish "no state needed" from "creation failed" any more than the real
    /// ABI can (see `PluginStrategy::new`'s doc).
    fn dummy_handle() -> *mut c_void {
        std::ptr::without_provenance_mut(1)
    }

    // A hand-built fake plugin: no `.so`, no dlopen. `fake_on_bar` wraps its received `BrokerRef`
    // as a `guest::HostBroker` and submits a market order sized off the bar's close — proving
    // guest.rs and host.rs interoperate correctly with each other, not just in isolation.
    extern "C" fn fake_create(_params_ptr: *const u8, _params_len: usize) -> *mut c_void {
        dummy_handle()
    }
    extern "C" fn fake_destroy(_handle: *mut c_void) {}
    extern "C" fn fake_warmup(_handle: *mut c_void) -> usize {
        2
    }
    extern "C" fn fake_on_bar(
        _handle: *mut c_void,
        r: BrokerRef,
        bar: *const CBar,
    ) -> PluginStatus {
        // SAFETY: `bar` is the live, aligned `*const CBar` `PluginStrategy::on_bar` passed for
        // exactly this call.
        let close = unsafe { (*bar).close };
        let mut broker = crate::guest::HostBroker::new(r);
        vike_model::Broker::submit_market(&mut broker, "BTCUSDT", 1, close);
        PluginStatus::Ok
    }
    extern "C" fn fake_on_bar_panicking(
        _handle: *mut c_void,
        _r: BrokerRef,
        _bar: *const CBar,
    ) -> PluginStatus {
        PluginStatus::Panicked
    }

    // ---- the thirteen ABI_VERSION 3 dispatch slots, as inert stubs -----------------------------
    //
    // Only the slots a given test drives are overridden on the returned vtable; everything else
    // answers `Ok` and does nothing. A stub per slot rather than one shared function because the
    // signatures genuinely differ — that difference is the ABI.

    extern "C" fn stub_on_start(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_stop(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_quote_tick(
        _h: *mut c_void,
        _r: BrokerRef,
        _q: *const CQuoteTick,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_trade_tick(
        _h: *mut c_void,
        _r: BrokerRef,
        _t: *const CTradeTick,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_order_book(_h: *mut c_void, _r: BrokerRef, _b: BookRef) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_schedule(
        _h: *mut c_void,
        _r: BrokerRef,
        _t: *const u8,
        _n: usize,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_fill(_h: *mut c_void, _r: BrokerRef, _f: *const CFill) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_feed_status(_h: *mut c_void, _r: BrokerRef, _s: u32) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_mark(
        _h: *mut c_void,
        _r: BrokerRef,
        _m: *const CMarkTick,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_reference_quote(
        _h: *mut c_void,
        _r: BrokerRef,
        _v: *const u8,
        _n: usize,
        _q: *const CQuoteTick,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_flow(
        _h: *mut c_void,
        _r: BrokerRef,
        _f: *const CFlowToxicity,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_order_event(
        _h: *mut c_void,
        _r: BrokerRef,
        _e: *const COrderLifecycle,
    ) -> PluginStatus {
        PluginStatus::Ok
    }
    extern "C" fn stub_on_params_updated(
        _h: *mut c_void,
        _r: BrokerRef,
        _p: *const u8,
        _n: usize,
    ) -> PluginStatus {
        PluginStatus::Ok
    }

    fn fake_vtable() -> PluginVTable {
        PluginVTable {
            create: fake_create,
            destroy: fake_destroy,
            warmup: fake_warmup,
            on_start: stub_on_start,
            on_bar: fake_on_bar,
            on_quote_tick: stub_on_quote_tick,
            on_trade_tick: stub_on_trade_tick,
            on_order_book: stub_on_order_book,
            on_schedule: stub_on_schedule,
            on_fill: stub_on_fill,
            on_feed_status: stub_on_feed_status,
            on_mark: stub_on_mark,
            on_reference_quote: stub_on_reference_quote,
            on_flow: stub_on_flow,
            on_order_event: stub_on_order_event,
            on_params_updated: stub_on_params_updated,
            on_stop: stub_on_stop,
        }
    }

    fn bar(close: f64) -> Bar {
        Bar {
            ts: 1,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// `CBar`'s round trip is exhaustively covered in `abi.rs` (Task 1); this is a thin re-check
    /// that host.rs's own re-export path still round-trips, including the optionals — kept per
    /// Task 3's brief, but not duplicating that file's full coverage.
    #[test]
    fn a_bar_survives_the_round_trip_including_its_optionals() {
        let b = Bar {
            ts: 1,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            funding: Some(0.01),
            bid: None,
            ask: Some(1.6),
            symbol: Some("BTCUSDT".to_string()),
        };
        let back = CBar::from_bar(&b).to_bar();
        assert_eq!(back.ts, b.ts);
        assert_eq!(back.close, b.close);
        assert_eq!(back.funding, Some(0.01));
        assert_eq!(back.bid, None, "an absent optional must not arrive as Some(NaN)");
        assert_eq!(back.symbol.as_deref(), Some("BTCUSDT"));
    }

    #[test]
    fn warmup_crosses_the_vtable() {
        let strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(fake_vtable(), ""));
        assert_eq!(strategy.warmup(), 2);
    }

    #[test]
    fn on_bar_reaches_the_real_broker_through_both_wrappers() {
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(fake_vtable(), ""));
        let mut broker = FakeBroker::default();
        strategy.on_bar(&mut broker, &bar(42.0));
        assert_eq!(broker.submits, vec![("BTCUSDT".to_string(), 1, 42.0)]);
    }

    /// A plugin that reports `Panicked` must not abort the process or unwind out of `on_bar` — it
    /// is a reported, contained failure. `tests/load_refusals.rs` (Task 4) proves the stronger
    /// claim (a REAL panic inside a compiled `.so`, caught by ITS OWN catch_unwind); this proves
    /// the host's side of the contract: receiving `Panicked` never panics the caller.
    #[test]
    fn a_panicked_status_is_reported_and_does_not_panic_the_caller() {
        let mut vt = fake_vtable();
        vt.on_bar = fake_on_bar_panicking;
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut broker = FakeBroker::default();
        strategy.on_bar(&mut broker, &bar(1.0));
        assert!(broker.submits.is_empty(), "a panicked dispatch must not have reached the broker");
    }

    /// Important-4: a null `create()` handle must be reported as `PluginStatus::BadHandle` and
    /// must NEVER be handed to `warmup`/`on_bar` — both plugin functions below panic if called at
    /// all, so this test fails LOUDLY if the guard regresses. ⚠ Not "a real panic, not a swallowed
    /// one" (an earlier version of this comment said that, and a round-2 review caught it): both
    /// vtable slots are `extern "C" fn` pointers, and the abort-on-unwind-across-`extern "C"`
    /// behavior is baked into a function's OWN compiled body by its declared ABI, regardless of
    /// whether the call crosses a real `dlopen` boundary or stays in-process — so a regression
    /// here would ABORT the test process at the `must_not_be_called_*` call site, not raise a
    /// catchable Rust panic `#[test]`'s own harness could report as a normal failure. Either way
    /// the guard's absence is unmistakable; it just would not look like an ordinary red test.
    #[test]
    fn a_null_handle_is_never_handed_to_the_plugin() {
        extern "C" fn create_returns_null(_p: *const u8, _n: usize) -> *mut c_void {
            std::ptr::null_mut()
        }
        extern "C" fn must_not_be_called_warmup(_h: *mut c_void) -> usize {
            panic!("warmup must never be called with a null handle");
        }
        extern "C" fn must_not_be_called_on_bar(
            _h: *mut c_void,
            _r: BrokerRef,
            _b: *const CBar,
        ) -> PluginStatus {
            panic!("on_bar must never be called with a null handle");
        }
        let mut vt = fake_vtable();
        vt.create = create_returns_null;
        vt.warmup = must_not_be_called_warmup;
        vt.on_bar = must_not_be_called_on_bar;
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut broker = FakeBroker::default();

        assert_eq!(strategy.warmup(), 0, "a null handle must default warmup to 0");
        strategy.on_bar(&mut broker, &bar(1.0)); // must not panic and must not reach the broker
        assert!(broker.submits.is_empty());
        // ⚠ This test proves the `warmup`/`on_bar` guards, and ONLY those — `Drop`'s OWN
        // null-handle skip (`if !self.handle.is_null() { (self.vt.destroy)(self.handle); }`) is
        // NOT exercised here: `fake_destroy` is a no-op, so this test would pass identically
        // whether or not that skip existed. Proving it would need the same `must_not_be_called_*`
        // trap on `destroy`, which this test does not set up.
    }

    /// Critical-1: a panic inside the HOST's own `Broker` impl — reached from a `t_*` thunk the
    /// PLUGIN calls into during its `on_bar` — must not abort the process, and must surface as a
    /// genuine (catchable, on THIS side) panic from `PluginStrategy::on_bar` rather than silently
    /// producing a wrong number. `catch_unwind` here is what proves "did not abort": an escaped
    /// unwind across the `extern "C"` thunk beneath this call would already have aborted the
    /// process before this line could ever run.
    #[test]
    fn a_host_side_panic_inside_a_thunk_poisons_the_dispatch_and_is_re_raised() {
        struct PanickingBroker;
        impl Broker for PanickingBroker {
            fn submit_market(&mut self, _s: &str, _side: i32, _qty: f64) {}
            fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _price: f64) {}
            fn position(&self, _s: &str) -> f64 {
                panic!("deliberate host-side panic for the Critical-1 mutation proof")
            }
            fn price(&self, _s: &str) -> f64 {
                0.0
            }
            fn equity(&self) -> f64 {
                0.0
            }
            fn bars(&self, _s: &str) -> &[Bar] {
                &[]
            }
            fn index(&self) -> usize {
                0
            }
            fn now(&self) -> i64 {
                0
            }
        }
        impl HftBroker for PanickingBroker {
            fn position(&self) -> f64 {
                0.0
            }
            fn submit_limit_tagged(&mut self, _t: &str, _side: i32, _qty: f64, _price: f64) {}
            fn modify_tagged(&mut self, _t: &str, _q: Option<f64>, _p: Option<f64>) {}
            fn cancel_tagged(&mut self, _t: &str) {}
        }

        // A fake plugin whose on_bar calls the ONE thing that panics: Broker::position via the
        // guest's own HostBroker wrapper (crossing the same real BrokerVTable path production
        // code uses, not calling the thunk directly).
        extern "C" fn on_bar_calls_position(
            _h: *mut c_void,
            r: BrokerRef,
            _bar: *const CBar,
        ) -> PluginStatus {
            let broker = crate::guest::HostBroker::new(r);
            let _ = Broker::position(&broker, "BTCUSDT"); // reaches PanickingBroker::position
            PluginStatus::Ok
        }
        let mut vt = fake_vtable();
        vt.on_bar = on_bar_calls_position;
        let mut strategy: Box<dyn Strategy<PanickingBroker>> =
            Box::new(PluginStrategy::<PanickingBroker>::new(vt, ""));
        let mut broker = PanickingBroker;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            strategy.on_bar(&mut broker, &bar(1.0));
        }));
        assert!(
            result.is_err(),
            "the host-side panic must be re-raised from on_bar, not swallowed"
        );
    }

    /// Two different concrete brokers used in the SAME process must each get thunks that
    /// downcast `ctx` to THEIR OWN type — pinning the exact hazard `broker_vtable`'s doc warns
    /// about (a naive function-local `static` would hand every `B` the first caller's thunks).
    #[test]
    fn broker_vtable_is_correct_per_distinct_broker_type() {
        #[derive(Default)]
        struct OtherBroker {
            submits: Vec<(String, i32, f64)>,
        }
        impl Broker for OtherBroker {
            fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
                self.submits.push((symbol.to_string(), side, qty));
            }
            fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _price: f64) {}
            fn position(&self, _s: &str) -> f64 {
                0.0
            }
            fn price(&self, _s: &str) -> f64 {
                0.0
            }
            fn equity(&self) -> f64 {
                0.0
            }
            fn bars(&self, _s: &str) -> &[Bar] {
                &[]
            }
            fn index(&self) -> usize {
                0
            }
            fn now(&self) -> i64 {
                0
            }
        }
        impl HftBroker for OtherBroker {
            fn position(&self) -> f64 {
                0.0
            }
            fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
            fn modify_tagged(
                &mut self,
                _tag: &str,
                _new_qty: Option<f64>,
                _new_price: Option<f64>,
            ) {
            }
            fn cancel_tagged(&mut self, _tag: &str) {}
        }

        // Force both instantiations to exist in the same binary, in either order.
        let vt_fake = broker_vtable::<FakeBroker>();
        let vt_other = broker_vtable::<OtherBroker>();

        let mut a = FakeBroker::default();
        (vt_fake.submit_market)((&mut a as *mut FakeBroker).cast(), b"X".as_ptr(), 1, 1, 1.0);
        assert_eq!(a.submits, vec![("X".to_string(), 1, 1.0)]);

        let mut o = OtherBroker::default();
        (vt_other.submit_market)((&mut o as *mut OtherBroker).cast(), b"Y".as_ptr(), 1, -1, 2.0);
        assert_eq!(o.submits, vec![("Y".to_string(), -1, 2.0)]);
    }

    // ---- ABI_VERSION 3: every wired hook actually reaches the plugin --------------------------

    thread_local! {
        /// Which vtable slot each recording stub below was entered through, in order.
        static REACHED: std::cell::RefCell<Vec<&'static str>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn note(hook: &'static str) -> PluginStatus {
        REACHED.with(|v| v.borrow_mut().push(hook));
        PluginStatus::Ok
    }

    /// **The structural witness for this whole widening.** `PluginStrategy` implements
    /// `Strategy<B>`, and every hook it does NOT override falls through to the trait's own no-op
    /// default — which compiles, runs, and silently delivers nothing. That is exactly the failure
    /// the unwired-hook refusal was invented for, and adding a `PluginVTable` FIELD does not
    /// prevent it: a field can be declared, bound by the loader, exported by the template, and
    /// still never called, because the one line that calls it lives in the trait impl.
    ///
    /// So this drives all fifteen through the `Strategy` trait — the surface the backtest engine
    /// uses — and asserts each one arrived at its own slot, by name. Deleting any single
    /// `fn on_*` from the impl above makes it fail naming that hook.
    #[test]
    fn every_wired_hook_reaches_the_plugin_through_the_strategy_trait() {
        extern "C" fn r_on_start(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
            note("on_start")
        }
        extern "C" fn r_on_stop(_h: *mut c_void, _r: BrokerRef) -> PluginStatus {
            note("on_stop")
        }
        extern "C" fn r_on_bar(_h: *mut c_void, _r: BrokerRef, _b: *const CBar) -> PluginStatus {
            note("on_bar")
        }
        extern "C" fn r_on_quote_tick(
            _h: *mut c_void,
            _r: BrokerRef,
            _q: *const CQuoteTick,
        ) -> PluginStatus {
            note("on_quote_tick")
        }
        extern "C" fn r_on_trade_tick(
            _h: *mut c_void,
            _r: BrokerRef,
            _t: *const CTradeTick,
        ) -> PluginStatus {
            note("on_trade_tick")
        }
        extern "C" fn r_on_order_book(_h: *mut c_void, _r: BrokerRef, _b: BookRef) -> PluginStatus {
            note("on_order_book")
        }
        extern "C" fn r_on_schedule(
            _h: *mut c_void,
            _r: BrokerRef,
            _t: *const u8,
            _n: usize,
        ) -> PluginStatus {
            note("on_schedule")
        }
        extern "C" fn r_on_fill(_h: *mut c_void, _r: BrokerRef, _f: *const CFill) -> PluginStatus {
            note("on_fill")
        }
        extern "C" fn r_on_feed_status(_h: *mut c_void, _r: BrokerRef, _s: u32) -> PluginStatus {
            note("on_feed_status")
        }
        extern "C" fn r_on_mark(
            _h: *mut c_void,
            _r: BrokerRef,
            _m: *const CMarkTick,
        ) -> PluginStatus {
            note("on_mark")
        }
        extern "C" fn r_on_reference_quote(
            _h: *mut c_void,
            _r: BrokerRef,
            _v: *const u8,
            _n: usize,
            _q: *const CQuoteTick,
        ) -> PluginStatus {
            note("on_reference_quote")
        }
        extern "C" fn r_on_flow(
            _h: *mut c_void,
            _r: BrokerRef,
            _f: *const CFlowToxicity,
        ) -> PluginStatus {
            note("on_flow")
        }
        extern "C" fn r_on_order_event(
            _h: *mut c_void,
            _r: BrokerRef,
            _e: *const COrderLifecycle,
        ) -> PluginStatus {
            note("on_order_event")
        }
        extern "C" fn r_on_params_updated(
            _h: *mut c_void,
            _r: BrokerRef,
            _p: *const u8,
            _n: usize,
        ) -> PluginStatus {
            note("on_params_updated")
        }
        extern "C" fn r_warmup(_h: *mut c_void) -> usize {
            REACHED.with(|v| v.borrow_mut().push("warmup"));
            7
        }

        REACHED.with(|v| v.borrow_mut().clear());
        let vt = PluginVTable {
            create: fake_create,
            destroy: fake_destroy,
            warmup: r_warmup,
            on_start: r_on_start,
            on_bar: r_on_bar,
            on_quote_tick: r_on_quote_tick,
            on_trade_tick: r_on_trade_tick,
            on_order_book: r_on_order_book,
            on_schedule: r_on_schedule,
            on_fill: r_on_fill,
            on_feed_status: r_on_feed_status,
            on_mark: r_on_mark,
            on_reference_quote: r_on_reference_quote,
            on_flow: r_on_flow,
            on_order_event: r_on_order_event,
            on_params_updated: r_on_params_updated,
            on_stop: r_on_stop,
        };
        let mut s: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut b = FakeBroker::default();

        assert_eq!(s.warmup(), 7);
        s.on_start(&mut b);
        s.on_bar(&mut b, &bar(1.0));
        s.on_quote_tick(&mut b, &quote_tick());
        s.on_trade_tick(&mut b, &trade_tick());
        s.on_order_book(&mut b, &seeded_book());
        s.on_schedule(&mut b, "rebalance");
        s.on_fill(&mut b, &a_fill());
        s.on_feed_status(&mut b, vike_model::FeedStatus::Stale);
        s.on_mark(&mut b, &a_mark());
        s.on_reference_quote(&mut b, "binance", &quote_tick());
        s.on_flow(&mut b, vike_model::FlowToxicity { bid: 0.1, ask: 0.9, ts: 5 });
        s.on_order_event(&mut b, &a_lifecycle());
        s.on_params_updated(&mut b, &some_params());
        s.on_stop(&mut b);

        let reached = REACHED.with(|v| v.borrow().clone());
        let mut missing: Vec<&&str> =
            WIRED_HOOKS.iter().filter(|h| !reached.contains(&(**h))).collect();
        missing.sort_unstable();
        assert!(
            missing.is_empty(),
            "these WIRED hooks never reached the plugin — `PluginStrategy`'s `Strategy` impl is \
             still falling through to the trait's own no-op default for them, which is the silent \
             divergence the whole vtable exists to end: {missing:?}\nreached: {reached:?}"
        );
        assert_eq!(
            reached.len(),
            WIRED_HOOKS.len(),
            "one dispatch must reach exactly one slot: {reached:?}"
        );
    }

    // ---- shared payloads for the two tests above/below ----

    fn quote_tick() -> vike_marketdata::QuoteTick {
        vike_marketdata::QuoteTick {
            ts: 10,
            local_ts: 11,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.5,
            ask_size: 2.5,
            symbol: "BTCUSDT".to_string(),
        }
    }
    fn trade_tick() -> vike_marketdata::TradeTick {
        vike_marketdata::TradeTick {
            ts: 12,
            local_ts: 13,
            price: 100.0,
            size: 0.25,
            is_buyer_maker: false,
            symbol: "BTCUSDT".to_string(),
        }
    }
    fn a_fill() -> vike_model::Fill {
        vike_model::Fill {
            side: 1,
            size: 2.0,
            price: 100.5,
            fee: 0.01,
            ts: 14,
            is_maker: false,
            symbol: "BTCUSDT".to_string(),
        }
    }
    fn a_mark() -> vike_model::MarkTick {
        vike_model::MarkTick { symbol: "btcusdt".to_string(), price: 64_000.0, ts: 15 }
    }
    fn a_lifecycle() -> vike_model::OrderLifecycle {
        vike_model::OrderLifecycle {
            client_order_id: "coid-1".to_string(),
            tag: Some("bid-1".to_string()),
            kind: vike_model::OrderEventKind::Rejected { reason: "min notional".to_string() },
        }
    }
    fn some_params() -> vike_model::StrategyParams {
        // The CONTROLLER variant, chosen because its `TripleBarrier` carries four `Option`
        // fields left at `None` — the exact shape a TOML hop could not serialise at all. A test
        // that used a fully-populated bag would pass under either encoding and prove nothing
        // about the choice `guest::strategy_params_from_json` argues for.
        vike_model::StrategyParams::PositionController(vike_model::ControllerParams::new(
            5_000,
            1.5,
            // Every leg left UNARMED — four `None`s, which is `TripleBarrier`'s documented
            // default and the ordinary state of a controller nobody has set a stop on.
            vike_model::TripleBarrier::default(),
            0.75,
        ))
    }
    fn seeded_book() -> vike_marketdata::L2Book {
        let mut book = vike_marketdata::L2Book::new(0.5);
        book.apply_snapshot(
            42,
            &[
                vike_marketdata::BookLevel::new(99.5, 3.0),
                vike_marketdata::BookLevel::new(99.0, 5.0),
                vike_marketdata::BookLevel::new(98.5, 7.0),
            ],
            &[
                vike_marketdata::BookLevel::new(100.0, 2.0),
                vike_marketdata::BookLevel::new(100.5, 4.0),
            ],
        );
        book
    }

    /// The book CURSOR, end to end through both wrappers: the host flattens its real `L2Book`, the
    /// plugin rebuilds one from the cursor, and every public read must agree.
    ///
    /// ⚠ It compares the READS rather than the struct, and that is the stronger claim available:
    /// `L2Book` has no `PartialEq` and its level maps are private, so what a user strategy can
    /// actually observe IS this list of accessors. A reconstruction that happened to hold the same
    /// bytes but answered `best_bid` differently would be useless; one that answers every accessor
    /// identically is indistinguishable from the original to any strategy.
    #[test]
    fn the_book_cursor_rebuilds_a_book_the_plugin_cannot_tell_from_the_original() {
        thread_local! {
            static SEEN: std::cell::RefCell<Option<Vec<(String, String)>>> =
                const { std::cell::RefCell::new(None) };
        }

        /// Every public read of an `L2Book`, as `(name, debug-formatted value)` — a string so one
        /// vector can hold `Option<BookLevel>`, `Option<f64>`, `usize` and `u64` together, and so
        /// a failure prints what differed rather than a bare `false`.
        fn probe(b: &vike_marketdata::L2Book) -> Vec<(String, String)> {
            let mut out = vec![
                ("tick_size".to_string(), format!("{:?}", b.tick_size)),
                ("last_seq".to_string(), format!("{:?}", b.last_seq)),
                ("bid_levels".to_string(), format!("{:?}", b.bid_levels())),
                ("ask_levels".to_string(), format!("{:?}", b.ask_levels())),
                ("best_bid".to_string(), format!("{:?}", b.best_bid())),
                ("best_ask".to_string(), format!("{:?}", b.best_ask())),
                ("mid".to_string(), format!("{:?}", b.mid())),
                ("spread".to_string(), format!("{:?}", b.spread())),
                ("imbalance".to_string(), format!("{:?}", b.imbalance())),
                ("top_n(8)".to_string(), format!("{:?}", b.top_n(8))),
                ("vwap_buy_6".to_string(), format!("{:?}", b.avg_px_for_quantity(1, 6.0))),
                ("vwap_sell_6".to_string(), format!("{:?}", b.avg_px_for_quantity(-1, 6.0))),
                ("depth_buy_100.5".to_string(), format!("{:?}", b.quantity_for_price(1, 100.5))),
                ("depth_sell_99.0".to_string(), format!("{:?}", b.quantity_for_price(-1, 99.0))),
                ("sim_buy_3".to_string(), format!("{:?}", b.simulate_fill(1, 3.0))),
            ];
            for px in [98.5, 99.0, 99.5, 100.0, 100.5] {
                out.push((format!("bid_qty_at({px})"), format!("{:?}", b.bid_qty_at(px))));
                out.push((format!("ask_qty_at({px})"), format!("{:?}", b.ask_qty_at(px))));
            }
            out
        }

        extern "C" fn rebuild_and_record(
            _h: *mut c_void,
            _r: BrokerRef,
            b: BookRef,
        ) -> PluginStatus {
            let rebuilt = crate::guest::book_from_ref(b);
            SEEN.with(|s| *s.borrow_mut() = Some(probe(&rebuilt)));
            PluginStatus::Ok
        }

        SEEN.with(|s| *s.borrow_mut() = None);
        let mut vt = fake_vtable();
        vt.on_order_book = rebuild_and_record;
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut broker = FakeBroker::default();

        let original = seeded_book();
        strategy.on_order_book(&mut broker, &original);

        let rebuilt = SEEN
            .with(|s| s.borrow().clone())
            .expect("the plugin's on_order_book must have been reached");
        let expected = probe(&original);
        let diffs: Vec<String> = expected
            .iter()
            .zip(&rebuilt)
            .filter(|((_, a), (_, b))| a != b)
            .map(|((name, a), (_, b))| format!("{name}: host {a} vs plugin {b}"))
            .collect();
        assert!(
            diffs.is_empty(),
            "the rebuilt book answers differently from the host's own:\n  {}",
            diffs.join("\n  ")
        );
        assert_eq!(expected.len(), rebuilt.len(), "the probe lists must be the same shape");
    }

    /// An EMPTY book must cross as an empty book rather than as a panic or a fabricated level —
    /// the boundary case `side_len == 0` on both sides, which is what a `GapStart` leaves behind
    /// and therefore an ordinary state rather than an exotic one.
    #[test]
    fn an_empty_book_crosses_as_an_empty_book() {
        thread_local! {
            static LEVELS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((9, 9)) };
        }
        extern "C" fn count(_h: *mut c_void, _r: BrokerRef, b: BookRef) -> PluginStatus {
            let rebuilt = crate::guest::book_from_ref(b);
            LEVELS.with(|c| c.set((rebuilt.bid_levels(), rebuilt.ask_levels())));
            PluginStatus::Ok
        }
        let mut vt = fake_vtable();
        vt.on_order_book = count;
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut broker = FakeBroker::default();
        strategy.on_order_book(&mut broker, &vike_marketdata::L2Book::new(0.5));
        assert_eq!(LEVELS.with(std::cell::Cell::get), (0, 0));
    }

    /// The params bag must arrive DECODED, with its `None`s intact — the property that decides
    /// JSON over TOML (`guest::strategy_params_from_json`'s own doc carries the measurement).
    #[test]
    fn a_params_bag_crosses_as_json_and_decodes_to_the_same_value() {
        thread_local! {
            static DECODED: std::cell::RefCell<Option<vike_model::StrategyParams>> =
                const { std::cell::RefCell::new(None) };
        }
        extern "C" fn decode(
            _h: *mut c_void,
            _r: BrokerRef,
            p: *const u8,
            n: usize,
        ) -> PluginStatus {
            // Read back through `CStrRef`, which is exactly what the cdylib template does — so
            // this stub exercises the production decode path rather than a second spelling of
            // it, and adds no `unsafe` site of its own (the one that matters lives in `abi.rs`,
            // once, for every borrowed string in this ABI).
            let text = crate::abi::CStrRef { ptr: p, len: n }.read();
            match crate::guest::strategy_params_from_json(&text) {
                Some(v) => {
                    DECODED.with(|d| *d.borrow_mut() = Some(v));
                    PluginStatus::Ok
                }
                None => PluginStatus::BadParams,
            }
        }
        let mut vt = fake_vtable();
        vt.on_params_updated = decode;
        let mut strategy: Box<dyn Strategy<FakeBroker>> =
            Box::new(PluginStrategy::<FakeBroker>::new(vt, ""));
        let mut broker = FakeBroker::default();
        let sent = some_params();
        strategy.on_params_updated(&mut broker, &sent);
        assert_eq!(
            DECODED.with(|d| d.borrow().clone()),
            Some(sent),
            "the params bag must arrive byte-equal after the JSON hop, `None` fields included"
        );
    }
}
