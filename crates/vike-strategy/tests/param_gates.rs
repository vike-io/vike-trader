//! Pins [`vike_strategy::PARAM_GATES`] — and the ABSENCE of a row — against what the strategies
//! actually DO, by driving them.
//!
//! # The class this closes
//!
//! Five review rounds each found one more `[strategy.params]` key that was spelled right
//! ([`vike_strategy::unknown_params`]), typed right ([`vike_strategy::mistyped_params`]), routed
//! right ([`vike_strategy::misrouted_params`]) and genuinely read by `from_params` — and whose value
//! the strategy then never LOOKED AT, because another key in the same table sent it down a different
//! branch. Each one was found by a person reading a reader; none of them was found by a test. They
//! were not five bugs, they were one class, and a sixth patch would not have ended it.
//!
//! The class has exactly one observable, and it is behavioural: **two params tables differing ONLY
//! in key `K` drive the strategy to the SAME calls.** That is what this file measures. Nothing here
//! reads a doc comment or a field name.
//!
//! # Why this is not the source scan `param_keys_gate.rs` runs
//!
//! That gate resolves each key's LOOKUP SITE — `params.get("anchor_price").and_then(as_f64)` — and
//! every one of those sites is an unconditional statement inside `from_params`, by construction: a
//! reader reads the whole table into fields and only THEN does the strategy branch. The
//! conditionality lives at the FIELD-USE site in another function
//! (`crates/vike-strategy/src/grid_dca.rs`'s `Grid::arm` reads `self.anchor_price` only in the
//! `AnchorMode::Fixed` arm), so extending `lookup_sites` to "notice a read inside a conditional"
//! would find nothing at all and be vacuously green forever — the failure shape this repo has
//! already been bitten by three times. [`the_lookup_sites_of_this_class_are_unconditional`] asserts
//! that rather than asserting it in prose, so the claim can rot loudly.
//!
//! # The two directions
//!
//! - [`an_ungated_key_moves_the_strategy`] — direction 2, the CLASS-CLOSER. Every declared key with
//!   no [`vike_strategy::PARAM_GATES`] row must change the call trace at its strategy's base table.
//!   A key that does not is a new instance of the class and fails HERE, with its own name in the
//!   message, whether or not anybody thought to look for it.
//! - [`a_gated_key_is_inert_until_its_gate_is_armed`] — direction 1. Every row must be TRUE: with
//!   the gate unmet the two values are call-for-call identical, and with it armed they differ. A row
//!   that exempted a LIVE key would remove a real knob from direction 2's input.
//!
//! ⚠ **[`vike_strategy::PARAM_GATES`] is this file's own input and nothing else's.** It is the
//! declaration of which keys are legitimately read only under a stated condition, so direction 2
//! knows what to exempt and direction 1 can then prove the exemption. It is NOT an annotation
//! source: `vike_strategy::resolved_params` marks nothing, and
//! `vike_strategy::PARAM_GATES`' own doc records why the two rounds that had it mark the mount line
//! were deleted rather than repaired.
//!
//! # The residual, measured rather than assumed
//!
//! [`the_shapes_this_harness_cannot_see`] DEMONSTRATES each blind spot with a real, executed
//! example, the way `crates/vike-ops/tests/settings_registry.rs`'s
//! `the_shapes_the_store_scanner_cannot_see` does — so the hole cannot be quietly widened, and
//! cannot be quietly closed either. One of the shapes recorded there is a DEAD CONFIGURATION rather
//! than a dead key — a table under which the strategy places no order AT ALL, so both values of
//! every key are equally dead and neither direction above can see it. That block's own `DEAD` table
//! is the measured ledger of them, and [`the_scripted_market_moves_every_strategy`] is the cheap
//! general floor under it: no per-key claim, only "this strategy places at least one order at its
//! base table".
//!
//! ⚠ **Every row of that ledger is now REFUSED AT LOAD, and each row asserts it.**
//! [`vike_strategy::unarmable_params`] decides — from the ladder builders themselves — whether a
//! table can rest a rung under any anchor it permits, and the daemon's profile validation
//! (`crates/vike-tradehub/src/config.rs`'s `validate_strategy`) refuses it when it cannot. This
//! harness still cannot SEE the shape: it drives strategies directly, one layer beneath any
//! profile, so both values of every key remain equally dead here. What each row now carries is that
//! the gate one layer up catches it. The instruction on the rows is unchanged — a row that stops
//! being ZERO means the strategy was fixed, and deleting it is that fix's other half.

use std::collections::BTreeMap;

use toml::Value;

use vike_model::{Bar, Broker, Fill, HftBroker, QuoteTick};
use vike_strategy::{
    param_gate, param_routes, resolved_params, strategy_by_name, unarmable_params, ParamKeys,
    ParamRoutes, PARAM_GATES, PARAM_KEYS, PORTABLE_STRATEGIES,
};

// ===================================================================================================
// The recording exchange
// ===================================================================================================

/// One resting limit order, in submission order (the crate's deterministic-replay convention — a
/// `Vec`, never a map, so a replay cannot reorder fills).
#[derive(Debug, Clone)]
struct Resting {
    /// `Some` for the TAGGED (`HftBroker`) lane, whose orders are cancellable/modifiable by tag;
    /// `None` for the portable `Broker` lane, which has no cancel verb at all.
    tag: Option<String>,
    symbol: String,
    side: i32,
    qty: f64,
    price: f64,
}

/// A `Broker` + `HftBroker` double that RECORDS every call and matches resting limits against the
/// price the driver has just published — the smallest thing that gives every strategy a real
/// lifecycle (rest → fill → react) without reaching for the simulator, which lives two layers up.
///
/// It is a PROBE, not a fill model: a marketable limit fills whole, immediately, at its own limit
/// price. Nothing here needs to be realistic — the whole method compares one trace against another
/// trace produced by the SAME rules, so any deterministic matcher that lets the strategies advance
/// is sound. What it must never be is lossy: every call is logged, because a key that changes only
/// a cancel or only a modify is exactly the key a submissions-only log would call inert.
#[derive(Default)]
struct TraceBroker {
    px: f64,
    now: i64,
    /// Signed position per symbol (the portable `Broker::position`).
    pos: BTreeMap<String, f64>,
    /// The single signed position the TAGGED lane reports (`HftBroker::position`).
    net: f64,
    /// Bars delivered so far, per symbol — `FundingCarryController::evaluate` reads its funding book
    /// straight off `Broker::bars(symbol).last()`, so a broker with no bars would silently make
    /// every carry knob inert.
    bars: BTreeMap<String, Vec<Bar>>,
    /// Every call, in order. THIS is the observable the whole file compares.
    log: Vec<String>,
    resting: Vec<Resting>,
    /// Fills produced but not yet folded into the strategy (market orders fill on submission).
    pending: Vec<Fill>,
}

