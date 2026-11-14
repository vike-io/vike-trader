//! Grid: a two-sided ladder of resting limits around an anchor. `rungs` buy limits BELOW and sell
//! limits ABOVE the `anchor`, `step` apart, `size` each (the sell side is an opening short when
//! flat). A rung oscillates: its ENTRY fill places a take-profit one `step` back toward the anchor;
//! that TP's fill RE-ARMS the entry, so inventory is bounded by `rungs · size` per side. A HARD STOP
//! fires when price leaves `[anchor − band, anchor + band]`: flatten and HALT (no re-arms, no new
//! legs). `bounded01` (Polymarket) clamps the band to `[tick, 1 − tick]` and skips any rung on/past
//! a wall.
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! why = """\
//!     same symbol-stamping guarantee as `buy_hold`: `vike_core::runtime`'s \
//!     `BarClose` arm stamps `bar.symbol` before `drive_strategy`, and this \
//!     strategy's resting legs are single-venue single-symbol, so nothing it \
//!     submits needs a leg the mount does not declare"""
//! routes = "single_leg"
//!
//! [params.anchor]
//! type = "text"
//! default = "first"
//!
//! [params.anchor_price]
//! type = "number"
//! default = 0.0
//! needs = { anchor = ["fixed"] }
//!
//! [params.step]
//! type = "number"
//! default = 1.0
//!
//! [params.rungs]
//! type = "integer"
//! default = 3
//!
//! [params.size]
//! type = "number"
//! default = 1.0
//!
//! [params.band]
//! type = "number"
//! default = 10.0
//!
//! [params.bounded01]
//! type = "bool"
//! default = false
//!
//! [params.tick]
//! type = "number"
//! default = 0.001
//! needs = { bounded01 = ["true"] }
//!
//! [params.symbol]
//! type = "text"
//! route = "symbol"
//! ```
//!
//! ## No cancel verb
//!
//! Every order goes UNTAGGED ([`OrderBroker::place`] with the tag `""`), and an untagged order has
//! no tag a later [`OrderBroker::cancel`] could name, so the grid is designed to need none.
//! CAVEAT: a limit resting before a halt cannot be pulled, so [`Grid`] re-flattens on every event
//! while halted (inventory stays bounded).
//!
//! Knobs come from `from_params` (a READER: unknown keys ignored, missing keys default), so a sweep
//! grids over them with no per-strategy wiring.
//!
//! ## The EMPTY ladder is a load-time answer, not a market outcome
//!
//! `arm` builds the WHOLE order set from the params plus ONE anchor, once, and never rebuilds it
//! from nothing, so a table whose rung set is empty for every anchor it permits can never place an
//! order. [`Grid::arms_no_rung`] answers that from the same code that rests the rungs; [`refuse`]
//! is what a mount asks. It is NOT "will this strategy trade" (waiting on the market is correct, and
//! undecidable at load).

use vike_model::params::{ParamValue, Params};

use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(Grid::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
/// A reader may clamp or fall back (`anchor` ⇒ `"first"` on anything but `"fixed"`), and every one
/// of those shows up here.
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    /// An OPTIONAL symbol, in the THREE states it actually has — absent, explicitly EMPTY, and set.
    /// `Some("")` is a STATED value `drive` refuses to trade on (it guards on `is_empty()`).
    fn opt_symbol(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "(from the feed)".to_string(),
            Some("") => "(empty — this strategy cannot trade)".to_string(),
            Some(s) => s.to_string(),
        }
    }
    fn anchor(m: AnchorMode) -> String {
        match m {
            AnchorMode::Fixed => "fixed",
            AnchorMode::FirstPrice => "first",
        }
        .to_string()
    }
    let g = Grid::from_params(params);
    vec![
        ("anchor", anchor(g.anchor_mode)),
        ("anchor_price", number(g.anchor_price)),
        ("step", number(g.step)),
        ("rungs", g.rungs.to_string()),
        ("size", number(g.size)),
        ("band", number(g.band)),
        ("bounded01", g.bounded01.to_string()),
        ("tick", number(g.tick)),
        ("symbol", opt_symbol(&g.symbol)),
    ]
}

