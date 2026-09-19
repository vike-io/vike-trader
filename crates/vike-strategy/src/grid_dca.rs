//! Two portable reference strategies — a two-sided GRID maker ([`Grid`]) and a directional
//! DCA-accumulation strategy ([`DcaAccumulate`]) — written as pure `impl<B: Broker> Strategy<B>`
//! (the `buy_hold` shape), so the SAME type runs in the backtest sweep harness AND live unchanged.
//!
//! NET-NEW Rust surface — NO Python twin, so NOT parity-gated: these are behavioral reference
//! strategies (does the ladder rest, fill, re-arm, and stop as designed?), not f64-oracle ports.
//!
//! ## Portability constraint: no cancel verb
//!
//! The portable [`Broker`] surface is the common denominator of the backtest and live stacks:
//! `submit_market` / `submit_limit` + the account/market reads. It deliberately has NO `cancel`
//! (only the concrete `SimBroker` / live broker expose `cancel_all`). Both strategies are therefore
//! designed to need none:
//!
//! - **[`Grid`]** rests a band of buy limits BELOW and sell limits ABOVE an `anchor`, `rungs` per
//!   side, spaced by `step`, `size` units each (a two-sided mean-reversion grid — the sell side is
//!   an opening short when flat). A rung is an oscillator: when its ENTRY limit fills the strategy
//!   places a take-profit limit one `step` back toward the anchor; when THAT fills (a completed
//!   round-trip) it RE-ARMS the entry. Inventory is bounded by `rungs · size` per side. A HARD STOP
//!   fires when price leaves `[anchor − band, anchor + band]`: the strategy flattens the net
//!   position and HALTS (no re-arms, no new legs). On a 0..1-bounded market (Polymarket) set
//!   `bounded01` — the band clamps to `[tick, 1 − tick]` and any rung whose price would fall on/past
//!   a wall is skipped.
//! - **[`DcaAccumulate`]** ladders `rungs` scale-in entries away from the anchor against the trade
//!   direction (long ⇒ buys below; short ⇒ sells above), tracks the volume-weighted average entry,
//!   and closes the WHOLE position with a SINGLE take-profit at `avg · (1 ± tp)`. After the exit it
//!   re-anchors and re-ladders for the next cycle.
//!
//! CAVEAT (the price of no-cancel): a resting limit placed before a halt / TP cannot be pulled.
//! [`Grid`] re-flattens every bar while halted so a stray fill from a residual limit is closed on
//! the next event (inventory stays bounded); [`DcaAccumulate`]'s residual entries from a prior
//! cycle stay resting and may re-fill if price revisits those levels. Both are documented,
//! bounded-per-cycle behaviors, acceptable for a reference strategy and honest about the seam.
//!
//! Both read their knobs from the harness TOML params table via `from_params` (the `BuyHold`
//! reader convention — unknown keys ignored, missing keys fall back to defaults), so a sweep grids
//! over `step` / `band` / `size` / `rungs` with no per-strategy sweep wiring.
//!
//! ## The EMPTY ladder is a load-time answer, not a market outcome
//!
//! Both `arm`s build their whole order set from the params plus ONE anchor, once, and neither ever
//! rebuilds it from nothing — so a params table whose rung set is empty for every anchor it permits
//! describes a mount that can never place an order, on any price path. [`Grid::arms_no_rung`] and
//! [`DcaAccumulate::arms_no_rung`] answer that question from the same code that rests the rungs,
//! and `vike_strategy::unarmable_params` is what a consumer asks. That is deliberately NOT the
//! question "will this strategy trade" — a strategy waiting on a market condition is correct, and
//! nothing at load can tell it from a dead one. This is the narrower, decidable question.

use toml::Value;

use vike_model::{Bar, Broker, Fill, QuoteTick, Strategy};

/// Read a TOML value as `f64`, accepting a TOML float OR integer (`size = 1` == `size = 1.0`) —
/// the same lenient numeric reader the registry's `buy_hold`/`cheap_np` params use.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// How the grid / ladder center (`anchor`) is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorMode {
    /// Center on the FIRST observed price (bar close / quote mid). The default.
    FirstPrice,
    /// Center on the explicit `anchor_price` param (a fixed reference level).
    Fixed,
}