impl TraceBroker {
    fn rest(&mut self, tag: Option<&str>, symbol: &str, side: i32, qty: f64, price: f64) {
        self.resting.push(Resting {
            tag: tag.map(str::to_string),
            symbol: symbol.to_string(),
            side,
            qty,
            price,
        });
    }

    fn fill_now(&mut self, symbol: &str, side: i32, qty: f64, price: f64, is_maker: bool) {
        self.pending.push(Fill {
            side,
            size: qty,
            price,
            fee: 0.0,
            ts: self.now,
            is_maker,
            symbol: symbol.to_string(),
        });
    }

    /// Every resting order the current price has crossed, removed and turned into a fill: a BUY
    /// fills once the market trades at or below its limit, a SELL at or above.
    fn match_resting(&mut self) {
        let px = self.px;
        let mut i = 0;
        while i < self.resting.len() {
            let r = self.resting[i].clone();
            let crossed = (r.side > 0 && px <= r.price) || (r.side < 0 && px >= r.price);
            if crossed {
                self.resting.remove(i);
                self.fill_now(&r.symbol, r.side, r.qty, r.price, true);
            } else {
                i += 1;
            }
        }
    }

    /// Drain the fills produced so far, applying each to the position books FIRST — a strategy that
    /// reads `position()` inside its own `on_fill` (`TrailingScalper` does, to decide whether it is
    /// truly flat) must see the fill it is being told about.
    fn take_fills(&mut self) -> Vec<Fill> {
        let fills = std::mem::take(&mut self.pending);
        for f in &fills {
            *self.pos.entry(f.symbol.clone()).or_insert(0.0) += f.side as f64 * f.size;
            self.net += f.side as f64 * f.size;
        }
        fills
    }
}

impl Broker for TraceBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.log.push(format!("market {symbol} {side} {qty}"));
        let px = self.px;
        self.fill_now(symbol, side, qty, px, false);
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        self.log.push(format!("limit {symbol} {side} {qty} {price}"));
        self.rest(None, symbol, side, qty, price);
    }
    fn position(&self, symbol: &str) -> f64 {
        self.pos.get(symbol).copied().unwrap_or(0.0)
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.px
    }
    fn equity(&self) -> f64 {
        1_000_000.0
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        self.bars.get(symbol).map(Vec::as_slice).unwrap_or(&[])
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
}

impl HftBroker for TraceBroker {
    fn position(&self) -> f64 {
        self.net
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.log.push(format!("tagged {tag} {side} {qty} {price}"));
        self.rest(Some(tag), "", side, qty, price);
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.log.push(format!("modify {tag} {new_qty:?} {new_price:?}"));
        for r in self.resting.iter_mut().filter(|r| r.tag.as_deref() == Some(tag)) {
            if let Some(q) = new_qty {
                r.qty = q;
            }
            if let Some(p) = new_price {
                r.price = p;
            }
        }
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.log.push(format!("cancel {tag}"));
        self.resting.retain(|r| r.tag.as_deref() != Some(tag));
    }
}

// ===================================================================================================
// The scripted market
// ===================================================================================================

/// A mean-reverting-ish path in `(0, 1)`, so the SAME script runs at unit scale (where `bounded01`
/// and `tick` mean something) and at a 100 scale (where the ordinary `step`/`band` knobs do).
const PATH: [f64; 12] = [0.50, 0.53, 0.47, 0.58, 0.42, 0.62, 0.38, 0.66, 0.34, 0.70, 0.30, 0.52];

/// Milliseconds between steps. Chosen so the whole script spans `0..=110_000` — `trailing_scalper`'s
/// entry cutoffs are compared against `market_open_ms`/`market_close_ms` inside that window.
const STEP_MS: i64 = 10_000;

/// How many fill→react rounds are folded after each market event. A grid entry fill places a
/// take-profit, whose fill re-arms the entry, whose fill... — the cascade is bounded here rather
/// than by a fill model, because the comparison only needs both sides bounded identically.
const ROUNDS: usize = 3;

/// Which feed the driver plays. The distinction exists for ONE declared residual: `PairsZScore`
/// reads `funding_a`/`funding_b` only when the bar carries no funding of its own, so a
/// funding-BEARING feed makes those two keys inert for a reason no gate over the params table could
/// ever express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Feed {
    /// `A`/`B` (the pairs legs) carry no funding; `F1`/`F2` (the carry legs) do.
    Plain,
    /// ...and now `A`/`B` carry funding too.
    FundedPairLegs,
}

enum Ev {
    Bar(Bar),
    Quote(QuoteTick),
}

