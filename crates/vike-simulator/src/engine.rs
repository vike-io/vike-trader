//! Multi-asset / portfolio backtesting on one shared cash account.
//! Exact port of `core/multi_symbol_engine.py::StrategyEngine` (origin/main).
//!
//! Aligned per-symbol bar series stepped timestamp-by-timestamp; orders submitted during a
//! step fill at each symbol's NEXT bar open (no look-ahead). Equity = cash + Σ pos·price·mult.
//!
//! Rust adaptation (same pattern as the single engine): strategy handlers take `&mut OrderBrokerSimulator`
//! (the ctx) instead of a back-reference. The fill dispatchers are ENGINE methods (not core
//! methods) so `strategy.on_fill` fires synchronously after each applied fill — Python's exact
//! firing point. The ledger mirror emitters (`_on_fill`/`_on_funding`) become collected
//! `vike_model::Fill`/`MirrorFunding` vecs (drained by the ledger integration), preserving order.
//!
//! ## The end-of-run resolution probe sentinel
//!
//! [`RESOLUTION_PROBE_SENTINEL`] is the "did this market EVER resolve?" timestamp the end-of-run
//! sweep probes a [`ResolutionSource`] at when no explicit `StrategyEngineConfig::resolution_end_timestamp` is
//! configured. It is deliberately NOT `i64::MAX`: the source is CALLER code, and the natural way
//! to write one is windowed arithmetic on the probe ts (`ts + window`, `ts - lookback`, a
//! `ts .. ts + horizon` range check). With `i64::MAX` any such `+` overflows — a debug-build panic
//! and a silently wrong wrap in release, in third-party code that did nothing unreasonable.
//!
//! We pick a saturating sentinel far below the type's ceiling instead of documenting a
//! "don't add to the probe ts" contract, because a contract only helps callers who read it and
//! this engine cannot see their arithmetic. `i64::MAX / 4` is ~7.3e16 ms (~2.3 million years past
//! the epoch), so it is unambiguously "after every real resolution" while leaving three quarters
//! of the positive range as headroom — any sane window added to it still fits comfortably.

// Index loops + wide verb signatures mirror the Python engine line-for-line (six parallel
// per-symbol state fields; verbs take Python's full kwarg surface) — parity beats idiom here.
#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]

use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{Fill, OrderDrop, OrderKind, OrderResting, Strategy};

/// The end-of-run resolution probe timestamp (ms since epoch) used when
/// [`StrategyEngineConfig::resolution_end_timestamp`] is `None`. Far past any real market resolution, yet far
/// enough below `i64::MAX` that a caller's `probe_timestamp + window` arithmetic cannot overflow — see
/// the module doc for the full rationale.
pub const RESOLUTION_PROBE_SENTINEL: i64 = i64::MAX / 4;

use crate::queue_model::QueueTracker;

// The OrderBrokerSimulator fill-engine family lives in a sibling module (behavior byte-identical),
// re-exported here as `engine::{OrderBrokerSimulator, StrategyEngineConfig, …}` so the crate root can name them as
// `vike_simulator::…`. That is the ONE in-crate vocabulary hop: `sim_broker` is private and `engine` is
// `pub(crate)`, so each of these types has exactly one PUBLIC name (docs/decisions/0087).
mod sim_broker;
pub use sim_broker::{
    ConversionRate, ConversionSeries, DEFAULT_IMPACT_WINDOW, DecideMode, EquitySampling,
    FillModelKind, InBarRules, InputSeries, Inputs, MirrorFunding, OptionExpirySource,
    OrderBrokerSimulator, ResolutionSource, SettlementFill, SettlementVariation, ShortfallCover,
    StrategyEngineConfig,
};
use sim_broker::{
    InBar, InputView, OrderBrokerSimulatorSymbolState, OrderEmulationMarketHook,
    QuoteConversionCursor, path_of, settle_unrealized_pnl,
};

