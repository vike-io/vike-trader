//! Pins each roster file's header `needs` — and the ABSENCE of one — against what the strategies
//! actually DO, by driving them. A `needs` entry is the deleted registry's `PARAM_GATES` row of that
//! key, moved into the file's header; this gate moved with it, from `vike-strategy`'s tests, re-pointed
//! at the shipped files (the roster: `param_gates/roster.rs`).
//!
//! # The class this closes
//!
//! A `[strategy.params]` key spelled right (`StrategyFileHeader::unknown_params`), typed right
//! (`StrategyFileHeader::mistyped_params`), routed right (`StrategyFileHeader::misrouted_params`)
//! and read by `from_params`, whose value the strategy then never LOOKS AT because another key sent
//! it down a different branch. Five review rounds each found one by reading a reader; no test did.
//!
//! Its one observable is behavioural: **two params tables differing ONLY in key `K` drive the
//! strategy to the SAME calls.** That is what this file measures; nothing here reads a doc comment
//! or a field name.
//!
//! # Why not the source scan `param_keys_gate.rs` runs
//!
//! Every LOOKUP SITE is an unconditional statement inside `from_params`; the condition lives at the
//! FIELD-USE site in another function (`strategies/grid_dca/grid.rs`'s `Grid::anchor_at` reads
//! `self.anchor_price` only in the `AnchorMode::Fixed` arm), so a scan for "a read inside a
//! conditional" would be vacuously green forever.
//! `the_lookup_sites_of_this_class_are_unconditional` (`param_gates/blind_spots.rs`) asserts that.
//!
//! # The two directions
//!
//! - [`an_ungated_key_moves_the_strategy`] — direction 2, the CLASS-CLOSER: every declared key
//!   with no `needs` must change the call trace at its file's base table, so a new instance fails
//!   HERE, by name, whether or not anybody looked for it.
//! - [`a_gated_key_is_inert_until_its_gate_is_armed`] — direction 1: every `needs` must be TRUE
//!   (inert with the gate unmet, live with it armed). A `needs` exempting a LIVE key shrinks
//!   direction 2's input.
//!
//! ⚠ **A header's `needs` is this file's input**: it tells direction 2 what to exempt and direction
//! 1 what to prove. No mount line marks an inert key with it (the deleted registry's `PARAM_GATES`
//! doc recorded why: a marker is a positive claim about a key, and making it correctly across every
//! combination of every other key is a static analysis, not a documentation feature).
//!
//! # The residual, measured rather than assumed
//!
//! `crates/vike-strategies-check/tests/param_gates/blind_spots.rs`'s
//! `the_shapes_this_harness_cannot_see` DEMONSTRATES each blind spot with an executed example, so
//! the hole can be neither quietly widened nor quietly closed. One shape is a DEAD CONFIGURATION — a
//! table under which the strategy places no order at all, so both values of every key are equally
//! dead and neither direction sees it. Its `DEAD` table is the measured ledger;
//! [`the_scripted_market_moves_every_strategy`] is the general floor under it.
//!
//! ⚠ **Every `DEAD` row is also REFUSED AT LOAD, and asserts it.** The file's `refuse` export (the
//! deleted registry's `unarmable_params`) asks the ladder builders whether a table can rest a rung,
//! and a mount asks it before any strategy exists (`crates/vike-rust-strategy/src/host.rs`'s
//! `refusal`). This harness drives strategies one layer beneath any mount, so it still cannot SEE
//! the shape. A row that stops being ZERO means the strategy was fixed; deleting it is that fix's
//! other half.

use vike_model::params::{ParamValue, Params};
use vike_strategies_check::takers;

#[path = "param_gates/blind_spots.rs"]
mod blind_spots;
#[path = "param_gates/market.rs"]
mod market;
// `param_keys_gate.rs` compiles the same file: a shared test module, so an `allow`.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "param_gates/roster.rs"]
mod roster;
#[path = "param_gates/trace_broker.rs"]
mod trace_broker;