/// The WHOLE-TABLE refusal the key checks cannot reach: a params table under which this grid rests
/// NO order at all — on any market, at any price — with the reason, or `None` when the mount can
/// trade. MEASURED: a grid whose `bounded01` left `step` at its compiled `1.0` loads, mounts, echoes
/// a clean line and never submits. The rule is EMPTINESS ([`Grid::arms_no_rung`]), never a
/// suspicious value. A MOUNT rule: a backtest sweep may legitimately visit a degenerate corner.
pub fn refuse(params: &Params) -> Option<String> {
    if !Grid::from_params(params).arms_no_rung() {
        return None;
    }
    // The reason is the RESOLUTION (`step=1 bounded01=true`) in the operator's own vocabulary,
    // without claiming to know which knob they meant to type.
    let echo = echo(params).iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
    let name = "grid";
    Some(format!(
        "`{name}` rests NO rung at any anchor these params permit, so this mount can never place an \
         order — on any market, at any price. It resolved to: {echo}"
    ))
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
    /// Read the registry params table (`BuyHold::from_params` convention: a READER, not a schema —
    /// unknown keys ignored, missing keys take `Grid::default()`). Each key is the field of the
    /// same name, except `anchor` = `"first"` | `"fixed"` (→ `anchor_mode`).
    pub fn from_params(params: &Params) -> Self {
        let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        let d = Grid::default();
        Grid {
            anchor_mode: read_anchor_mode(params),
            anchor_price: f("anchor_price").unwrap_or(d.anchor_price),
            step: f("step").unwrap_or(d.step),
            rungs: read_rungs(params).unwrap_or(d.rungs),
            size: f("size").unwrap_or(d.size),
            band: f("band").unwrap_or(d.band),
            bounded01: params.get("bounded01").and_then(ParamValue::as_bool).unwrap_or(d.bounded01),
            tick: f("tick").unwrap_or(d.tick),
            symbol: params.get("symbol").and_then(ParamValue::as_str).map(str::to_string),
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

    /// Whether the ladder leg at `index` is ARMED (entry resting, unfilled); `false` out of range.
    ///
    /// A narrow observable for the engine-driven RE-ARM gate in this crate's
    /// `tests/grid_dca_engine.rs`, so `GridLeg` / `LegState` can stay private.
    pub fn leg_is_armed(&self, index: usize) -> bool {
        self.legs.get(index).is_some_and(|l| l.state == LegState::Armed)
    }

    /// One clamp: a bounded-market price must sit strictly inside `(tick, 1 − tick)`.
    fn on_grid(&self, price: f64) -> bool {
        !self.bounded01 || (price > self.tick && price < 1.0 - self.tick)
    }

    /// The anchor these params resolve to at `price` — the mode select plus the bounded-market
    /// clamp. Shared by [`Grid::arm`] and [`Grid::arms_no_rung`] so the predicate cannot drift from
    /// the ladder.
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

    /// Whether these params rest NO rung for EVERY anchor they permit — a mount that can never
    /// place an order. Decidable at LOAD because `arm` runs exactly once (`drive` calls it only
    /// while `self.anchor.is_none()`, and nothing puts the anchor back) and its rung set is a pure
    /// function of the params plus that one anchor.
    ///
    /// The anchors quantified over, and why they are the whole set:
    ///
    /// - [`AnchorMode::Fixed`] — the params NAME the anchor, so there is exactly one.
    /// - [`AnchorMode::FirstPrice`] on a `bounded01` market — the anchor is clamped into
    ///   `[tick, 1 − tick]` and admission is MONOTONE in it per side (a buy rung at `a − k·step`
    ///   survives at the TOP wall if anywhere, a sell rung at `a + k·step` at the BOTTOM), so the
    ///   two walls bracket every anchor; `the_two_walls_bracket_every_bounded_anchor` sweeps the
    ///   interior.
    /// - [`AnchorMode::FirstPrice`] off one — `on_grid` is always true, so one probe is exact.
    pub fn arms_no_rung(&self) -> bool {
        if self.bounded01 && 1.0 - self.tick <= self.tick {
            // The walls have met: `(tick, 1 − tick)` is EMPTY and no anchor can help. Answered here
            // because `anchor_at`'s `clamp` would PANIC on that inverted range.
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
    fn arm<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
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
            let entry = OrderPlacement::Limit { price: leg.entry_price };
            broker.place(symbol, "", leg.entry_side, self.size, entry, false);
        }
    }

    /// Close the net position at market (the hard-stop flatten / stray-fill mop-up while halted).
    fn flatten<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str) {
        let pos = broker.position(symbol);
        if pos != 0.0 {
            let side = vike_model::closing_side(pos);
            broker.place(symbol, "", side, pos.abs(), OrderPlacement::Market, false);
        }
    }

    /// Per-bar / per-tick driver: arm on the first event, then enforce the hard-stop band.
    fn drive<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
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
    fn handle_fill<B: OrderBroker>(&mut self, broker: &mut B, side: i32, price: f64) {
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
        let symbol = self.resolved_symbol.clone();
        if is_entry {
            self.legs[i].state = LegState::Holding;
            let take_profit = OrderPlacement::Limit { price: leg.tp_price };
            broker.place(&symbol, "", -leg.entry_side, self.size, take_profit, false);
        } else {
            self.legs[i].state = LegState::Armed;
            let entry = OrderPlacement::Limit { price: leg.entry_price };
            broker.place(&symbol, "", leg.entry_side, self.size, entry, false);
        }
    }
}

impl<B: OrderBroker> Strategy<B> for Grid {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.drive(broker, &symbol_of(self.symbol.as_deref(), bar.symbol.as_deref()), bar.close);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.drive(broker, &symbol_of(self.symbol.as_deref(), Some(&q.symbol)), q.mid());
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        self.handle_fill(broker, fill.side, fill.price);
    }
}

