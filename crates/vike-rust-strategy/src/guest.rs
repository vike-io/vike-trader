//! The GUEST side: a [`BrokerRef`] wearing the ordinary [`vike_model::OrderBroker`] trait, so a user
//! strategy compiled into a strategy file calls `broker.place(..)` and never learns FFI exists.
//!
//! ⚠ **The trait impl is the FACE's** ([`face`], decision 0296's step 6): every method's body is written
//! once here (`broker_method!`), and the face of the SDK the file compiles against lists them, so an older
//! SDK whose API differs builds against this boundary through its adapter's face, a renamed method under
//! its old name with the same body. A hook's lent bar or tick is read in place ([`bar_in_place`] and its
//! two siblings); an SDK whose lent rows are laid out otherwise is retired, never adapted (verdict 7).
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
//! broker keeps no ledger", where the simulator keeps one). `OrderBroker::input` stays on its default,
//! which IS `input_bars(name).last()`, the answer every broker gives.
//! **`OrderBroker::input_whole` is wired at `ABI_VERSION` 10** (decision 0271's amendment) the same way:
//! its default `None` would leave a file marked `runs_on = "backtest"` reading no future where the same
//! source compiled in reads the whole series.
//! **`OrderBroker::publish_forecast` is wired at `ABI_VERSION` 11** (book W35): its default records
//! nothing, so a strategy file's forecasts would vanish where the same source compiled in records them.
//!
//! ⚠ **Since `ABI_VERSION` 14 the history reads (`bars`, the two input reads, `closed_trades`) read the
//! HOST's rows where it keeps them**: each is one vtable call that lends a slice ([`lent`]), so nothing
//! is copied across and nothing is kept between dispatches. A host whose rows move or are rewritten
//! between dispatches (paper, live) is read as it is now. Until 13 the file copied each history into
//! a mirror it kept where the host's only grew, and copied it whole on every dispatch where it did not.
//!
//! ⚠ **`vike_model::OrderBroker` is implemented at `ABI_VERSION` 5, every verb FORWARDED.** Four
//! of them have a trait default, and the default is the LIVE meaning (`place_exit_leg`,
//! `place_at_close` and `place_close_from_fill` as plain `place` calls, `rests_reduce_only_while_flat`
//! as `false`) that `OrderBrokerSimulator` overrides with its in-bar rules. Left on the default here, a strategy file
//! backtest would fill differently from the same source compiled in, so each crosses to the host's own
//! answer. `place_group`'s legs cross as a lent slice of `abi::OrderListLegAbi` rows (`ABI_VERSION` 15;
//! JSON text before), built from the legs and borrowing them for the call: nothing is encoded, so
//! nothing can fail to be.
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
//! `ABI_VERSION` 8 exports lend and take ([`names_json`], [`params_json`]) and the state's msgpack
//! check ([`state_from_bytes`], 15) live here for the same reason, and so do `ABI_VERSION` 9's: the mount facts
//! ([`facts_from_json`]), the `echo` line ([`echo_json`]) and the two forms of `build`
//! ([`BuildEntry`]).

use std::cell::UnsafeCell;

use vike_marketdata::{L2Book, L2BookLevel};
use vike_model::OrderBroker;

use crate::abi::{
    BookRef, BrokerRef, BrokerVTableAbi, L2BookLevelAbi, LentBarAbi, LentTickQuoteAbi,
    LentTickTradeAbi, OptionAbi, SIDE_ASK, SIDE_BID, SliceAbi, StrAbi,
};

/// What [`HostBroker::symbols`] and [`HostBroker::input_names`] copied from the host: the run's universe
/// and its input list, each filled once (the host's do not change while a strategy file is called).
#[derive(Default)]
struct Cache {
    symbols: Option<Vec<String>>,
    input_names: Option<Vec<String>>,
}

/// The host's list `at` answers, entry by entry until `None` (`symbol_at`, `input_name_at`).
fn host_list(
    ctx: *mut core::ffi::c_void,
    at: extern "C" fn(*mut core::ffi::c_void, usize) -> OptionAbi<StrAbi>,
) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(s) = at(ctx, out.len()).get() {
        out.push(s.read());
    }
    out
}