fn a_bar(symbol: &str, ts: i64, close: f64, funding: Option<f64>) -> Bar {
    Bar {
        ts,
        open: close,
        high: close * 1.02,
        low: close * 0.98,
        close,
        volume: 1.0,
        funding,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

/// Which symbols a strategy's feed carries. ⚠ **Per strategy, and it is load-bearing rather than
/// tidy.** A single-symbol strategy that also receives another symbol's bars is driven by a price
/// path it would never see on its mount: measured on the first run of this harness, `Grid` (whose
/// `symbol` is unset, so it takes each bar's own) hit its hard-stop band on the SECOND bar of an
/// interleaved feed and halted for the rest of the script, which made `band`, `step` and `size` all
/// look inert — three false instances of the very class this file exists to detect.
///
/// `A`/`B` carry no funding (the two legs `pairs_zscore` spreads, and the lane whose absent funding
/// is what makes its `funding_a`/`funding_b` fallbacks live); `F1`/`F2` do (the two venues
/// `funding_carry` builds a carry book from, and the funding decision points `funding_capture`
/// acts on).
fn feed_symbols(strategy: &str) -> &'static [&'static str] {
    match strategy {
        "funding_capture" => &["F1"],
        "funding_carry" => &["F1", "F2"],
        "pairs_zscore" => &["A", "B"],
        _ => &["A"],
    }
}

/// One pass of the script at `scale`, carrying only `symbols`.
fn events(scale: f64, feed: Feed, symbols: &[&str]) -> Vec<Ev> {
    let leg_funding = match feed {
        Feed::Plain => None,
        Feed::FundedPairLegs => Some(0.002),
    };
    let mut out = Vec::new();
    for (i, frac) in PATH.iter().enumerate() {
        let ts = i as i64 * STEP_MS;
        let a = frac * scale;
        let b = PATH[(i + 5) % PATH.len()] * scale;
        let f2 = PATH[(i + 2) % PATH.len()] * scale;
        for (symbol, close, funding) in [
            ("A", a, leg_funding),
            ("B", b, leg_funding),
            ("F1", a, Some(0.01)),
            ("F2", f2, Some(-0.01)),
        ] {
            if symbols.contains(&symbol) {
                out.push(Ev::Bar(a_bar(symbol, ts, close, funding)));
            }
        }
        // The TICK lane, on whichever symbol this strategy's feed leads with —
        // `trailing_scalper` trades on quotes ALONE and would otherwise never run at all.
        if let Some(symbol) = symbols.first() {
            let mid = if *symbol == "F2" {
                f2
            } else if *symbol == "B" {
                b
            } else {
                a
            };
            out.push(Ev::Quote(QuoteTick {
                ts: ts + 1,
                local_ts: ts + 1,
                bid: mid * 0.99,
                ask: mid * 1.01,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: (*symbol).to_string(),
            }));
        }
    }
    out
}

/// Resolve `name` with `params` and drive it through the whole script at BOTH price scales, one
/// fresh strategy per scale, returning every broker call it made.
///
/// Two scales rather than one because the discriminating price regime differs per knob: `tick` and
/// `bounded01` only mean anything inside `(0, 1)`, while a `band`/`step` at 100-scale prices is what
/// separates an armed ladder from a halted one. Concatenating the two passes can only ADD
/// discriminating power — a key live in either regime is reported live.
fn trace(name: &str, params: &Value, feed: Feed) -> Vec<String> {
    let mut log = Vec::new();
    for scale in [1.0_f64, 100.0] {
        log.push(format!("== scale {scale}"));
        let mut strategy = strategy_by_name::<TraceBroker>(name, params)
            .unwrap_or_else(|e| panic!("{name} must resolve: {e}"));
        let mut broker = TraceBroker::default();
        for ev in events(scale, feed, feed_symbols(name)) {
            match ev {
                Ev::Bar(bar) => {
                    broker.now = bar.ts;
                    broker.px = bar.close;
                    let symbol = bar.symbol.clone().unwrap_or_default();
                    broker.bars.entry(symbol).or_default().push(bar.clone());
                    strategy.on_bar(&mut broker, &bar);
                }
                Ev::Quote(q) => {
                    broker.now = q.ts;
                    broker.px = q.mid();
                    strategy.on_quote_tick(&mut broker, &q);
                }
            }
            for _ in 0..ROUNDS {
                broker.match_resting();
                let fills = broker.take_fills();
                if fills.is_empty() {
                    break;
                }
                for f in fills {
                    strategy.on_fill(&mut broker, &f);
                }
            }
        }
        log.append(&mut broker.log);
    }
    log
}

// ===================================================================================================
// The probe table
// ===================================================================================================

/// The MINIMAL context in which a strategy trades at all — everything a knob needs around it before
/// "does this value change anything" is a question with a meaningful answer. Empty for the
/// strategies the script already exercises as they come.
///
/// ⚠ Each non-empty base is a NECESSITY, not a preference, and each one is here for a stated
/// reason. A base that quietly armed something else would weaken every probe under it.
fn base(strategy: &str) -> &'static str {
    match strategy {
        // The harness only stamps a cooldown when an executor CLOSES, so `cooldown_ms` (and the
        // barrier legs themselves) need at least one barrier armed or nothing ever completes.
        "momentum" => "tp = 3.0\n",
        // A carry needs TWO venues in the funding book before `best_carry_to_open` returns
        // anything, and the book is filled per (venue, symbol) through the harness venue map.
        "funding_carry" => "tp = 3.0\n[venues]\nF1 = \"binance\"\nF2 = \"okx\"\n",
        // Two legs, a window short enough for a 12-bar script, and a cost floor low enough that the
        // strategy actually crosses — otherwise every cost knob would read as inert because nothing
        // ever entered.
        "pairs_zscore" => {
            "symbol_a = \"A\"\nsymbol_b = \"B\"\nperiod = 3\nentry_z = 0.5\nexit_z = 0.1\n\
             taker_fee = 0.0\nhalf_spread_bps = 0.0\nhold_intervals = 1.0\nfunding_a = 0.001\n\
             funding_b = -0.001\n"
        }
        _ => "",
    }
}

/// One key, two contrasting values, and — for a gated key — the two TOML fragments that put its
/// [`PARAM_GATES`] gate on each side of its condition.
struct Probe {
    strategy: &'static str,
    key: &'static str,
    /// Two values of `key`, as TOML value text (the right-hand side of `key = …`).
    a: &'static str,
    b: &'static str,
    /// Overlay that leaves the gate UNMET (`""` when the strategy's base table already does — which
    /// is the shape of the operator-facing hazard: a key that does nothing on an otherwise-default
    /// profile).
    off: &'static str,
    /// Overlay that MEETS the gate (`""` only if the base table already does). Every gated row today
    /// arms with a fragment: each one is a key that does nothing on an otherwise-default profile,
    /// which is the operator-facing shape of this class.
    on: &'static str,
}

const fn p(strategy: &'static str, key: &'static str, a: &'static str, b: &'static str) -> Probe {
    Probe { strategy, key, a, b, off: "", on: "" }
}

/// A GATED probe: the same row plus the two fragments. Both are checked against
/// [`vike_strategy::Gate::unmet`] rather than trusted (`every_probe_matches_its_gate_row`).
const fn g(
    strategy: &'static str,
    key: &'static str,
    a: &'static str,
    b: &'static str,
    off: &'static str,
    on: &'static str,
) -> Probe {
    Probe { strategy, key, a, b, off, on }
}

