//! The GUEST side: a [`BrokerRef`] wearing the ordinary [`vike_model::OrderBroker`] trait, so a user
//! strategy compiled into a strategy file calls `broker.place(..)` and never learns FFI exists.
//!
//! ⚠ Every method of `Broker` as it stood on 2026-09-21 (`crates/vike-model/src/strategy/mod.rs`)
//! is wired below to a real vtable slot — `submit_market`, `submit_limit`, `position`, `price`,
//! `equity`, `bars`, `index`, `now`, plus the two DEFAULTED reads `quote_vwap` and
//! `depth_within_price`. None is left on the trait's own default: a defaulted read here would
//! silently answer "no liquidity data" / "I don't know" instead of asking the host, which is
//! exactly the silent-divergence class this crate exists to avoid. Since `ABI_VERSION` 8 the two
//! verbs have no slot of their own: each crosses as `OrderBroker::place` of an untagged market or
//! limit order, which is what the trait defines them as (`OrderBroker::place`'s doc), until the
//! one broker interface retires them.
//!
//! ⚠ **One later read IS left on its default: `OrderBroker::data_delay`** (decision 0140, added after
//! that date). [`BrokerVTableAbi`] has no slot for it, so a strategy file reads `None` ("no label
//! stated") even on a delayed stream. It is information, never a gate, so nothing a strategy file does
//! changes because of it; carrying it is an `ABI_VERSION` bump with a slot, not an edit here.
//!
//! ⚠ **`OrderBroker::contract_value` IS wired, at `ABI_VERSION` 4**: a strategy file's position executor folds
//! its PnL at the value it reads, so its default `None` (a multiplier of 1) would be a wrong NUMBER,
//! not a missing label. **`OrderBroker::instrument_grid` is wired at 5** for the same reason: the order
//! language snaps its levels to the tick it reads and counts a trail in it.
//!
//! ⚠ **`OrderBroker::input_bars` and `OrderBroker::closed_trades` are wired at `ABI_VERSION` 6** (decision
//! 0271): both defaults are a wrong answer, not a missing label (no input bars, and `None`, "this
//! broker keeps no ledger", where the simulator keeps one). Each is copied once per dispatch into a
//! cache beside the bars' and refilled only when the host answers another count. `OrderBroker::input`
//! stays on its default, which IS `input_bars(name).last()`, the answer every broker gives.
//!
//! ⚠ **`vike_model::OrderBroker` is implemented at `ABI_VERSION` 5, every verb FORWARDED.** Four
//! of them have a trait default, and the default is the LIVE meaning (`place_exit_leg`,
//! `place_at_close` and `place_close_from_fill` as plain `place` calls, `rests_reduce_only_while_flat`
//! as `false`) that `OrderBrokerSimulator` overrides with its in-bar rules. Left on the default here, a strategy file
//! backtest would fill differently from the same source compiled in, so each crosses to the host's own
//! answer. `place_group`'s legs cross as JSON text (the [`strategy_params_from_json`] argument): its
//! encode cannot fail on these types, so a failure is a panic the template's `catch_unwind` reports,
//! never a list silently not sent.
//!
//! ⚠ **Since `ABI_VERSION` 8 every order verb crosses with the SYMBOL it names** (decision 0277's
//! final seam), so an order the order language sends on another symbol (`vike_tactics::Tactics::on`)
//! reaches the host's broker as itself, which places it there or refuses an undeclared symbol in its
//! own words, exactly as for the same source compiled in; and `OrderBroker::symbols` reads the run's
//! universe. `HftBroker` is no longer implemented: its verbs are `OrderBroker`'s, and a `build`
//! bounding `B: HftBroker` fails its Build with the one-line fix
//! (`vike_model::host_build::moved_name::renamed_name_hint`).
//!
//! ⚠ **`ABI_VERSION` 3 adds two guest-side DECODERS beside the broker wrapper**, because the
//! remaining `Strategy` hooks carry payloads a `#[repr(C)]` mirror cannot express:
//! [`book_from_ref`] rebuilds an `L2Book` through the cursor `abi.rs` declares, and
//! [`strategy_params_from_json`] decodes the one payload whose only faithful text form is JSON.
//! Both live HERE rather than in the cdylib template so the template's own dependency table gains
//! nothing for them — the user-facing surface the design's open question 3 is about. The JSON the
//! `ABI_VERSION` 8 exports lend and take ([`names_json`], [`params_json`], [`state_json`],
//! [`state_from_json`]) lives here for the same reason, and so do `ABI_VERSION` 9's: the mount facts
//! ([`facts_from_json`]), the `echo` line ([`echo_json`]) and the two forms of `build`
//! ([`BuildEntry`]).