// ...and the OPT-IN queue-position twins of the tick fill pass in a second one. Private: the
// only way in is `fill_pending_tick`'s `self.queue.is_some()` branch, exactly as before.
mod queued;

mod bar_fills;
mod construct;
mod deferral;
mod delivery;
mod exits;
mod liquidation;
mod run_bars;
mod run_ticks;
mod settlement;

/// A tick event for `run_ticks` (Quote, Trade, or a recorded L2 book event).
#[derive(Debug, Clone)]
pub enum TickEvent {
    Quote(TickQuote),
    Trade(TickTrade),
    Book(L2BookUpdate),
}

/// A tick's symbol, BORROWED. A `fn` rather than a closure so the elided lifetime ties the
/// returned `&str` to the tick (a closure would unify it to one concrete lifetime and force the
/// caller to own a `String`): `run_ticks` resolves a symbol SLOT per tick, and on a 100M-tick
/// tape one `String` clone per tick purely to compare it is the whole cost of routing.
fn tick_symbol(t: &TickEvent) -> &str {
    match t {
        TickEvent::Quote(q) => &q.symbol,
        TickEvent::Trade(tr) => &tr.symbol,
        TickEvent::Book(b) => &b.symbol,
    }
}

/// The `first_fill` [`StrategyEngine::rest_remainder`] takes for an order the event priced at `fp`
/// and the volume cap filled none of: an opening auction's market-to-limit rests at the auction's
/// price `fp`, every other order as it was (`None`).
fn unfilled_limit(o: &OrderResting, fp: f64) -> Option<f64> {
    (o.kind == OrderKind::AuctionOpenToLimit).then_some(fp)
}

fn kind_str(k: OrderKind) -> &'static str {
    match k {
        OrderKind::Market => "market",
        OrderKind::MarketClose => "market_close",
        OrderKind::LimitClose => "limit_close",
        OrderKind::Limit => "limit",
        OrderKind::Stop => "stop",
        OrderKind::Trailing => "trailing",
        OrderKind::StopLimit => "stop_limit",
        OrderKind::TakeProfit => "take_profit",
        OrderKind::MarketToLimit => "market_to_limit",
        OrderKind::AuctionOpen => "auction_open",
        OrderKind::AuctionClose => "auction_close",
        OrderKind::TrailingLimit => "trailing_limit",
        OrderKind::TrailingTakeProfit => "trailing_take_profit",
        OrderKind::AuctionOpenToLimit => "auction_open_to_limit",
    }
}

/// "SYMBOL.VENUE" (bare symbol when no venue) — `core/instrument_id.py::format_instrument`.
pub fn format_instrument(venue: Option<&str>, symbol: &str) -> String {
    match venue {
        Some(v) => format!("{symbol}.{}", v.to_uppercase()),
        None => symbol.to_string(),
    }
}

/// The verdict of the per-order gate sequence ([`StrategyEngine::gate_pending`]) — the ONE
/// answer every untagged fill lane acts on.
enum OrderGate {
    /// Keeps resting: a wait discipline deferred it, the stop-release emulator converted it into
    /// a market child for the NEXT event, or the price condition simply is not met. The caller
    /// pushes the order back onto `pending`.
    Rest,
    /// Triggers on this event, at this fill price.
    Trigger(f64),
    /// Over without a fill: an auction order its one auction did not fill (`auction_passed` in
    /// [`OrderBrokerSimulator::dropped`]). The caller does not push it back.
    End,
}