/// One row per declared, non-ROUTE key of every enumerated strategy — exhaustive by
/// [`every_declared_key_has_a_probe_or_is_a_route_key`], so a knob added to a reader cannot join the
/// table without somebody stating what it changes.
///
/// ⚠ ROUTE keys (`symbol`, `venue`, `venues`, `symbol_a`/`symbol_b`) are deliberately absent, and
/// the exclusion is checked rather than assumed. Their "is this consumed" question has a different
/// and STRICTER answer one table over: `vike_strategy::PARAM_ROUTES` says the mount OVERRIDES them,
/// and `vike_strategy::misrouted_params` refuses a value that disagrees with the mount. Probing them
/// here would also mis-report `momentum`'s `venue`, which is a LABEL — two harnesses differing only
/// in their venue tag submit identical orders, which `registry.rs`'s
/// `the_harness_venue_is_a_label_and_not_an_order_destination` proves on purpose.
const PROBES: &[Probe] = &[
    // ---- buy_hold -----------------------------------------------------------------------------
    p("buy_hold", "size", "1.0", "7.5"),
    // ---- grid ---------------------------------------------------------------------------------
    p("grid", "anchor", "\"first\"", "\"fixed\""),
    // INSTANCE 4 of the class: unread unless the anchor mode is `fixed`.
    g("grid", "anchor_price", "0.4", "0.9", "", "anchor = \"fixed\"\n"),
    // ⚠ The three LADDER knobs are UNGATED, and that is the round-7 deletion rather than an
    // omission: they carried gate rows for the degenerate ladder (`arm` returns before reading any
    // of them once one is ≤ 0), which is a configuration where EVERY key is inert — so exempting
    // three of them from direction 2 bought nothing and cost the class-closer its input. See
    // `vike_strategy::PARAM_GATES`' doc and the executed demonstration in
    // `the_shapes_this_harness_cannot_see`. Ungated, they answer to direction 2 at the base table
    // like any other knob.
    p("grid", "step", "0.01", "0.05"),
    p("grid", "rungs", "1", "4"),
    p("grid", "size", "1.0", "6.0"),
    p("grid", "band", "0.02", "9.0"),
    p("grid", "bounded01", "false", "true"),
    // INSTANCE 5: consulted only inside `bounded01` branches. The `on` fragment also brings the
    // step inside the 0..1 market — with the default `step = 1.0` every rung is off-grid whatever
    // the tick is, so the armed half would compare two empty ladders and prove nothing.
    g("grid", "tick", "0.001", "0.45", "", "bounded01 = true\nstep = 0.05\n"),
    // ---- dca_accumulate -----------------------------------------------------------------------
    p("dca_accumulate", "side", "1", "-1"),
    p("dca_accumulate", "anchor", "\"first\"", "\"fixed\""),
    // ⚠ 100-scale values, unlike `grid`'s: `DcaAccumulate::arm` ladders in ONE direction and skips
    // any rung whose price has stepped past zero, so a sub-1.0 anchor at the default `step = 1.0`
    // rests nothing at all — and two empty ladders are equal for a reason that has nothing to do
    // with the gate. (An anchor PRICE is absolute, so the two-scale script does not rescale it.)
    g("dca_accumulate", "anchor_price", "40.0", "90.0", "", "anchor = \"fixed\"\n"),
    // ...and the same three ladder knobs, ungated for the same reason as `grid`'s above.
    p("dca_accumulate", "step", "0.01", "0.05"),
    p("dca_accumulate", "rungs", "1", "4"),
    p("dca_accumulate", "size", "1.0", "6.0"),
    p("dca_accumulate", "tp", "0.01", "9.0"),
    // ---- trailing_scalper ---------------------------------------------------------------------
    p("trailing_scalper", "qty", "1.0", "6.0"),
    p("trailing_scalper", "half_spread", "0.001", "0.2"),
    p("trailing_scalper", "exit_delay_ms", "0", "1000000"),
    p("trailing_scalper", "profit_target", "0.0", "0.05"),
    // INSTANCES 6-9: `entries_allowed` needs BOTH halves of each pair `> 0`, so each of the four
    // keys is inert while its partner is unset — which is the state the batch tool ships in.
    g("trailing_scalper", "entry_open_delay_ms", "1", "90000", "", "market_open_ms = 1\n"),
    g("trailing_scalper", "market_open_ms", "1", "90000", "", "entry_open_delay_ms = 1\n"),
    // ⚠ Both values must bite INSIDE the script's window. A 1 ms cutoff against a 100 s close
    // suppresses only the last ticks, by which time the scalper is HOLDING — and the Holding arm
    // never consults `entries_allowed` at all, so the met run would trace identically to the unmet
    // one and the row would look vacuous while being true.
    g(
        "trailing_scalper",
        "entry_cutoff_before_close_ms",
        "50000",
        "90000",
        "",
        "market_close_ms = 100000\n",
    ),
    g(
        "trailing_scalper",
        "market_close_ms",
        "1",
        "100000",
        "",
        "entry_cutoff_before_close_ms = 1\n",
    ),
    // ---- momentum -----------------------------------------------------------------------------
    p("momentum", "qty", "1.0", "6.0"),
    p("momentum", "threshold", "0.0", "1000000.0"),
    p("momentum", "tp", "0.5", "1000000.0"),
    p("momentum", "sl", "0.5", "1000000.0"),
    p("momentum", "time_limit_ms", "1", "1000000000"),
    p("momentum", "trailing", "0.5", "1000000.0"),
    p("momentum", "cooldown_ms", "0", "1000000000"),
    // ---- funding_carry ------------------------------------------------------------------------
    p("funding_carry", "qty", "1.0", "6.0"),
    p("funding_carry", "tp", "0.5", "1000000.0"),
    p("funding_carry", "sl", "0.5", "1000000.0"),
    p("funding_carry", "time_limit_ms", "1", "1000000000"),
    p("funding_carry", "trailing", "0.5", "1000000.0"),
    // ⚠ `0.0` rather than a small positive: the horizon AMORTIZES the one-time round-trip taker
    // cost, so two positive horizons that both clear `entry_threshold = 0` open the same legs at
    // the same sizes. Zero is the value that flips the decision, which is what a probe needs.
    p("funding_carry", "hold_periods", "0.0", "1000.0"),
    p("funding_carry", "entry_threshold", "0.0", "1000000.0"),
    p("funding_carry", "cooldown_ms", "0", "1000000000"),
    // ---- funding_capture ----------------------------------------------------------------------
    p("funding_capture", "threshold", "0.0", "1.0"),
    p("funding_capture", "qty", "1.0", "6.0"),
    // ---- pairs_zscore -------------------------------------------------------------------------
    p("pairs_zscore", "period", "3", "8"),
    p("pairs_zscore", "entry_z", "0.5", "1000.0"),
    p("pairs_zscore", "exit_z", "0.1", "0.9"),
    p("pairs_zscore", "beta", "1.0", "0.4"),
    p("pairs_zscore", "notional", "1000.0", "50.0"),
    p("pairs_zscore", "taker_fee", "0.0", "0.9"),
    p("pairs_zscore", "half_spread_bps", "0.0", "900000.0"),
    p("pairs_zscore", "hold_intervals", "1.0", "1000000.0"),
    // ⚠ SIGN-SENSITIVE, and not symmetrically: `spread_carry_cost` sums `ratio * mid * rate`, and
    // leg B's ratio is `-beta`. So a large POSITIVE `funding_a` and a large NEGATIVE `funding_b`
    // are the values that push the cost floor above the edge; the opposite signs make entry
    // EASIER, which the base table's near-zero rates already allow — and two tables that both
    // enter trace alike.
    p("pairs_zscore", "funding_a", "0.0", "900.0"),
    p("pairs_zscore", "funding_b", "0.0", "-900.0"),
    p("pairs_zscore", "max_half_life", "0.0", "0.001"),
];

// ===================================================================================================
// Helpers
// ===================================================================================================

fn table(src: &str) -> toml::value::Table {
    toml::from_str::<Value>(src)
        .unwrap_or_else(|e| panic!("test TOML {src:?}: {e}"))
        .as_table()
        .expect("a params fragment is a table")
        .clone()
}

fn a_value(src: &str) -> Value {
    toml::from_str::<Value>(&format!("v = {src}"))
        .unwrap_or_else(|e| panic!("test value {src:?}: {e}"))
        .get("v")
        .cloned()
        .expect("the wrapper table has `v`")
}

