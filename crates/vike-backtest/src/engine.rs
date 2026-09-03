//! Multi-asset / portfolio backtesting on one shared cash account.
//! Exact port of `core/multi_symbol_engine.py::StrategyEngine` (origin/main).
//!
//! Aligned per-symbol bar series stepped timestamp-by-timestamp; orders submitted during a
//! step fill at each symbol's NEXT bar open (no look-ahead). Equity = cash + Σ pos·price·mult.
//!
//! Rust adaptation (same pattern as the single engine): strategy handlers take `&mut SimBroker`
//! (the ctx) instead of a back-reference. The fill dispatchers are ENGINE methods (not core
//! methods) so `strategy.on_fill` fires synchronously after each applied fill — Python's exact
//! firing point. The ledger mirror emitters (`_on_fill`/`_on_funding`) become collected
//! `MirrorFill`/`MirrorFunding` vecs (drained by the ledger integration), preserving order.
//!
//! ## The end-of-run resolution probe sentinel
//!
//! [`RESOLUTION_PROBE_SENTINEL`] is the "did this market EVER resolve?" timestamp the end-of-run
//! sweep probes a [`ResolutionSource`] at when no explicit `EngineParams::resolution_end_ts` is
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

use std::rc::Rc;

use indexmap::IndexMap;
use vike_model::{
    Bar, BookUpdate, BookUpdateKind, DeltaDecision, FeeSchedule, Fill, L2Book, OrderKind, Position,
    QuoteTick, SeqPolicy, SessionCalendar, Strategy, TradeTick, WorkingOrder,
};

/// The end-of-run resolution probe timestamp (ms since epoch) used when
/// [`EngineParams::resolution_end_ts`] is `None`. Far past any real market resolution, yet far
/// enough below `i64::MAX` that a caller's `probe_ts + window` arithmetic cannot overflow — see
/// the module doc for the full rationale.
pub const RESOLUTION_PROBE_SENTINEL: i64 = i64::MAX / 4;

use crate::broker_sim::{adverse_fill_price, fee as fee_fn, funding_charge};
use crate::fill_resolution::resolve_intrabar_fills;
use crate::queue_model::QueueTracker;
use crate::result::BacktestResult;
use crate::schedule::Schedule;
use crate::sizing::PassThroughSizer;

// The SimBroker fill-engine family lives in a sibling module (behavior byte-identical);
// re-exported so `engine::{SimBroker, EngineParams, MirrorFill, MirrorFunding}` and the
// crate-root `vike_backtest::*` paths are unchanged.
mod sim_broker;
use sim_broker::SymbolState;
pub use sim_broker::{
    EngineParams, EquitySampling, FillModelKind, MirrorFill, MirrorFunding, OptionExpirySource,
    OptionRight, OptionSpec, ResolutionSource, SettlementFill, SimBroker, VariationSettlement,
    DEFAULT_IMPACT_WINDOW,
};

// ...and the OPT-IN queue-position twins of the tick fill pass in a second one. Private: the
// only way in is `fill_pending_tick`'s `self.queue.is_some()` branch, exactly as before.
mod queued;

/// A tick event for `run_ticks` (Quote, Trade, or a recorded L2 book event).
#[derive(Debug, Clone)]
pub enum Tick {
    Quote(QuoteTick),
    Trade(TradeTick),
    Book(BookUpdate),
}

/// Fold one recorded book event into the replay book slot. Returns whether a state-changing
/// apply happened (Snapshot or in-sequence Delta) — the on_order_book delivery gate.
/// Integrity rule (the recorded gap-sentinel, replayed): the recorded chain is feed-local
/// contiguous, so a Delta applies ONLY when the shared [`L2Book::delta_decision`] law says
/// Apply under [`SeqPolicy::Strict`] (`seq == last_seq + 1` — the same decision bybit's live
/// depth fold consults); anything else (gap, orphan, §B marker) drops the book until the
/// next Snapshot re-anchors — identical to what the live consumer knew at the time.
fn apply_book_event(slot: &mut Option<std::rc::Rc<L2Book>>, b: &BookUpdate) -> bool {
    match b.kind {
        BookUpdateKind::Snapshot => {
            let mut book = L2Book::new(b.tick_size);
            book.apply_snapshot(b.seq, &b.bids, &b.asks);
            *slot = Some(std::rc::Rc::new(book));
            true
        }
        // `Rc::make_mut` is O(1) whenever this is the only handle — which it is by the time the
        // next event arrives, since the `on_order_book` delivery clone is dropped at the end of
        // that callback. It clones (once) only if a strategy has deliberately retained one.
        BookUpdateKind::Delta => match slot.as_mut().map(std::rc::Rc::make_mut) {
            Some(book) if book.delta_decision(b.seq, SeqPolicy::Strict) == DeltaDecision::Apply => {
                book.apply_delta(b.seq, &b.bids, &b.asks);
                true
            }
            _ => {
                *slot = None; // discontinuity — untrustworthy until the next anchor
                false
            }
        },
        BookUpdateKind::GapStart | BookUpdateKind::Stale => {
            *slot = None;
            false
        }
        BookUpdateKind::LiveResume => false,
    }
}

/// A tick's symbol, BORROWED. A `fn` rather than a closure so the elided lifetime ties the
/// returned `&str` to the tick (a closure would unify it to one concrete lifetime and force the
/// caller to own a `String`): `run_ticks` resolves a symbol SLOT per tick, and on a 100M-tick
/// tape one `String` clone per tick purely to compare it is the whole cost of routing.
fn tick_symbol(t: &Tick) -> &str {
    match t {
        Tick::Quote(q) => &q.symbol,
        Tick::Trade(tr) => &tr.symbol,
        Tick::Book(b) => &b.symbol,
    }
}
use crate::timeframe::{parse_timeframe, resample};

fn kind_str(k: OrderKind) -> &'static str {
    match k {
        OrderKind::Market => "market",
        OrderKind::MarketClose => "market_close",
        OrderKind::LimitClose => "limit_close",
        OrderKind::Limit => "limit",
        OrderKind::Stop => "stop",
        OrderKind::Trailing => "trailing",
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
}