/// One grid rung's state: its ENTRY limit is resting (`Armed`), or the entry filled and its
/// take-profit limit is resting (`Holding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegState {
    Armed,
    Holding,
}

/// One computed grid rung: the entry price/side and its take-profit price (one `step` back toward
/// the anchor). `tp_side` is always `-entry_side`.
#[derive(Clone, Copy, Debug)]
struct GridLeg {
    /// +1 for a buy-side rung (below the anchor), −1 for a sell-side rung (above).
    entry_side: i32,
    entry_price: f64,
    tp_price: f64,
    state: LegState,
}

/// A two-sided GRID maker. See the module doc for the contract; params via [`Grid::from_params`].
#[derive(Debug, Clone)]
pub struct Grid {
    // --- params (read once from the profile) ---
    /// How the `anchor` is chosen (`anchor` = `"first"` | `"fixed"`).
    pub anchor_mode: AnchorMode,
    /// The fixed anchor used when `anchor_mode == Fixed`.
    pub anchor_price: f64,
    /// Price gap between adjacent rungs.
    pub step: f64,
    /// Rung count PER SIDE (buys below + sells above).
    pub rungs: usize,
    /// Order size per rung.
    pub size: f64,
    /// Hard-stop half-width around the anchor: price outside `[anchor ± band]` flattens + halts.
    pub band: f64,
    /// 0..1-bounded market (Polymarket): clamp the band to `[tick, 1 − tick]` and skip walled rungs.
    pub bounded01: bool,
    /// The 0..1 market tick (only consulted when `bounded01`).
    pub tick: f64,
    /// Explicit symbol override; `None` uses whatever symbol the first bar/tick carries.
    pub symbol: Option<String>,

    // --- runtime state ---
    anchor: Option<f64>,
    resolved_symbol: String,
    legs: Vec<GridLeg>,
    lo: f64,
    hi: f64,
    halted: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Grid {
            anchor_mode: AnchorMode::FirstPrice,
            anchor_price: 0.0,
            step: 1.0,
            rungs: 3,
            size: 1.0,
            band: 10.0,
            bounded01: false,
            tick: 0.001,
            symbol: None,
            anchor: None,
            resolved_symbol: String::new(),
            legs: Vec::new(),
            lo: 0.0,
            hi: 0.0,
            halted: false,
        }
    }
}

impl Grid {
    /// Read the registry params table (the `BuyHold::from_params` convention — a READER, not a
    /// schema: unknown keys are ignored, missing keys fall back to the defaults).
    ///
    /// | key | meaning | default |
    /// |---|---|---|
    /// | `anchor` | `"first"` \| `"fixed"` | `"first"` |
    /// | `anchor_price` | fixed anchor (when `anchor = "fixed"`) | `0.0` |
    /// | `step` | price gap between rungs | `1.0` |
    /// | `rungs` | rung count per side | `3` |
    /// | `size` | order size per rung | `1.0` |
    /// | `band` | hard-stop half-width around the anchor | `10.0` |
    /// | `bounded01` | 0..1-bounded market (clamp band, skip walled rungs) | `false` |
    /// | `tick` | 0..1 market tick (when `bounded01`) | `0.001` |
    /// | `symbol` | explicit symbol override | first bar/tick symbol |
    pub fn from_params(params: &Value) -> Self {
        let f = |k: &str| params.get(k).and_then(as_f64);
        let d = Grid::default();
        let anchor_mode = match params.get("anchor").and_then(Value::as_str) {
            Some(m) if m.eq_ignore_ascii_case("fixed") => AnchorMode::Fixed,
            _ => AnchorMode::FirstPrice,
        };
        Grid {
            anchor_mode,
            anchor_price: f("anchor_price").unwrap_or(d.anchor_price),
            step: f("step").unwrap_or(d.step),
            rungs: read_rungs(params).unwrap_or(d.rungs),
            size: f("size").unwrap_or(d.size),
            band: f("band").unwrap_or(d.band),
            bounded01: params.get("bounded01").and_then(Value::as_bool).unwrap_or(d.bounded01),
            tick: f("tick").unwrap_or(d.tick),
            symbol: params.get("symbol").and_then(Value::as_str).map(str::to_string),
            ..d
        }
    }