use market::{Feed, ROUNDS, RecordedEvent, events, feed_symbols};
use roster::{
    BUY_HOLD, DCA_ACCUMULATE, FUNDING_CAPTURE, FUNDING_CARRY, GRID, MOMENTUM, OUT, PAIRS_ZSCORE,
    ROSTER, TRAILING_SCALPER, header,
};
use trace_broker::TraceBroker;

/// Build `file` with `params` (the shipped file, through the test stand-in) and drive it through the
/// whole script at BOTH price scales, one fresh strategy per scale, returning every broker call it
/// made. Two scales because `tick` and `bounded01` mean something only inside `(0, 1)` while
/// `band`/`step` bite at 100 scale; concatenating the passes can only ADD discriminating power.
fn trace(file: &str, params: &Params, feed: Feed) -> Vec<String> {
    let mut log = Vec::new();
    for scale in [1.0_f64, 100.0] {
        log.push(format!("== scale {scale}"));
        let mut strategy = vike_strategies_check::stand_in::<TraceBroker>(file, params)
            .unwrap_or_else(|e| panic!("{file} must build: {e}"));
        let mut broker = TraceBroker::default();
        for event in events(scale, feed, feed_symbols(file)) {
            match event {
                RecordedEvent::Bar(bar) => {
                    broker.now = bar.timestamp;
                    broker.price = bar.close;
                    let symbol = bar.symbol.clone().unwrap_or_default();
                    broker.bars.entry(symbol).or_default().push(bar.clone());
                    strategy.on_bar(&mut broker, &bar);
                }
                RecordedEvent::Quote(q) => {
                    broker.now = q.timestamp;
                    broker.price = q.mid();
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

/// The file's `echo` export: what each declared key RESOLVED to (the deleted registry's
/// `resolved_params`), the rows its header's `needs` are judged against.
fn echo(file: &str) -> fn(&Params) -> Vec<(&'static str, String)> {
    match file {
        BUY_HOLD => takers::buy_hold::echo,
        GRID => takers::grid::echo,
        DCA_ACCUMULATE => takers::dca_accumulate::echo,
        TRAILING_SCALPER => takers::trailing_scalper::echo,
        MOMENTUM => takers::momentum::echo,
        FUNDING_CARRY => takers::funding_carry::echo,
        FUNDING_CAPTURE => takers::funding_capture::echo,
        PAIRS_ZSCORE => takers::pairs_zscore::echo,
        other => panic!("{other} is not on the roster, so it reports no rows here"),
    }
}

// ===================================================================================================
// The probe table
// ===================================================================================================

/// The MINIMAL context in which a strategy trades at all (empty when the script exercises it as
/// it comes). ⚠ Each non-empty base is a NECESSITY with a stated reason: a base that quietly armed
/// something else would weaken every probe under it.
fn base(file: &str) -> &'static str {
    match file {
        // The harness stamps a cooldown only when an executor CLOSES, so `cooldown_ms` (and the
        // barrier legs) need one barrier armed or nothing ever completes.
        MOMENTUM => "tp = 3.0\n",
        // A carry needs TWO venues in the funding book (filled per (venue, symbol) through the
        // harness venue map) before `best_carry_to_open` returns anything.
        FUNDING_CARRY => "tp = 3.0\n[venues]\nF1 = \"binance\"\nF2 = \"okx\"\n",
        // Two legs, a window short enough for 12 bars, and a cost floor low enough to cross —
        // else every cost knob reads inert because nothing ever entered.
        PAIRS_ZSCORE => {
            "symbol_a = \"A\"\nsymbol_b = \"B\"\nperiod = 3\nentry_z = 0.5\nexit_z = 0.1\n\
             taker_fee = 0.0\nhalf_spread_bps = 0.0\nhold_intervals = 1.0\nfunding_a = 0.001\n\
             funding_b = -0.001\n"
        }
        _ => "",
    }
}

/// One key, two contrasting values, and — for a gated key — the two TOML fragments that put its
/// header `needs` on each side of its condition.
struct Probe {
    file: &'static str,
    key: &'static str,
    /// Two values of `key`, as TOML value text (the right-hand side of `key = …`).
    a: &'static str,
    b: &'static str,
    /// Overlay that leaves the gate UNMET (`""` when the base table already does).
    off: &'static str,
    /// Overlay that MEETS the gate (`""` only if the base table already does — no gated row today).
    on: &'static str,
}

const fn p(file: &'static str, key: &'static str, a: &'static str, b: &'static str) -> Probe {
    Probe { file, key, a, b, off: "", on: "" }
}

/// A GATED probe: the same row plus the two fragments. Both are checked against
/// `StrategyFileHeader::unmet_needs` rather than trusted (`every_probe_matches_its_gate_row`).
const fn g(
    file: &'static str,
    key: &'static str,
    a: &'static str,
    b: &'static str,
    off: &'static str,
    on: &'static str,
) -> Probe {
    Probe { file, key, a, b, off, on }
}

/// One row per declared, non-ROUTE key of every roster file — exhaustive by
/// [`every_declared_key_has_a_probe_or_is_a_route_key`], so a knob added to a reader cannot join
/// without somebody stating what it changes.
///
/// ⚠ ROUTE keys (`symbol`, `venue`, `venues`, `symbol_a`/`symbol_b`) are deliberately absent, and
/// that is checked: the header's `route` and `StrategyFileHeader::misrouted_params` answer their
/// consumption question more strictly, and probing `momentum`'s `venue` (a LABEL: no `Broker`
/// submit verb takes a venue, so its value changes no call) would mis-report it as inert.
const PROBES: &[Probe] = &[
    // ---- buy_hold -----------------------------------------------------------------------------
    p(BUY_HOLD, "size", "1.0", "7.5"),
    // ---- grid ---------------------------------------------------------------------------------
    p(GRID, "anchor", "\"first\"", "\"fixed\""),
    // INSTANCE 4 of the class: unread unless the anchor mode is `fixed`.
    g(GRID, "anchor_price", "0.4", "0.9", "", "anchor = \"fixed\"\n"),
    // ⚠ The three LADDER knobs are UNGATED on purpose: a degenerate ladder (one of them ≤ 0) makes
    // EVERY key inert, so gating three of them only cost direction 2 its input (the deleted
    // registry's `PARAM_GATES` doc, and `the_shapes_this_harness_cannot_see`).
    p(GRID, "step", "0.01", "0.05"),
    p(GRID, "rungs", "1", "4"),
    p(GRID, "size", "1.0", "6.0"),
    p(GRID, "band", "0.02", "9.0"),
    p(GRID, "bounded01", "false", "true"),
    // INSTANCE 5: consulted only inside `bounded01` branches. `on` also sets a 0..1 step: at the
    // default `step = 1.0` every rung is off-grid, and the armed half would compare empty ladders.
    g(GRID, "tick", "0.001", "0.45", "", "bounded01 = true\nstep = 0.05\n"),
    // ---- dca_accumulate -----------------------------------------------------------------------
    p(DCA_ACCUMULATE, "side", "1", "-1"),
    p(DCA_ACCUMULATE, "anchor", "\"first\"", "\"fixed\""),
    // ⚠ 100-scale values, unlike `grid`'s: `DcaAccumulate::arm` ladders ONE way and skips rungs
    // past zero, so a sub-1.0 anchor at `step = 1.0` rests nothing (an anchor PRICE is absolute;
    // the two-scale script does not rescale it).
    g(DCA_ACCUMULATE, "anchor_price", "40.0", "90.0", "", "anchor = \"fixed\"\n"),
    // ...and the same three ladder knobs, ungated for the same reason as `grid`'s above.
    p(DCA_ACCUMULATE, "step", "0.01", "0.05"),
    p(DCA_ACCUMULATE, "rungs", "1", "4"),
    p(DCA_ACCUMULATE, "size", "1.0", "6.0"),
    p(DCA_ACCUMULATE, "tp", "0.01", "9.0"),
    // ---- trailing_scalper ---------------------------------------------------------------------
    p(TRAILING_SCALPER, "qty", "1.0", "6.0"),
    p(TRAILING_SCALPER, "half_spread", "0.001", "0.2"),
    p(TRAILING_SCALPER, "exit_delay_ms", "0", "1000000"),
    p(TRAILING_SCALPER, "profit_target", "0.0", "0.05"),
    // INSTANCES 6-9: `entries_allowed` needs BOTH halves of each pair `> 0`, so each of the four
    // keys is inert while its partner is unset — which is the state the batch tool ships in.
    g(TRAILING_SCALPER, "entry_open_delay_ms", "1", "90000", "", "market_open_ms = 1\n"),
    g(TRAILING_SCALPER, "market_open_ms", "1", "90000", "", "entry_open_delay_ms = 1\n"),
    // ⚠ Both values must bite INSIDE the script's window: a 1 ms cutoff against a 100 s close
    // suppresses only the last ticks, when the scalper is HOLDING and never consults
    // `entries_allowed`, so the row would look vacuous while being true.
    g(
        TRAILING_SCALPER,
        "entry_cutoff_before_close_ms",
        "50000",
        "90000",
        "",
        "market_close_ms = 100000\n",
    ),
    g(TRAILING_SCALPER, "market_close_ms", "1", "100000", "", "entry_cutoff_before_close_ms = 1\n"),
    // ---- momentum -----------------------------------------------------------------------------
    p(MOMENTUM, "qty", "1.0", "6.0"),
    p(MOMENTUM, "threshold", "0.0", "1000000.0"),
    p(MOMENTUM, "tp", "0.5", "1000000.0"),
    p(MOMENTUM, "sl", "0.5", "1000000.0"),
    p(MOMENTUM, "time_limit_ms", "1", "1000000000"),
    p(MOMENTUM, "trailing", "0.5", "1000000.0"),
    p(MOMENTUM, "cooldown_ms", "0", "1000000000"),
    // ---- funding_carry ------------------------------------------------------------------------
    p(FUNDING_CARRY, "qty", "1.0", "6.0"),
    p(FUNDING_CARRY, "tp", "0.5", "1000000.0"),
    p(FUNDING_CARRY, "sl", "0.5", "1000000.0"),
    p(FUNDING_CARRY, "time_limit_ms", "1", "1000000000"),
    p(FUNDING_CARRY, "trailing", "0.5", "1000000.0"),
    // ⚠ `0.0`, not a small positive: the horizon AMORTIZES the round-trip taker cost, so two
    // positive horizons that both clear `entry_threshold = 0` open the same legs. Zero flips it.
    p(FUNDING_CARRY, "hold_periods", "0.0", "1000.0"),
    p(FUNDING_CARRY, "entry_threshold", "0.0", "1000000.0"),
    p(FUNDING_CARRY, "cooldown_ms", "0", "1000000000"),
    // ---- funding_capture ----------------------------------------------------------------------
    p(FUNDING_CAPTURE, "threshold", "0.0", "1.0"),
    p(FUNDING_CAPTURE, "qty", "1.0", "6.0"),
    // ---- pairs_zscore -------------------------------------------------------------------------
    p(PAIRS_ZSCORE, "period", "3", "8"),
    p(PAIRS_ZSCORE, "entry_z", "0.5", "1000.0"),
    p(PAIRS_ZSCORE, "exit_z", "0.1", "0.9"),
    p(PAIRS_ZSCORE, "beta", "1.0", "0.4"),
    p(PAIRS_ZSCORE, "notional", "1000.0", "50.0"),
    p(PAIRS_ZSCORE, "taker_fee", "0.0", "0.9"),
    p(PAIRS_ZSCORE, "half_spread_bps", "0.0", "900000.0"),
    p(PAIRS_ZSCORE, "hold_intervals", "1.0", "1000000.0"),
    // ⚠ SIGN-SENSITIVE: `spread_carry_cost` sums `ratio * mid * rate` and leg B's ratio is
    // `-beta`, so only a large POSITIVE `funding_a` / NEGATIVE `funding_b` lifts the cost floor
    // above the edge; the opposite signs make entry easier, and two tables that both enter trace
    // alike.
    p(PAIRS_ZSCORE, "funding_a", "0.0", "900.0"),
    p(PAIRS_ZSCORE, "funding_b", "0.0", "-900.0"),
    p(PAIRS_ZSCORE, "max_half_life", "0.0", "0.001"),
];

// ===================================================================================================
// Helpers
// ===================================================================================================

fn table(src: &str) -> Params {
    toml::from_str::<Params>(src).unwrap_or_else(|e| panic!("test TOML {src:?}: {e}"))
}

fn a_value(src: &str) -> ParamValue {
    toml::from_str::<Params>(&format!("v = {src}"))
        .unwrap_or_else(|e| panic!("test value {src:?}: {e}"))
        .get("v")
        .cloned()
        .expect("the wrapper table has `v`")
}

/// `base(file)` overlaid with `extra`, merged by INSERTION: a base may set a key an overlay also
/// sets, and TOML rejects a duplicated key.
fn params(file: &str, extra: &str) -> Params {
    let mut t = table(base(file));
    for (k, v) in table(extra) {
        t.insert(k, v);
    }
    t
}

/// The probe's own table: the base, plus its arming fragment when one is asked for, plus the key
/// under test at `value` (inserted LAST, so it always wins).
fn params_for(probe: &Probe, value: &str, extra: &str) -> Params {
    let mut t = params(probe.file, extra);
    t.insert(probe.key.to_string(), a_value(value));
    t
}

/// Whether the file's header declares a `needs` for `key` (the deleted registry's `PARAM_GATES`
/// row): the key is consumed only under some OTHER key's value.
fn gated(file: &str, key: &str) -> bool {
    header(file).param(key).is_some_and(|declaration| !declaration.needs.is_empty())
}

/// Whether this key's declared `needs` is MET on the given table, asked of
/// `StrategyFileHeader::unmet_needs` over the file's real `echo` rows rather than trusted.
fn gate_met(probe: &Probe, params: &Params) -> bool {
    header(probe.file).unmet_needs(probe.key, &echo(probe.file)(params)).is_empty()
}

/// The keys the file's header declares a `route` for (the deleted registry's `PARAM_ROUTES`).
fn route_keys(file: &str) -> Vec<String> {
    header(file).params.into_iter().filter(|(_, p)| p.route.is_some()).map(|(key, _)| key).collect()
}

fn probe_for(file: &str, key: &str) -> Option<&'static Probe> {
    PROBES.iter().find(|p| p.file == file && p.key == key)
}

// ===================================================================================================
// Structure
// ===================================================================================================

/// Exhaustiveness: an unprobed key is one nobody asked whether the strategy reads, which is how
/// five instances of this class shipped.
#[test]
fn every_declared_key_has_a_probe_or_is_a_route_key() {
    let mut probed = 0usize;
    for file in ROSTER {
        for (key, declaration) in header(file).params {
            if declaration.route.is_some() {
                assert!(
                    probe_for(file, &key).is_none(),
                    "{file}'s `{key}` is a ROUTE key AND has a probe row — its consumption question \
                     is answered by its header `route` (`StrategyFileHeader::misrouted_params`), \
                     and probing it here would mis-report a venue LABEL as an inert knob"
                );
                continue;
            }
            assert!(
                probe_for(file, &key).is_some(),
                "{file} declares `{key}` with no probe row in this file. Nothing then checks \
                 whether the strategy READS that value, which is the defect class this gate exists \
                 to close — add a row with two contrasting values (and, if it is only read under \
                 some other key, a `needs` in the file's header plus the `off`/`on` fragments that \
                 put it on each side of its condition)."
            );
            probed += 1;
        }
    }
    // Both directions at once: every non-route declared key found a row, and no row is left over.
    assert_eq!(probed, PROBES.len(), "a probe row names a key no roster header declares");
    assert!(probed > 20, "only {probed} keys are probed — the gate has gone nearly vacuous");
    for probe in PROBES {
        assert!(ROSTER.contains(&probe.file), "{} is probed but is not on the roster", probe.file);
        assert!(
            header(probe.file).param(probe.key).is_some(),
            "{}'s probe names `{}`, which is not a declared key",
            probe.file,
            probe.key
        );
        assert_ne!(
            probe.a, probe.b,
            "{}'s `{}` probe contrasts a value with itself",
            probe.file, probe.key
        );
    }
}

/// A probe's `off`/`on` fragments and its header `needs` must agree about whether the key is
/// gated, and the fragments must really put the DECLARED `needs` on either side of its condition.
/// ⚠ Otherwise a fragment could change something else and the behavioural halves measure that.
#[test]
fn every_probe_matches_its_gate_row() {
    for probe in PROBES {
        if !gated(probe.file, probe.key) {
            assert!(
                probe.off.is_empty() && probe.on.is_empty(),
                "{}'s `{}` declares no `needs`, so it must carry no gate fragments — the header and \
                 the probe disagree about whether this key is read conditionally at all",
                probe.file,
                probe.key
            );
            continue;
        }
        assert!(
            !gate_met(probe, &params_for(probe, probe.a, probe.off)),
            "{}'s `{}`: its declared `needs` is still MET under the probe's `off` fragment, so \
             the inert half of `a_gated_key_is_inert_until_its_gate_is_armed` would prove nothing",
            probe.file,
            probe.key
        );
        assert!(
            gate_met(probe, &params_for(probe, probe.a, probe.on)),
            "{}'s `{}`: the probe's `on` fragment does not satisfy the `needs` its header \
             declares — the armed half would then be testing some other change",
            probe.file,
            probe.key
        );
    }
}

// ===================================================================================================
// The two behavioural directions
// ===================================================================================================

/// **Direction 2 — the class-closer.** A declared key with no `needs` must MOVE the strategy: two
/// values of a key nothing declares conditional must produce two different call traces. A key
/// failing here is conditionally read (declare its `needs`) or read by nothing (delete it).
#[test]
fn an_ungated_key_moves_the_strategy() {
    for probe in PROBES.iter().filter(|p| !gated(p.file, p.key)) {
        let one = trace(probe.file, &params_for(probe, probe.a, ""), Feed::Plain);
        let other = trace(probe.file, &params_for(probe, probe.b, ""), Feed::Plain);
        assert_ne!(
            one, other,
            "{}'s `{}` = {} and = {} drove IDENTICAL calls, so nothing reads it at this file's \
             base table — while the mount echo still reports the value it resolved to, and an \
             operator who typed it has no way to tell. That is the declared-but-unconsumed class: \
             declare the `needs` naming the key that disarms it, or delete the key.",
            probe.file, probe.key, probe.a, probe.b
        );
    }
}

/// **Direction 1 — every `needs` is TRUE.** With it unmet the key changes NOTHING (licensing
/// direction 2 to skip it); with it met it changes something (the knob is real).
///
/// ⚠ The THIRD assertion is the non-vacuity guard: an inert half can be two strategies that do
/// nothing AT ALL, true even for a nonsense row. Requiring `off` and `on` to differ proves the two
/// halves are two configurations (it is what exposed the `rungs = 0` rows round 7 deleted).
#[test]
fn a_gated_key_is_inert_until_its_gate_is_armed() {
    for probe in PROBES.iter().filter(|p| gated(p.file, p.key)) {
        let unmet_a = trace(probe.file, &params_for(probe, probe.a, probe.off), Feed::Plain);
        let unmet_b = trace(probe.file, &params_for(probe, probe.b, probe.off), Feed::Plain);
        assert_eq!(
            unmet_a, unmet_b,
            "{}'s `{}` CHANGED the strategy with its declared `needs` unmet — the header exempts a \
             LIVE knob from direction 2, which is the class-closer's input shrinking silently. Fix \
             the file's `needs`.",
            probe.file, probe.key
        );
        let met_a = trace(probe.file, &params_for(probe, probe.a, probe.on), Feed::Plain);
        let met_b = trace(probe.file, &params_for(probe, probe.b, probe.on), Feed::Plain);
        assert_ne!(
            met_a, met_b,
            "{}'s `{}` changes nothing even with its `needs` MET, so the declaration is about a \
             knob that does nothing at all — the equality above would then hold for the trivial \
             reason and prove nothing.",
            probe.file, probe.key
        );
        assert_ne!(
            met_a, unmet_a,
            "{}'s `{}`: the `off` and `on` fragments drove the SAME strategy, so the two halves \
             above are one configuration compared with itself",
            probe.file, probe.key
        );
    }
}

/// The floor under both directions, with NO per-key claim: **every roster file places at least one
/// order at its base table.** Two empty traces are equal for the emptiest reason, and a dead
/// CONFIGURATION is invisible to the per-key class; such configurations are MEASURED one row each
/// in `the_shapes_this_harness_cannot_see`'s `DEAD` ledger, and this is the floor they sit on.
///
/// ⚠ [`base`] is where a strategy that legitimately needs context declares it, with a written
/// reason (`pairs_zscore` with no legs, `funding_carry` with no venue map trade nothing). A
/// strategy that no-ops at its own defaults is a declared row, never a silent pass here.
#[test]
fn the_scripted_market_moves_every_strategy() {
    for file in ROSTER {
        assert!(
            calls(file, &params(file, "")) > 0,
            "{file} submitted NOTHING over the whole script at its base table. Either the strategy \
             silently does nothing at its own defaults — which is the defect, not the test — or the \
             base table/feed no longer gives it what it needs; meanwhile every probe under it is \
             comparing one silence with another."
        );
    }
}

/// Broker calls in a trace, scale markers discounted: "did this configuration DO anything".
fn calls(file: &str, params: &Params) -> usize {
    trace(file, params, Feed::Plain).iter().filter(|l| !l.starts_with("== scale")).count()
}

/// The floor: every shipped file outside the gallery is either on the roster and probed or an
/// `OUT` row with its reason, so this file cannot pass by checking a shrinking set.
#[test]
fn the_gate_has_a_non_empty_input() {
    let unclassified = roster::unclassified();
    assert!(
        unclassified.is_empty(),
        "the shipped files and the roster disagree, so some file is checked by nothing or the roster \
         names a file that is gone: {unclassified:#?}"
    );
    for file in ROSTER {
        let routes = route_keys(file);
        let knobs = header(file).params.iter().filter(|(key, _)| !routes.contains(key)).count();
        assert_eq!(
            PROBES.iter().filter(|p| p.file == *file).count(),
            knobs,
            "{file} declares {knobs} non-route keys but this file probes a different number"
        );
    }
    for (file, _) in OUT {
        assert!(
            !PROBES.iter().any(|p| p.file == *file),
            "{file} is not on the roster, so it can have no probe rows"
        );
    }
    assert!(
        ROSTER.iter().any(|file| header(file).params.iter().any(|(_, p)| !p.needs.is_empty())),
        "no roster header declares a `needs` — direction 1 checks nothing"
    );
}