/// The rows the host lent (`ABI_VERSION` 14) as a slice tied to `owner`'s borrow (the [`HostBroker`] the
/// read was asked of, or the lent row itself), so safe code can keep it neither past the dispatch nor
/// across a `&mut` verb (the only door through which the host's rows change). An empty answer is `&[]`;
/// a null or misaligned pointer with rows is a contract violation, refused loudly inside the template's
/// `catch_unwind`, never read.
fn lent<T, O: ?Sized>(_owner: &O, rows: SliceAbi<T>) -> &[T] {
    if rows.len == 0 {
        return &[];
    }
    assert!(
        !rows.ptr.is_null() && rows.ptr.is_aligned(),
        "strategy file ABI contract violated: the host lent {} rows at a null or misaligned pointer",
        rows.len
    );
    // SAFETY: the host built `rows` from a live slice of its own (`abi::SliceAbi::of` in its thunk), and
    // its rows change only between dispatches or under a `&mut` verb; the returned slice borrows
    // `owner`, which lives for this one synchronous dispatch and takes `&mut` for every verb, so no
    // change can happen while it is held. `T` is laid out as the row the host lent: the boundary's own
    // row, or an SDK type `crate::abi::lent_in_place!` proved laid out as it at the reading site
    // (`abi::layout` is checked at load); the pointer is non-null and aligned (asserted above).
    unsafe { std::slice::from_raw_parts(rows.ptr, rows.len) }
}

/// The boundary's `row` (a lent bar or tick a hook gets), read in place as the SDK's `T`. ⚠ Only where the
/// caller proved `T` laid out as `R` (`crate::abi::lent_in_place!`, at its site): a cast, nothing copied.
#[inline]
fn in_place<R, T>(row: &R) -> &T {
    &lent(row, SliceAbi::of(std::slice::from_ref(row)).cast())[0]
}

// The bar or tick the host lends a hook, as the SDK's type the file compiles against: in place, under
// `lent_in_place!`, which does not compile against an SDK whose type is laid out otherwise. Such an SDK is
// never built here: it is retired (decision 0296's verdict 7; `crate::sdk::usable`'s `RowsMoved`).

/// The bar the host lent `on_bar`.
#[inline]
pub fn bar_in_place(row: &LentBarAbi) -> &vike_marketdata::Bar {
    crate::abi::lent_in_place!(bar);
    in_place(row)
}

/// The quote the host lent `on_quote_tick` or `on_reference_quote`.
#[inline]
pub fn quote_in_place(row: &LentTickQuoteAbi) -> &vike_marketdata::TickQuote {
    crate::abi::lent_in_place!(tick_quote);
    in_place(row)
}

/// The trade the host lent `on_trade_tick`.
#[inline]
pub fn trade_in_place(row: &LentTickTradeAbi) -> &vike_marketdata::TickTrade {
    crate::abi::lent_in_place!(tick_trade);
    in_place(row)
}

/// The cache behind `&self`, as `&mut`. A macro, not a `fn(&self) -> &mut Cache` (a shape
/// `clippy::mut_from_ref` refuses), so the one `unsafe` act is ONE textual site.
macro_rules! cache_mut {
    ($self:expr) => {{
        // SAFETY: `HostBroker` is used synchronously, single-threaded, for one dispatch, and is
        // neither `Send` nor `Sync`, so no other `&mut Cache` is alive. Its two entries (the universe
        // and the input list) are each filled once and never refilled, so a slice handed back from
        // either is never invalidated.
        unsafe { &mut *$self.cache.get() }
    }};
}

