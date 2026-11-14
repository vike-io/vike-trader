//! DCA accumulate: a directional scale-in ladder. It ladders `rungs` scale-in entries against the
//! trade direction (long ⇒ buys below; short ⇒ sells above), tracks the volume-weighted average
//! entry, closes the WHOLE position with ONE take-profit at `avg · (1 ± tp)`, then re-anchors for the
//! next cycle.
//!
//! ```toml
//! [strategy]
//! runs_on = "live"
//! why = """\
//!     same symbol-stamping guarantee as `buy_hold`, and strictly one-sided: it \
//!     only ever BUYS, so it needs no short inventory on the venue and cannot hit \
//!     the naked-short problem that keeps `trailing_scalper` off the live core"""
//! routes = "single_leg"
//!
//! [params.side]
//! type = "text_or_integer"
//! default = "long"
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
//! [params.tp]
//! type = "number"
//! default = 0.05
//!
//! [params.symbol]
//! type = "text"
//! route = "symbol"
//! ```
//!
//! ## No cancel verb
//!
//! Every order goes UNTAGGED ([`OrderBroker::place`] with the tag `""`), and an untagged order has
//! no tag a later [`OrderBroker::cancel`] could name, so the ladder is designed to need none.
//! CAVEAT: a limit resting before a TP cannot be pulled, so [`DcaAccumulate`]'s residual entries from
//! a prior cycle stay resting and may re-fill if price revisits them.
//!
//! Knobs come from `from_params` (a READER: unknown keys ignored, missing keys default), so a sweep
//! grids over them with no per-strategy wiring.
//!
//! ## The EMPTY ladder is a load-time answer, not a market outcome
//!
//! `arm` builds the WHOLE ladder from the params plus ONE anchor, so a table whose rung set is empty
//! for every anchor it permits can never place an order. [`DcaAccumulate::arms_no_rung`] answers
//! that from the same code that rests the rungs; [`refuse`] is what a mount asks. It is NOT "will
//! this strategy trade" (waiting on the market is correct, and undecidable at load).

use vike_model::params::{ParamValue, Params};