    /// Whether the hard stop has fired (price left the band). Once halted the grid only flattens.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// The resolved anchor, or `None` before the first bar/tick sets it.
    pub fn anchor(&self) -> Option<f64> {
        self.anchor
    }

    /// Whether the ladder leg at `idx` is ARMED — its entry order is resting and unfilled.
    ///
    /// A NARROW observable, deliberately not `pub` state: the round-trip RE-ARM gate (a filled rung
    /// whose take-profit closes must return to `Armed`, resting exactly one fresh entry) needs the
    /// real `StrategyEngine`, which sits above this crate, so it runs as `vike-backtest`'s
    /// `tests/grid_dca_engine.rs`. This is the single field that gate has to see — `GridLeg` and
    /// `LegState` stay private so the ladder's internal state machine remains an implementation
    /// detail. Returns `false` for an out-of-range `idx` (a grid that has not armed yet has no
    /// legs).
    pub fn leg_is_armed(&self, idx: usize) -> bool {
        self.legs.get(idx).is_some_and(|l| l.state == LegState::Armed)
    }

    /// One clamp: a bounded-market price must sit strictly inside `(tick, 1 − tick)`.
    fn on_grid(&self, price: f64) -> bool {
        !self.bounded01 || (price > self.tick && price < 1.0 - self.tick)
    }

    /// The anchor these params resolve to at `price` — the mode select plus the bounded-market
    /// clamp.
    ///
    /// Lifted out of [`Grid::arm`] so [`Grid::arms_no_rung`] can ask the SAME code where the ladder
    /// would centre. A second copy of the clamp is a second thing to keep in step, and a predicate
    /// about a ladder is only worth having if the ladder itself computes it.
    fn anchor_at(&self, price: f64) -> f64 {
        let raw_anchor = match self.anchor_mode {
            AnchorMode::FirstPrice => price,
            AnchorMode::Fixed => self.anchor_price,
        };
        if self.bounded01 { raw_anchor.clamp(self.tick, 1.0 - self.tick) } else { raw_anchor }
    }

    /// The rungs this ladder rests around `anchor` — the WHOLE of `arm`'s rung construction, in
    /// submission order, so nothing can compute a rung set that differs from the one submitted.
    fn legs_at(&self, anchor: f64) -> Vec<GridLeg> {
        let mut legs = Vec::new();
        if self.rungs == 0 || self.size <= 0.0 || self.step <= 0.0 {
            return legs; // inert config: no rungs to rest
        }
        for k in 1..=self.rungs {
            let off = k as f64 * self.step;
            for entry_side in [1i32, -1i32] {
                // +1 (buy) rests BELOW the anchor, −1 (sell) ABOVE.
                let entry_price = anchor - entry_side as f64 * off;
                if !self.on_grid(entry_price) {
                    continue; // walled on a 0..1 market — skip this rung
                }
                // take-profit one step back toward the anchor (buy: +step above; sell: −step below).
                let tp_price = entry_price + entry_side as f64 * self.step;
                legs.push(GridLeg { entry_side, entry_price, tp_price, state: LegState::Armed });
            }
        }
        legs
    }