/// A [`BrokerRef`] wearing [`OrderBroker`] (its impl is the face's, [`face`]). Holds the host's vtable
/// already dereferenced once (at construction) rather than re-deref'ing `r.vtable` on every call.
pub struct HostBroker {
    ctx: *mut core::ffi::c_void,
    vtable: &'static BrokerVTableAbi,
    /// `OrderBroker::symbols` takes `&self`, so filling it lazily needs interior mutability — see the
    /// `// SAFETY:` note at `cache_mut!` for why a raw cell is sound here rather than a `RefCell` (which
    /// cannot hand back a `&'a [String]` tied to `&self` without holding its guard alive).
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

/// **One `vike_model::OrderBroker` method of [`HostBroker`], by name, forwarded to its vtable slot**: what
/// a face lists, `broker_method!(position)`, or `broker_method!(history_only_grows as inputs_only_grow)`
/// for an SDK that spells it `inputs_only_grow` (the same body under the SDK's name, so the renamed
/// method costs exactly what the host's does). The bodies are the ones this impl always had. Every verb
/// crosses to the host's broker as itself, the symbol it names included (`ABI_VERSION` 8): the host places
/// an order on that symbol, or refuses one its run does not hold, in its own words, as for the same source
/// compiled in. A history read takes the SDK's row in place, under `lent_in_place!`: against an SDK whose
/// row is laid out otherwise it does not compile, and such an SDK is retired, never adapted (decision 0296's
/// verdict 7).
macro_rules! broker_method {
    ($method:ident) => {
        $crate::guest::broker_method!($method as $method);
    };
    (place as $name:ident) => {
        fn $name(
            &mut self,
            symbol: &str,
            tag: &str,
            side: i32,
            qty: f64,
            order: ::vike_model::OrderPlacement,
            reduce_only: bool,
        ) {
            let (s, t) = (symbol, tag);
            let order = $crate::abi::OrderPlacementAbi::of(order);
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
    };
    (place_exit_leg as $name:ident) => {
        fn $name(
            &mut self,
            symbol: &str,
            exit: &str,
            tag: &str,
            side: i32,
            qty: f64,
            order: ::vike_model::OrderPlacement,
        ) {
            let order = $crate::abi::OrderPlacementAbi::of(order);
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
    };
    (place_at_close as $name:ident) => {
        fn $name(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, reduce_only: bool) {
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
    };
    (place_close_from_fill as $name:ident) => {
        fn $name(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, level: f64) {
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
    };
    (rests_reduce_only_while_flat as $name:ident) => {
        fn $name(&self) -> bool {
            (self.vtable.rests_reduce_only_while_flat)(self.ctx)
        }
    };
    (place_group as $name:ident) => {
        fn $name(&mut self, symbol: &str, legs: Vec<(String, ::vike_model::OrderListLeg)>) {
            let lent: Vec<$crate::abi::OrderListLegAbi> =
                legs.iter().map(|(tag, leg)| $crate::abi::OrderListLegAbi::of(tag, leg)).collect();
            (self.vtable.place_group)(
                self.ctx,
                symbol.as_ptr(),
                symbol.len(),
                $crate::abi::SliceAbi::of(&lent),
            );
        }
    };
    (amend as $name:ident) => {
        fn $name(
            &mut self,
            tag: &str,
            new_qty: Option<f64>,
            new_price: Option<f64>,
            new_trigger: Option<f64>,
        ) {
            let (q, p, t) = (new_qty.into(), new_price.into(), new_trigger.into());
            (self.vtable.amend)(self.ctx, tag.as_ptr(), tag.len(), q, p, t);
        }
    };
    (cancel as $name:ident) => {
        fn $name(&mut self, tag: &str) {
            (self.vtable.cancel)(self.ctx, tag.as_ptr(), tag.len());
        }
    };
    (deny as $name:ident) => {
        fn $name(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
            let (s, t, r) = (symbol, tag, reason);
            let ctx = self.ctx;
            (self.vtable.deny)(
                ctx,
                s.as_ptr(),
                s.len(),
                t.as_ptr(),
                t.len(),
                qty,
                r.as_ptr(),
                r.len(),
            );
        }
    };
    (publish_forecast as $name:ident) => {
        fn $name(&mut self, symbol: &str, forecast: ::vike_model::Forecast) {
            let (value, horizon_ms) = (forecast.value(), forecast.horizon_ms());
            let confidence = forecast.confidence().into();
            let s = symbol;
            (self.vtable.publish_forecast)(
                self.ctx,
                s.as_ptr(),
                s.len(),
                value,
                horizon_ms,
                confidence,
            );
        }
    };
    (average_price as $name:ident) => {
        fn $name(&self, symbol: &str) -> f64 {
            (self.vtable.average_price)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol))
        }
    };
    // The host's universe, copied once per `HostBroker` (one dispatch): `symbol_at` until `None`.
    (symbols as $name:ident) => {
        fn $name(&self) -> &[String] {
            let cache = $crate::guest::cache_mut!(self);
            cache
                .symbols
                .get_or_insert_with(|| $crate::guest::host_list(self.ctx, self.vtable.symbol_at))
        }
    };
    (position as $name:ident) => {
        fn $name(&self, symbol: &str) -> f64 {
            (self.vtable.position)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol))
        }
    };
    (price as $name:ident) => {
        fn $name(&self, symbol: &str) -> f64 {
            (self.vtable.price)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol))
        }
    };
    (equity as $name:ident) => {
        fn $name(&self) -> f64 {
            (self.vtable.equity)(self.ctx)
        }
    };
    (index as $name:ident) => {
        fn $name(&self) -> usize {
            (self.vtable.index)(self.ctx)
        }
    };
    (now as $name:ident) => {
        fn $name(&self) -> i64 {
            (self.vtable.now)(self.ctx)
        }
    };
    // The host's bars of `symbol`, lent, read in place.
    (bars as $name:ident) => {
        fn $name(&self, symbol: &str) -> &[::vike_marketdata::Bar] {
            $crate::abi::lent_in_place!(bar);
            let rows = (self.vtable.bars)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol));
            $crate::guest::lent(self, rows.cast())
        }
    };
    (quote_vwap as $name:ident) => {
        fn $name(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
            (self.vtable.quote_vwap)(self.ctx, symbol.as_ptr(), symbol.len(), side, qty).into()
        }
    };
    (depth_within_price as $name:ident) => {
        fn $name(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
            (self.vtable.depth_within_price)(
                self.ctx,
                symbol.as_ptr(),
                symbol.len(),
                side,
                limit_px,
            )
        }
    };
    (contract_value as $name:ident) => {
        fn $name(&self, symbol: &str) -> Option<::vike_model::ContractValue> {
            (self.vtable.contract_value)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol)).into()
        }
    };
    (instrument_grid as $name:ident) => {
        fn $name(&self, symbol: &str) -> Option<::vike_model::Grid> {
            (self.vtable.instrument_grid)(self.ctx, $crate::abi::NameOrIndexAbi::text(symbol))
                .into()
        }
    };
    // The host's input bars of `name`, lent, read in place.
    (input_bars as $name:ident) => {
        fn $name(&self, name: &str) -> &[::vike_marketdata::Bar] {
            $crate::abi::lent_in_place!(bar);
            let rows = (self.vtable.input_bars)(self.ctx, $crate::abi::NameOrIndexAbi::text(name));
            $crate::guest::lent(self, rows.cast())
        }
    };
    // The host's whole series of `name` (`ABI_VERSION` 10), lent, read in place. `None` is the host
    // serving none, never an empty series.
    (input_whole as $name:ident) => {
        fn $name(&self, name: &str) -> Option<&[::vike_marketdata::Bar]> {
            $crate::abi::lent_in_place!(bar);
            let at = $crate::abi::NameOrIndexAbi::text(name);
            Some($crate::guest::lent(self, (self.vtable.input_whole)(self.ctx, at).get()?.cast()))
        }
    };
    // The reads by index (`ABI_VERSION` 15): the same slots, the index crossing in place of the text, so
    // the host answers through its own `_at` read. An index is into the host's own lists, which
    // `symbols` and `input_names` copy.
    (position_at as $name:ident) => {
        fn $name(&self, index: usize) -> f64 {
            (self.vtable.position)(self.ctx, $crate::abi::NameOrIndexAbi::index(index))
        }
    };
    (price_at as $name:ident) => {
        fn $name(&self, index: usize) -> f64 {
            (self.vtable.price)(self.ctx, $crate::abi::NameOrIndexAbi::index(index))
        }
    };
    (average_price_at as $name:ident) => {
        fn $name(&self, index: usize) -> f64 {
            (self.vtable.average_price)(self.ctx, $crate::abi::NameOrIndexAbi::index(index))
        }
    };
    (bars_at as $name:ident) => {
        fn $name(&self, index: usize) -> &[::vike_marketdata::Bar] {
            $crate::abi::lent_in_place!(bar);
            let rows = (self.vtable.bars)(self.ctx, $crate::abi::NameOrIndexAbi::index(index));
            $crate::guest::lent(self, rows.cast())
        }
    };
    (instrument_grid_at as $name:ident) => {
        fn $name(&self, index: usize) -> Option<::vike_model::Grid> {
            (self.vtable.instrument_grid)(self.ctx, $crate::abi::NameOrIndexAbi::index(index))
                .into()
        }
    };
    (contract_value_at as $name:ident) => {
        fn $name(&self, index: usize) -> Option<::vike_model::ContractValue> {
            (self.vtable.contract_value)(self.ctx, $crate::abi::NameOrIndexAbi::index(index)).into()
        }
    };
    // The host's input list, copied once per `HostBroker` (one dispatch): `input_name_at` until `None`.
    (input_names as $name:ident) => {
        fn $name(&self) -> &[String] {
            let cache = $crate::guest::cache_mut!(self);
            cache.input_names.get_or_insert_with(|| {
                $crate::guest::host_list(self.ctx, self.vtable.input_name_at)
            })
        }
    };
    (input_bars_at as $name:ident) => {
        fn $name(&self, index: usize) -> &[::vike_marketdata::Bar] {
            $crate::abi::lent_in_place!(bar);
            let rows =
                (self.vtable.input_bars)(self.ctx, $crate::abi::NameOrIndexAbi::index(index));
            $crate::guest::lent(self, rows.cast())
        }
    };
    (input_whole_at as $name:ident) => {
        fn $name(&self, index: usize) -> Option<&[::vike_marketdata::Bar]> {
            $crate::abi::lent_in_place!(bar);
            let at = $crate::abi::NameOrIndexAbi::index(index);
            Some($crate::guest::lent(self, (self.vtable.input_whole)(self.ctx, at).get()?.cast()))
        }
    };
    (history_only_grows as $name:ident) => {
        fn $name(&self) -> bool {
            (self.vtable.history_only_grows)(self.ctx)
        }
    };
    // The host's closed rows, lent, read in place. `None` is the host's "no ledger", never an empty one.
    (closed_trades as $name:ident) => {
        fn $name(&self) -> Option<&[::vike_model::TradeClosedRow]> {
            $crate::abi::lent_in_place!(trade_closed_row);
            Some($crate::guest::lent(self, (self.vtable.closed_trades)(self.ctx).get()?.cast()))
        }
    };
}