use std::cell::UnsafeCell;
use std::collections::HashMap;

use vike_marketdata::{Bar, L2Book, L2BookLevel};
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement, TradeClosedRow};

use crate::abi::{
    BarAbi, BookRef, BrokerRef, BrokerVTableAbi, L2BookLevelAbi, OrderPlacementAbi, SIDE_ASK,
    SIDE_BID,
};

/// One symbol's cached bar mirror, plus the host `index` it was last filled at.
struct Bucket {
    at_index: Option<usize>,
    bars: Vec<Bar>,
}

/// What [`HostBroker`]'s slice-returning reads copied from the host, each in its own allocation: the
/// bars by symbol, the input bars by name (refilled when the host's count moves), the closed rows and
/// the run's universe (filled once: the host's does not change while a strategy file is called).
#[derive(Default)]
struct Cache {
    bars: HashMap<String, Bucket>,
    inputs: HashMap<String, Vec<Bar>>,
    closed: Vec<TradeClosedRow>,
    symbols: Option<Vec<String>>,
}

/// The cache behind `&self`, as `&mut`. A macro, not a `fn(&self) -> &mut Cache` (a shape
/// `clippy::mut_from_ref` refuses), so the one `unsafe` act is ONE textual site.
macro_rules! cache_mut {
    ($self:expr) => {{
        // SAFETY: `HostBroker` is used synchronously, single-threaded, for one dispatch, and is
        // neither `Send` nor `Sync`, so no other `&mut Cache` is alive. A slice a read handed back
        // borrows a `Vec`'s heap buffer, never the map holding it, so filling ANOTHER entry (which
        // may move the map's `Vec` headers, never their buffers) cannot invalidate it. The SAME entry
        // is refilled only when the host answers a new index (`bars`) or a new count (the input bars,
        // the closed rows), and the host's answers change only between dispatches or under a `&mut`
        // verb, neither of which can happen while a `&self` borrow from an earlier read is alive. The
        // universe is filled once and never refilled.
        unsafe { &mut *$self.cache.get() }
    }};
}

/// A [`BrokerRef`] wearing [`OrderBroker`]. Holds the host's vtable already dereferenced once (at
/// construction) rather than re-deref'ing `r.vtable` on every call.
pub struct HostBroker {
    ctx: *mut core::ffi::c_void,
    vtable: &'static BrokerVTableAbi,
    /// `OrderBroker::bars` takes `&self`, so refilling it lazily needs interior mutability — see the
    /// `// SAFETY:` notes at `cache_mut!` and in [`HostBroker::bars`] for why a raw cell is sound
    /// here rather than a `RefCell` (which cannot hand back a `&'a [Bar]` tied to `&self` without
    /// holding its guard alive). Keyed by symbol, deliberately NOT a single `Vec`: refilling one
    /// symbol's bars must never reallocate — and so never invalidate — a slice already returned
    /// for a DIFFERENT symbol earlier in the same call. The input bars and the closed rows
    /// (`ABI_VERSION` 6) and the universe (8) share the cell, each in its own allocation for the
    /// same reason.
    cache: UnsafeCell<Cache>,
}

impl HostBroker {
    pub fn new(r: BrokerRef) -> Self {
        // SAFETY: `r.vtable` is host-owned and `'static` for the whole lifetime of a loaded
        // strategy file — the loader refuses to construct a `BrokerRef` otherwise (see `loader.rs`), and
        // this crate's ABI contract (`abi.rs`'s module doc) is the authority a caller across the
        // boundary must uphold. Dereferenced exactly once, here, rather than on every vtable call.
        let vtable = unsafe { &*r.vtable };
        Self { ctx: r.ctx, vtable, cache: UnsafeCell::default() }
    }
}