/// Runs a [`Strategy`] over aligned per-symbol bar series (backtest event engine).
pub struct StrategyEngine<S: Strategy<SimBroker>> {
    pub core: SimBroker,
    pub strategy: S,
    /// Opt-in queue-position tracker ([`crate::queue_model`]) — `Some` only when
    /// `EngineParams::queue_model` was set; `None` (default) keeps every fill path
    /// byte-identical. Consulted exclusively by the `run_ticks` tick/book replay lanes.
    queue: Option<QueueTracker>,
    /// Opt-in order-latency config ([`crate::latency`]) — `Some` only when
    /// `EngineParams::latency_model` was set. Held here, NOT armed: `run_ticks` installs the
    /// live gate into `core.latency` for the duration of a tick replay and removes it after,
    /// so the bar path (`run`) and the vector kernel structurally cannot consult it.
    latency: Option<crate::latency::LatencyModelKind>,
    /// Fills awaiting response-latency delivery to the strategy: `(visible_ns, fill)`, in the
    /// order they occurred. Only ever non-empty inside an ARMED `run_ticks`; the default path
    /// never touches it (see [`Self::fire_on_fill`]).
    deferred_fills: Vec<(i64, Fill)>,
    /// Tick-lane equity-curve density ([`EngineParams::equity_sampling`]). `EveryTick` (the
    /// default) is the frozen behaviour; only `run_ticks` reads it.
    equity_sampling: EquitySampling,
}

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    pub fn new(bars_by_symbol: Vec<(String, Vec<Bar>)>, strategy: S, p: EngineParams) -> Self {
        let symbols: Vec<String> = bars_by_symbol.iter().map(|(s, _)| s.clone()).collect();
        // Pre-tag each bar with its instrument id once at construction.
        let bars: Vec<Rc<Vec<Bar>>> = bars_by_symbol
            .iter()
            .map(|(s, series)| {
                Rc::new(
                    series
                        .iter()
                        .map(|b| {
                            let mut nb = b.clone();
                            nb.symbol = Some(format_instrument(p.default_venue.as_deref(), s));
                            nb
                        })
                        .collect(),
                )
            })
            .collect();
        let lengths: std::collections::BTreeSet<usize> = bars.iter().map(|v| v.len()).collect();
        assert!(lengths.len() <= 1, "all symbol series must have the same length (aligned)");
        let n = lengths.into_iter().next().unwrap_or(0);

        let mut sym: Vec<SymbolState> = bars_by_symbol
            .iter()
            .map(|(_, series)| SymbolState {
                pos: Position::default(),
                pending: Vec::new(),
                realized: 0.0,
                stop: None,
                entry_fee: 0.0,
                entry_ts: 0,
                price: if n > 0 { series[0].open } else { 0.0 },
                hi_since: 0.0,
                lo_since: f64::INFINITY,
                tf: Vec::new(),
                sub: vec![Vec::new(); n],
                tagged: IndexMap::new(),
                settled_profit: 0.0,
            })
            .collect();
        // per-symbol higher-TF aggregates
        for (si, st) in sym.iter_mut().enumerate() {
            for tf in &p.timeframes {
                let ms = parse_timeframe(tf).expect("valid timeframe");
                st.tf.push((tf.clone(), ms, resample(&bars[si], ms)));
            }
        }
        // granular sub-bars bucketed into their coarse step
        for (gs, subs) in &p.granular_by_symbol {
            if subs.is_empty() {
                continue;
            }
            let si = symbols.iter().position(|s| s == gs).expect("granular symbol unknown");
            let edges: Vec<i64> = bars[si].iter().map(|b| b.ts).collect();
            let mut sorted_subs: Vec<Bar> = subs.clone();
            sorted_subs.sort_by_key(|b| b.ts);
            for sub in sorted_subs {
                // bisect_right(edges, sub.ts) - 1
                let i = edges.partition_point(|&e| e <= sub.ts) as i64 - 1;
                if i < 0 {
                    continue; // precedes the first coarse bar
                }
                sym[si].sub[i as usize].push(sub);
            }
        }
        let now = if !symbols.is_empty() && n > 0 { bars[0][0].ts } else { 0 };
        // Opt-in queue-position tracker (tick/book replay only): built ONCE from the Copy
        // config fields; `None` (the default) means the tracker never exists — byte-identical.
        let queue = p
            .queue_model
            .map(|k| QueueTracker::new(k, p.queue_seed_depth, p.queue_min_hold_ms, symbols.len()));
        // Resolve each symbol's contract multiplier ONCE (index si -> f64) so hot paths (equity_now,
        // per-symbol curve, apply_fill, liquidation) index a Vec instead of scanning `mult` by string
        // on every call. Byte-identical to multiplier_of()'s first-match-else-default lookup — matters
        // for a mixed futures/stocks/tokens book with many per-symbol multipliers.
        let mult_by_si: Vec<f64> = symbols
            .iter()
            .map(|s| {
                p.multipliers
                    .iter()
                    .find(|(name, _)| name == s)
                    .map(|(_, m)| *m)
                    .unwrap_or(p.multiplier)
            })
            .collect();
        let n_sym_resolved = symbols.len(); // captured before `symbols` moves into the struct
                                            // Per-symbol session calendars for the opt-in gate (see `EngineParams::session_gate`).
                                            // EMPTY unless the gate is on, so the default fill path pays one `Vec::get`/`is_empty`
                                            // branch and never indexes (mirrors `last_print_ts`). The KEY is per SYMBOL — each resolves
                                            // to its per-symbol override, else the venue-only `default_venue` calendar, else the
                                            // fail-permissive always-open row — which is the fix for one `default_venue` per run.
        let session: Vec<Option<SessionCalendar>> = if p.session_gate {
            let venue_default = p.default_venue.as_deref().map(vike_model::session_for);
            symbols.iter().map(|s| p.session_calendars.get(s).copied().or(venue_default)).collect()
        } else {
            Vec::new()
        };
        // THE ONE JUDGE (deny-vs-clamp phase 2): mount the literal live RiskGate when armed —
        // `None` (no leverage, no risk_limits, or the clamp escape hatch) keeps the submit path
        // byte-identical to the pre-phase-2 engine. See `SimBroker::build_risk_gate`.
        let risk_gate = SimBroker::build_risk_gate(&p);
        // Fee-schedule bridge (fee model follow-up 3): `Some` OVERRIDES the flat
        // `maker_fee`/`taker_fee`/`fee_rate` chain with the venue schedule's own
        // `maker_taker_rates()` fractions; `None` (default) is the frozen precedence, untouched.
        // See `EngineParams::fee_schedule`.
        let (maker_fee, taker_fee) = match p.fee_schedule {
            Some(schedule) => schedule.maker_taker_rates(),
            None => (p.maker_fee.unwrap_or(p.fee_rate), p.taker_fee.unwrap_or(p.fee_rate)),
        };
        // ...with ONE exception (port backlog G7): `ProbabilityScaled`'s `qty × rate × p(1−p)`
        // curve has NO flat equivalent — `maker_taker_rates()` reports `(0.0, 0.0)` for it, so
        // flattening it would charge exactly zero while looking configured. That shape (and only
        // that shape) is carried through to the fill site verbatim and applied via
        // `FeeSchedule::commission`. Every other configuration leaves `fee_curve` `None`, so the
        // frozen flat-rate fold is byte-identical.
        let fee_curve = match p.fee_schedule {
            Some(s @ FeeSchedule::ProbabilityScaled { .. }) => Some(s),
            _ => None,
        };
        let core = SimBroker {
            symbols,
            bars,
            n,
            fee_rate: p.fee_rate,
            cash: p.cash,
            funding_paid: 0.0,
            slippage: p.slippage,
            maker_fee,
            taker_fee,
            fee_curve,
            multiplier: p.multiplier,
            mult: p.multipliers,
            mult_by_si,
            leverage: p.leverage,
            clamp_to_leverage: p.clamp_to_leverage,
            risk_gate,
            maint_margin: p.maint_margin,
            venue_style_liquidation: p.venue_style_liquidation,
            liq_buffer: p.liq_buffer,
            cash_gate: p.cash_gate,
            active_mask: p.active_mask,
            max_open_positions: p.max_open_positions,
            max_open_long: p.max_open_long,
            max_open_short: p.max_open_short,
            sizer: p.sizer.unwrap_or_else(|| Box::new(PassThroughSizer)),
            volume_limit: p.volume_limit.filter(|v| *v != 0.0),
            equity_peak: p.cash,
            step: 0,
            dropped: Vec::new(),
            below_min_reversals: 0,
            trades: Vec::new(),
            intrabar_both_hit: 0,
            sym,
            now,
            fill_model: p.fill_model,
            emulator_release_stops: p.emulator_release_stops,
            index: 0,
            schedule: Schedule::new(),
            mirror_fills: if p.mirror { Some(Vec::new()) } else { None },
            mirror_funding: if p.mirror { Some(Vec::new()) } else { None },
            properties: p.properties.clone(),
            funding_source: p.funding_source.clone(),
            venue: p.default_venue.clone(),
            // The per-symbol resolved latch is allocated ONLY with a resolution OR option-expiry
            // source (both settle terminally and share the latch), so the default order-intake path
            // stays a single `is_empty` branch (see `push_pending`).
            resolved: if p.resolution.is_some() || p.option_specs.is_some() {
                vec![false; n_sym_resolved]
            } else {
                Vec::new()
            },
            resolution: p.resolution,
            resolution_end_ts: p.resolution_end_ts,
            option_specs: p.option_specs,
            option_expiry_end_ts: p.option_expiry_end_ts,
            settlements: Vec::new(),
            settlement_period_ms: p.settlement_period_ms,
            settle_bucket: None,
            variation_settlements: Vec::new(),
            // Same allocate-only-when-configured trick as `resolved` above: with the wait
            // discipline off this stays EMPTY and the fill lanes never index it (see
            // `SimBroker::defer_stale`), keeping the default path byte-identical.
            last_print_ts: if p.max_price_staleness_ms.is_some() {
                vec![None; n_sym_resolved]
            } else {
                Vec::new()
            },
            max_price_staleness_ms: p.max_price_staleness_ms,
            stale_deferrals: 0,
            // Resolved ONCE per symbol at construction (see the `session` let-binding above); the
            // fill lanes then pay a single `Vec::get` per event, no string match — see
            // `EngineParams::session_gate` and `SimBroker::session_closed`.
            session,
            session_deferrals: 0,
            impact: p.impact,
            impact_window: p.impact_window,
            slippage_saturations: 0,
            latency: None, // armed by `run_ticks` only (see `StrategyEngine::latency`)
            shadow_pos: Vec::new(), // sized at arm time, cleared at disarm (see `run_ticks`)
            books: Vec::new(), // sized by `run_ticks` only when the tape carries book events
        };
        StrategyEngine {
            core,
            strategy,
            queue,
            latency: p.latency_model,
            deferred_fills: Vec::new(),
            equity_sampling: p.equity_sampling,
        }
    }

    /// Deliver a fill to the strategy.
    ///
    /// Immediate unless the opt-in latency gate is ARMED, in which case the fill is held until
    /// `exch_ts + response()` — the response leg. `exch_ts` is `core.now`, the timestamp of the
    /// tick whose matching produced the fill. Held fills are released by
    /// [`Self::deliver_due_fills`] at the top of each subsequent tick, and flushed once at the
    /// end of `run_ticks` so nothing is silently swallowed.
    fn fire_on_fill(&mut self, fill: Fill) {
        if let Some(g) = self.core.latency.as_ref() {
            let desc = crate::latency::LatencyOrder::new(fill.side, fill.size, Some(fill.price));
            let visible_ns = self
                .core
                .now
                .saturating_mul(1_000_000)
                .saturating_add(g.response_ns(self.core.now, &desc));
            self.deferred_fills.push((visible_ns, fill));
            return;
        }
        self.strategy.on_fill(&mut self.core, &fill);
    }

    /// Release every response-delayed fill whose visibility stamp has arrived at `ts_ms`, in
    /// occurrence order. No-op (and no allocation) when nothing is deferred.
    fn deliver_due_fills(&mut self, ts_ms: i64) {
        if self.deferred_fills.is_empty() {
            return;
        }
        let now_ns = ts_ms.saturating_mul(1_000_000);
        let mut due: Vec<Fill> = Vec::new();
        let mut still: Vec<(i64, Fill)> = Vec::new();
        for (vis, f) in self.deferred_fills.drain(..) {
            if vis <= now_ns {
                due.push(f);
            } else {
                still.push((vis, f));
            }
        }
        self.deferred_fills = still;
        for f in due {
            // The shadow moves with the DELIVERY, not with the match — that is what makes a
            // position-polling strategy (`SpreadMaker`) actually feel the response leg. Advanced
            // BEFORE the callback so `on_fill` reads an inventory that already includes this fill.
            self.core.advance_shadow(&f);
            // The callback may itself submit — which re-enters the (still armed) gate, so the
            // reaction is delayed by a fresh entry latency, exactly like the live path.
            self.strategy.on_fill(&mut self.core, &f);
        }
    }

    /// Deliver EVERY still-held fill regardless of its visibility stamp — the end-of-run flush.
    /// A backtest must not end with fills the strategy was never told about (its `on_stop`
    /// accounting would disagree with `BacktestResult`).
    fn flush_deferred_fills(&mut self) {
        for (_, f) in std::mem::take(&mut self.deferred_fills) {
            self.core.advance_shadow(&f);
            self.strategy.on_fill(&mut self.core, &f);
        }
    }

    /// Apply every in-flight order action whose delivery stamp has arrived at `ts_ms`. No-op
    /// when the gate is not armed.
    fn deliver_due_orders(&mut self, ts_ms: i64) {
        // The drain is scoped so the `&mut core.latency` borrow ends before `apply_in_flight`
        // takes `&mut core` (the queue is drained into an owned Vec).
        let due = {
            let Some(g) = self.core.latency.as_mut() else { return };
            if g.in_flight_len() == 0 {
                return;
            }
            g.drain_due(ts_ms)
        };
        for f in due {
            self.core.apply_in_flight(f.action);
        }
    }

    /// The per-fill volume cap (`volume_limit`): clamp `size` down to `volume_limit * vol`,
    /// recording the dropped remainder under `"volume_cap"`. The ONE copy of the clamp shared
    /// verbatim by every fill-dispatch site — the three `fill_pending*` lanes (via
    /// [`Self::dispatch_fill`]) and both `fill_step_gated` loops. Returns the (possibly clamped) size.
    fn volume_clamp(&mut self, si: usize, size: f64, vol: f64) -> f64 {
        let mut fill_size = size;
        if let Some(vl) = self.core.volume_limit {
            let allowed = vl * vol;
            if fill_size > allowed {
                let dropped = fill_size - allowed;
                let sym = self.core.symbols[si].clone();
                self.core.dropped.push((sym, "volume_cap".into(), dropped, 0.0));
                fill_size = allowed;
            }
        }
        fill_size
    }

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
    /// filled instead of by what it merely INTENDED to fill.
    fn dispatch_fill(&mut self, si: usize, o: &WorkingOrder, fp: f64, vol: f64, ts: i64) -> f64 {
        let pos = self.core.sym[si].pos;
        let opening = pos.size == 0.0 || (pos.size > 0.0) == (o.side > 0);
        if opening && !self.core.is_active_idx(si) {
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
        let fill_size = self.volume_clamp(si, o.size, vol);
        if fill_size > 0.0 {
            // Maker/taker by ORDER KIND, not by crossing aggressiveness: a MARKETABLE limit books
            // as a maker fill. See the classification caveat in `vike_model::fees` — it flips the
            // fee SIGN under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
            let is_maker = o.kind == OrderKind::Limit;
            if let Some(fill) = self.core.apply_fill(si, o.side, fill_size, fp, ts, is_maker) {
                let applied = fill.size;
                if o.stop.is_some() && self.core.sym[si].pos.size != 0.0 {
                    self.core.sym[si].stop = o.stop;
                }
                self.fire_on_fill(fill);
                return applied;
            }
        }
        0.0
    }

    /// Protective-stop breach for ONE symbol against a bar's `open`/`low`/`high`/`ts`: if an
    /// armed stop is crossed (long: `low <= stop`; short: `high >= stop`) close the position at
    /// the shared trigger oracle's price and fire `on_fill`. Trigger-law wave 2 (law-map A3/A4)
    /// unified the three inline copies' semantics:
    ///
    /// - the FILL PRICE is `vike_model::order_fill_price`'s Stop arm — a bar that OPENS through
    ///   the stop fills at the gapped open (ADVERSE), exactly like a resting stop order; a
    ///   non-gapping breach still fills at the stop, byte-identical to the retired at-stop law
    ///   (on the tick lane the event is a one-price bar, so a through-print fills at the print);
    /// - the OCO sibling-cancel runs on EVERY lane (coarse / granular / tick — it was
    ///   granular-only, letting a wide bar fire the stop AND the TP into a spurious reversal).
    ///
    /// No-ops when no stop is armed / flat.
    fn check_stop(&mut self, si: usize, open: f64, low: f64, high: f64, ts: i64) {
        let stop = self.core.sym[si].stop;
        let pos = self.core.sym[si].pos;
        let (Some(stop), true) = (stop, pos.size != 0.0) else {
            return;
        };
        // Session gate (opt-in; no-op by default), checked only once a stop is actually ARMED so
        // the counter measures refused stop passes rather than every quiet event. Unlike the
        // staleness discipline — which deliberately exempts protective stops so a stale price
        // cannot strand live risk — a CLOSED venue cannot fill a stop at all. The stop stays
        // armed and fires on the first in-session event at that event's price, which is the
        // adverse-gap arm documented above. Through [`Self::defer_session`], the ONE writer of
        // `session_deferrals` — the armed-stop guard above already established the "only count a
        // refused pass" precondition, exactly as the tagged lanes' empty-map check does.
        if self.defer_session(si, ts) {
            return;
        }
        let was_long = pos.size > 0.0;
        let side = if was_long { -1 } else { 1 };
        // A throwaway probe order + bar hand the armed stop level to the ONE trigger oracle.
        // The Stop arm reads `open`/`high`/`low` only — `close` is a placeholder, and a bare
        // crossing check carries no volume/quote context — and never mutates the probe.
        let mut probe = WorkingOrder::new(OrderKind::Stop, side, pos.size.abs());
        probe.price = Some(stop);
        let probe_bar = Bar {
            ts,
            open,
            high,
            low,
            close: open,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        };
        let Some(fp) = vike_model::order_fill_price(&mut probe, &probe_bar) else {
            return;
        };
        if let Some(fill) = self.core.apply_fill(si, side, pos.size.abs(), fp, ts, false) {
            self.core.sym[si].stop = None;
            self.cancel_protective_exits(si, was_long);
            self.fire_on_fill(fill);
        }
    }

    /// Protective-stop breach BEFORE this step's fills (skips granular symbols).
    fn check_protective_stops(&mut self, cur_idx: usize, skip: &[bool]) {
        for si in 0..self.core.symbols.len() {
            if skip[si] {
                continue;
            }
            // Read the crossing scalars without cloning the Bar (String-allocating); the armed-stop
            // guard lives in `check_stop`, which no-ops when nothing is armed.
            let (open, low, high, ts) = {
                let bar = &self.core.bars[si][cur_idx];
                (bar.open, bar.low, bar.high, bar.ts)
            };
            self.check_stop(si, open, low, high, ts);
        }
    }

    /// Binary-resolution settlement sweep over ALL symbols at this step's bar timestamps (the
    /// opt-in `EngineParams::resolution` source, pm-economics lane). A no-op — zero calls, zero
    /// allocation — when no source is configured, so the default path is byte-identical. Runs
    /// BEFORE the step's stop/fill phase so a just-resolved symbol can neither fill resting
    /// orders nor keep a position past resolution.
    fn check_resolutions(&mut self, cur_idx: usize) {
        if self.core.resolution.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            let ts = self.core.bars[si][cur_idx].ts;
            self.check_resolution_at(si, ts);
        }
    }

    /// One symbol's IN-LOOP resolution check: probe AND settle at this event's own `ts`
    /// (shared by the bar-step sweep and the tick path).
    #[inline]
    fn check_resolution_at(&mut self, si: usize, ts: i64) {
        self.resolve_at(si, ts, ts);
    }

    /// One symbol's resolution check: ask the source at `probe_ts`, settle at `settle_ts` (the two
    /// differ only in the END-OF-RUN sweep, see [`Self::final_resolution_sweep`]).
    /// When the source reports `Some(payout)`: LATCH the symbol resolved, cancel everything
    /// resting (pending + tagged + protective stop — a resolved market can no longer trade, and
    /// the fill phase runs right after this), then close any open position at the payout via the
    /// fee-free settlement fill ([`SimBroker::settle_at_payout`]); the strategy learns of it
    /// through `on_fill`. Idempotent: once flat with nothing resting, later checks are no-ops.
    ///
    /// The latch comes FIRST, before the clear and before the settlement fires `on_fill`, because
    /// `on_fill` re-entry is the hole it closes: a strategy that re-enters from inside `on_fill`
    /// (re-hedge / re-enter on close) would otherwise push a fresh order into `pending`/`tagged`
    /// AFTER this clear but BEFORE this same step's fill phase, and fill it against the resolution
    /// bar — manufacturing fee-free PnL every time the market settled again. With the latch set,
    /// `SimBroker::push_pending` / `submit_limit_tagged` refuse those orders outright.
    fn resolve_at(&mut self, si: usize, probe_ts: i64, settle_ts: i64) {
        let payout =
            self.core.resolution.as_ref().and_then(|f| f(&self.core.symbols[si], probe_ts));
        let Some(payout) = payout else {
            return;
        };
        self.core.mark_resolved(si);
        self.core.sym[si].pending.clear();
        self.core.sym[si].tagged.clear();
        self.core.sym[si].stop = None;
        if let Some(fill) = self.core.settle_at_payout(si, payout, settle_ts) {
            self.fire_on_fill(fill);
        }
    }

    /// END-OF-RUN resolution sweep — the counterpart to the in-loop checks, run once after the
    /// event loop in BOTH `run` and `run_ticks`. Returns whether anything settled.
    ///
    /// Why it exists: a real prediction-market tape STOPS at the trading halt and the resolution
    /// posts later, so every recorded event carries `ts < res_ts` and the in-loop probes (which
    /// only ever ask at event timestamps) never see the market resolve. Without this sweep the
    /// canonical use case — hold YES to expiry over a recorded tape — silently no-ops: no
    /// settlement, no closed trade in the metrics, and `final_equity` marking the position at the
    /// last traded price (0.97) instead of the payout (1.0).
    ///
    /// So the sweep probes at [`EngineParams::resolution_end_ts`], defaulting to
    /// [`RESOLUTION_PROBE_SENTINEL`] ("did this market EVER resolve?"), while stamping the
    /// settlement at that same timestamp when set and otherwise at the symbol's LAST event ts —
    /// never at the sentinel, which would poison the closed trade's `exit_ts`. A no-op — zero
    /// calls — when no source is configured.
    fn final_resolution_sweep(&mut self, last_ts: &[i64]) -> bool {
        if self.core.resolution.is_none() {
            return false;
        }
        let before = self.core.settlements.len();
        let probe_ts = self.core.resolution_end_ts.unwrap_or(RESOLUTION_PROBE_SENTINEL);
        for si in 0..self.core.symbols.len() {
            let settle_ts = self.core.resolution_end_ts.unwrap_or(last_ts[si]);
            self.resolve_at(si, probe_ts, settle_ts);
        }
        self.core.settlements.len() != before
    }

    /// Option-expiry settlement sweep over ALL symbols at this step's bar timestamps (the opt-in
    /// [`EngineParams::option_specs`] source, options lane). A no-op — zero calls, zero allocation —
    /// when no source is configured, so the default path is byte-identical. Runs BEFORE the step's
    /// stop/fill phase (right after [`Self::check_resolutions`]) so a just-expired option can neither
    /// fill resting orders nor keep a position past expiry.
    fn check_option_expiries(&mut self, cur_idx: usize) {
        if self.core.option_specs.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            let ts = self.core.bars[si][cur_idx].ts;
            self.check_option_expiry_at(si, ts);
        }
    }

    /// One symbol's IN-LOOP option-expiry check: probe AND settle at this event's own `ts` (shared
    /// by the bar-step sweep and the tick path).
    #[inline]
    fn check_option_expiry_at(&mut self, si: usize, ts: i64) {
        self.expire_at(si, ts, ts);
    }

    /// One symbol's option-expiry check: if `symbols[si]` is a configured option whose `expiry_ts`
    /// is at/before `probe_ts` (see [`SimBroker::option_payout`]), settle any open position to CASH
    /// at its intrinsic value, stamped at `settle_ts` (the two differ only in the END-OF-RUN sweep,
    /// see [`Self::final_option_expiry_sweep`]). The mechanism MIRRORS [`Self::resolve_at`] exactly:
    /// LATCH the symbol resolved FIRST (so the settlement's own `on_fill` re-entry is refused), then
    /// cancel everything resting (pending + tagged + protective stop — an expired option can no
    /// longer trade), then close the position at the intrinsic via the fee-free
    /// [`SimBroker::settle_at_payout`]; the strategy learns of it through `on_fill`. Idempotent:
    /// once flat with nothing resting, later checks add no settlement.
    fn expire_at(&mut self, si: usize, probe_ts: i64, settle_ts: i64) {
        let Some(payout) = self.core.option_payout(si, probe_ts) else {
            return;
        };
        self.core.mark_resolved(si);
        self.core.sym[si].pending.clear();
        self.core.sym[si].tagged.clear();
        self.core.sym[si].stop = None;
        if let Some(fill) = self.core.settle_at_payout(si, payout, settle_ts) {
            self.fire_on_fill(fill);
        }
    }

    /// END-OF-RUN option-expiry sweep — the counterpart to the in-loop checks, run once after the
    /// event loop in BOTH `run` and `run_ticks`. Settles any option still open at run end whose
    /// `expiry_ts` is at/before the as-of ([`EngineParams::option_expiry_end_ts`], defaulting to
    /// each symbol's LAST event ts — a safe idempotent no-op that never force-settles a still-live
    /// option; set a FUTURE as-of to settle one expiring just past the tape). Returns whether
    /// anything settled. A no-op — zero calls — when no source is configured.
    fn final_option_expiry_sweep(&mut self, last_ts: &[i64]) -> bool {
        if self.core.option_specs.is_none() {
            return false;
        }
        let before = self.core.settlements.len();
        for si in 0..self.core.symbols.len() {
            let as_of = self.core.option_expiry_end_ts.unwrap_or(last_ts[si]);
            self.expire_at(si, as_of, as_of);
        }
        self.core.settlements.len() != before
    }

    /// Fill resting TAGGED maker quotes ([`HftBroker`]) against `event` (a coarse bar, a granular
    /// sub-bar, or a tick-derived bar) through the SAME crossing-fill model + `apply_fill` cost path
    /// as untagged orders — so tagged fills can never drift from the golden-gated fill math. A
    /// resting bid (buy limit) fills when the market moves at-or-below its price; a resting ask
    /// (sell limit) at-or-above — the `OrderKind::Limit` branch of `order_fill_price` (Bar model) /
    /// `TickFillModel` (L1 quote model). Tagged limits fill as MAKER (`is_maker = true`), and — via
    /// the shared `apply_fill` — snap to the point-in-time instrument grid and are gated below the
    /// PIT min exactly like untagged fills (PR-2a / #165). Filled tags are removed (see
    /// [`SymbolState::tagged`]); the maker learns of the fill via `on_fill`.
    ///
    /// SIMPLE CROSSING MODEL — a defensible FIRST model, NOT full L2 queue-position modeling. Its
    /// documented limits (R8 `L2BookFillModel` queue modeling is the follow-up — see `fill_model.rs`):
    /// - NO queue position: a resting quote fills the instant price touches it, as if always at the
    ///   FRONT of the queue. Real venues fill by price/time priority, so a passive quote can sit
    ///   unfilled while size ahead of it trades. This OVER-fills the maker (optimistic fill rate).
    /// - Full-size fill on the crossing event (no partial fills / no size-ahead depletion).
    /// - Intrabar straddle: if one event's range spans BOTH quotes, BOTH fill this step (in
    ///   insertion order — bid before ask); a single bar can't reveal the true intrabar path.
    /// - modify-in-place is FREE (no queue priority to forfeit), unlike a real re-price.
    ///
    /// Net effect: sim maker fills/PnL are an OPTIMISTIC bound. Treat this as a behavioral / plumbing
    /// check (does the maker rest, fill, skew, and break as designed?), NOT a fill-rate/PnL forecast.
    fn fill_tagged(&mut self, si: usize, event: &Bar) {
        if self.core.sym[si].tagged.is_empty() {
            return;
        }
        // Session gate (opt-in; no-op by default) — the ONE maker-lane gate, shared verbatim with
        // the queued twin (see `defer_tagged_session`). A resting quote cannot be crossed by a
        // market that is shut; it stays in the tag map and is retried on the first in-session
        // event. The tag map is non-empty here (checked above), so this reduces to the plain
        // session check the inline guard used to be.
        if self.defer_tagged_session(si, event.ts) {
            return;
        }
        // Snapshot the tags in insertion order, then apply each crossing fill in turn (a bid fill can
        // turn the ask into a reduce, exactly as two sequential fills would). Iterating the snapshot
        // (not the live map) makes removing the current tag mid-walk safe. Tagged orders are always
        // `OrderKind::Limit`, whose fill model never mutates the order, so a no-fill leaves the
        // original resting in the map untouched (the working clone is simply discarded).
        let tags: Vec<String> = self.core.sym[si].tagged.keys().cloned().collect();
        for tag in tags {
            let Some(mut o) = self.core.sym[si].tagged.get(&tag).cloned() else { continue };
            let Some(fp) = self.core.fill_price_for(si, &mut o, event) else { continue };
            self.core.sym[si].tagged.shift_remove(&tag); // crossed → terminal; retire the tag
                                                         // The SAME `apply_fill` the untagged lanes use, which snaps price/size to the
                                                         // point-in-time instrument grid and gates sub-min OPENING fills (PR-2a / #165) — so the
                                                         // tagged maker lane inherits that grid-snapping for free and can never drift from the
                                                         // untagged math. `None` means the PIT grid gated the fill (below min_qty/min_notional,
                                                         // or a dust size after step-rounding); the tag is already retired (the crossing consumed
                                                         // it, exactly as an untagged triggered-but-gated order is dropped from `pending`), so we
                                                         // skip `fire_on_fill` and let the maker re-quote on its next `on_quote_tick`.
            if let Some(fill) = self.core.apply_fill(si, o.side, o.size, fp, event.ts, true) {
                self.fire_on_fill(fill);
            }
        }
    }

    /// The deferral check the shared fill lanes use: must this order WAIT rather than fill on this
    /// event? Two opt-in disciplines answer here so no lane can consult one and miss the other, and
    /// each counts its own deferral for diagnostics. `false` — and no counter write — whenever both
    /// are off (the byte-identical default).
    ///
    /// - **Session** ([`Self::defer_session`]): this SYMBOL's market is shut, so no kind fills.
    ///   Checked first — a closed market subsumes any question about how fresh its last price was.
    /// - **Staleness** ([`Self::defer_stale_wait`]): the market is open but this symbol's price is
    ///   stale, so market-ish kinds wait for a fresh print.
    ///
    /// A deferred order is not canceled or rejected: it stays resting exactly as it was and is
    /// retried on the next event, so it fills at the next in-session / freshly-printed price.
    ///
    /// Reached through [`Self::gate_pending`], which is what every untagged fill lane calls; the
    /// two halves are ALSO callable on their own, and the queued tick lane's price-conditional
    /// branch (`engine::queued`) does exactly that — it needs the session half (a closed market
    /// fills nothing, queued limits and maker quotes included) WITHOUT the staleness half, which
    /// exempts price-conditional kinds anyway.
    #[inline]
    fn defer_fill(&mut self, si: usize, kind: OrderKind, ts: i64) -> bool {
        // `||` short-circuits, so `defer_session` runs first (and alone when it defers) exactly as
        // the two-`if` form did — byte-identical for every lane that calls this.
        self.defer_session(si, ts) || self.defer_stale_wait(si, kind, ts)
    }

    /// The **session** half of [`Self::defer_fill`] (see [`SimBroker::session_closed`]): defer —
    /// and count one session deferral — when this SYMBOL's market is shut at `ts`. UNIVERSAL: a
    /// closed market can fill no kind, resting limit and maker quote alike, so the queued lanes
    /// consult this for every order rather than takers only.
    ///
    /// It is the engine's ONLY writer of `session_deferrals` and its only caller of
    /// `session_closed`: the untagged lanes reach it through [`Self::gate_pending`], the maker
    /// lanes through [`Self::defer_tagged_session`], and [`Self::check_stop`] calls it directly.
    /// Every one of those sites establishes its own "count a REFUSED pass, not a quiet event"
    /// precondition (a non-empty tag map, an armed stop) before calling.
    #[inline]
    fn defer_session(&mut self, si: usize, ts: i64) -> bool {
        if self.core.session_closed(si, ts) {
            self.core.session_deferrals += 1;
            return true;
        }
        false
    }

    /// The **staleness** half of [`Self::defer_fill`] (see [`crate::staleness`]): defer — and count
    /// one stale deferral — when the market is open but this symbol's last price is stale, so a
    /// market-ish kind waits for a fresh print. Price-conditional kinds are exempt (a repeated
    /// stale price cannot spuriously satisfy a condition), which is why the queued lane runs this
    /// on its taker branch only.
    #[inline]
    fn defer_stale_wait(&mut self, si: usize, kind: OrderKind, ts: i64) -> bool {
        if self.core.defer_stale(si, kind, ts) {
            self.core.stale_deferrals += 1;
            return true;
        }
        false
    }

    /// The **tagged-lane** session gate, one function for both maker lanes ([`Self::fill_tagged`]
    /// and its queued twin, whose comment used to read "the queued twin of the guard in
    /// `fill_tagged`"): defer — and count one session deferral — when this symbol's market is shut
    /// at `ts` AND tags actually rest, so the counter measures refused MAKER passes only and never
    /// a quiet symbol with nothing quoted. `&&` short-circuits, so an empty tag map never reaches
    /// [`Self::defer_session`] and therefore never writes the counter.
    ///
    /// A deferred pass leaves the tag map exactly as it was: a resting quote a shut market cannot
    /// cross stays quoted and is retried on the first in-session event.
    #[inline]
    fn defer_tagged_session(&mut self, si: usize, ts: i64) -> bool {
        !self.core.sym[si].tagged.is_empty() && self.defer_session(si, ts)
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
    #[inline]
    fn gate_pending(&mut self, si: usize, o: &mut WorkingOrder, event: &Bar) -> OrderGate {
        // Wait disciplines (opt-in, both off by default): a market order against fill-forwarded
        // data — or any order on a shut market — keeps resting instead of filling.
        if self.defer_fill(si, o.kind, event.ts) {
            return OrderGate::Rest;
        }
        // opt-in emulator-mirroring stop release (no-op by default): a triggered stop converts to
        // a resting market child and fills at the NEXT event's price.
        if self.core.stop_released(si, o, event) {
            return OrderGate::Rest;
        }
        match self.core.fill_price_for(si, o, event) {
            None => OrderGate::Rest,
            Some(fp) => OrderGate::Trigger(fp),
        }
    }

    /// Coarse per-symbol fill phase (`_fill_pending`).
    fn fill_pending(&mut self, si: usize, cur_idx: usize) {
        // Most bars have no pending orders. Python `_fill_pending` is a no-op on empty pending
        // (empty loop, reassign empty list), so returning early is behavior-identical — and it
        // skips the per-symbol Bar clone that otherwise runs every single bar. The tagged (HFT
        // maker) lane is checked in the SAME breath so the fast path stays byte-identical for every
        // non-HFT strategy (tagged is always empty there → `has_tagged` false → same early return).
        let has_tagged = !self.core.sym[si].tagged.is_empty();
        if self.core.sym[si].pending.is_empty() && !has_tagged {
            return;
        }
        let series = Rc::clone(&self.core.bars[si]); // refcount bump, not a Bar/String clone
        let bar = &series[cur_idx];
        if has_tagged {
            self.fill_tagged(si, bar);
        }
        if self.core.sym[si].pending.is_empty() {
            return;
        }
        let mut triggered: Vec<(WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<WorkingOrder> = Vec::new();
        let pending = std::mem::take(&mut self.core.sym[si].pending);
        for mut o in pending {
            // THE gate sequence, shared with every other lane (see `gate_pending`). A deferred /
            // stop-released / untriggered order keeps resting at this bar's own ts.
            match self.gate_pending(si, &mut o, bar) {
                OrderGate::Rest => still.push(o),
                OrderGate::Trigger(fp) => triggered.push((o, fp)),
            }
        }
        self.core.sym[si].pending = still;
        let resolved = if triggered.len() > 1 {
            let (r, both) = resolve_intrabar_fills(triggered, self.core.sym[si].pos.size);
            self.core.intrabar_both_hit += both;
            r
        } else {
            triggered
        };
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            self.dispatch_fill(si, &o, fp, bar.volume, bar.ts);
        }
    }

    /// OCO sibling-cancel after a protective-stop fill (`_cancel_protective_exits`): the resting
    /// closing-side exits are the fired stop's kind-linked OCO siblings — the shared
    /// `vike_exec::is_protective_exit_sibling` law (ONE resolver with the paper book's
    /// coid-linked `ContingencyBook` half). Runs from `check_stop` on EVERY lane since
    /// trigger-law wave 2 (it was granular-only).
    fn cancel_protective_exits(&mut self, si: usize, was_long: bool) {
        let closing_side = if was_long { -1 } else { 1 };
        self.core.sym[si]
            .pending
            .retain(|o| !vike_exec::is_protective_exit_sibling(o.kind, o.side, closing_side));
    }

    /// Granular sub-bar fill phase (`_fill_pending_granular`).
    fn fill_pending_granular(&mut self, si: usize, i: usize) {
        let sub_bars: Vec<Bar> = self.core.sym[si].sub[i].clone();
        for sub in &sub_bars {
            // 0) Wait discipline (opt-in): a sub-bar is a FIRST-CLASS print, not merely intrabar
            // detail of the step's coarse verdict. Granular data is the finest tier available on
            // this lane, so its own volume/quote evidence is the best print evidence there is —
            // and the age below is measured against `sub.ts`, so recording only the coarse ts
            // would make every later sub-bar of a fully-fresh step spuriously stale. No-op unless
            // configured (see `crate::staleness`).
            self.core.note_print(si, sub);
            // 1) protective stop first within this sub-bar (breach cancels the OCO sibling exit)
            self.check_stop(si, sub.open, sub.low, sub.high, sub.ts);
            // 2) resting/market orders that trigger on THIS sub-bar, in pending order.
            // NOTE: no dust guard here (unlike the coarse/tick lanes) — preserved from the original.
            let pending = std::mem::take(&mut self.core.sym[si].pending);
            let mut still: Vec<WorkingOrder> = Vec::new();
            for mut o in pending {
                // THE gate sequence (see `gate_pending`), run at `sub.ts`: the staleness age is
                // measured against the last fresh print, which the `note_print` above may have
                // just set to THIS sub-bar (age 0). A fully-printing granular step therefore
                // never defers; a quiet stretch WITHIN a step does, which is the whole point of
                // having the finer tier. The stop-release arm likewise fills the NEXT sub-bar.
                match self.gate_pending(si, &mut o, sub) {
                    OrderGate::Rest => still.push(o),
                    OrderGate::Trigger(fp) => {
                        self.dispatch_fill(si, &o, fp, sub.volume, sub.ts);
                    }
                }
            }
            self.core.sym[si].pending = still;
        }
    }

    /// Shared-cash gated fill phase (`_fill_step_gated`): reductions first, opens by
    /// weight desc (ties: trigger order), unfundable dropped.
    fn fill_step_gated(&mut self, cur_idx: usize) {
        let mut opens: Vec<(usize, WorkingOrder, f64, usize)> = Vec::new();
        let mut frees: Vec<(usize, WorkingOrder, f64, usize)> = Vec::new();
        let mut seq = 0usize;
        for si in 0..self.core.symbols.len() {
            let bar = self.core.bars[si][cur_idx].clone();
            let pending = std::mem::take(&mut self.core.sym[si].pending);
            let mut still: Vec<WorkingOrder> = Vec::new();
            for mut o in pending {
                // THE gate sequence (see `gate_pending`), run BEFORE the cash gate — an order the
                // disciplines deferred, or the emulator released as a stop child, was never
                // triggered, so it must not consume an open slot or cash budget this step.
                let OrderGate::Trigger(fp) = self.gate_pending(si, &mut o, &bar) else {
                    still.push(o);
                    continue;
                };
                let pos = self.core.sym[si].pos;
                let increasing = pos.size == 0.0 || (pos.size > 0.0) == (o.side > 0);
                if increasing && !self.core.is_active_idx(si) {
                    continue; // inactive member: dropped before the cash gate
                }
                if increasing {
                    opens.push((si, o, fp, seq));
                } else {
                    frees.push((si, o, fp, seq));
                }
                seq += 1;
            }
            self.core.sym[si].pending = still;
        }
        // reductions/closes first — free cash, never gated
        for (si, o, fp, _) in frees {
            let bar_ts = self.core.bars[si][cur_idx].ts;
            let bar_vol = self.core.bars[si][cur_idx].volume;
            let fill_size = self.volume_clamp(si, o.size, bar_vol);
            if fill_size > 0.0 {
                let is_maker = o.kind == OrderKind::Limit;
                if let Some(fill) =
                    self.core.apply_fill(si, o.side, fill_size, fp, bar_ts, is_maker)
                {
                    self.fire_on_fill(fill);
                }
            }
        }
        // opens/adds: highest weight first, ties by trigger order (Python sorted = stable)
        opens.sort_by(|a, b| {
            b.1.weight
                .partial_cmp(&a.1.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.3.cmp(&b.3))
        });
        for (si, o, fp, _) in opens {
            let sym = self.core.symbols[si].clone();
            if self.core.sym[si].pos.size == 0.0 && self.core.at_open_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            if self.core.sym[si].pos.size == 0.0 && o.side > 0 && self.core.at_long_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            if self.core.sym[si].pos.size == 0.0 && o.side < 0 && self.core.at_short_cap() {
                self.core.dropped.push((sym, kind_str(o.kind).into(), o.size, o.weight));
                continue;
            }
            let bar_ts = self.core.bars[si][cur_idx].ts;
            let bar_vol = self.core.bars[si][cur_idx].volume;
            let fill_size = self.volume_clamp(si, o.size, bar_vol);
            if fill_size <= 0.0 {
                continue;
            }
            // Maker/taker classification is hoisted ABOVE the cash pre-check because
            // `slippage_for` needs it: the impact addend is taker-only, so the gate must ask the
            // same question `apply_fill` will. Same rule as `dispatch_fill` — by ORDER KIND, not
            // by crossing aggressiveness.
            let is_maker = o.kind == OrderKind::Limit;
            // Pre-check only (cash gate + fee estimate). Uses `slippage_for` so it charges the
            // SAME price `apply_fill` is about to charge — otherwise an impact-priced fill could
            // pass a gate computed on the flat slippage and overdraw. Byte-identical without an
            // impact model (see `SimBroker::slippage_for`).
            let slipped = adverse_fill_price(
                fp,
                o.side,
                self.core.slippage_for(si, fill_size, bar_ts, is_maker),
            );
            let rate = if is_maker { self.core.maker_fee } else { self.core.taker_fee };
            let mult = self.core.multiplier_of_idx(si);
            let fee = fee_fn(fill_size, slipped, rate, mult);
            let cash_impact = -(o.side as f64 * fill_size) * slipped * mult - fee;
            if self.core.cash + cash_impact < 0.0 {
                self.core.dropped.push((sym, kind_str(o.kind).into(), fill_size, o.weight));
                continue;
            }
            if let Some(fill) = self.core.apply_fill(si, o.side, fill_size, fp, bar_ts, is_maker) {
                if o.stop.is_some() && self.core.sym[si].pos.size != 0.0 {
                    self.core.sym[si].stop = o.stop;
                }
                self.fire_on_fill(fill);
            }
        }
    }

    /// WL removal-day exit: force-close positions now out of membership, at the bar's OPEN.
    fn close_inactive(&mut self, cur_idx: usize) {
        if self.core.active_mask.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            if self.core.sym[si].pos.size != 0.0 && !self.core.is_active_idx(si) {
                let pos_size = self.core.sym[si].pos.size;
                let side = vike_model::closing_side(pos_size);
                let bar = self.core.bars[si][cur_idx].clone();
                if let Some(fill) =
                    self.core.apply_fill(si, side, pos_size.abs(), bar.open, bar.ts, false)
                {
                    self.fire_on_fill(fill);
                }
            }
        }
    }

    /// Account-level margin call at the bar's intrabar ADVERSE marks — the ONE
    /// scope-parameterized liquidation law (`vike_model::liquidation`), shared with the live
    /// watchdog (`vike_exec::check_margin_call`). The backtest's positions have no per-position
    /// margin mode, so the whole account IS the cross pool.
    ///
    /// DEFAULT path (`venue_style_liquidation == false`): the LEAN cross-pool workflow —
    /// `pool_breached` (the `pool_equity ≤ maintenance` law PLUS the `liq_buffer` grace arm,
    /// default 0.10 = the live watchdog's buffer) then `cross_liquidation_plan`: biggest losers
    /// first, close only the EXCESS over the line, stop as soon as healthy. **This is a real
    /// behavior change for margined backtests** (they previously wiped the whole account at the
    /// bare trigger); the old model is preserved bit-for-bit behind the opt-in stress knob.
    ///
    /// STRESS KNOB (`venue_style_liquidation == true`): the pre-law behavior, verbatim —
    /// trigger `eq_adv ≤ maint_margin · notional_adv` and force-close ALL positions at their
    /// adverse marks.
    fn check_liquidation(&mut self, cur_idx: usize) {
        if self.core.maint_margin <= 0.0 {
            return;
        }
        let held: Vec<usize> =
            (0..self.core.symbols.len()).filter(|&si| self.core.sym[si].pos.size != 0.0).collect();
        if held.is_empty() {
            return;
        }
        let mut eq_adv = self.core.cash;
        let mut notional_adv = 0.0;
        for &si in &held {
            let pos = self.core.sym[si].pos;
            let bar = &self.core.bars[si][cur_idx];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            eq_adv +=
                vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
            notional_adv +=
                vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        }
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — total wipe at trigger
            if eq_adv <= self.core.maint_margin * notional_adv {
                for &si in &held {
                    let pos = self.core.sym[si].pos;
                    let bar = self.core.bars[si][cur_idx].clone();
                    let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
                    let side = vike_model::closing_side(pos.size);
                    if let Some(fill) =
                        self.core.apply_fill(si, side, pos.size.abs(), adverse, bar.ts, false)
                    {
                        self.fire_on_fill(fill);
                    }
                }
            }
            return;
        }
        // shared law: maintenance folded per row (|size|·adverse·mult·rate — the same fold
        // shape as `Account::margin_in_use`), then the LEAN losers-first excess-only plan
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiqCandidate> = Vec::with_capacity(held.len());
        for &si in &held {
            let pos = self.core.sym[si].pos;
            let bar = &self.core.bars[si][cur_idx];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let mult = self.core.multiplier_of_idx(si);
            maint_adv += pos.size.abs() * adverse * mult * self.core.maint_margin;
            candidates.push(vike_model::LiqCandidate {
                id: si,
                size: pos.size,
                mark: adverse,
                mult,
                upnl: pos.size * (adverse - pos.avg_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liq_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (si, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let pos = self.core.sym[si].pos;
            let bar = self.core.bars[si][cur_idx].clone();
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let side = vike_model::closing_side(pos.size);
            if let Some(fill) = self.core.apply_fill(si, side, qty, adverse, bar.ts, false) {
                self.fire_on_fill(fill);
            }
        }
    }

    pub fn run(&mut self) -> BacktestResult {
        let n_sym = self.core.symbols.len();
        let mut equity_curve: Vec<f64> = Vec::with_capacity(self.core.n);
        let mut equity_ts: Vec<i64> = Vec::with_capacity(self.core.n);
        let mut per_symbol_curve: Vec<Vec<f64>> = vec![Vec::with_capacity(self.core.n); n_sym];
        // Reused across bars: the per-symbol "has sub-bars this step" flags. Refilled in place each
        // bar (clear + extend keeps the capacity), so there's no per-bar Vec malloc/free.
        let mut granular: Vec<bool> = Vec::with_capacity(n_sym);
        self.strategy.on_start(&mut self.core);
        for i in 0..self.core.n {
            self.core.step = i;
            self.core.now = if n_sym > 0 { self.core.bars[0][i].ts } else { 0 };
            // symbols with sub-bars THIS step (non-gated path only)
            granular.clear();
            granular.extend(
                (0..n_sym).map(|si| !self.core.cash_gate && !self.core.sym[si].sub[i].is_empty()),
            );
            // opt-in binary-resolution settlement, BEFORE stops/fills (no-op when unconfigured)
            self.check_resolutions(i);
            // opt-in option-expiry settlement, likewise BEFORE stops/fills (no-op when unconfigured)
            self.check_option_expiries(i);
            // opt-in stale-price freshness bookkeeping, BEFORE the fill phase so a bar that IS a
            // fresh print has age 0 and fills on the spot (see `crate::staleness`). Guarded so
            // the default path is a single branch per step, not per symbol.
            if self.core.max_price_staleness_ms.is_some() {
                for si in 0..n_sym {
                    // scalars only — no Bar clone, and no borrow of core held across the write
                    let (ts, fresh) = {
                        let b = &self.core.bars[si][i];
                        (b.ts, crate::staleness::is_fresh_print(b))
                    };
                    self.core.note_print_at(si, ts, fresh);
                }
            }
            self.check_protective_stops(i, &granular);
            if self.core.cash_gate {
                self.fill_step_gated(i);
            } else {
                for si in 0..n_sym {
                    if granular[si] {
                        self.fill_pending_granular(si, i);
                    } else {
                        self.fill_pending(si, i);
                    }
                }
            }
            self.core.index = i;
            for si in 0..n_sym {
                // ABLATION: read Copy scalars instead of cloning the Bar (no String alloc)
                let (bclose, bfunding, bhigh, blow, bts) = {
                    let b = &self.core.bars[si][i];
                    (b.close, b.funding, b.high, b.low, b.ts)
                };
                self.core.sym[si].price = bclose;
                // Funding rate for this step: a recorded `Bar.funding` is the on-the-bar truth and
                // WINS; otherwise the opt-in `funding_source` seam (venue funding history) is
                // consulted — `None` (no source / no venue / no funding event at this ts) leaves
                // the path byte-identical to the pre-seam engine. See `EngineParams::funding_source`.
                let funding_rate = match bfunding {
                    Some(f) => Some(f),
                    None => self.core.funding_rate_for(si, bts),
                };
                if let Some(funding) = funding_rate {
                    if self.core.sym[si].pos.size != 0.0 {
                        let mult = self.core.multiplier_of_idx(si);
                        let fc = funding_charge(self.core.sym[si].pos.size, bclose, funding, mult);
                        self.core.cash -= fc;
                        // Observability twin of `Account.funding_paid`: net funding cashflow =
                        // Σ(-fc) = Σ MirrorFunding.amount (R4-gated). Does NOT touch cash/equity.
                        self.core.funding_paid -= fc;
                        if fc != 0.0 {
                            if let Some(sink) = &mut self.core.mirror_funding {
                                sink.push(MirrorFunding {
                                    symbol: self.core.symbols[si].clone(),
                                    amount: -fc,
                                    ts: bts,
                                });
                            }
                        }
                    }
                }
                // MAE/MFE extremes for open positions (this bar's high/low; no look-ahead)
                if self.core.sym[si].pos.size != 0.0 {
                    self.core.sym[si].hi_since = self.core.sym[si].hi_since.max(bhigh);
                    self.core.sym[si].lo_since = self.core.sym[si].lo_since.min(blow);
                }
            }
            // Opt-in variation-margin settlement (no-op when unconfigured). Placed AFTER the
            // price/funding loop above — the mark must be this step's close and the funding charge
            // for the step must already have been taken — and BEFORE close_inactive/liquidation, so
            // a forced close in this same step exits against the freshly settled cost basis.
            self.core.settle_variation_margin(self.core.now);
            self.close_inactive(i);
            self.check_liquidation(i);
            let ts = if n_sym > 0 { self.core.bars[0][i].ts } else { 0 };
            if i >= self.strategy.warmup() {
                // Strategy._on_step: fan the bundle out per symbol (symbols order).
                // Rc::clone is a refcount bump (no Bar/String copy); `series` is a local owning
                // handle, so &series[i] doesn't borrow core -> &mut core coexists, and bars_for/
                // forming_for still read the live self.core.bars.
                for si in 0..n_sym {
                    let series = Rc::clone(&self.core.bars[si]);
                    self.strategy.on_bar(&mut self.core, &series[i]);
                }
                let due = self.core.schedule.check_due(ts, i);
                for tag in due {
                    self.strategy.on_schedule(&mut self.core, &tag);
                }
            }
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            equity_curve.push(eq);
            equity_ts.push(ts);
            for si in 0..n_sym {
                let st = &self.core.sym[si];
                per_symbol_curve[si].push(
                    st.realized
                        + st.pos.size
                            * (st.price - st.pos.avg_price)
                            * self.core.multiplier_of_idx(si),
                );
            }
        }
        // END-OF-RUN binary-resolution sweep (no-op when unconfigured): the tape may stop before
        // the market resolves, which is the normal prediction-market shape. Settle at each
        // symbol's LAST bar ts; when anything settled, re-point the curves' final sample at the
        // post-settlement equity so `equity_curve.last()` still agrees with `final_equity` (the
        // sample COUNT never changes — no extra point, no ts shift).
        if self.core.resolution.is_some() {
            let last_ts: Vec<i64> = (0..n_sym)
                .map(|si| if self.core.n > 0 { self.core.bars[si][self.core.n - 1].ts } else { 0 })
                .collect();
            if self.final_resolution_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_sym {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        let st = &self.core.sym[si];
                        *last = st.realized
                            + st.pos.size
                                * (st.price - st.pos.avg_price)
                                * self.core.multiplier_of_idx(si);
                    }
                }
            }
        }
        // END-OF-RUN option-expiry sweep (no-op when unconfigured): the tape may stop before an
        // option's expiry_ts (see `final_option_expiry_sweep`). Same curve re-point as the binary
        // sweep above so `equity_curve.last()` stays equal to `final_equity` (sample COUNT
        // unchanged) whenever something settled.
        if self.core.option_specs.is_some() {
            let last_ts: Vec<i64> = (0..n_sym)
                .map(|si| if self.core.n > 0 { self.core.bars[si][self.core.n - 1].ts } else { 0 })
                .collect();
            if self.final_option_expiry_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
                for si in 0..n_sym {
                    if let Some(last) = per_symbol_curve[si].last_mut() {
                        let st = &self.core.sym[si];
                        *last = st.realized
                            + st.pos.size
                                * (st.price - st.pos.avg_price)
                                * self.core.multiplier_of_idx(si);
                    }
                }
            }
        }
        self.strategy.on_stop(&mut self.core);
        let per_symbol_pnl: Vec<(String, f64)> = (0..n_sym)
            .map(|si| {
                let st = &self.core.sym[si];
                (
                    self.core.symbols[si].clone(),
                    st.realized
                        + st.pos.size
                            * (st.price - st.pos.avg_price)
                            * self.core.multiplier_of_idx(si),
                )
            })
            .collect();
        BacktestResult {
            n_trades: self.core.trades.len(),
            trades: self.core.trades.clone(),
            equity_curve,
            final_equity: self.core.equity_now(),
            per_symbol_pnl,
            per_symbol_curves: self.core.symbols.iter().cloned().zip(per_symbol_curve).collect(),
            equity_ts,
            intrabar_both_hit: self.core.intrabar_both_hit,
            stale_deferrals: self.core.stale_deferrals,
            session_deferrals: self.core.session_deferrals,
            // Already-accumulated diagnostics mirrored out for the zero-trade analyzer (a clone
            // after the fold loop, not new hot-path counting — see BacktestResult's field docs).
            dropped: self.core.dropped.clone(),
            below_min_reversals: self.core.below_min_reversals,
            warmup: self.strategy.warmup(),
            funding_paid: self.core.funding_paid,
        }
    }

    /// Per-tick multi-symbol run: k-way merge by ts (ties: stream order — heapq.merge
    /// semantics), route each tick to its symbol; on_bar does NOT fire.
    pub fn run_ticks(&mut self, ticks_by_symbol: &[(String, Vec<Tick>)]) -> BacktestResult {
        let n_sym = self.core.symbols.len();
        let mut equity_curve: Vec<f64> = Vec::new();
        let mut equity_ts: Vec<i64> = Vec::new();
        // ARM the opt-in order-latency gate for the duration of this replay ONLY (see
        // `StrategyEngine::latency`). `None` (default) installs nothing at all, so every line
        // below stays on the frozen zero-latency path.
        //
        // The gate is armed WITH the per-symbol venue-hold table (`SimBroker::taker_hold_table_ns`),
        // resolved once at the run start from the PIT properties seam. That table is EMPTY unless a
        // properties source is configured AND some symbol declares a hold, and an empty table makes
        // the gate compute exactly the delivery stamp it computed before the table existed.
        let armed_gate = self.latency.as_ref().map(|kind| {
            let run_start = ticks_by_symbol
                .iter()
                .filter_map(|(_, t)| t.first())
                .map(|t| match t {
                    Tick::Quote(q) => q.ts,
                    Tick::Trade(tr) => tr.ts,
                    Tick::Book(b) => b.ts,
                })
                .min()
                .unwrap_or(0);
            crate::latency::LatencyGate::with_holds(kind, self.core.taker_hold_table_ns(run_start))
        });
        self.core.latency = armed_gate;
        // ...and with it the strategy-visible SHADOW position (the response leg's other half; see
        // `SimBroker::shadow_pos`), seeded from the true positions so a pre-seeded book starts
        // agreeing with the exchange. Stays EMPTY — and every read stays the frozen line — when
        // no latency model is configured.
        self.core.shadow_pos = if self.core.latency.is_some() {
            (0..n_sym).map(|si| self.core.sym[si].pos.size).collect()
        } else {
            Vec::new()
        };
        self.strategy.on_start(&mut self.core);

        // stable k-way merge by (ts, stream index)
        let streams: Vec<&Vec<Tick>> =
            ticks_by_symbol.iter().filter(|(_, t)| !t.is_empty()).map(|(_, t)| t).collect();
        let mut cursors = vec![0usize; streams.len()];
        let tick_ts = |t: &Tick| match t {
            Tick::Quote(q) => q.ts,
            Tick::Trade(tr) => tr.ts,
            Tick::Book(b) => b.ts,
        };
        // Tick-lane equity-curve density, copied out of `self` once so the fold below reads a
        // `Copy` local instead of re-borrowing `self` between the `&mut self` engine calls.
        // `EveryTick` (the default) takes the frozen push-every-tick arm.
        let equity_sampling = self.equity_sampling;
        // The run's LAST PRICED tick ts, tracked ONLY under a thinning `EveryN` so the subsample
        // can be closed with a final sample after the loop (see there). Stays `None` — and the
        // block that reads it is dead — on the default and `Off` paths.
        let mut last_priced_ts: Option<i64> = None;
        // Replay book state per symbol: None until the first Snapshot anchors it, dropped on
        // any integrity break (seq discontinuity or a recorded §B gap/stale marker) until the
        // next Snapshot — the gap-sentinel rule, replayed.
        //
        // Held on the BROKER (rather than as a local, as it was before the L2 fill tier) so the
        // `FillModelKind::L2Book` price law and the `Broker::quote_vwap`/`depth_within_price`
        // strategy reads see the same book this loop maintains. Sized here and cleared at the end
        // of the replay, exactly like `shadow_pos`, so no other engine path ever carries one.
        self.core.books = vec![None; n_sym];
        // Last event ts seen per symbol — the settlement stamp for the end-of-run resolution sweep
        // when no explicit `resolution_end_ts` is given. `0` means "this symbol saw no ticks".
        let mut last_ts: Vec<i64> = vec![0; n_sym];
        let mut i = 0usize;
        loop {
            let mut best: Option<(i64, usize)> = None;
            for (k, s) in streams.iter().enumerate() {
                if cursors[k] < s.len() {
                    let ts = tick_ts(&s[cursors[k]]);
                    if best.is_none() || ts < best.unwrap().0 {
                        best = Some((ts, k));
                    }
                }
            }
            let Some((_, k)) = best else { break };
            // BORROWED, not cloned. `streams[k]` is a `&Vec<Tick>` reborrowed out of the
            // `ticks_by_symbol` PARAMETER, so `tick`'s lifetime comes from the caller's slice and
            // is independent of `self` — which is what lets it stay live across the `&mut self`
            // engine calls below. The clone this replaces cost one `String` allocation per
            // quote/trade and three (symbol + both `Vec<Level>`) per book event, on every tick of
            // the tape, purely to hand the fold a value it only ever reads.
            let stream: &[Tick] = streams[k];
            let tick: &Tick = &stream[cursors[k]];
            cursors[k] += 1;

            let sym = tick_symbol(tick);
            let Some(si) = self.core.symbols.iter().position(|s| s.as_str() == sym) else {
                i += 1;
                continue; // unknown symbol — skipped (Python logs a warning)
            };
            if let Tick::Book(b) = tick {
                self.core.now = b.ts;
                last_ts[si] = b.ts;
                // Latency gate (opt-in, no-ops when unarmed): book events advance the clock, so
                // an in-flight order/cancel whose delivery stamp has arrived lands HERE too —
                // otherwise a book-only stretch of tape would freeze the exchange's view.
                self.deliver_due_orders(b.ts);
                self.deliver_due_fills(b.ts);
                // book events advance time too: settle a just-resolved symbol before delivering
                // the book (no-op when no resolution source is configured). Runs BEFORE the
                // queue-depth read so a resolution-driven cancel is already reflected in the
                // tracked set the `pre`/`apply` pair folds over.
                self.check_resolution_at(si, b.ts);
                self.check_option_expiry_at(si, b.ts);
                // Queue-model depth hook (opt-in; `pre` is None when no tracker exists): read
                // the tracked levels' resting qty BEFORE the event mutates the book, apply,
                // then fold the old→new change into every tracked queue state.
                let pre =
                    self.queue.as_ref().map(|t| t.pre_depth(si, self.core.books[si].as_deref()));
                let applied = apply_book_event(&mut self.core.books[si], b);
                if let Some(pre) = pre {
                    if let Some(tracker) = self.queue.as_mut() {
                        tracker.apply_depth(si, &pre, self.core.books[si].as_deref());
                    }
                }
                if applied {
                    // An `Rc` clone (O(1)) rather than a borrow, so the book stays IN the broker
                    // for the duration of the callback: a strategy handling `on_order_book` can
                    // call `quote_vwap`/`depth_within_price` on its own symbol and get the book it
                    // was just handed, instead of a hole where the book used to be.
                    if let Some(book) = self.core.books[si].clone() {
                        // book events don't advance the warmup index `i` (they carry no price
                        // semantics); deliver once a valid book exists.
                        self.strategy.on_order_book(&mut self.core, &book);
                    }
                }
                continue; // no fill/equity/i bump — quotes/trades own price semantics (v1)
            }
            // Queue-model seed fallback (opt-in): remember the freshest L1 quote so a bookless
            // replay can still seed a new resting order's front from the matching quote size.
            if let (Some(tracker), Tick::Quote(q)) = (self.queue.as_mut(), tick) {
                tracker.note_quote(si, q);
            }
            let event = match tick {
                Tick::Quote(q) => vike_model::quote_tick_to_bar(q),
                Tick::Trade(t) => vike_model::trade_tick_to_bar(t),
                // `Tick::Book` is intercepted above and `continue`d — it never reaches the
                // price path. Rust match exhaustiveness is a static check (blind to the
                // `continue`), so this arm is required; it fabricates no price semantics.
                Tick::Book(_) => unreachable!("book ticks are intercepted before the price path"),
            };
            self.core.now = event.ts;
            last_ts[si] = event.ts;
            // Latency gate (opt-in, no-ops when unarmed) — BEFORE this tick's fill phase, so an
            // order submitted `entry()` ago is resting in time to be matched, and a cancel
            // submitted too recently is NOT yet applied and therefore cannot save its order.
            self.deliver_due_orders(event.ts);
            // ...and the response leg: fills whose visibility stamp has arrived reach the
            // strategy now, late, exactly as an ack does live.
            self.deliver_due_fills(event.ts);
            // opt-in binary-resolution settlement BEFORE this tick's stop/fill phase (no-op
            // when unconfigured): cancels the symbol's resting orders and closes any open
            // position at the payout, so nothing fills past resolution.
            self.check_resolution_at(si, event.ts);
            // opt-in option-expiry settlement, likewise BEFORE the fill phase (no-op unconfigured)
            self.check_option_expiry_at(si, event.ts);
            // Freshness bookkeeping before the fill phase (no-op unless configured). A
            // `Tick::Trade` is a TRANSACTION and therefore a print regardless of its reported
            // size — some venues emit zero-size/index prints, which `trade_tick_to_bar` projects
            // to `volume == 0.0` with no quote. Deciding freshness from the tick KIND (not only
            // from the projected bar) is what makes the tick lane inert BY CONSTRUCTION rather
            // than by convention of the projection.
            let fresh_print =
                matches!(tick, Tick::Trade(_)) || crate::staleness::is_fresh_print(&event);
            self.core.note_print_at(si, event.ts, fresh_print);
            // `Rc` clone (O(1)) so the fill pass can take `&mut self` while still seeing the
            // book — the same reason the `on_order_book` delivery above clones.
            let book_now = self.core.books.get(si).and_then(Clone::clone);
            self.fill_pending_tick(si, &event, tick, book_now.as_deref());
            self.core.sym[si].price = event.close;
            // Opt-in variation-margin settlement (no-op when unconfigured), the tick path's twin of
            // `run`'s. The cadence bucket is one global clock, so the FIRST tick to cross a boundary
            // settles EVERY open symbol at its own last-seen price — the other symbols need no tick
            // of their own to be marked. Placed after this tick's price update and before the
            // liquidation check, mirroring the bar path's ordering.
            self.core.settle_variation_margin(event.ts);
            self.check_liquidation_tick(si, &event);
            // TWO DELIBERATE equity samples, NOT a redundant recompute (a perf audit flagged the
            // second call; it must stay). The peak is stamped with the PRE-dispatch equity because
            // the strategy callback below can read `drawdown_now()`, which divides by
            // `equity_peak` — the peak has to already include this tick's mark. The curve is
            // stamped with the POST-dispatch equity. The two agree for every strategy that only
            // uses the documented `Broker` verbs (they touch `pending` alone; `apply_fill` and the
            // settlement sweeps are `pub(crate)` and unreachable from a callback), but `SimBroker`
            // exposes `cash` and `sym` as PUBLIC fields, so a strategy CAN move equity in-callback
            // and the two samples are then genuinely different values. Collapsing them would
            // silently change which one lands where. (The bar lane `run` samples once, AFTER its
            // dispatch, and is a different — also deliberate — ordering.)
            let eq = self.core.equity_now();
            self.core.equity_peak = self.core.equity_peak.max(eq);
            if i >= self.strategy.warmup() {
                self.core.index = i;
                match tick {
                    Tick::Quote(q) => self.strategy.on_quote_tick(&mut self.core, q),
                    Tick::Trade(t) => self.strategy.on_trade_tick(&mut self.core, t),
                    Tick::Book(_) => unreachable!("book ticks are intercepted before dispatch"),
                }
            }
            // Equity-curve record (see `EquitySampling`). `EveryTick` — the default — is the frozen
            // unconditional push; the other arms exist only because two `Vec`s growing 16 bytes per
            // tick is 1.6 GB on a 100M-tick tape, paid by every point of a sweep.
            match equity_sampling {
                // `EveryN(0 | 1)` names the same sampling as `EveryTick` (documented on the
                // variant), so it joins the frozen arm rather than dividing by a degenerate
                // stride — which also leaves `n >= 2` guaranteed below.
                EquitySampling::EveryTick | EquitySampling::EveryN(0 | 1) => {
                    equity_curve.push(self.core.equity_now());
                    equity_ts.push(event.ts);
                }
                EquitySampling::EveryN(n) => {
                    if i.is_multiple_of(n) {
                        equity_curve.push(self.core.equity_now());
                        equity_ts.push(event.ts);
                    }
                    last_priced_ts = Some(event.ts);
                }
                EquitySampling::Off => {}
            }
            i += 1;
        }
        // Close an `EquitySampling::EveryN` subsample with the run's LAST priced tick when the
        // stride did not already land on it. Nothing has moved equity since that tick (the merge
        // loop just ended), so `equity_now()` here IS that tick's equity. This keeps the two
        // invariants the settlement sweeps below rely on: `equity_curve.last()` agrees with
        // `final_equity`, and `last_mut()` patches the FINAL row rather than a stale mid-run one.
        // `last_priced_ts` is `None` on the default (`EveryTick`) and `Off` paths, so this whole
        // block is dead there — byte-identical.
        if let Some(ts) = last_priced_ts {
            if equity_ts.last() != Some(&ts) {
                equity_curve.push(self.core.equity_now());
                equity_ts.push(ts);
            }
        }
        // END-OF-RUN binary-resolution sweep — the tick path's twin of `run`'s (see
        // `final_resolution_sweep`): a recorded tape normally stops at the trading halt, before
        // the resolution posts. No-op when unconfigured.
        if self.core.resolution.is_some() {
            // a symbol that saw no ticks at all settles at the run's last known ts
            let now = self.core.now;
            for t in last_ts.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_resolution_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
            }
        }
        // END-OF-RUN option-expiry sweep — the tick path's twin of `run`'s (see
        // `final_option_expiry_sweep`). No-op when unconfigured. The `last_ts` 0→now fixup is
        // repeated (idempotent) so this arm is correct even when no `resolution` source ran it.
        if self.core.option_specs.is_some() {
            let now = self.core.now;
            for t in last_ts.iter_mut() {
                if *t == 0 {
                    *t = now;
                }
            }
            if self.final_option_expiry_sweep(&last_ts) {
                if let Some(last) = equity_curve.last_mut() {
                    *last = self.core.equity_now();
                }
            }
        }
        // Response-leg flush: every still-held fill reaches the strategy before `on_stop`, so a
        // strategy's own accounting cannot end the run disagreeing with `BacktestResult`.
        // In-flight ORDER actions are deliberately NOT flushed — an action still in flight when
        // the tape ends never reached the venue, so it must not retro-actively apply.
        self.flush_deferred_fills();
        // DISARM: the gate exists only for the duration of a tick replay (armed at the top of
        // `run_ticks`), which is what keeps the bar path from ever seeing one. The shadow goes
        // with it — every fill has now been delivered, so from `on_stop` onward the strategy reads
        // exchange truth again and cannot end the run disagreeing with `BacktestResult`.
        self.core.latency = None;
        self.core.shadow_pos = Vec::new();
        // The replay books go with them: they are tick-replay state, so no later read (`on_stop`,
        // a caller inspecting the broker, a subsequent bar `run`) can be answered off a stale book.
        self.core.books = Vec::new();
        self.strategy.on_stop(&mut self.core);
        let per_symbol_pnl: Vec<(String, f64)> = (0..n_sym)
            .map(|si| {
                let st = &self.core.sym[si];
                (
                    self.core.symbols[si].clone(),
                    st.realized
                        + st.pos.size
                            * (st.price - st.pos.avg_price)
                            * self.core.multiplier_of_idx(si),
                )
            })
            .collect();
        BacktestResult {
            n_trades: self.core.trades.len(),
            trades: self.core.trades.clone(),
            equity_curve,
            final_equity: self.core.equity_now(),
            per_symbol_pnl,
            per_symbol_curves: Vec::new(),
            equity_ts,
            intrabar_both_hit: self.core.intrabar_both_hit,
            stale_deferrals: self.core.stale_deferrals,
            session_deferrals: self.core.session_deferrals,
            // Already-accumulated diagnostics mirrored out for the zero-trade analyzer (a clone
            // after the fold loop, not new hot-path counting — see BacktestResult's field docs).
            dropped: self.core.dropped.clone(),
            below_min_reversals: self.core.below_min_reversals,
            warmup: self.strategy.warmup(),
            funding_paid: self.core.funding_paid,
        }
    }

    /// Per-tick fill for one symbol (`_fill_pending_tick`). `tick` is the RAW replayed tick
    /// (`event` is its Bar-shaped projection) and `book` the symbol's current replay book —
    /// both consulted ONLY by the opt-in queue-model branch; the `None` (default) path below
    /// is the frozen pre-queue code, byte-identical.
    fn fill_pending_tick(&mut self, si: usize, event: &Bar, tick: &Tick, book: Option<&L2Book>) {
        // protective stop on this tick (a breach OCO-cancels the resting exit siblings, same
        // law as every other lane — trigger-law wave 2)
        self.check_stop(si, event.open, event.low, event.high, event.ts);
        if self.queue.is_some() {
            self.fill_pending_tick_queued(si, event, tick, book);
            return;
        }
        // resting/market orders
        let pending = std::mem::take(&mut self.core.sym[si].pending);
        let mut triggered: Vec<(WorkingOrder, f64)> = Vec::new();
        let mut still: Vec<WorkingOrder> = Vec::new();
        for mut o in pending {
            // THE gate sequence (see `gate_pending`). Its staleness half is INERT on this lane by
            // construction — `event` is the symbol's own just-arrived quote/trade tick, hence a
            // fresh print with age 0 (see `crate::staleness`) — and is wired anyway so the policy
            // has ONE definition, not two.
            match self.gate_pending(si, &mut o, event) {
                OrderGate::Rest => still.push(o),
                OrderGate::Trigger(fp) => triggered.push((o, fp)),
            }
        }
        self.core.sym[si].pending = still;
        let resolved = if triggered.len() > 1 {
            let (r, both) = resolve_intrabar_fills(triggered, self.core.sym[si].pos.size);
            self.core.intrabar_both_hit += both;
            r
        } else {
            triggered
        };
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            self.dispatch_fill(si, &o, fp, event.volume, event.ts);
        }
        // The HFT maker lane: cross this tick against the resting tagged quotes (the maker is driven
        // by on_quote_tick / on_order_book, which only fire on this tick path). A no-op unless an
        // `HftBroker` strategy is mounted, so every existing tick-replay test is unaffected.
        self.fill_tagged(si, event);
    }

    /// Tick-path margin call (`_check_liquidation_tick` twin): the triggering symbol is judged
    /// at the tick's ADVERSE print, every other held symbol at its last seen price. Same
    /// law/knob split as [`Self::check_liquidation`]:
    ///
    /// - DEFAULT: the shared cross-pool law — `pool_breached` (+ `liq_buffer`) then the
    ///   losers-first `cross_liquidation_plan` over ALL held positions (the pool is shared, so
    ///   the plan may partially close positions other than the triggering symbol, each at the
    ///   mark it was judged at). Previously a breach here closed ONLY the triggering symbol,
    ///   in full — a per-symbol wipe the one law replaces.
    /// - STRESS KNOB (`venue_style_liquidation`): the pre-law behavior verbatim — bare
    ///   trigger, close only the triggering symbol, full size.
    fn check_liquidation_tick(&mut self, si: usize, event: &Bar) {
        if self.core.maint_margin <= 0.0 {
            return;
        }
        let pos = self.core.sym[si].pos;
        if pos.size == 0.0 {
            return;
        }
        let adverse = if pos.size > 0.0 { event.low } else { event.high };
        let mut eq_adv = self.core.cash
            + vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        let mut notional_adv =
            vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        for other in 0..self.core.symbols.len() {
            if other == si {
                continue;
            }
            let p = self.core.sym[other].pos;
            if p.size != 0.0 {
                eq_adv += vike_model::signed_notional(
                    p.size,
                    self.core.sym[other].price,
                    self.core.multiplier_of_idx(other),
                );
                notional_adv += vike_model::gross_notional(
                    p.size,
                    self.core.sym[other].price,
                    self.core.multiplier_of_idx(other),
                );
            }
        }
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — close ONLY `si`, whole
            if notional_adv > 0.0 && eq_adv <= self.core.maint_margin * notional_adv {
                let side = vike_model::closing_side(pos.size);
                if let Some(fill) =
                    self.core.apply_fill(si, side, pos.size.abs(), adverse, event.ts, false)
                {
                    self.fire_on_fill(fill);
                }
            }
            return;
        }
        // shared law over the whole pool at the judged marks (si: adverse; others: last price)
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiqCandidate> = Vec::new();
        for idx in 0..self.core.symbols.len() {
            let p = self.core.sym[idx].pos;
            if p.size == 0.0 {
                continue;
            }
            let mark = if idx == si { adverse } else { self.core.sym[idx].price };
            let mult = self.core.multiplier_of_idx(idx);
            maint_adv += p.size.abs() * mark * mult * self.core.maint_margin;
            candidates.push(vike_model::LiqCandidate {
                id: idx,
                size: p.size,
                mark,
                mult,
                upnl: p.size * (mark - p.avg_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liq_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (idx, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let p = self.core.sym[idx].pos;
            let mark = if idx == si { adverse } else { self.core.sym[idx].price };
            let side = vike_model::closing_side(p.size);
            if let Some(fill) = self.core.apply_fill(idx, side, qty, mark, event.ts, false) {
                self.fire_on_fill(fill);
            }
        }
    }
}