/// Read the `anchor` param: `"fixed"` (any case) ⇒ [`AnchorMode::Fixed`], anything else (or
/// nothing) ⇒ [`AnchorMode::FirstPrice`].
fn read_anchor_mode(params: &Params) -> AnchorMode {
    match params.get("anchor").and_then(ParamValue::as_str) {
        Some(m) if m.eq_ignore_ascii_case("fixed") => AnchorMode::Fixed,
        _ => AnchorMode::FirstPrice,
    }
}

/// The symbol an event drives: the `symbol` param when set, else the event's own (`""` for none).
fn symbol_of(param: Option<&str>, event: Option<&str>) -> String {
    param.or(event).unwrap_or_default().to_string()
}

/// Read the `rungs` param as a non-negative count (a negative integer clamps to `0`), or `None`
/// when absent/non-integer.
fn read_rungs(params: &Params) -> Option<usize> {
    params.get("rungs").and_then(ParamValue::as_integer).map(|i| i.max(0) as usize)
}

// The pure surface, copied from `crates/vike-strategy/src/strategies/grid_dca.rs`'s tests (the grid
// half). Run by `crates/vike-strategies-check` (`cargo test -p vike-strategies-check`); a build of
// this file compiles no test. The LADDER-BEHAVIOUR tests need the real engine:
// `crates/vike-strategies-check/tests/takers.rs`.
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
        let p: Params = toml::from_str(toml).unwrap();
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
        let g = Grid::from_params(&Params::new());
        assert_eq!(g.anchor_mode, AnchorMode::FirstPrice);
        assert_eq!(g.rungs, 3);
        assert_eq!(g.symbol, None);
    }

    /// `rungs` is an `Int`-only knob (`read_rungs` is `ParamValue::as_integer`): the same number
    /// arriving as a `Float` (`5.0`, the shape a sweep override that writes every value as a float
    /// produces) is ABSENT, and the count stays at its compiled default. `Int` and `Float` are
    /// distinct kinds; only the lenient `ParamValue::as_f64` widens, and only `Int` → `Float`.
    #[test]
    fn an_int_only_knob_reads_an_int_and_treats_a_float_as_absent() {
        let as_int: Params = [("rungs", ParamValue::Int(5))].into_iter().collect();
        let as_float: Params = [("rungs", ParamValue::Float(5.0))].into_iter().collect();
        assert_eq!(read_rungs(&as_int), Some(5), "an Int is present");
        assert_eq!(read_rungs(&as_float), None, "a Float is ABSENT to an Int-only knob");
        assert_eq!(Grid::from_params(&as_int).rungs, 5);
        assert_eq!(Grid::from_params(&as_float).rungs, 3, "...so the compiled default runs");
        // The other direction is the lenient read: an `Int` satisfies a `Number` knob.
        let size_int: Params = [("size", ParamValue::Int(2))].into_iter().collect();
        assert_eq!(Grid::from_params(&size_int).size, 2.0);
    }

    fn grid_of(src: &str) -> Grid {
        Grid::from_params(&toml::from_str::<Params>(src).expect("test TOML"))
    }

    /// The configuration this predicate was written for, and the near-misses it must NOT refuse.
    /// The near-misses are the point: the rule is "the ladder is empty", never "this key holds a
    /// suspicious value", so a bounded grid with a step that fits keeps loading.
    #[test]
    fn arms_no_rung_refuses_the_empty_ladder_and_nothing_else() {
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
        }
        // ...and the defaults are armable, or every row above would hold trivially.
        assert!(!Grid::default().arms_no_rung());
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

    /// The refactor's own guard: `arm` must rest exactly what `legs_at` computes, in that order —
    /// the predicate is only sound because the ladder is built in one place.
    #[test]
    fn arm_rests_exactly_the_computed_ladder() {
        let g = grid_of("step = 1.0\nrungs = 2\nsize = 1.0");
        let legs = g.legs_at(100.0);
        assert_eq!(
            legs.iter().map(|l| (l.entry_side, l.entry_price)).collect::<Vec<_>>(),
            vec![(1, 99.0), (-1, 101.0), (1, 98.0), (-1, 102.0)],
            "k-major, buy-then-sell — the submission order `arm` folds"
        );
    }
}
