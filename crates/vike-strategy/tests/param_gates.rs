//! Pins [`vike_strategy::PARAM_GATES`] — and the ABSENCE of a row — against what the strategies
//! actually DO, by driving them.
//!
//! # The class this closes
//!
//! A `[strategy.params]` key spelled right ([`vike_strategy::unknown_params`]), typed right
//! ([`vike_strategy::mistyped_params`]), routed right ([`vike_strategy::misrouted_params`]) and
//! read by `from_params`, whose value the strategy then never LOOKS AT because another key sent it
//! down a different branch. Five review rounds each found one by reading a reader; no test did.
//!
//! Its one observable is behavioural: **two params tables differing ONLY in key `K` drive the
//! strategy to the SAME calls.** That is what this file measures; nothing here reads a doc comment
//! or a field name.
//!
//! # Why not the source scan `param_keys_gate.rs` runs
//!
//! Every LOOKUP SITE is an unconditional statement inside `from_params`; the condition lives at the
//! FIELD-USE site in another function (`crates/vike-strategy/src/strategies/grid_dca.rs`'s
//! `Grid::arm` reads `self.anchor_price` only in the `AnchorMode::Fixed` arm), so a scan for "a
//! read inside a conditional" would be vacuously green forever.
//! `the_lookup_sites_of_this_class_are_unconditional` (`param_gates/blind_spots.rs`) asserts that.
//!
//! # The two directions
//!
//! - [`an_ungated_key_moves_the_strategy`] — direction 2, the CLASS-CLOSER: every declared key
//!   with no [`vike_strategy::PARAM_GATES`] row must change the call trace at its strategy's base
//!   table, so a new instance fails HERE, by name, whether or not anybody looked for it.
//! - [`a_gated_key_is_inert_until_its_gate_is_armed`] — direction 1: every row must be TRUE
//!   (inert with the gate unmet, live with it armed). A row exempting a LIVE key shrinks
//!   direction 2's input.
//!
//! ⚠ **[`vike_strategy::PARAM_GATES`] is this file's own input and nothing else's**: it tells
//! direction 2 what to exempt and direction 1 what to prove. It is NOT an annotation source
//! (`vike_strategy::PARAM_GATES`' own doc records why the mount-line marking was deleted).
//!
//! # The residual, measured rather than assumed
//!
//! `crates/vike-strategy/tests/param_gates/blind_spots.rs`'s `the_shapes_this_harness_cannot_see`
//! DEMONSTRATES each blind spot with an executed example, so the hole can be neither quietly
//! widened nor quietly closed. One shape is a DEAD CONFIGURATION — a table under which the
//! strategy places no order at all, so both values of every key are equally dead and neither
//! direction sees it. Its `DEAD` table is the measured ledger;
//! [`the_scripted_market_moves_every_strategy`] is the general floor under it.
//!
//! ⚠ **Every `DEAD` row is also REFUSED AT LOAD, and asserts it.**
//! [`vike_strategy::unarmable_params`] asks the ladder builders whether a table can rest a rung,
//! and the daemon's profile validation (`crates/vike-tradehub/src/config/validate.rs`'s
//! `validate_strategy`) refuses it when it cannot. This harness drives strategies one layer beneath
//! any profile, so it still cannot SEE the shape. A row that stops being ZERO means the strategy
//! was fixed; deleting it is that fix's other half.

use toml::Value;

use vike_strategy::{
    PARAM_GATES, PARAM_KEYS, PORTABLE_STRATEGIES, ParamKeys, ParamRoutes, param_gate, param_routes,
    resolved_params, strategy_by_name,
};

#[path = "param_gates/blind_spots.rs"]
mod blind_spots;
#[path = "param_gates/market.rs"]
mod market;
#[path = "param_gates/trace_broker.rs"]
mod trace_broker;

use market::{Ev, Feed, ROUNDS, events, feed_symbols};
use trace_broker::TraceBroker;

/// Resolve `name` with `params` and drive it through the whole script at BOTH price scales, one
/// fresh strategy per scale, returning every broker call it made. Two scales because `tick` and
/// `bounded01` mean something only inside `(0, 1)` while `band`/`step` bite at 100 scale;
/// concatenating the passes can only ADD discriminating power.
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