    /// Whether these params rest NO rung — for EVERY anchor they permit, and therefore on every
    /// market. A `true` here is a mount that can never place an order.
    ///
    /// It is decidable at LOAD because `arm` runs exactly once (`drive` calls it only while
    /// `self.anchor.is_none()`, and nothing ever puts the anchor back) and its rung set is a pure
    /// function of the params plus that one anchor. So an empty ladder is not a state the grid
    /// trades out of once the market moves: the market never gets a say. That is what separates
    /// this question from "will this strategy place an order", which is about the price path and is
    /// not a load-time question at all.
    ///
    /// The anchors quantified over, and why they are the whole set:
    ///
    /// - [`AnchorMode::Fixed`] — the params NAME the anchor, so there is exactly one.
    /// - [`AnchorMode::FirstPrice`] on a `bounded01` market — the anchor is clamped into
    ///   `[tick, 1 − tick]`, and admission is MONOTONE in it per side: a buy rung prices at
    ///   `a − k·step`, so it survives at the TOP wall if it survives at any anchor at all, and a
    ///   sell rung at `a + k·step`, so it survives at the BOTTOM. The two walls therefore bracket
    ///   every anchor between them — `the_two_walls_bracket_every_bounded_anchor` sweeps the
    ///   interior rather than trusting the argument.
    /// - [`AnchorMode::FirstPrice`] off one — `on_grid` is unconditionally true, so admission does
    ///   not depend on the anchor and one probe is exact.
    pub fn arms_no_rung(&self) -> bool {
        if self.bounded01 && 1.0 - self.tick <= self.tick {
            // The walls have met: `(tick, 1 − tick)` is EMPTY, so `on_grid` admits no price at all
            // and no anchor can help. Answered here because `anchor_at`'s `clamp` would panic on
            // that inverted range — a refusal reaches the operator, a panicking worker thread does
            // not.
            return true;
        }
        let first_prices: [f64; 2] = match (self.anchor_mode, self.bounded01) {
            // The one case where the anchor is BOTH unknown at load and able to change the answer.
            (AnchorMode::FirstPrice, true) => [self.tick, 1.0 - self.tick],
            // `Fixed` ignores the price outright; an unbounded `FirstPrice` ladder admits every
            // rung at every anchor. Either way the second probe is the first one again.
            _ => [0.0, 0.0],
        };
        first_prices.iter().all(|p| self.legs_at(self.anchor_at(*p)).is_empty())
    }

    /// Set the anchor + band and rest every rung's entry limit ONCE.
    fn arm<B: Broker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
        let anchor = self.anchor_at(price);
        self.anchor = Some(anchor);
        self.resolved_symbol = symbol.to_string();
        let (mut lo, mut hi) = (anchor - self.band, anchor + self.band);
        if self.bounded01 {
            lo = lo.max(self.tick);
            hi = hi.min(1.0 - self.tick);
        }
        self.lo = lo;
        self.hi = hi;
        for leg in self.legs_at(anchor) {
            self.legs.push(leg);
            broker.submit_limit(symbol, leg.entry_side, self.size, leg.entry_price);
        }
    }

    /// Close the net position at market (the hard-stop flatten / stray-fill mop-up while halted).
    fn flatten<B: Broker>(&mut self, broker: &mut B, symbol: &str) {
        let pos = broker.position(symbol);
        if pos != 0.0 {
            let side = vike_model::closing_side(pos);
            broker.submit_market(symbol, side, pos.abs());
        }
    }

    /// Per-bar / per-tick driver: arm on the first event, then enforce the hard-stop band.
    fn drive<B: Broker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
        if symbol.is_empty() || !price.is_finite() {
            return;
        }
        if self.anchor.is_none() {
            self.arm(broker, symbol, price);
            return;
        }
        if self.halted {
            self.flatten(broker, symbol); // mop up any stray fill from a residual resting limit
            return;
        }
        if !(self.lo..=self.hi).contains(&price) {
            self.halted = true;
            self.flatten(broker, symbol);
        }
    }

    /// Advance the matching rung on a fill: an ENTRY fill places the take-profit (→ `Holding`); a
    /// TAKE-PROFIT fill re-arms the entry (→ `Armed`). While halted, nothing is (re-)placed.
    fn handle_fill<B: Broker>(&mut self, broker: &mut B, side: i32, price: f64) {
        if self.halted {
            return;
        }
        // Match the fill to the resting leg whose expected (side, price) is CLOSEST — grid rung
        // prices are step-separated and (side, price) is unique, so the nearest is unambiguous.
        let mut best: Option<(usize, bool, f64)> = None; // (leg, is_entry, distance)
        for (i, leg) in self.legs.iter().enumerate() {
            let (want_side, want_price, is_entry) = match leg.state {
                LegState::Armed => (leg.entry_side, leg.entry_price, true),
                LegState::Holding => (-leg.entry_side, leg.tp_price, false),
            };
            if want_side != side {
                continue;
            }
            let dist = (want_price - price).abs();
            let better = match best {
                None => true,
                Some((_, _, d)) => dist < d,
            };
            if better {
                best = Some((i, is_entry, dist));
            }
        }
        let Some((i, is_entry, _)) = best else {
            return;
        };
        let leg = self.legs[i];
        let sym = self.resolved_symbol.clone();
        if is_entry {
            self.legs[i].state = LegState::Holding;
            broker.submit_limit(&sym, -leg.entry_side, self.size, leg.tp_price);
        } else {
            self.legs[i].state = LegState::Armed;
            broker.submit_limit(&sym, leg.entry_side, self.size, leg.entry_price);
        }
    }
}