/// Every verb crosses to the host's broker as itself, the symbol it names included (`ABI_VERSION`
/// 8): the host places an order on that symbol, or refuses one its run does not hold, in its own
/// words, as for the same source compiled in.
impl OrderBroker for HostBroker {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        let (s, t) = (symbol, tag);
        let order = OrderPlacementAbi::of(order);
        let ctx = self.ctx;
        (self.vtable.place)(
            ctx,
            s.as_ptr(),
            s.len(),
            t.as_ptr(),
            t.len(),
            side,
            qty,
            order,
            reduce_only,
        );
    }
    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    ) {
        let order = OrderPlacementAbi::of(order);
        (self.vtable.place_exit_leg)(
            self.ctx,
            symbol.as_ptr(),
            symbol.len(),
            exit.as_ptr(),
            exit.len(),
            tag.as_ptr(),
            tag.len(),
            side,
            qty,
            order,
        );
    }
    fn place_at_close(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, reduce_only: bool) {
        let (s, t) = (symbol, tag);
        let ctx = self.ctx;
        (self.vtable.place_at_close)(
            ctx,
            s.as_ptr(),
            s.len(),
            t.as_ptr(),
            t.len(),
            side,
            qty,
            reduce_only,
        );
    }
    fn place_close_from_fill(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, level: f64) {
        let (s, t) = (symbol, tag);
        let ctx = self.ctx;
        (self.vtable.place_close_from_fill)(
            ctx,
            s.as_ptr(),
            s.len(),
            t.as_ptr(),
            t.len(),
            side,
            qty,
            level,
        );
    }
    fn rests_reduce_only_while_flat(&self) -> bool {
        (self.vtable.rests_reduce_only_while_flat)(self.ctx)
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        let json = crate::abi::order_list_json(&legs);
        let (s, j) = (symbol, json.as_str());
        (self.vtable.place_group)(self.ctx, s.as_ptr(), s.len(), j.as_ptr(), j.len());
    }
    fn amend(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        let (q, p, t) = (new_qty.into(), new_price.into(), new_trigger.into());
        (self.vtable.amend)(self.ctx, tag.as_ptr(), tag.len(), q, p, t);
    }
    fn cancel(&mut self, tag: &str) {
        (self.vtable.cancel)(self.ctx, tag.as_ptr(), tag.len());
    }
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        let (s, t, r) = (symbol, tag, reason);
        let ctx = self.ctx;
        (self.vtable.deny)(ctx, s.as_ptr(), s.len(), t.as_ptr(), t.len(), qty, r.as_ptr(), r.len());
    }
    fn average_price(&self, symbol: &str) -> f64 {
        (self.vtable.average_price)(self.ctx, symbol.as_ptr(), symbol.len())
    }
    /// The host's universe, copied once per `HostBroker` (one dispatch): `symbol_at` until `None`.
    fn symbols(&self) -> &[String] {
        let cache = cache_mut!(self);
        cache.symbols.get_or_insert_with(|| {
            let mut out = Vec::new();
            while let Some(s) = (self.vtable.symbol_at)(self.ctx, out.len()).get() {
                out.push(s.read());
            }
            out
        })
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
        let index = (self.vtable.index)(self.ctx);
        // Why `cache_mut!` is sound here: `HostBroker` is used exactly the way every FFI dispatch
        // entry point in this design uses it — synchronously, single-threaded, for the duration of one call — and is
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
        // advances the engine's step between a strategy file entry point being called and it returning
        // (`abi.rs`'s module doc: the whole exchange is one borrowed, synchronous call). So two
        // `bars(symbol)` calls that read the SAME `index` are necessarily within the SAME dispatch
        // and see the cache untouched (no refill runs, nothing is invalidated); a call that reads
        // an ADVANCED `index` is necessarily a NEW dispatch, and the refill it triggers cannot
        // invalidate a slice from the PREVIOUS dispatch because nothing can still be holding one —
        // a `&[Bar]` borrowed from `&self` cannot outlive the call that produced it once that
        // call's own stack frame is gone, dispatch-fresh `HostBroker` or not.
        let bucket = cache_mut!(self)
            .bars
            .entry(symbol.to_string())
            .or_insert_with(|| Bucket { at_index: None, bars: Vec::new() });
        if bucket.at_index != Some(index) {
            let n = (self.vtable.bars_len)(self.ctx, symbol.as_ptr(), symbol.len());
            bucket.bars.clear();
            bucket.bars.reserve(n);
            for i in 0..n {
                let mut out = BarAbi::empty();
                // SAFETY: `out` is a live, aligned, owned `BarAbi` on this stack frame; the host
                // writes into it through the pointer and returns `false` without writing when `i`
                // is out of range, so `out` is never read uninitialised.
                let wrote =
                    (self.vtable.bar_at)(self.ctx, symbol.as_ptr(), symbol.len(), i, &mut out);
                if wrote {
                    bucket.bars.push(out.to_bar());
                }
            }
            bucket.at_index = Some(index);
        }
        &bucket.bars
    }
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        (self.vtable.quote_vwap)(self.ctx, symbol.as_ptr(), symbol.len(), side, qty).into()
    }
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        (self.vtable.depth_within_price)(self.ctx, symbol.as_ptr(), symbol.len(), side, limit_px)
    }
    fn contract_value(&self, symbol: &str) -> Option<vike_model::ContractValue> {
        (self.vtable.contract_value)(self.ctx, symbol.as_ptr(), symbol.len()).into()
    }
    fn instrument_grid(&self, symbol: &str) -> Option<vike_model::Grid> {
        (self.vtable.instrument_grid)(self.ctx, symbol.as_ptr(), symbol.len()).into()
    }
    /// The host's input bars of `name`, copied when the host answers a count the cache does not
    /// hold (once per dispatch in practice: the view moves only between them). A bar the host does
    /// not answer below its own count is a contract violation, refused loudly, never skipped.
    fn input_bars(&self, name: &str) -> &[Bar] {
        let (p, n) = (name.as_ptr(), name.len());
        let len = (self.vtable.input_bars_len)(self.ctx, p, n);
        let bars = cache_mut!(self).inputs.entry(name.to_string()).or_default();
        if bars.len() != len {
            bars.clear();
            for i in 0..len {
                let bar = (self.vtable.input_bar_at)(self.ctx, p, n, i).get().unwrap_or_else(|| {
                    panic!(
                        "strategy file ABI contract violated: the host has {len} bars of input {name:?} \
                         and not bar {i}"
                    )
                });
                bars.push(bar.to_bar());
            }
        }
        bars
    }
    /// The host's closed rows, copied the same way. `None` is the host's "no ledger", never an
    /// empty one.
    fn closed_trades(&self) -> Option<&[TradeClosedRow]> {
        let len = (self.vtable.closed_trades_len)(self.ctx).get()?;
        let rows = &mut cache_mut!(self).closed;
        if rows.len() != len {
            rows.clear();
            for i in 0..len {
                let row = (self.vtable.closed_trade_at)(self.ctx, i).get().unwrap_or_else(|| {
                    panic!(
                        "strategy file ABI contract violated: the host has {len} closed rows and not \
                         row {i}"
                    )
                });
                rows.push(row.to_closed_row());
            }
        }
        Some(rows.as_slice())
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
/// **Faithfulness.** `tick_size` and `last_sequence` cross explicitly and the levels are re-folded
/// through `L2Book::apply_snapshot`, so every public read — `best_bid`/`best_ask`/`mid`/`spread`/
/// `imbalance`/`bid_quantity_at`/`top_n`/`average_price_for_quantity`/`quantity_for_price`/`simulate_fill` —
/// answers exactly what the host's own book answers. The one place a float could bite is the tick
/// grid: a level's price crosses as the host book's read of its tick index
/// (`vike_marketdata::TickFraction::price_of`) and is re-quantised by
/// `(price / tick_size).round_ties_even()`, whose error is many orders of magnitude below the
/// half-tick that rounding decides, so the tick index comes back identical and reads back the
/// same price through the same rule.
///
/// ⚠ **Caller contract: only from inside a `catch_unwind`.** The vtable calls below can panic
/// through `guarded`'s poison latch on the host's side; the template's `vike_strategy_on_order_book`
/// export is the wrapper that makes that safe.
pub fn book_from_ref(r: BookRef) -> L2Book {
    // SAFETY: `r.vtable` is host-owned and `'static` for the whole lifetime of a loaded strategy file —
    // the same contract `HostBroker::new` relies on for `BrokerVTableAbi`, stated in `abi.rs`'s
    // module doc. Dereferenced exactly once, here, rather than on every cursor call.
    let vt = unsafe { &*r.vtable };
    let side = |which: i32| -> Vec<L2BookLevel> {
        let n = (vt.side_len)(r.ctx, which);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // `out_level` is an OWNED local, so reading it back after the call needs no `unsafe`
            // on this side — only the host's write through the pointer does, exactly as
            // `HostBroker::bars` splits the same act with `bar_at`.
            let mut out_level = L2BookLevelAbi::empty();
            if (vt.level_at)(r.ctx, which, i, &mut out_level) {
                out.push(L2BookLevel::new(out_level.price, out_level.qty));
            }
        }
        out
    };
    let bids = side(SIDE_BID);
    let asks = side(SIDE_ASK);
    let mut book = L2Book::new((vt.tick_size)(r.ctx));
    book.apply_snapshot((vt.last_sequence)(r.ctx), &bids, &asks);
    book
}