/// Every method a face names, each by `broker_method!`: `broker_methods!(place, …, closed_trades)`, a
/// renamed one as `history_only_grows as inputs_only_grow`.
macro_rules! broker_methods {
    ($($method:ident $(as $name:ident)?),+ $(,)?) => {
        $($crate::guest::broker_method!($method $(as $name)?);)+
    };
}

pub(crate) use {broker_method, broker_methods, cache_mut};

/// **The face of the SDK this crate compiles against** (decision 0296's step 6, the adapter): the
/// `vike_model::OrderBroker` impl of [`HostBroker`], every method by `broker_methods!`. This crate's own
/// is `src/guest/face.rs`. In the glue the builder compiles for an older SDK through its adapter
/// (`feature = "adapted"`, which only that glue turns on) it is the adapter's `face.rs`
/// (`crate::sdk::SdkAdapter`): the same list with that SDK's differences, a renamed method as
/// `name as old_name` (the same body, so it costs nothing). A row laid out otherwise is no difference a
/// face can carry: such an SDK is retired (`crate::sdk::usable`).
#[cfg_attr(feature = "adapted", path = "../adapter/face.rs")]
pub mod face;

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
            // on this side — only the host's write through the pointer does.
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

/// `Strategy::stated_fee`'s answer as the JSON text the template's export lends the host
/// (`host::StrategyVTableAbi::stated_fee`, `ABI_VERSION` 13). A failure panics, as `params_json`'s does.
pub fn fee_json(fee: &vike_model::FeeSchedule) -> String {
    serde_json::to_string(fee).expect("a FeeSchedule always encodes as JSON")
}