impl<B: Broker> Strategy<B> for Grid {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = self.symbol.clone().or_else(|| bar.symbol.clone()).unwrap_or_default();
        self.drive(broker, &symbol, bar.close);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let symbol = self.symbol.clone().unwrap_or_else(|| q.symbol.clone());
        self.drive(broker, &symbol, q.mid());
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        self.handle_fill(broker, fill.side, fill.price);
    }
}

/// A directional DCA-accumulation strategy. See the module doc; params via
/// [`DcaAccumulate::from_params`].
#[derive(Debug, Clone)]
pub struct DcaAccumulate {
    // --- params ---
    /// Trade direction: +1 long (accumulate on dips), −1 short (accumulate on rallies).
    pub side: i32,
    /// How the ladder anchor is chosen (`anchor` = `"first"` | `"fixed"`).
    pub anchor_mode: AnchorMode,
    /// The fixed anchor used when `anchor_mode == Fixed`.
    pub anchor_price: f64,
    /// Price gap between adjacent scale-in entries.
    pub step: f64,
    /// Number of scale-in ladder entries.
    pub rungs: usize,
    /// Order size per ladder entry.
    pub size: f64,
    /// Take-profit distance as a FRACTION of the average entry (`0.05` = +5% long / −5% short).
    pub tp: f64,
    /// Explicit symbol override; `None` uses whatever symbol the first bar/tick carries.
    pub symbol: Option<String>,

    // --- runtime state ---
    anchor: Option<f64>,
    filled_size: f64,
    avg_entry: f64,
    closing: bool,
}

impl Default for DcaAccumulate {
    fn default() -> Self {
        DcaAccumulate {
            side: 1,
            anchor_mode: AnchorMode::FirstPrice,
            anchor_price: 0.0,
            step: 1.0,
            rungs: 3,
            size: 1.0,
            tp: 0.05,
            symbol: None,
            anchor: None,
            filled_size: 0.0,
            avg_entry: 0.0,
            closing: false,
        }
    }
}

impl DcaAccumulate {
    /// Read the registry params table (same READER convention as [`Grid::from_params`]).
    ///
    /// | key | meaning | default |
    /// |---|---|---|
    /// | `side` | `"long"`/`+n` \| `"short"`/`"sell"`/`-n` | `long` |
    /// | `anchor` | `"first"` \| `"fixed"` | `"first"` |
    /// | `anchor_price` | fixed anchor (when `anchor = "fixed"`) | `0.0` |
    /// | `step` | price gap between scale-in entries | `1.0` |
    /// | `rungs` | number of scale-in entries | `3` |
    /// | `size` | order size per entry | `1.0` |
    /// | `tp` | take-profit fraction of average entry | `0.05` |
    /// | `symbol` | explicit symbol override | first bar/tick symbol |
    pub fn from_params(params: &Value) -> Self {
        let f = |k: &str| params.get(k).and_then(as_f64);
        let d = DcaAccumulate::default();
        let anchor_mode = match params.get("anchor").and_then(Value::as_str) {
            Some(m) if m.eq_ignore_ascii_case("fixed") => AnchorMode::Fixed,
            _ => AnchorMode::FirstPrice,
        };
        DcaAccumulate {
            side: read_side(params),
            anchor_mode,
            anchor_price: f("anchor_price").unwrap_or(d.anchor_price),
            step: f("step").unwrap_or(d.step),
            rungs: read_rungs(params).unwrap_or(d.rungs),
            size: f("size").unwrap_or(d.size),
            tp: f("tp").unwrap_or(d.tp),
            symbol: params.get("symbol").and_then(Value::as_str).map(str::to_string),
            ..d
        }
    }

    /// The accumulated (unsigned) position size — 0 before any entry fills.
    pub fn filled_size(&self) -> f64 {
        self.filled_size
    }

    /// The volume-weighted average entry price of the accumulated position (0 when flat).
    pub fn avg_entry(&self) -> f64 {
        self.avg_entry
    }