/// `base(strategy)` overlaid with `extra`, merged by INSERTION rather than by string concatenation —
/// a base may legitimately mention a key an overlay also sets (`pairs_zscore`'s does, several
/// times), and TOML rejects a duplicated key outright.
fn params(strategy: &str, extra: &str) -> Value {
    let mut t = table(base(strategy));
    for (k, v) in table(extra) {
        t.insert(k, v);
    }
    Value::Table(t)
}

/// The probe's own table: the base, plus its arming fragment when one is asked for, plus the key
/// under test at `value` (inserted LAST, so it always wins).
fn params_for(probe: &Probe, value: &str, extra: &str) -> Value {
    let mut t = match params(probe.strategy, extra) {
        Value::Table(t) => t,
        _ => unreachable!("`params` builds a table"),
    };
    t.insert(probe.key.to_string(), a_value(value));
    Value::Table(t)
}

/// Whether this key's declared gate is MET on the given table — read straight out of
/// [`vike_strategy::Gate::unmet`] over the real [`resolved_params`] rows, so a probe's `arm` is
/// checked against the gate it claims to arm rather than trusted to be the right fragment.
fn gate_met(probe: &Probe, params: &Value) -> bool {
    let Some(gate) = param_gate(probe.strategy, probe.key) else {
        return true;
    };
    let rows = resolved_params(probe.strategy, params).expect("an enumerated name reports rows");
    gate.unmet(&rows).is_empty()
}

fn route_keys(strategy: &str) -> Vec<&'static str> {
    match param_routes(strategy) {
        Some(ParamRoutes::SingleLeg(k)) | Some(ParamRoutes::MultiLeg(k, _)) => {
            k.iter().map(|(n, _)| *n).collect()
        }
        _ => Vec::new(),
    }
}

fn probe_for(strategy: &str, key: &str) -> Option<&'static Probe> {
    PROBES.iter().find(|p| p.strategy == strategy && p.key == key)
}

// ===================================================================================================
// Structure
// ===================================================================================================

/// Exhaustiveness — the same construction `live_capable_table_is_exhaustive` uses, and for the
/// harder reason: an unprobed key is one nobody ever asked whether the strategy reads, which is
/// precisely how five instances of this class shipped.
#[test]
fn every_declared_key_has_a_probe_or_is_a_route_key() {
    let mut probed = 0usize;
    for (name, keys) in PARAM_KEYS {
        let ParamKeys::Declared(declared) = keys else {
            continue;
        };
        let routes = route_keys(name);
        for (key, _) in declared.iter() {
            if routes.contains(key) {
                assert!(
                    probe_for(name, key).is_none(),
                    "{name}'s `{key}` is a ROUTE key AND has a probe row — its consumption question \
                     is answered by PARAM_ROUTES/misrouted_params, and probing it here would \
                     mis-report a venue LABEL as an inert knob"
                );
                continue;
            }
            assert!(
                probe_for(name, key).is_some(),
                "{name} declares `{key}` with no probe row in this file. Nothing then checks \
                 whether the strategy READS that value, which is the defect class this gate exists \
                 to close — add a row with two contrasting values (and, if it is only read under \
                 some other key, a PARAM_GATES row plus the `off`/`on` fragments that put its gate \
                 on each side of its condition)."
            );
            probed += 1;
        }
    }
    // Both directions at once: every non-route declared key found a row, and no row is left over.
    assert_eq!(probed, PROBES.len(), "a probe row names a key no PARAM_KEYS row declares");
    assert!(probed > 20, "only {probed} keys are probed — the gate has gone nearly vacuous");
    for probe in PROBES {
        let Some(ParamKeys::Declared(declared)) = vike_strategy::param_keys(probe.strategy) else {
            panic!("{} is probed but declares no keys", probe.strategy)
        };
        assert!(
            declared.iter().any(|(n, _)| *n == probe.key),
            "{}'s probe names `{}`, which is not a declared key",
            probe.strategy,
            probe.key
        );
        assert_ne!(
            probe.a, probe.b,
            "{}'s `{}` probe contrasts a value with itself",
            probe.strategy, probe.key
        );
    }
}

/// A probe's `off`/`on` fragments and its [`PARAM_GATES`] row must agree about whether the key is
/// gated at all — and the fragments must really put the DECLARED gate on either side of its
/// condition, which is asked of [`vike_strategy::Gate::unmet`] rather than assumed.
///
/// ⚠ Without this, a probe could "arm" a gate with a fragment that changes something else entirely
/// and the behavioural halves below would be measuring that instead.
#[test]
fn every_probe_matches_its_gate_row() {
    for probe in PROBES {
        let gated = param_gate(probe.strategy, probe.key).is_some();
        if !gated {
            assert!(
                probe.off.is_empty() && probe.on.is_empty(),
                "{}'s `{}` has no PARAM_GATES row, so it must carry no gate fragments — the row and \
                 the probe disagree about whether this key is read conditionally at all",
                probe.strategy,
                probe.key
            );
            continue;
        }
        assert!(
            !gate_met(probe, &params_for(probe, probe.a, probe.off)),
            "{}'s `{}`: its declared gate is still MET under the probe's `off` fragment, so the \
             inert half of `a_gated_key_is_inert_until_its_gate_is_armed` would prove nothing",
            probe.strategy,
            probe.key
        );
        assert!(
            gate_met(probe, &params_for(probe, probe.a, probe.on)),
            "{}'s `{}`: the probe's `on` fragment does not satisfy the gate PARAM_GATES declares \
             for it — the armed half would then be testing some other change",
            probe.strategy,
            probe.key
        );
    }
}

// ===================================================================================================
// The two behavioural directions
// ===================================================================================================

/// **Direction 2 — the class-closer.** A declared key with no gate row must MOVE the strategy.
///
/// This is the test that finds the next instance without anybody looking for it: it does not know
/// what `anchor_price` means, only that two different values of a key nothing declares conditional
/// must produce two different call traces. A key that fails here is either a new
/// conditionally-read key (add its [`PARAM_GATES`] row) or a key nothing reads at all (delete it).
#[test]
fn an_ungated_key_moves_the_strategy() {
    for probe in PROBES.iter().filter(|p| param_gate(p.strategy, p.key).is_none()) {
        let one = trace(probe.strategy, &params_for(probe, probe.a, ""), Feed::Plain);
        let other = trace(probe.strategy, &params_for(probe, probe.b, ""), Feed::Plain);
        assert_ne!(
            one, other,
            "{}'s `{}` = {} and = {} drove IDENTICAL calls, so nothing reads it at this strategy's \
             base table — while the mount echo still reports the value it resolved to, and an \
             operator who typed it has no way to tell. That is the declared-but-unconsumed class: \
             give it a PARAM_GATES row naming the key that disarms it, or delete the key.",
            probe.strategy, probe.key, probe.a, probe.b
        );
    }
}