/// The MINIMAL context in which a strategy trades at all (empty when the script exercises it as
/// it comes). ⚠ Each non-empty base is a NECESSITY with a stated reason: a base that quietly armed
/// something else would weaken every probe under it.
fn base(strategy: &str) -> &'static str {
    match strategy {
        // The harness stamps a cooldown only when an executor CLOSES, so `cooldown_ms` (and the
        // barrier legs) need one barrier armed or nothing ever completes.
        "momentum" => "tp = 3.0\n",
        // A carry needs TWO venues in the funding book (filled per (venue, symbol) through the
        // harness venue map) before `best_carry_to_open` returns anything.
        "funding_carry" => "tp = 3.0\n[venues]\nF1 = \"binance\"\nF2 = \"okx\"\n",
        // Two legs, a window short enough for 12 bars, and a cost floor low enough to cross —
        // else every cost knob reads inert because nothing ever entered.
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
    /// Overlay that leaves the gate UNMET (`""` when the base table already does).
    off: &'static str,
    /// Overlay that MEETS the gate (`""` only if the base table already does — no gated row today).
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
/// [`every_declared_key_has_a_probe_or_is_a_route_key`], so a knob added to a reader cannot join
/// without somebody stating what it changes.
///
/// ⚠ ROUTE keys (`symbol`, `venue`, `venues`, `symbol_a`/`symbol_b`) are deliberately absent, and
/// that is checked: `vike_strategy::PARAM_ROUTES`/`misrouted_params` answer their consumption
/// question more strictly, and probing `momentum`'s `venue` (a LABEL —
/// `the_harness_venue_is_a_label_and_not_an_order_destination`) would mis-report it as inert.
const PROBES: &[Probe] = &[
    // ---- buy_hold -----------------------------------------------------------------------------
    p("buy_hold", "size", "1.0", "7.5"),
    // ---- grid ---------------------------------------------------------------------------------
    p("grid", "anchor", "\"first\"", "\"fixed\""),
    // INSTANCE 4 of the class: unread unless the anchor mode is `fixed`.
    g("grid", "anchor_price", "0.4", "0.9", "", "anchor = \"fixed\"\n"),
    // ⚠ The three LADDER knobs are UNGATED on purpose: a degenerate ladder (one of them ≤ 0) makes
    // EVERY key inert, so gating three of them only cost direction 2 its input (see
    // `vike_strategy::PARAM_GATES`' doc and `the_shapes_this_harness_cannot_see`).
    p("grid", "step", "0.01", "0.05"),
    p("grid", "rungs", "1", "4"),
    p("grid", "size", "1.0", "6.0"),
    p("grid", "band", "0.02", "9.0"),
    p("grid", "bounded01", "false", "true"),
    // INSTANCE 5: consulted only inside `bounded01` branches. `on` also sets a 0..1 step: at the
    // default `step = 1.0` every rung is off-grid, and the armed half would compare empty ladders.
    g("grid", "tick", "0.001", "0.45", "", "bounded01 = true\nstep = 0.05\n"),
    // ---- dca_accumulate -----------------------------------------------------------------------
    p("dca_accumulate", "side", "1", "-1"),
    p("dca_accumulate", "anchor", "\"first\"", "\"fixed\""),
    // ⚠ 100-scale values, unlike `grid`'s: `DcaAccumulate::arm` ladders ONE way and skips rungs
    // past zero, so a sub-1.0 anchor at `step = 1.0` rests nothing (an anchor PRICE is absolute;
    // the two-scale script does not rescale it).
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
    // ⚠ Both values must bite INSIDE the script's window: a 1 ms cutoff against a 100 s close
    // suppresses only the last ticks, when the scalper is HOLDING and never consults
    // `entries_allowed`, so the row would look vacuous while being true.
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
    // ⚠ `0.0`, not a small positive: the horizon AMORTIZES the round-trip taker cost, so two
    // positive horizons that both clear `entry_threshold = 0` open the same legs. Zero flips it.
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
    // ⚠ SIGN-SENSITIVE: `spread_carry_cost` sums `ratio * mid * rate` and leg B's ratio is
    // `-beta`, so only a large POSITIVE `funding_a` / NEGATIVE `funding_b` lifts the cost floor
    // above the edge; the opposite signs make entry easier, and two tables that both enter trace
    // alike.
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

/// `base(strategy)` overlaid with `extra`, merged by INSERTION: a base may set a key an overlay
/// also sets, and TOML rejects a duplicated key.
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

/// Whether this key's declared gate is MET on the given table, asked of
/// [`vike_strategy::Gate::unmet`] over the real [`resolved_params`] rows rather than trusted.
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

/// Exhaustiveness: an unprobed key is one nobody asked whether the strategy reads, which is how
/// five instances of this class shipped.
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
/// gated, and the fragments must really put the DECLARED gate on either side of its condition.
/// ⚠ Otherwise a fragment could change something else and the behavioural halves measure that.
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

/// **Direction 2 — the class-closer.** A declared key with no gate row must MOVE the strategy:
/// two values of a key nothing declares conditional must produce two different call traces. A key
/// failing here is conditionally read (add its [`PARAM_GATES`] row) or read by nothing (delete it).
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

/// **Direction 1 — every row is TRUE.** With its gate unmet the key changes NOTHING (licensing
/// direction 2 to skip it); with its gate met it changes something (the knob is real).
///
/// ⚠ The THIRD assertion is the non-vacuity guard: an inert half can be two strategies that do
/// nothing AT ALL, true even for a nonsense row. Requiring `off` and `on` to differ proves the two
/// halves are two configurations (it is what exposed the `rungs = 0` rows round 7 deleted).
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

/// The floor under both directions, with NO per-key claim: **every strategy places at least one
/// order at its base table.** Two empty traces are equal for the emptiest reason, and a dead
/// CONFIGURATION is invisible to the per-key class; such configurations are MEASURED one row each
/// in `the_shapes_this_harness_cannot_see`'s `DEAD` ledger, and this is the floor they sit on.
///
/// ⚠ [`base`] is where a strategy that legitimately needs context declares it, with a written
/// reason (`pairs_zscore` with no legs, `funding_carry` with no venue map trade nothing). A
/// strategy that no-ops at its own defaults is a declared row, never a silent pass here.
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

/// Broker calls in a trace, scale markers discounted: "did this configuration DO anything".
fn calls(name: &str, params: &Value) -> usize {
    trace(name, params, Feed::Plain).iter().filter(|l| !l.starts_with("== scale")).count()
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