    /// The ladder anchor for the current cycle (`None` when flat / re-anchoring next cycle).
    pub fn anchor(&self) -> Option<f64> {
        self.anchor
    }

    /// The scale-in entries this ladder rests around `anchor` — the WHOLE of `arm`'s rung
    /// construction, in submission order, so nothing can compute a ladder that differs from the one
    /// submitted.
    fn entries_at(&self, anchor: f64) -> Vec<f64> {
        let mut entries = Vec::new();
        if self.rungs == 0 || self.size <= 0.0 || self.step <= 0.0 {
            return entries; // inert config: no ladder to rest
        }
        for k in 1..=self.rungs {
            // long ⇒ buys BELOW the anchor; short ⇒ sells ABOVE.
            let entry_price = anchor - self.side as f64 * (k as f64 * self.step);
            if entry_price <= 0.0 {
                continue; // stepped past zero — nothing sane to rest
            }
            entries.push(entry_price);
        }
        entries
    }

    /// Whether these params rest NO scale-in entry — for every anchor they permit, and therefore on
    /// every market. The [`Grid::arms_no_rung`] twin; its doc carries the argument for why this is a
    /// load-time question at all.
    ///
    /// `arm` runs once per CYCLE here rather than once for good, but the conclusion is the same: a
    /// new cycle begins only when a take-profit fill clears the anchor (`handle_fill`'s
    /// opposite-side arm), which cannot happen without an entry fill, which cannot happen without a
    /// rested entry. An empty ladder is terminal.
    ///
    /// The anchors quantified over:
    ///
    /// - [`AnchorMode::Fixed`] — the params NAME the anchor, so there is exactly one.
    /// - [`AnchorMode::FirstPrice`] — the anchor is whatever the feed prints first, so the honest
    ///   question is whether ANY price rests something. The deepest rung sits `rungs · step` from
    ///   the anchor, so an anchor one step beyond that admits every rung of a LONG ladder, and a
    ///   short ladder steps away from zero and admits every rung at any non-negative anchor. A
    ///   first-price ladder is therefore refused only when it rests nothing at any price at all —
    ///   i.e. when the degenerate `rungs`/`size`/`step` guard already returned empty.
    pub fn arms_no_rung(&self) -> bool {
        let anchor = match self.anchor_mode {
            AnchorMode::Fixed => self.anchor_price,
            AnchorMode::FirstPrice => (self.rungs as f64 + 1.0) * self.step,
        };
        self.entries_at(anchor).is_empty()
    }

    /// Rest the scale-in ladder ONCE from the anchor, stepping AGAINST the trade direction.
    fn arm<B: Broker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
        let anchor = match self.anchor_mode {
            AnchorMode::FirstPrice => price,
            AnchorMode::Fixed => self.anchor_price,
        };
        self.anchor = Some(anchor);
        for entry_price in self.entries_at(anchor) {
            broker.submit_limit(symbol, self.side, self.size, entry_price);
        }
    }

    /// Per-bar / per-tick driver: arm on the first event, then check the single aggregate take-profit.
    fn drive<B: Broker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
        if symbol.is_empty() || !price.is_finite() {
            return;
        }
        if self.anchor.is_none() {
            self.arm(broker, symbol, price);
            return;
        }
        if self.filled_size > 0.0 && !self.closing {
            let target = self.avg_entry * (1.0 + self.side as f64 * self.tp);
            let hit = if self.side > 0 { price >= target } else { price <= target };
            if hit {
                broker.submit_market(symbol, -self.side, self.filled_size);
                self.closing = true;
            }
        }
    }

    /// Fold a fill: an ENTRY fill updates the volume-weighted average; the opposite-side TAKE-PROFIT
    /// fill resets state and drops the anchor so the next bar/tick re-ladders for a fresh cycle.
    fn handle_fill(&mut self, side: i32, size: f64, price: f64) {
        if side == self.side {
            let new_size = self.filled_size + size;
            if new_size > 0.0 {
                self.avg_entry = (self.avg_entry * self.filled_size + price * size) / new_size;
            }
            self.filled_size = new_size;
        } else {
            self.filled_size = 0.0;
            self.avg_entry = 0.0;
            self.closing = false;
            self.anchor = None; // re-arm a fresh ladder on the next event
        }
    }
}