/// Runs a [`Strategy`] over aligned per-symbol bar series (backtest event engine).
pub struct StrategyEngine<S: Strategy<OrderBrokerSimulator>> {
    pub core: OrderBrokerSimulator,
    pub strategy: S,
    /// Opt-in queue-position tracker ([`crate::queue_model`]) — `Some` only when
    /// `StrategyEngineConfig::queue_model` was set; `None` (default) keeps every fill path
    /// byte-identical. Consulted exclusively by the `run_ticks` tick/book replay lanes.
    queue: Option<QueueTracker>,
    /// Opt-in order-latency config ([`crate::latency`]) — `Some` only when
    /// `StrategyEngineConfig::latency_model` was set. Held here, NOT armed: `run_ticks` installs the
    /// live gate into `core.latency` for the duration of a tick replay and removes it after,
    /// so the bar path (`run`) and the vector kernel structurally cannot consult it.
    latency: Option<crate::latency::LatencyModelKind>,
    /// Fills awaiting response-latency delivery to the strategy: `(visible_ns, fill)`, in the
    /// order they occurred. Only ever non-empty inside an ARMED `run_ticks`; the default path
    /// never touches it (see [`Self::fire_on_fill`]).
    deferred_fills: Vec<(i64, Fill)>,
    /// Tick-lane equity-curve density ([`StrategyEngineConfig::equity_sampling`]). `EveryTick` (the
    /// default) is the frozen behaviour; only `run_ticks` reads it.
    equity_sampling: EquitySampling,
    /// The EFFECTIVE warm-up gate in steps, resolved ONCE in [`Self::new`] as
    /// `p.warmup.unwrap_or(0).max(strategy.warmup())` — the number both [`Self::run`] and
    /// [`Self::run_ticks`] compare their step index against before dispatching ANY strategy
    /// callback, and the number [`BacktestResult::warmup`] reports.
    ///
    /// ⚠ It is resolved here rather than read per step for two reasons, and the second is the
    /// load-bearing one. Cheaply: `strategy.warmup()` was a virtual call inside the fold loop.
    /// Correctly: the two REPORTING sites used to publish `strategy.warmup()` while the two GATE
    /// sites read the same expression, so the report could not disagree with the gate by
    /// construction — and the moment an outside FLOOR ([`StrategyEngineConfig::warmup`]) exists, keeping
    /// that property means one resolution feeding both. A report naming a warm-up the run did not
    /// actually apply is worse than no report: `vike_analytics::zero_trade` diagnoses a
    /// zero-trade run FROM this number.
    warmup: usize,
}

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// One resolved order's fill dispatch: open-caps gate (active-mask + open/long/short position
    /// caps) -> volume clamp -> `apply_fill` -> protective-stop arm -> `on_fill`. Extracted verbatim
    /// from the three `fill_pending*` lanes (`fill_pending` / `fill_pending_granular` /
    /// `fill_pending_tick`), which differ only in the `Bar` supplying `vol`/`ts` and in the caller's
    /// dust guard (granular omits it) — both kept at the call sites. A cap veto or a `None`
    /// `apply_fill` (the PIT-grid gate) simply skips this order, exactly as the inline `continue` did.
    ///
    /// Returns the qty that ACTUALLY hit the position — `0.0` on any cap veto, volume clamp to
    /// nothing, or PIT-grid rejection, and the grid-rounded size otherwise. Every pre-existing
    /// caller invokes it as a statement and discards that value (byte-identical behavior); the
    /// opt-in queue lane needs it to shrink a partially-filled resting order by what really
    /// filled instead of by what it merely INTENDED to fill. The fee is charged on the order's fees so
    /// far (`OrderBrokerSimulator::order_fees`), which a booked fill updates.
    fn dispatch_fill(&mut self, si: usize, o: &OrderResting, fp: f64, vol: f64, ts: i64) -> f64 {
        // An order-list leg its siblings' fills on this pass cancelled never fills, and one they
        // reduced fills what they left it (`OrderBrokerSimulator::group_leg_size`); any other order its size.
        let Some(size) = self.core.group_leg_size(si, o) else {
            return 0.0;
        };
        // A REDUCE-ONLY order never opens or enlarges: capped to the position now, or dropped in
        // words when the position is gone (`OrderBrokerSimulator::reduce_only_size`). One bool test otherwise.
        let size = if o.reduce_only {
            let room = self.core.reduce_only_size(si, o.side, size);
            if room <= 0.0 {
                return 0.0;
            }
            room
        } else {
            size
        };
        let pos = self.core.symbol[si].pos;
        let opening = !vike_model::is_reducing_direction(o.side, pos.size);
        if opening && !self.core.is_active_index(si) {
            return 0.0;
        }
        if pos.size == 0.0 && self.core.at_open_cap() {
            return 0.0;
        }
        if pos.size == 0.0 && o.side > 0 && self.core.at_long_cap() {
            return 0.0;
        }
        if pos.size == 0.0 && o.side < 0 && self.core.at_short_cap() {
            return 0.0;
        }
        let fill_size = self.volume_clamp(si, o, size, vol, fp);
        if fill_size > 0.0 {
            // Maker/taker by ORDER KIND, not by crossing aggressiveness: a MARKETABLE limit books
            // as a maker fill. See the classification caveat in `vike_model::money::fees` — it flips the
            // fee SIGN under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
            let is_maker = o.books_as_maker();
            let bounded = o.is_limit_bounded();
            let mut fees = self.core.order_fees_of(o.qid);
            if let Some(mut fill) = self.core.apply_order_fill(
                si, o.side, fill_size, fp, o.weight, ts, is_maker, bounded, &mut fees,
            ) {
                let applied = fill.size;
                if o.stop.is_some() && self.core.symbol[si].pos.size != 0.0 {
                    self.core.symbol[si].stop = o.stop;
                }
                let qid = self.rest_remainder(si, o, size - fill_size, Some(fill.price));
                self.core.note_order_fees(si, qid, fees);
                self.core.resolve_group_fill(si, o, applied, fill.price);
                // the fill names the order it came from (`OrderBrokerSimulator::tag_of`: `None` untagged)
                fill.tag = self.core.tag_of(o.qid);
                self.fire_on_fill(fill);
                return applied;
            }
            return 0.0;
        }
        self.rest_remainder(si, o, size, unfilled_limit(o, fp));
        0.0
    }

    /// The combo pass both lanes run after an event's other fills (`OrderBrokerSimulator::fill_combos`, on the
    /// marks `OrderBrokerSimulator::note_combo_step` / `note_combo_mark` noted): each leg fill it booked reaches
    /// `on_fill`, in leg order. Nothing while no combo rests.
    fn sweep_combos(&mut self) {
        for fill in self.core.fill_combos() {
            self.fire_on_fill(fill);
        }
    }

    /// What the volume cap left of a MARKET-TO-LIMIT, `rest` of it, stays on the book in place
    /// (`vike_model::order_fill_price`'s module law): as the plain `Limit` at `first_fill`, the
    /// price its first fill booked at, or as the market-to-limit it was when nothing filled yet.
    /// An opening auction's market-to-limit rests the same way, and when nothing filled its caller
    /// names the auction's price as `first_fill` ([`unfilled_limit`]): the auction is over, so it rests
    /// whole as the `Limit` there. An ALL-OR-NONE limit the cap held whole (nothing filled) rests as it
    /// was. Placed BEFORE that fill's `on_fill`, so a cancel there reaches it. Every other kind's
    /// remainder is `"volume_cap"`'s in [`OrderBrokerSimulator::dropped`], as before.
    ///
    /// Returns the `qid` the order goes on under: a remainder after a fill is the same order, so it
    /// is given one (`OrderBrokerSimulator::next_queue_id`) when it has none, for its fees so far to be found by
    /// (`OrderBrokerSimulator::note_order_fees`); otherwise `o`'s own.
    fn rest_remainder(
        &mut self,
        si: usize,
        o: &OrderResting,
        rest: f64,
        first_fill: Option<f64>,
    ) -> u64 {
        let rests = matches!(o.kind, OrderKind::MarketToLimit | OrderKind::AuctionOpenToLimit)
            || (o.all_or_none && first_fill.is_none());
        if !rests || rest <= 1e-12 {
            return o.qid;
        }
        let mut remainder = OrderResting { size: rest, ..o.clone() };
        if first_fill.is_some() {
            remainder.limit = first_fill;
            remainder.rest_as_limit();
            if remainder.qid == 0 {
                remainder.qid = self.core.next_queue_id();
            }
        }
        let qid = remainder.qid;
        self.core.symbol[si].pending.push(remainder);
        qid
    }

    /// THE per-order gate sequence — session → staleness → emulator stop-release → trigger price
    /// — in ONE place. Every untagged fill lane runs exactly this and now runs it by calling
    /// here: the coarse [`Self::fill_pending`], the granular [`Self::fill_pending_granular`], the
    /// cash-gated [`Self::fill_step_gated`], the tick [`Self::fill_pending_tick`], and the queued
    /// tick twin's TAKER branch (`engine::queued`). Each lane used to spell the same three calls
    /// inline, which is why the session and staleness disciplines each had to be written five
    /// times over and kept in sync by hand.
    ///
    /// `o` is `&mut` because two of the steps legitimately rewrite it: the stop-release emulator
    /// converts a triggered stop into a resting MARKET child, and `fill_price_for` advances a
    /// trailing stop's level. Both are the frozen behaviour, unchanged by living here — an
    /// [`OrderGate::Rest`] verdict still hands the caller an order it pushes straight back.
    ///
    /// NOT used by the queued twin's price-conditional (queued-limit) branch: that branch wants
    /// the session half but not the staleness half, and it interleaves queue-state identity work
    /// around its own `fill_price_for` call. It composes [`Self::defer_session`] by hand — the one
    /// documented divergence, stated there rather than duplicated here.
    ///
    /// Before all of it, the two auction holds, which are no discipline and count nothing: an
    /// opening-auction order waits for its auction instant ([`OrderResting::is_held`] on the
    /// release its verb set), and a closing-auction order is live only on its session's last bar
    /// ([`OrderBrokerSimulator::closes_session`]). An auction order its auction did not fill is over
    /// ([`OrderGate::End`]). An order sent inside a release is not in `pending` before it: it is
    /// HELD off the book and placed by `OrderBrokerSimulator::release_held`, which every lane calls first.
    #[inline]
    fn gate_pending(&mut self, si: usize, o: &mut OrderResting, event: &Bar) -> OrderGate {
        if o.is_held(event.timestamp, |s| self.core.release_price(s))
            || (o.kind == OrderKind::AuctionClose && !self.core.closes_session(si))
        {
            return OrderGate::Rest;
        }
        // Wait disciplines (opt-in, both off by default): a market order against fill-forwarded
        // data — or any order on a shut market — keeps resting instead of filling.
        if self.defer_fill(si, o.kind, event.timestamp) {
            return OrderGate::Rest;
        }
        // opt-in emulator-mirroring stop release (no-op by default): a triggered stop converts to
        // a resting market child and fills at the NEXT event's price.
        if self.core.stop_released(si, o, event) {
            return OrderGate::Rest;
        }
        match self.core.fill_price_for(si, o, event) {
            None if matches!(o.kind, OrderKind::AuctionOpen | OrderKind::AuctionClose) => {
                let symbol_name = self.core.symbols[si].clone();
                self.core.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: "auction_passed".into(),
                    size: o.size,
                    weight: o.weight,
                });
                OrderGate::End
            }
            None => OrderGate::Rest,
            Some(fp) => OrderGate::Trigger(fp),
        }
    }
}

/// The permutation proof for [`DecideMode`] — the property the mode exists to establish, checked
/// end to end through the real engine rather than argued in prose.
///
/// ⚠ The load-bearing test here is [`tests::sequential_is_order_dependent_the_defect_itself`],
/// not the invariance one. An invariance assertion over a fixture that is symmetric anyway passes
/// for no reason and reports a property nobody obtained; pinning that the DEFAULT mode genuinely
/// answers differently under the two symbol orders is what proves the fixture reaches the
/// mechanism. If the defect test ever goes green-by-accident, the invariance test beside it has
/// stopped meaning anything and both need a new fixture.
#[cfg(test)]
mod tests;