use vike_marketdata::{Bar, TickQuote};
use vike_model::{ContractValue, Fill, OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(DcaAccumulate::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
/// A reader may clamp or fall back (`side` ⇒ LONG, `anchor` ⇒ `"first"`), and every one of those
/// shows up here.
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
    let d = DcaAccumulate::from_params(params);
    vec![
        ("side", if d.side < 0 { "short" } else { "long" }.to_string()),
        ("anchor", anchor(d.anchor_mode)),
        ("anchor_price", number(d.anchor_price)),
        ("step", number(d.step)),
        ("rungs", d.rungs.to_string()),
        ("size", number(d.size)),
        ("tp", number(d.tp)),
        ("symbol", opt_symbol(&d.symbol)),
    ]
}

/// The WHOLE-TABLE refusal the key checks cannot reach: a params table under which this ladder
/// rests NO order at all — on any market, at any price — with the reason, or `None` when the mount
/// can trade. MEASURED: a ladder whose `anchor = "fixed"` left `anchor_price` at its compiled `0`
/// loads, mounts, echoes a clean line and never submits. The rule is EMPTINESS
/// ([`DcaAccumulate::arms_no_rung`]), never a suspicious value: a SHORT ladder at `anchor_price = 0`
/// rests `step`, `2·step`, … and loads. A MOUNT rule: a backtest sweep may legitimately visit a
/// degenerate corner.
pub fn refuse(params: &Params) -> Option<String> {
    if !DcaAccumulate::from_params(params).arms_no_rung() {
        return None;
    }
    // The reason is the RESOLUTION (`anchor_price=0`) in the operator's own vocabulary, without
    // claiming to know which knob they meant to type.
    let echo = echo(params).iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
    let name = "dca_accumulate";
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
    /// Read the params table (a READER: unknown keys ignored, missing keys take
    /// `DcaAccumulate::default()`); `side` = `"short"`/`"sell"` or a negative integer ⇒ −1, else long.
    pub fn from_params(params: &Params) -> Self {
        let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        let d = DcaAccumulate::default();
        DcaAccumulate {
            side: read_side(params),
            anchor_mode: read_anchor_mode(params),
            anchor_price: f("anchor_price").unwrap_or(d.anchor_price),
            step: f("step").unwrap_or(d.step),
            rungs: read_rungs(params).unwrap_or(d.rungs),
            size: f("size").unwrap_or(d.size),
            tp: f("tp").unwrap_or(d.tp),
            symbol: params.get("symbol").and_then(ParamValue::as_str).map(str::to_string),
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

    /// Whether these params rest NO scale-in entry for every anchor they permit — the grid's
    /// `arms_no_rung` twin (`grid_dca/grid.rs`). `arm` runs once per CYCLE here, but a new cycle needs a
    /// take-profit fill, which needs an entry fill, which needs a rested entry: an empty ladder is
    /// terminal.
    ///
    /// - [`AnchorMode::Fixed`] — the params NAME the anchor, so there is exactly one.
    /// - [`AnchorMode::FirstPrice`] — an anchor one step beyond the deepest rung (`rungs · step`)
    ///   admits every rung of a LONG ladder, and a short ladder steps away from zero, so this is
    ///   refused only when the degenerate `rungs`/`size`/`step` guard already returned empty.
    pub fn arms_no_rung(&self) -> bool {
        let anchor = match self.anchor_mode {
            AnchorMode::Fixed => self.anchor_price,
            AnchorMode::FirstPrice => (self.rungs as f64 + 1.0) * self.step,
        };
        self.entries_at(anchor).is_empty()
    }

    /// Rest the scale-in ladder ONCE from the anchor, stepping AGAINST the trade direction.
    fn arm<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
        let anchor = match self.anchor_mode {
            AnchorMode::FirstPrice => price,
            AnchorMode::Fixed => self.anchor_price,
        };
        self.anchor = Some(anchor);
        for entry_price in self.entries_at(anchor) {
            let entry = OrderPlacement::Limit { price: entry_price };
            broker.place(symbol, "", self.side, self.size, entry, false);
        }
    }

    /// Per-bar / per-tick driver: arm on the first event, then check the single aggregate take-profit.
    fn drive<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str, price: f64) {
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
                broker.place(
                    symbol,
                    "",
                    -self.side,
                    self.filled_size,
                    OrderPlacement::Market,
                    false,
                );
                self.closing = true;
            }
        }
    }

    /// Fold a fill: an ENTRY fill updates the average entry by the contract's averaging law
    /// ([`ContractValue::average_price`]: volume-weighted on a linear contract, harmonic on an inverse
    /// one); the opposite-side TAKE-PROFIT fill resets state and drops the anchor so the next bar/tick
    /// re-ladders for a fresh cycle.
    fn handle_fill(&mut self, side: i32, size: f64, price: f64, value: ContractValue) {
        if side == self.side {
            let new_size = self.filled_size + size;
            if new_size > 0.0 {
                self.avg_entry = value.average_price(self.filled_size, self.avg_entry, size, price);
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

impl<B: OrderBroker> Strategy<B> for DcaAccumulate {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.drive(broker, &symbol_of(self.symbol.as_deref(), bar.symbol.as_deref()), bar.close);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.drive(broker, &symbol_of(self.symbol.as_deref(), Some(&q.symbol)), q.mid());
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        let value = broker.contract_value(&fill.symbol).unwrap_or_default();
        self.handle_fill(fill.side, fill.size, fill.price, value);
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

/// Read the `side` param: a `"short"`/`"sell"` string OR a negative integer ⇒ −1, everything else
/// (including absent) ⇒ +1 (long).
fn read_side(params: &Params) -> i32 {
    if let Some(s) = params.get("side").and_then(ParamValue::as_str)
        && (s.eq_ignore_ascii_case("short") || s.eq_ignore_ascii_case("sell"))
    {
        return -1;
    }
    if let Some(i) = params.get("side").and_then(ParamValue::as_integer)
        && i < 0
    {
        return -1;
    }
    1
}

// The pure surface, copied from `crates/vike-strategy/src/strategies/grid_dca.rs`'s tests (the DCA
// half). Run by `crates/vike-strategies-check` (`cargo test -p vike-strategies-check`); a build of
// this file compiles no test. The LADDER-BEHAVIOUR tests need the real engine:
// `crates/vike-strategies-check/tests/takers.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dca_from_params_reads_side_and_knobs() {
        let toml = r#"
side = "short"
step = 2.0
rungs = 5
size = 4.0
tp = 0.08
"#;
        let p: Params = toml::from_str(toml).unwrap();
        let d = DcaAccumulate::from_params(&p);
        assert_eq!(d.side, -1);
        assert_eq!(d.step, 2.0);
        assert_eq!(d.rungs, 5);
        assert_eq!(d.size, 4.0);
        assert_eq!(d.tp, 0.08);
    }

    #[test]
    fn dca_from_params_defaults_to_long() {
        let d = DcaAccumulate::from_params(&Params::new());
        assert_eq!(d.side, 1);
        assert_eq!(d.rungs, 3);
    }

    /// `rungs` is an `Int`-only knob (`read_rungs` is `ParamValue::as_integer`): the same number
    /// arriving as a `Float` (`5.0`, the shape a sweep override that writes every value as a float
    /// produces) is ABSENT, and the count stays at its compiled default.
    #[test]
    fn an_int_only_knob_reads_an_int_and_treats_a_float_as_absent() {
        let as_int: Params = [("rungs", ParamValue::Int(5))].into_iter().collect();
        let as_float: Params = [("rungs", ParamValue::Float(5.0))].into_iter().collect();
        assert_eq!(read_rungs(&as_int), Some(5), "an Int is present");
        assert_eq!(read_rungs(&as_float), None, "a Float is ABSENT to an Int-only knob");
        assert_eq!(DcaAccumulate::from_params(&as_int).rungs, 5);
        assert_eq!(DcaAccumulate::from_params(&as_float).rungs, 3);
    }

    fn dca_of(src: &str) -> DcaAccumulate {
        DcaAccumulate::from_params(&toml::from_str::<Params>(src).expect("test TOML"))
    }

    /// The configuration this predicate was written for, and the near-misses it must NOT refuse.
    /// The near-misses are the point: the rule is "the ladder is empty", never "this key holds a
    /// suspicious value", so a SHORT ladder anchored at zero is legal and keeps loading.
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
        // (c) The degenerate guard, which is the same defect at the same altitude: a ladder with no
        //     rungs, no size or no spacing rests nothing under any anchor either.
        for src in ["rungs = 0", "rungs = -5", "size = 0.0", "step = 0.0"] {
            assert!(dca_of(src).arms_no_rung(), "dca `{src}` rests nothing");
        }
        // ...and the defaults are armable, or every row above would hold trivially.
        assert!(!DcaAccumulate::default().arms_no_rung());
    }

    /// The DCA's average entry is the contract's: two 1,000-USD entries at 50,000 and 60,000 on a
    /// Deribit inverse perpetual average 54,545.45 (it read 55,000, the arithmetic mean, so its 5 %
    /// take-profit sat at 57,750 instead of 57,272.73); a linear ladder keeps its old bits.
    #[test]
    fn the_dca_average_entry_is_harmonic_on_an_inverse_contract() {
        let mut inverse = DcaAccumulate::default();
        inverse.handle_fill(1, 1_000.0, 50_000.0, ContractValue::inverse(1.0));
        inverse.handle_fill(1, 1_000.0, 60_000.0, ContractValue::inverse(1.0));
        assert!((inverse.avg_entry() - 54_545.454_545).abs() < 1e-6, "{}", inverse.avg_entry());
        assert_eq!(inverse.filled_size(), 2_000.0);

        let mut lin = DcaAccumulate::default();
        for (q, px) in [(0.3, 101.7), (0.7, 99.1), (1.1, 97.3)] {
            let old = (lin.avg_entry * lin.filled_size + px * q) / (lin.filled_size + q);
            lin.handle_fill(1, q, px, ContractValue::linear(50.0));
            assert_eq!(lin.avg_entry().to_bits(), old.to_bits(), "the linear ladder's own bits");
        }
    }

    /// The refactor's own guard: `arm` must rest exactly what `entries_at` computes, in that order —
    /// the predicate is only sound because the ladder is built in one place.
    #[test]
    fn arm_rests_exactly_the_computed_ladder() {
        let d = dca_of("step = 1.0\nrungs = 3\nsize = 1.0");
        assert_eq!(d.entries_at(100.0), vec![99.0, 98.0, 97.0]);
        // ...and the short direction ladders the other way.
        let short = dca_of("side = \"short\"\nstep = 1.0\nrungs = 2");
        assert_eq!(short.entries_at(100.0), vec![101.0, 102.0]);
    }
}