impl<B: Broker> Strategy<B> for DcaAccumulate {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = self.symbol.clone().or_else(|| bar.symbol.clone()).unwrap_or_default();
        self.drive(broker, &symbol, bar.close);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let symbol = self.symbol.clone().unwrap_or_else(|| q.symbol.clone());
        self.drive(broker, &symbol, q.mid());
    }

    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        self.handle_fill(fill.side, fill.size, fill.price);
    }
}

/// Read the `rungs` param as a non-negative count (a negative integer clamps to `0`), or `None`
/// when absent/non-integer.
fn read_rungs(params: &Value) -> Option<usize> {
    params.get("rungs").and_then(Value::as_integer).map(|i| i.max(0) as usize)
}

/// Read the `side` param: a `"short"`/`"sell"` string OR a negative integer ⇒ −1, everything else
/// (including absent) ⇒ +1 (long).
fn read_side(params: &Value) -> i32 {
    if let Some(s) = params.get("side").and_then(Value::as_str)
        && (s.eq_ignore_ascii_case("short") || s.eq_ignore_ascii_case("sell"))
    {
        return -1;
    }
    if let Some(i) = params.get("side").and_then(Value::as_integer)
        && i < 0
    {
        return -1;
    }
    1
}

// The ten LADDER-BEHAVIOUR tests (arming, round-trip re-arm, band exit, the bounded-01 wall skip,
// the quote-tick path, and the DCA scale-in / average / take-profit ladder) fold these strategies
// through the REAL `StrategyEngine`/`SimBroker`, which live ABOVE this crate — so they run as
// `vike-backtest`'s `tests/grid_dca_engine.rs` instead of here. What stays below is the pure
// surface: the two `from_params` readers.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_from_params_reads_its_knobs() {
        let toml = r#"