/// **Direction 1 — every row is TRUE.** With its gate unmet the key changes NOTHING (which is what
/// licenses direction 2 to skip it); with its gate met it changes something (so the row is a
/// statement about a real knob rather than a dead one).
///
/// ⚠ The THIRD assertion is the non-vacuity guard, and it is not decoration: an inert half can be
/// two strategies that do nothing AT ALL, which is true but true for a reason that would also hold
/// if the gate row were nonsense. Requiring the `off` and `on` configurations to differ from each
/// other proves the two halves are genuinely two configurations. Round 7 deleted the six rows that
/// disarmed by making the whole ladder degenerate (`rungs = 0`) — this guard is what made that
/// weakness visible while they were here.
#[test]
fn a_gated_key_is_inert_until_its_gate_is_armed() {
    for probe in PROBES.iter().filter(|p| param_gate(p.strategy, p.key).is_some()) {
        let unmet_a = trace(probe.strategy, &params_for(probe, probe.a, probe.off), Feed::Plain);
        let unmet_b = trace(probe.strategy, &params_for(probe, probe.b, probe.off), Feed::Plain);
        assert_eq!(
            unmet_a, unmet_b,
            "{}'s `{}` CHANGED the strategy with its declared gate unmet — the row exempts a LIVE \
             knob from direction 2, which is the class-closer's input shrinking silently. Fix the \
             PARAM_GATES row.",
            probe.strategy, probe.key
        );
        let met_a = trace(probe.strategy, &params_for(probe, probe.a, probe.on), Feed::Plain);
        let met_b = trace(probe.strategy, &params_for(probe, probe.b, probe.on), Feed::Plain);
        assert_ne!(
            met_a, met_b,
            "{}'s `{}` changes nothing even with its gate MET, so the row is about a knob that does \
             nothing at all — the equality above would then hold for the trivial reason and prove \
             nothing.",
            probe.strategy, probe.key
        );
        assert_ne!(
            met_a, unmet_a,
            "{}'s `{}`: the `off` and `on` fragments drove the SAME strategy, so the two halves \
             above are one configuration compared with itself",
            probe.strategy, probe.key
        );
    }
}

/// The floor under both directions, and the one general check in this file with NO per-key claim in
/// it: **every strategy places at least one order at its base table.**
///
/// Two things rest on it. As a harness floor, an equality between two empty traces is not evidence
/// of anything — a strategy that never trades would make every one of its keys look inert, which is
/// direction 1 passing for the emptiest possible reason. And as a property in its own right, a
/// strategy that silently does nothing is worth catching: the class above is about a dead KEY, and
/// a whole dead CONFIGURATION is invisible to it, because both values of every key are equally dead
/// there — nothing differs, so nothing fails. Such configurations are reachable from an ordinary
/// table and are MEASURED, one row each, in [`the_shapes_this_harness_cannot_see`]'s `DEAD` ledger;
/// this is the floor they sit on.
///
/// ⚠ [`base`] is where a strategy that legitimately needs context declares it — `pairs_zscore` with
/// no legs and `funding_carry` with no venue map trade nothing by construction, and each row there
/// carries a written reason. A strategy that no-ops at its own defaults is a declared row, never a
/// silent pass here.
#[test]
fn the_scripted_market_moves_every_strategy() {
    for (name, keys) in PARAM_KEYS {
        if !matches!(keys, ParamKeys::Declared(_)) {
            continue;
        }
        assert!(
            calls(name, &params(name, "")) > 0,
            "{name} submitted NOTHING over the whole script at its base table. Either the strategy \
             silently does nothing at its own defaults — which is the defect, not the test — or the \
             base table/feed no longer gives it what it needs; meanwhile every probe under it is \
             comparing one silence with another."
        );
    }
}

/// Broker calls in a trace, with the two scale markers discounted — the plain "did this
/// configuration DO anything" question, as distinct from the trace EQUALITY the two directions ask.
fn calls(name: &str, params: &Value) -> usize {
    trace(name, params, Feed::Plain).iter().filter(|l| !l.starts_with("== scale")).count()
}

// ===================================================================================================
// The mutation self-tests
// ===================================================================================================

/// The harness itself must be able to fail: prove the trace distinguishes what it must and is not a
/// constant, and prove the two named instances of the class really are inert (so the rows that
/// declare them are not decoration).
///
/// A gate nobody proved can fail is how this class survived two rounds of review.
#[test]
fn the_harness_can_actually_fail() {
    let t =
        |name: &str, src: &str| trace(name, &toml::from_str::<Value>(src).unwrap(), Feed::Plain);
    // NOT a constant: the same strategy at two sizes traces differently...
    assert_ne!(t("buy_hold", "size = 1.0"), t("buy_hold", "size = 2.0"));
    // ...and at the SAME params it is deterministic, or every inequality above would be noise.
    assert_eq!(t("grid", "step = 0.02"), t("grid", "step = 0.02"));
    // ...and non-empty, or equality would hold for the emptiest reason there is.
    assert!(t("grid", "step = 0.02").len() > 10);

    // THE 4th INSTANCE, measured: a fixed anchor price on a first-price grid changes nothing.
    assert_eq!(
        t("grid", "anchor_price = 0.4"),
        t("grid", "anchor_price = 0.9"),
        "the reported finding: `anchor_price` is read only in `arm`'s AnchorMode::Fixed arm"
    );
    assert!(param_gate("grid", "anchor_price").is_some(), "...so it MUST carry a gate row");
    // THE 5th: `tick` outside a bounded-01 market.
    assert_eq!(t("grid", "tick = 0.001"), t("grid", "tick = 0.45"));
    assert!(param_gate("grid", "tick").is_some());
    // ...and both come alive once armed, which is what makes the rows claims rather than excuses.
    assert_ne!(
        t("grid", "anchor = \"fixed\"\nanchor_price = 0.4"),
        t("grid", "anchor = \"fixed\"\nanchor_price = 0.9")
    );
    assert_ne!(
        t("grid", "bounded01 = true\nstep = 0.05\ntick = 0.001"),
        t("grid", "bounded01 = true\nstep = 0.05\ntick = 0.45")
    );
    // ⚠ ...and the step is why the line above carries one: at the default `step = 1.0` EVERY rung
    // of a 0..1 grid is off the board whatever the tick is, so an armed comparison there compares
    // two empty ladders. Measured, not assumed — this is the shape that would have made the `tick`
    // row look proven while proving nothing.
    assert_eq!(
        t("grid", "bounded01 = true\ntick = 0.001"),
        t("grid", "bounded01 = true\ntick = 0.45"),
        "a bounded-01 grid at the default step rests nothing, so `tick` cannot show there"
    );
}