/// Decode a `vike_model::StrategyParams` from the JSON document `on_params_updated` carries.
///
/// ⚠ **JSON, not TOML, and the reason is MEASURED on the types rather than preferred.** The
/// obvious choice was TOML — it is the idiom `vike_strategy_create` already uses for the params
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
/// `PositionControllerParams` and `XemmParams`, with nothing holding it complete — a field added
/// upstream would simply stop arriving, which is the same failure wearing a different hat, and
/// the "second name for one contract" the design rejected `abi_stable` over.
///
/// ⚠ **`serde_json` costs a strategy file NOTHING it was not already linking**: `vike-model` — which
/// every strategy file depends on directly, because that is where `Strategy` lives — has had a normal
/// `serde_json` dependency since `save_state` existed. Naming it in this crate's manifest adds an
/// edge, not a crate, and the cdylib TEMPLATE's own dependency table is untouched (the helper is
/// here, so a user file still never names anything but the user-facing crates) — which is what
/// keeps the design's open question 3, the strategy file tier's narrower dependency SURFACE, closed.
pub fn strategy_params_from_json(json: &str) -> Option<vike_model::StrategyParams> {
    serde_json::from_str(json).ok()
}

/// `Strategy::inputs`' or `Strategy::symbols`' names as the JSON array text the template's exports
/// lend the host (`host::StrategyVTableAbi::inputs` and `symbols`), the [`strategy_params_from_json`]
/// argument: a name is any text, so a separator would need an escape. A list of strings always
/// encodes.
pub fn names_json(names: &[String]) -> String {
    serde_json::to_string(names).expect("a list of strings always encodes as JSON")
}