anchor = "fixed"
anchor_price = 0.5
step = 0.02
rungs = 4
size = 3.0
band = 0.15
bounded01 = true
tick = 0.01
symbol = "YES"
"#;
        let p: Value = toml::from_str(toml).unwrap();
        let g = Grid::from_params(&p);
        assert_eq!(g.anchor_mode, AnchorMode::Fixed);
        assert_eq!(g.anchor_price, 0.5);
        assert_eq!(g.step, 0.02);
        assert_eq!(g.rungs, 4);
        assert_eq!(g.size, 3.0);
        assert_eq!(g.band, 0.15);
        assert!(g.bounded01);
        assert_eq!(g.tick, 0.01);
        assert_eq!(g.symbol.as_deref(), Some("YES"));
    }

    #[test]
    fn grid_from_params_defaults_on_empty_table() {
        let g = Grid::from_params(&Value::Table(Default::default()));
        assert_eq!(g.anchor_mode, AnchorMode::FirstPrice);
        assert_eq!(g.rungs, 3);
        assert_eq!(g.symbol, None);
    }

    #[test]
    fn dca_from_params_reads_side_and_knobs() {
        let toml = r#"
side = "short"
step = 2.0
rungs = 5
size = 4.0
tp = 0.08
"#;
        let p: Value = toml::from_str(toml).unwrap();
        let d = DcaAccumulate::from_params(&p);
        assert_eq!(d.side, -1);
        assert_eq!(d.step, 2.0);
        assert_eq!(d.rungs, 5);
        assert_eq!(d.size, 4.0);
        assert_eq!(d.tp, 0.08);
    }

    #[test]
    fn dca_from_params_defaults_to_long() {
        let d = DcaAccumulate::from_params(&Value::Table(Default::default()));
        assert_eq!(d.side, 1);
        assert_eq!(d.rungs, 3);
    }

    fn grid_of(src: &str) -> Grid {
        Grid::from_params(&toml::from_str::<Value>(src).expect("test TOML"))
    }

    fn dca_of(src: &str) -> DcaAccumulate {
        DcaAccumulate::from_params(&toml::from_str::<Value>(src).expect("test TOML"))
    }

    /// The two configurations this predicate was written for, and the near-misses it must NOT
    /// refuse. The near-misses are the point: the rule is "the ladder is empty", never "this key
    /// holds a suspicious value", so a SHORT ladder anchored at zero and a bounded grid with a step
    /// that fits are both legal and both keep loading.
    #[test]
    fn arms_no_rung_refuses_the_empty_ladder_and_nothing_else() {
        // (a) A FIXED anchor left at its compiled default: every long rung prices at
        //     `0 − k·step` ≤ 0 and is skipped.
        assert!(dca_of("anchor = \"fixed\"").arms_no_rung());
        // ...and the same table with a real anchor is a working ladder.
        assert!(!dca_of("anchor = \"fixed\"\nanchor_price = 40.0").arms_no_rung());
        // ⚠ ...and the near-miss that would make this a rule about the VALUE `0`: a SHORT ladder
        // steps AWAY from zero, so it rests `step`, `2·step`, … and is perfectly armable there.
        assert!(!dca_of("anchor = \"fixed\"\nside = \"short\"\nstep = 0.05").arms_no_rung());
        // (b) A 0..1 grid at the compiled `step = 1.0`: one rung spacing spans the whole domain, so
        //     every rung falls on or past a wall whatever the anchor and whatever the tick.
        assert!(grid_of("bounded01 = true").arms_no_rung());
        assert!(grid_of("bounded01 = true\ntick = 0.45").arms_no_rung());
        // ...and a step that FITS the domain rests rungs, so it is not refused.
        assert!(!grid_of("bounded01 = true\nstep = 0.05").arms_no_rung());
        // (c) The degenerate guard, which is the same defect at the same altitude: a ladder with no
        //     rungs, no size or no spacing rests nothing under any anchor either.
        for src in ["rungs = 0", "rungs = -5", "size = 0.0", "step = 0.0"] {
            assert!(grid_of(src).arms_no_rung(), "grid `{src}` rests nothing");
            assert!(dca_of(src).arms_no_rung(), "dca `{src}` rests nothing");
        }
        // ...and the defaults of both are armable, or every row above would hold trivially.
        assert!(!Grid::default().arms_no_rung());
        assert!(!DcaAccumulate::default().arms_no_rung());
    }

    /// The MONOTONICITY [`Grid::arms_no_rung`] rests on, driven rather than argued: on a bounded
    /// market the two walls bracket every anchor between them, so probing them is probing all of
    /// them. A step small enough to fit is armable at every anchor; the step that spans the domain
    /// is armable at none.
    #[test]
    fn the_two_walls_bracket_every_bounded_anchor() {
        for step in ["0.01", "0.05", "0.2", "0.6", "1.0", "2.0"] {
            let g = grid_of(&format!("bounded01 = true\ntick = 0.01\nstep = {step}\nrungs = 2"));
            // The interior, swept: does ANY admissible anchor rest a rung?
            let mut any = false;
            for i in 1..=99 {
                let anchor = f64::from(i) / 100.0;
                any |= !g.legs_at(g.anchor_at(anchor)).is_empty();
            }
            assert_eq!(
                any,
                !g.arms_no_rung(),
                "step {step}: the two-wall probe and the swept interior must agree, or \
                 `arms_no_rung` is refusing a ladder some anchor would have armed"
            );
        }
    }

    /// The refactor's own guard: `arm` must rest exactly what `legs_at`/`entries_at` compute, in
    /// that order — the predicate is only sound because the ladder is built in one place.
    #[test]
    fn arm_rests_exactly_the_computed_ladder() {
        let g = grid_of("step = 1.0\nrungs = 2\nsize = 1.0");
        let legs = g.legs_at(100.0);
        assert_eq!(
            legs.iter().map(|l| (l.entry_side, l.entry_price)).collect::<Vec<_>>(),
            vec![(1, 99.0), (-1, 101.0), (1, 98.0), (-1, 102.0)],
            "k-major, buy-then-sell — the submission order `arm` folds"
        );
        let d = dca_of("step = 1.0\nrungs = 3\nsize = 1.0");
        assert_eq!(d.entries_at(100.0), vec![99.0, 98.0, 97.0]);
        // ...and the short direction ladders the other way.
        let short = dca_of("side = \"short\"\nstep = 1.0\nrungs = 2");
        assert_eq!(short.entries_at(100.0), vec![101.0, 102.0]);
    }
}