/// The source scanner in `crates/vike-strategy/tests/param_keys_gate.rs` CANNOT see this class, and
/// this asserts that rather than saying it.
///
/// Its unit of observation is the LOOKUP SITE, and every lookup site in every reader here is an
/// unconditional statement inside `from_params` — the reader reads the whole table into fields and
/// the strategy branches later, in another function, on a FIELD. So "notice a read sitting inside a
/// conditional" would find nothing to notice: a gate built that way would pass vacuously forever,
/// which is the exact failure mode this repo has hit three times.
///
/// The proof is the readers' own shape: `Grid::from_params` reads `anchor_price` in the same
/// straight-line struct literal as `step`, `size` and `band`, and it is only `Grid::anchor_at` —
/// with no `params` in scope at all — that decides whether the value is ever looked at.
#[test]
fn the_lookup_sites_of_this_class_are_unconditional() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/grid_dca.rs"),
    )
    .expect("grid_dca.rs");
    // The two ends of the class, in one straight-line initializer: the gated key and an ungated one
    // sit at the SAME syntactic depth, so no scanner over lookup sites can tell them apart.
    assert!(
        src.contains("anchor_price: f(\"anchor_price\").unwrap_or(d.anchor_price),"),
        "the gated key's lookup has moved — re-check whether a source scan could now see it"
    );
    assert!(
        src.contains("band: f(\"band\").unwrap_or(d.band),"),
        "the ungated key's lookup has moved"
    );
    // ...and the CONDITION is in another function, over a FIELD, with no params table in scope.
    assert!(
        src.contains("AnchorMode::Fixed => self.anchor_price,"),
        "the branch that decides whether `anchor_price` is ever read has moved out of \
         `Grid::anchor_at` / `DcaAccumulate::arm`"
    );
}

/// The residual, DEMONSTRATED. Each block below runs the case it describes, so the blind spot is
/// measured rather than assumed — and can be neither quietly widened nor quietly closed.
///
/// Three shapes: (1) inertness that depends on the FEED, (2) inertness only at a NON-BASE
/// combination, and (3) a DEAD CONFIGURATION — a table under which the strategy places no order at
/// all. (3) is the one that outlived rounds 6 and 7: it is not a dead key, so neither direction can
/// see it, and its rows are a measured ledger rather than a rule.
#[test]
fn the_shapes_this_harness_cannot_see() {
    let with = |name: &str, src: &str, feed: Feed| {
        trace(name, &toml::from_str::<Value>(src).unwrap(), feed)
    };

    // (1) MARKET-dependent inertness. `PairsZScore` reads `funding_a`/`funding_b` only as the
    //     FALLBACK for a bar that carries no funding of its own
    //     (`self.last_funding_a.unwrap_or(self.funding_a)`), so on a funding-BEARING feed both keys
    //     are inert — a condition no predicate over the params table could express, because the
    //     other half of it is the feed. This harness plays a funding-free `A`/`B` and therefore
    //     reports them live; here is the feed on which they are not.
    let pairs = |funding_a: &str, feed: Feed| {
        trace("pairs_zscore", &params("pairs_zscore", &format!("funding_a = {funding_a}\n")), feed)
    };
    assert_ne!(
        pairs("0.0", Feed::Plain),
        pairs("900.0", Feed::Plain),
        "the harness's own feed leaves `funding_a` live — that is what direction 2 measures"
    );
    assert_eq!(
        pairs("0.0", Feed::FundedPairLegs),
        pairs("900.0", Feed::FundedPairLegs),
        "...and on a funding-bearing feed the SAME key is inert, with no params-table gate able to \
         say so. Declared blind spot: a `PARAM_GATES` row is a predicate over the params, so \
         inertness that depends on the FEED is out of its reach by construction."
    );
    assert!(
        param_gate("pairs_zscore", "funding_a").is_none(),
        "no gate row is demanded for it, and none could be written"
    );

    // (2) Inertness only at a NON-BASE combination. Direction 2 measures each key at its strategy's
    //     base table, so a key that goes inert only under some other setting is not demanded a row.
    //     `grid`'s `band` is the executed example: with `bounded01` on, a band wider than the
    //     0..1 market is swallowed whole by the wall clamps in `Grid::arm`.
    assert_ne!(
        with("grid", "band = 0.02", Feed::Plain),
        with("grid", "band = 9.0", Feed::Plain),
        "at the base table `band` is live"
    );
    assert_eq!(
        with("grid", "bounded01 = true\nband = 9.0", Feed::Plain),
        with("grid", "bounded01 = true\nband = 99.0", Feed::Plain),
        "...and two bands that both exceed the 0..1 walls are indistinguishable. Declared blind \
         spot: the probe is one base table per strategy, not the cross-product of every key."
    );
    assert!(param_gate("grid", "band").is_none());

    // (3) A DEAD CONFIGURATION — the whole strategy silently places no order. This is the shape
    //     that outlived rounds 6 and 7, and it is invisible to BOTH directions above by
    //     construction: direction 2 compares two values of ONE key, and here both values of EVERY
    //     key are equally dead, so nothing differs and nothing fails. It is not a dead key; it is a
    //     dead table.
    //
    //     Each row is MEASURED — zero calls, against a base table that trades — so the finding is a
    //     number rather than a memory. ⚠ A row that stops being zero means the strategy was fixed;
    //     deleting the row is that fix's other half, not a workaround. The general floor under all
    //     of them is `the_scripted_market_moves_every_strategy`, which carries no per-key claim.
    //
    //     ⚠ Each row is also REFUSED AT LOAD now, and asserts that too. `unarmable_params` asks the
    //     ladder builders themselves whether any anchor these params permit rests a rung, and the
    //     daemon refuses the profile when none does. That does not close THIS blind spot — the rows
    //     below are still invisible to both directions above, because this harness drives the
    //     strategy directly and never sees a profile — it means the shape can no longer reach a live
    //     mount. The two halves are deliberately separate: a measurement here, a gate one layer up,
    //     and a row that loses either half is a row that has stopped being true.
    //
    //     ⚠ Rounds 6 and 7 tried to state part of this on the mount line as `(inert: …)`, and it is
    //     why that marking is gone (`vike_strategy::PARAM_GATES`' doc carries the history): an
    //     annotation can reach at most the keys a params predicate can name, while the
    //     configuration kills every key — including the ROUTE key, whose consumption question
    //     belongs to a different table entirely — so the line read as a statement that the unmarked
    //     knobs were in force.
    const DEAD: &[(&str, &str, &str)] = &[
        (
            "grid",
            "rungs = 0",
            "the degenerate ladder: `Grid::arm` returns at `rungs == 0 || size <= 0.0 || \
             step <= 0.0`, so nothing is ever rested",
        ),
        ("dca_accumulate", "rungs = 0", "...and `DcaAccumulate::arm`'s identical guard"),
        (
            "dca_accumulate",
            "anchor = \"fixed\"",
            "a FIXED anchor at its compiled default `anchor_price = 0`: every rung prices at \
             `0 - side * k * step` <= 0 and is skipped, while `drive` has already stamped \
             `anchor = Some(0.0)` so it never re-arms. An operator who armed the anchor MODE and \
             left the price unset gets a mount that can never trade",
        ),
        (
            "grid",
            "bounded01 = true",
            "a 0..1 grid at the compiled default `step = 1.0`: every rung is off the board — \
             `the_harness_can_actually_fail` leans on the same fact from the other side",
        ),
    ];
    for (name, src, why) in DEAD {
        assert!(
            calls(name, &params(name, "")) > 0,
            "{name}'s base table must trade, or the contrast below is empty"
        );
        let table: Value = toml::from_str(src).unwrap();
        assert_eq!(
            calls(name, &table),
            0,
            "`{name}` with `{src}` placed NO order at all when this row was measured ({why}) and \
             now places some — the strategy was fixed, so delete this row"
        );
        // ...and the OTHER half: a table this dead must not be mountable. The gate is one layer up
        // (a daemon profile), so what is asserted here is the predicate it consults.
        assert!(
            unarmable_params(name, &table).is_some(),
            "`{name}` with `{src}` rests nothing (measured, one line above) and \
             `unarmable_params` does NOT refuse it — the dead configuration is mountable again"
        );
        // ...and the dead table is dead for EVERY key, which is what makes it a configuration-level
        // fact rather than a key-level one: two values of any probed key trace identically under it.
        for probe in PROBES.iter().filter(|p| p.strategy == *name) {
            let one = params_for(probe, probe.a, src);
            let other = params_for(probe, probe.b, src);
            if calls(name, &one) > 0 || calls(name, &other) > 0 {
                continue; // the probe's own value re-animates the table (`rungs = 4` over `rungs = 0`)
            }
            assert_eq!(
                trace(name, &one, Feed::Plain),
                trace(name, &other, Feed::Plain),
                "{name}'s `{}` must be inert under `{src}`",
                probe.key
            );
        }
        // ...including the ROUTE key, which no gate table may name at all
        // (`every_declared_key_has_a_probe_or_is_a_route_key` refuses it a row on purpose) and which
        // genuinely routes the orders at the base table.
        assert_ne!(
            with(name, "symbol = \"A\"", Feed::Plain),
            with(name, "symbol = \"Z\"", Feed::Plain),
            "{name}'s `symbol` really does route the orders at the base table"
        );
        assert_eq!(
            with(name, &format!("{src}\nsymbol = \"A\""), Feed::Plain),
            with(name, &format!("{src}\nsymbol = \"Z\""), Feed::Plain),
            "...and is inert under `{src}` like everything else"
        );
    }

    // (4) The direction the residual CANNOT hide in. A key that changes nothing purely by
    //     coincidence of this script fails direction 2 LOUDLY (it looks like a missing gate row);
    //     it can never pass silently. So the residual only ever costs a false alarm, never a miss
    //     of the kind this file exists to prevent — which is why (1)-(3) are stated as the whole of
    //     it.
    assert_eq!(
        with("buy_hold", "size = 1.0", Feed::Plain),
        with("buy_hold", "size = 1.0", Feed::Plain)
    );
}