/// `Strategy::params`' answer as the JSON text the template's export lends the host
/// (`host::StrategyVTableAbi::params`), the inverse of [`strategy_params_from_json`]. A failure panics,
/// which the export's `catch_unwind` reports, never an answer of "none".
pub fn params_json(params: &vike_model::StrategyParams) -> String {
    serde_json::to_string(params).expect("a StrategyParams always encodes as JSON")
}

/// `Strategy::save_state`'s answer as the JSON text the template's export lends the host
/// (`host::StrategyVTableAbi::save_state`). A JSON value always encodes.
pub fn state_json(state: &serde_json::Value) -> String {
    state.to_string()
}

/// The state `Strategy::load_state` is handed, decoded from the JSON text the host sends
/// (`host::StrategyVTableAbi::load_state`); `None` for text that is not JSON.
pub fn state_from_json(json: &str) -> Option<serde_json::Value> {
    serde_json::from_str(json).ok()
}

/// The mount facts `vike_strategy_create` is handed (`ABI_VERSION` 9), decoded from the JSON text
/// the host sends; `None` for text that is not a [`vike_model::params::MountFacts`] document.
pub fn facts_from_json(json: &str) -> Option<vike_model::params::MountFacts> {
    serde_json::from_str(json).ok()
}

/// A file's optional `echo` export (its effective-params line) as the JSON text the template lends
/// the host (`host::StrategyVTableAbi::echo`): `[[key, value], …]`, in the file's order.
pub fn echo_json(echo: &[(&str, String)]) -> String {
    serde_json::to_string(echo).expect("a list of string pairs always encodes as JSON")
}

/// The strategy file's `tracing` subscriber (`ABI_VERSION` 9): every event the file (or a crate it
/// links, `vike-market-making` say) emits up to the host's level goes to the host's log slot, its message and
/// fields as text, so it is logged by the HOST's subscriber, in the host's format, wherever that one
/// writes (the sandbox child: its stderr). Spans are not carried.
pub struct TracingForward {
    log: crate::abi::LogFn,
    max: tracing::level_filters::LevelFilter,
}