/// `Strategy::stated_slippage`'s answer as the JSON text the template's export lends the host
/// (`host::StrategyVTableAbi::stated_slippage`, `ABI_VERSION` 13), as [`fee_json`] is.
pub fn slippage_json(slippage: &vike_model::ProfileEngineSlippage) -> String {
    serde_json::to_string(slippage).expect("a slippage table always encodes as JSON")
}

/// The state `Strategy::load_state` is handed, from the bytes the host lends for the call
/// (`host::StrategyVTableAbi::load_state`; `ABI_VERSION` 15: `vike_model::StrategyState`, msgpack with
/// field names), copied; `None` for bytes that are not one msgpack value. The template's
/// `vike_strategy_save_state` lends the strategy's own bytes back the other way, never re-encoded.
pub fn state_from_bytes(lent: SliceAbi<u8>) -> Option<vike_model::StrategyState> {
    let bytes = crate::abi::lent_bytes(lent);
    vike_model::StrategyState::check(bytes).ok()?;
    Some(vike_model::StrategyState::from_bytes(bytes.to_vec()))
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

/// The two forms of a strategy file's optional `echo` export, as [`BuildEntry`] is `build`'s:
/// `echo(params)`, or `echo(params, facts)` for a file whose effective params depend on the mount's
/// facts (the spread maker's grid, preset and fee floor), so the line names the strategy that RUNS.
/// `Form` is [`ParamsOnly`] or [`ParamsAndFacts`].
pub trait EchoEntry<Form> {
    /// The effective-params line over `params` (and `facts`, for the two-argument form).
    fn echo_from(
        self,
        params: &vike_model::params::Params,
        facts: &vike_model::params::MountFacts,
    ) -> Vec<(&'static str, String)>;
}

impl<F> EchoEntry<ParamsOnly> for F
where
    F: FnOnce(&vike_model::params::Params) -> Vec<(&'static str, String)>,
{
    fn echo_from(
        self,
        params: &vike_model::params::Params,
        _facts: &vike_model::params::MountFacts,
    ) -> Vec<(&'static str, String)> {
        self(params)
    }
}

impl<F> EchoEntry<ParamsAndFacts> for F
where
    F: FnOnce(
        &vike_model::params::Params,
        &vike_model::params::MountFacts,
    ) -> Vec<(&'static str, String)>,
{
    fn echo_from(
        self,
        params: &vike_model::params::Params,
        facts: &vike_model::params::MountFacts,
    ) -> Vec<(&'static str, String)> {
        self(params, facts)
    }
}

/// Echo through a file's `echo` export in whichever form it wrote ([`EchoEntry`]).
pub fn echo_entry<Form>(
    entry: impl EchoEntry<Form>,
    params: &vike_model::params::Params,
    facts: &vike_model::params::MountFacts,
) -> Vec<(&'static str, String)> {
    entry.echo_from(params, facts)
}

// A `BrokerRef`'s `ctx` is an erased `&mut OrderBrokerSimulator` owned by the HOST for the duration of the
// call the strategy file is currently inside; `HostBroker` never outlives that call (the strategy file's own
// `extern "C"` entry point constructs one, uses it, and drops it before returning), so it never
// needs to be `Send`/`Sync` and deliberately is not (the default: it holds a raw pointer, which
// opts it out of both auto traits already).

#[path = "guest_tests.rs"]
#[cfg(test)]
mod guest_tests;