/// The SOUNDNESS half of the load-time refusal, DRIVEN rather than argued: every table
/// [`vike_strategy::unarmable_params`] rejects must place ZERO orders over the whole scripted
/// market, at both scales.
///
/// ⚠ This is the direction that can do damage. A refusal that is too WEAK leaves a dead mount
/// mountable, which is the defect it was written for and which the `DEAD` ledger above pins one row
/// at a time. A refusal that is too STRONG rejects a profile that would have traded — the mistake
/// PR #1184 made once in the other direction, when it marked a key inert that a shipped profile
/// shape legitimately leaves inert. So the tables below are a CROSS PRODUCT rather than a handful
/// of hand-picked rows: whatever combination the predicate refuses, the harness must independently
/// find silent.
///
/// The converse is deliberately NOT asserted, because it is false and should be: a table can rest
/// rungs at some anchor and still trade nothing on THIS script (a bounded grid whose step only fits
/// near a wall rests nothing at the script's mid-domain first price). That is a live configuration
/// on the wrong market, not a dead one, and refusing it would be exactly the over-refusal above.
#[test]
fn nothing_the_refusal_rejects_would_have_traded() {
    /// Ladder fragments, combined pairwise — the knobs that decide whether a rung can rest at all,
    /// each at a value that kills the ladder and one that keeps it alive.
    const FRAGMENTS: &[&str] = &[
        "",
        "rungs = 0",
        "rungs = -5",
        "rungs = 4",
        "size = 0.0",
        "size = 2.0",
        "step = 0.0",
        "step = 0.05",
        "step = 1.0",
        "anchor = \"fixed\"",
        "anchor = \"fixed\"\nanchor_price = 0.5",
        "bounded01 = true",
        "bounded01 = true\ntick = 0.02",
        "side = \"short\"",
    ];
    let merged = |a: &str, b: &str| {
        let mut t = table(a);
        for (k, v) in table(b) {
            t.insert(k, v);
        }
        Value::Table(t)
    };
    let (mut refused, mut armable) = (0usize, 0usize);
    for name in ["grid", "dca_accumulate"] {
        for a in FRAGMENTS {
            for b in FRAGMENTS {
                let params = merged(a, b);
                if unarmable_params(name, &params).is_none() {
                    armable += 1;
                    continue;
                }
                refused += 1;
                assert_eq!(
                    calls(name, &params),
                    0,
                    "`{name}` with `{a}` + `{b}` is REFUSED at load, and yet it placed orders over \
                     the scripted market — the refusal is rejecting a mount that would have traded"
                );
            }
        }
    }
    // Both halves non-empty, or the assertion above holds for the emptiest possible reason.
    assert!(refused > 20, "only {refused} tables were refused — the sweep has gone nearly vacuous");
    assert!(armable > 20, "only {armable} tables survived — the refusal is rejecting everything");
}

/// The floor: the whole roster is either enumerated-and-probed or explicitly not enumerated, so this
/// file cannot pass by checking a shrinking set.
#[test]
fn the_gate_has_a_non_empty_input() {
    for name in PORTABLE_STRATEGIES {
        match vike_strategy::param_keys(name) {
            Some(ParamKeys::Declared(declared)) => {
                let routes = route_keys(name);
                let knobs = declared.iter().filter(|(k, _)| !routes.contains(k)).count();
                assert_eq!(
                    PROBES.iter().filter(|p| p.strategy == *name).count(),
                    knobs,
                    "{name} declares {knobs} non-route keys but this file probes a different number"
                );
            }
            Some(ParamKeys::NotEnumerated(_)) => assert!(
                !PROBES.iter().any(|p| p.strategy == *name),
                "{name} enumerates no keys, so it can have no probe rows"
            ),
            None => panic!("{name} is on the roster with no PARAM_KEYS row"),
        }
    }
    assert!(!PARAM_GATES.is_empty(), "PARAM_GATES is empty — direction 1 checks nothing");
}