impl TracingForward {
    /// Forward to `log` every event up to the level `max_level` codes (`abi::log_level_filter`).
    pub fn new(log: crate::abi::LogFn, max_level: u32) -> Self {
        TracingForward { log, max: crate::abi::log_level_filter(max_level) }
    }
}

impl tracing::Subscriber for TracingForward {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(self.max)
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut text = EventText(String::new());
        event.record(&mut text);
        let metadata = event.metadata();
        (self.log)(
            crate::abi::log_level_code(*metadata.level()),
            crate::abi::StrAbi::of(metadata.target()),
            crate::abi::StrAbi::of(&text.0),
        );
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// An event's message, then each other field as ` name=value`.
struct EventText(String);

impl tracing::field::Visit for EventText {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field, format_args!("{value}"));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.push(field, format_args!("{value:?}"));
    }
}

impl EventText {
    fn push(&mut self, field: &tracing::field::Field, value: std::fmt::Arguments<'_>) {
        use std::fmt::Write as _;
        let gap = if self.0.is_empty() { "" } else { " " };
        let _ = if field.name() == "message" {
            write!(self.0, "{gap}{value}")
        } else {
            write!(self.0, "{gap}{}={value}", field.name())
        };
    }
}

/// Install [`TracingForward`] as this strategy file's `tracing` dispatcher, once: the template's
/// `vike_strategy_set_log` export, which the host's loader calls at load. A second call (the same
/// file loaded twice) changes nothing.
pub fn forward_tracing(log: crate::abi::LogFn, max_level: u32) {
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = tracing::dispatcher::set_global_default(tracing::Dispatch::new(
            TracingForward::new(log, max_level),
        ));
    });
}

/// The two forms of a strategy file's `build` entry, so the template calls whichever the file
/// wrote: `build(params)`, or `build(params, facts)` for a file that reads the mount's facts.
/// `Form` is [`ParamsOnly`] or [`ParamsAndFacts`]; a function item has exactly one arity, so the
/// compiler infers it and a file never names this trait.
pub trait BuildEntry<B: OrderBroker, Form> {
    /// The strategy, built from `params` (and `facts`, for the two-argument form).
    fn build_from(
        self,
        params: &vike_model::params::Params,
        facts: &vike_model::params::MountFacts,
    ) -> Box<dyn vike_model::Strategy<B> + Send>;
}

/// [`BuildEntry`]'s one-argument form: `build(params)`.
pub struct ParamsOnly;

/// [`BuildEntry`]'s two-argument form: `build(params, facts)`.
pub struct ParamsAndFacts;

impl<B: OrderBroker, F> BuildEntry<B, ParamsOnly> for F
where
    F: FnOnce(&vike_model::params::Params) -> Box<dyn vike_model::Strategy<B> + Send>,
{
    fn build_from(
        self,
        params: &vike_model::params::Params,
        _facts: &vike_model::params::MountFacts,
    ) -> Box<dyn vike_model::Strategy<B> + Send> {
        self(params)
    }
}

impl<B: OrderBroker, F> BuildEntry<B, ParamsAndFacts> for F
where
    F: FnOnce(
        &vike_model::params::Params,
        &vike_model::params::MountFacts,
    ) -> Box<dyn vike_model::Strategy<B> + Send>,
{
    fn build_from(
        self,
        params: &vike_model::params::Params,
        facts: &vike_model::params::MountFacts,
    ) -> Box<dyn vike_model::Strategy<B> + Send> {
        self(params, facts)
    }
}

/// Build through a file's `build` entry in whichever form it wrote ([`BuildEntry`]).
pub fn build_entry<B: OrderBroker, Form>(
    entry: impl BuildEntry<B, Form>,
    params: &vike_model::params::Params,
    facts: &vike_model::params::MountFacts,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    entry.build_from(params, facts)
}

// A `BrokerRef`'s `ctx` is an erased `&mut OrderBrokerSimulator` owned by the HOST for the duration of the
// call the strategy file is currently inside; `HostBroker` never outlives that call (the strategy file's own
// `extern "C"` entry point constructs one, uses it, and drops it before returning), so it never
// needs to be `Send`/`Sync` and deliberately is not (the default: it holds a raw pointer, which
// opts it out of both auto traits already).

#[path = "guest_tests.rs"]
#[cfg(test)]
mod guest_tests;
