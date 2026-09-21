//! The engine-driven gates for [`vike_strategy::cheap_np::CheapNp`] — extracted from that module's
//! own `#[cfg(test)] mod tests` when the strategy left the simulator crate.
//!
//! It could not travel as an inline module: these gates fold the strategy through the REAL
//! `vike_backtest::engine::StrategyEngine`, which lives a layer ABOVE this crate. They reach it
//! through the UPWARD DEV edge this crate's manifest declares — legal because
//! `crates/vike-ops/tests/layer_gate.rs` walks NORMAL edges only, and routine here rather than
//! novel (`vike-bridge-core` dev-depends on ten venue bridges that normal-depend on IT).
//!
//! ⚠ The imports below are NOT what the inline module had, and the difference is not cosmetic.
//! Inside the module, `use super::*` picked up the parent's own PRIVATE
//! `use crate::fair_value::{H, SIGMA_LOOKBACK_S, THETA, cheap_gate, ...}`, so those names were in
//! scope without ever being named here. A plain `use` is not a `pub use`, so
//! `use vike_strategy::cheap_np::*` re-exports none of them — they are imported explicitly, or
//! `THETA`, `H`, `SIGMA_LOOKBACK_S` and `cheap_gate` are simply undefined.

use vike_backtest::engine::{EngineParams, StrategyEngine, Tick};
use vike_model::fair::UPDOWN_WINDOW_SECS;
use vike_model::{Bar, BookLevel, TradeTick};
use vike_strategy::cheap_np::*;
use vike_strategy::fair_value::{H, SIGMA_LOOKBACK_S, THETA, cheap_gate, wc};

const SPOT: &str = "BTCUSDT";
const STS: i64 = 1_772_323_200;

fn up(sts: i64) -> String {
    format!("btc-updown-5m-{sts}#0")
}
fn dn(sts: i64) -> String {
    format!("btc-updown-5m-{sts}#1")
}

#[test]
fn token_symbol_parses_slug_and_outcome() {
    assert_eq!(TokenId::parse(&up(STS)), Some(TokenId { sts: STS, oidx: 0 }));
    assert_eq!(TokenId::parse(&dn(STS)), Some(TokenId { sts: STS, oidx: 1 }));
}

#[test]
fn token_symbol_rejects_off_grid_and_malformed() {
    // sts % 300 != 0 -> not a 5-minute window (the Python's own guard)
    assert_eq!(TokenId::parse("btc-updown-5m-1772323201#0"), None);
    assert_eq!(TokenId::parse("btc-updown-5m-1772323200#2"), None, "outcome must be 0|1");
    assert_eq!(TokenId::parse("btc-updown-5m-1772323200"), None, "no outcome suffix");
    assert_eq!(TokenId::parse("BTCUSDT"), None, "the spot series is never a token");
    assert_eq!(TokenId::parse("btc-updown-5m-abc#0"), None);
    assert_eq!(TokenId::parse("btc-updown-5m-0#0"), None, "sts must be positive");
}

/// A spot series: `n` one-second samples from `sts - warm` seconds, drifting up by `bp` basis
/// points per second so `p_up` is meaningfully above 0.5 and σ is non-degenerate.
fn spot_ticks(from_s: i64, n: i64, start: f64, bp: f64) -> Vec<Tick> {
    (0..n)
        .map(|i| {
            let px = start * (1.0 + bp * 1e-4 * i as f64) + if i % 2 == 0 { 0.5 } else { 0.0 };
            Tick::Trade(TradeTick {
                ts: (from_s + i) * 1000,
                local_ts: 0,
                price: px,
                size: 1.0,
                is_buyer_maker: false,
                symbol: SPOT.to_string(),
            })
        })
        .collect()
}

fn print_tick(sym: &str, ts_s: i64, price: f64, buyer_maker: bool) -> Tick {
    Tick::Trade(TradeTick {
        ts: ts_s * 1000,
        local_ts: 0,
        price,
        size: 1.0,
        is_buyer_maker: buyer_maker,
        symbol: sym.to_string(),
    })
}

fn run(strat: CheapNp, series: Vec<(String, Vec<Tick>)>) -> StrategyEngine<CheapNp> {
    run_with(strat, series, EngineParams { cash: 1000.0, ..Default::default() })
}

fn run_with(
    strat: CheapNp,
    series: Vec<(String, Vec<Tick>)>,
    params: EngineParams,
) -> StrategyEngine<CheapNp> {
    let symbols: Vec<(String, Vec<Bar>)> =
        series.iter().map(|(s, _)| (s.clone(), Vec::new())).collect();
    let mut e = StrategyEngine::new(symbols, strat, params);
    e.run_ticks(&series);
    e
}

/// A full-book SNAPSHOT tick for `sym` at `ts_s`, with the given ask ladder.
fn book_tick(sym: &str, ts_s: i64, asks: &[BookLevel]) -> Tick {
    Tick::Book(vike_model::BookUpdate {
        ts: ts_s * 1000,
        local_ts: ts_s * 1000,
        seq: ts_s as u64,
        kind: vike_model::BookUpdateKind::Snapshot,
        tick_size: 0.001,
        bids: vec![BookLevel::new(0.05, 1_000.0)],
        asks: asks.to_vec(),
        symbol: sym.to_string(),
    })
}

/// The `enters_on_the_first_qualifying_print_and_holds` fixture, plus an ask ladder resting on
/// the UP token from `STS + 20` onward. One print at `t = 40` clears the gate at 0.20.
fn with_book(asks: &[BookLevel]) -> Vec<(String, Vec<Tick>)> {
    let mut series = with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4);
    // The book rides the UP token's own stream, so the k-way merge delivers it before the
    // same-second print (equal ts breaks by stream order, and this entry precedes the print).
    let ups = series.iter_mut().find(|(s, _)| *s == up(STS)).expect("the up stream");
    ups.1.insert(0, book_tick(&up(STS), STS + 20, asks));
    series
}

/// 90 seconds of spot warmup ending at the window open, then the window's prints.
fn with_warm_spot(prints: Vec<Tick>, drift_bp: f64) -> Vec<(String, Vec<Tick>)> {
    // 200 samples of spot: 110 before the open (warmup + s_open) and 90 into the window
    let spot = spot_ticks(STS - 110, 200, 60_000.0, drift_bp);
    let mut by: Vec<(String, Vec<Tick>)> = vec![(SPOT.to_string(), spot)];
    let mut up_p = Vec::new();
    let mut dn_p = Vec::new();
    for t in prints {
        let Tick::Trade(tt) = &t else { unreachable!() };
        if tt.symbol.ends_with("#0") {
            up_p.push(t);
        } else {
            dn_p.push(t);
        }
    }
    by.push((up(STS), up_p));
    by.push((dn(STS), dn_p));
    by
}

#[test]
fn enters_on_the_first_qualifying_print_and_holds() {
    // Up-drifting spot: the UP token at 0.20 is mispriced -> the gate fires.
    let prints = vec![
        // t = 10 -> tte 290, OUTSIDE the time window even though the price qualifies
        print_tick(&up(STS), STS + 10, 0.20, false),
        // t = 40 -> the first print that clears band + time + theta
        print_tick(&up(STS), STS + 40, 0.20, false),
        // later qualifying prints must NOT re-enter (Hold)
        print_tick(&up(STS), STS + 60, 0.20, false),
        print_tick(&up(STS), STS + 80, 0.20, false),
    ];
    let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.entries, 1, "exactly one entry per window");
    assert_eq!(e.strategy.flips, 0);
    assert_eq!(e.core.position_of(&up(STS)).size, 1.0, "1 unit, held");
    assert_eq!(e.core.position_of(&dn(STS)).size, 0.0);
    assert_eq!(e.core.position_of(SPOT).size, 0.0, "the spot series is NEVER traded");
}

/// A window's tape starting BEFORE its own open must not corrupt (or kill) `s_open`.
///
/// This is not a synthetic corner: on the real April 2026 tape 8,678 of 8,687 windows print
/// before their open, and 6,206 of them print before the σ lookback the driver loads spot
/// over. The proof shape is a differential one — the same qualifying print must produce a
/// BYTE-IDENTICAL signal whether or not a pre-open print preceded it.
#[test]
fn a_pre_open_print_leaves_s_open_untouched() {
    let entry = print_tick(&up(STS), STS + 40, 0.20, false);
    let baseline = run(CheapNp::new(SPOT), with_warm_spot(vec![entry.clone()], 0.4));
    assert_eq!(baseline.strategy.entries, 1, "control: the entry fires without a lead-in");

    // (a) a print while the spot buffer has samples but has NOT yet reached the open —
    //     latching here would book a STALE `s_open` (the spot 50 s before the open).
    let stale = run(
        CheapNp::new(SPOT),
        with_warm_spot(vec![print_tick(&up(STS), STS - 50, 0.20, false), entry.clone()], 0.4),
    );
    // (b) a print before the spot feed exists at all — latching here would book `None` and
    //     the window could never trade again.
    let empty = run(
        CheapNp::new(SPOT),
        with_warm_spot(vec![print_tick(&up(STS), STS - 200, 0.20, false), entry], 0.4),
    );

    for (label, e) in [("stale-buffer lead-in", &stale), ("empty-buffer lead-in", &empty)] {
        assert_eq!(e.strategy.entries, 1, "{label}: the window must still enter");
        assert_eq!(
            e.strategy.signals, baseline.strategy.signals,
            "{label}: the fired signal must be identical to the no-lead-in control"
        );
    }
}

#[test]
fn out_of_band_and_out_of_time_prints_never_enter() {
    let prints = vec![
        print_tick(&up(STS), STS + 40, 0.40, false), // above the band
        print_tick(&up(STS), STS + 50, 0.05, false), // below the band
        print_tick(&up(STS), STS + 10, 0.20, false), // tte 290 > 270
        print_tick(&up(STS), STS + 290, 0.20, false), // tte 10 < 15
    ];
    let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.entries, 0);
}

#[test]
fn the_wrong_side_never_qualifies_on_an_up_move() {
    // Spot drifting UP: the DOWN token at 0.20 has a deeply negative edge.
    let prints = vec![print_tick(&dn(STS), STS + 40, 0.20, false)];
    let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.entries, 0);
    assert_eq!(e.core.position_of(&dn(STS)).size, 0.0);
}

#[test]
fn maker_side_prints_are_ignored_when_taker_buys_only() {
    let prints = vec![print_tick(&up(STS), STS + 40, 0.20, true)];
    let e = run(CheapNp::new(SPOT), with_warm_spot(prints.clone(), 0.4));
    assert_eq!(e.strategy.entries, 0, "is_buyer_maker = a taker SELL, not a taker buy");

    let mut s = CheapNp::new(SPOT);
    s.taker_buys_only = false;
    let e = run(s, with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.entries, 1, "flag off -> the same print enters");
}

#[test]
fn no_entry_before_sigma_is_warm() {
    // Only 20 spot seconds observed -> trailing_sigma is None (warmup floor 30) -> no gate.
    let spot = spot_ticks(STS - 10, 20, 60_000.0, 0.4);
    let series = vec![
        (SPOT.to_string(), spot),
        (up(STS), vec![print_tick(&up(STS), STS + 40, 0.20, false)]),
    ];
    let e = run(CheapNp::new(SPOT), series);
    assert_eq!(e.strategy.entries, 0);
}

#[test]
fn no_entry_without_a_window_open_price() {
    // Spot starts AFTER the window open -> price_at(spot, sts) is None -> s_open unknown.
    let spot = spot_ticks(STS + 5, 120, 60_000.0, 0.4);
    let series = vec![
        (SPOT.to_string(), spot),
        (up(STS), vec![print_tick(&up(STS), STS + 100, 0.20, false)]),
    ];
    let e = run(CheapNp::new(SPOT), series);
    assert_eq!(e.strategy.entries, 0);
}

#[test]
fn each_window_gets_its_own_entry() {
    let spot = spot_ticks(STS - 110, 800, 60_000.0, 0.4);
    let mut series = vec![(SPOT.to_string(), spot)];
    for w in 0..2i64 {
        let sts = STS + w * UPDOWN_WINDOW_SECS;
        series.push((up(sts), vec![print_tick(&up(sts), sts + 40, 0.20, false)]));
    }
    let e = run(CheapNp::new(SPOT), series);
    assert_eq!(e.strategy.entries, 2, "one entry per WINDOW, not per run");
}

#[test]
fn flip_mode_exits_the_held_token_and_enters_the_opposite() {
    // Spot drifts UP for the first half of the window (Up qualifies), then reverses hard so the
    // DOWN token qualifies later. Built as an explicit spot path rather than a constant drift.
    let mut spot: Vec<Tick> = Vec::new();
    for i in 0..260i64 {
        let s = STS - 110 + i;
        // up to +60 bp by t=+40, then back down through the open by t=+120
        let rel = (s - STS) as f64;
        let px = if rel <= 40.0 {
            60_000.0 * (1.0 + 1.5e-5 * rel.max(0.0))
        } else {
            60_000.0 * (1.0 + 1.5e-5 * 40.0 - 4.0e-5 * (rel - 40.0))
        };
        spot.push(Tick::Trade(TradeTick {
            ts: s * 1000,
            local_ts: 0,
            price: px + if i % 2 == 0 { 0.5 } else { 0.0 },
            size: 1.0,
            is_buyer_maker: false,
            symbol: SPOT.to_string(),
        }));
    }
    // Filler prints at 0.50 (OUT of the cheap band, so they can never arm the gate) exist only
    // so the market orders have a later print of their own symbol to fill against — the sim
    // engine fills a market order at the NEXT event of that symbol.
    let series = vec![
        (SPOT.to_string(), spot.clone()),
        (
            up(STS),
            vec![
                print_tick(&up(STS), STS + 40, 0.20, false),
                print_tick(&up(STS), STS + 100, 0.50, false),
                print_tick(&up(STS), STS + 200, 0.50, false),
                print_tick(&up(STS), STS + 250, 0.50, false),
            ],
        ),
        (
            dn(STS),
            vec![
                print_tick(&dn(STS), STS + 140, 0.20, false),
                print_tick(&dn(STS), STS + 200, 0.50, false),
                print_tick(&dn(STS), STS + 250, 0.50, false),
            ],
        ),
    ];

    // sanity: the two prints really do straddle the gate the way the test intends
    let hold = run(CheapNp::new(SPOT), series.clone());
    assert_eq!(hold.strategy.entries, 1);
    assert_eq!(hold.strategy.flips, 0, "Hold never flips");
    assert_eq!(hold.core.position_of(&up(STS)).size, 1.0);
    assert_eq!(hold.core.position_of(&dn(STS)).size, 0.0);

    let mut s = CheapNp::new(SPOT);
    s.mode = CheapNpMode::Flip;
    let flip = run(s, series);
    assert_eq!(flip.strategy.entries, 1);
    assert_eq!(flip.strategy.flips, 1, "the reversal is a run start -> one flip");
    assert_eq!(flip.core.position_of(&up(STS)).size, 0.0, "the Up token was sold");
    assert_eq!(flip.core.position_of(&dn(STS)).size, 1.0, "and Down entered");
}

#[test]
fn flip_mode_ignores_repeat_prints_on_the_held_side() {
    let prints = vec![
        print_tick(&up(STS), STS + 40, 0.20, false),
        print_tick(&up(STS), STS + 60, 0.20, false),
        print_tick(&up(STS), STS + 80, 0.20, false),
    ];
    let mut s = CheapNp::new(SPOT);
    s.mode = CheapNpMode::Flip;
    let e = run(s, with_warm_spot(prints, 0.4));
    assert_eq!((e.strategy.entries, e.strategy.flips), (1, 0), "same side = not a run start");
}

#[test]
fn signals_record_the_fired_print_not_the_fill() {
    let prints = vec![
        print_tick(&up(STS), STS + 10, 0.20, false), // out of time
        print_tick(&up(STS), STS + 40, 0.20, false), // THE entry
        print_tick(&up(STS), STS + 60, 0.31, false), // later print — the fill price, not a signal
    ];
    let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.signals.len(), 1, "one signal per fired gate");
    let s = e.strategy.signals[0];
    assert_eq!((s.sts, s.oidx, s.is_flip), (STS, 0, false));
    assert_eq!(s.ts, (STS + 40) * 1000, "the PRINT's ts, in ms");
    assert_eq!(s.ask, 0.20, "the price the gate scored, not the next print's 0.31");
    // and it is the gate's own edge, not a recomputation
    assert!(s.edge > THETA, "{}", s.edge);
    assert_eq!(e.strategy.signals.len(), e.strategy.entries + e.strategy.flips);
}

#[test]
fn flip_signals_are_tagged_and_ordered() {
    let prints = vec![
        print_tick(&up(STS), STS + 40, 0.20, false),
        print_tick(&dn(STS), STS + 140, 0.20, false),
    ];
    // reuse the explicit reversal path from the flip test above by driving Hold's spot shape:
    // here a constant up-drift means the DOWN print cannot qualify, so only the entry fires.
    let mut s = CheapNp::new(SPOT);
    s.mode = CheapNpMode::Flip;
    let e = run(s, with_warm_spot(prints, 0.4));
    assert_eq!(e.strategy.signals.len(), 1);
    assert!(!e.strategy.signals[0].is_flip, "the first fire is never a flip");
    assert_eq!(e.strategy.signals.len(), e.strategy.entries + e.strategy.flips);
}

#[test]
fn from_params_reads_every_knob_and_defaults_the_rest() {
    let p: toml::Value = toml::from_str(
        r#"
spot_symbol = "BTCUSDT"
mode = "FLIP"
size = 3
theta = 0.02
h = 900.0
sigma_lookback_s = 600
sigma_scale = 1.25
taker_buys_only = false
"#,
    )
    .unwrap();
    let s = CheapNp::from_params(&p);
    assert_eq!(s.spot_symbol, "BTCUSDT");
    assert_eq!(s.mode, CheapNpMode::Flip);
    assert_eq!((s.size, s.theta, s.h, s.sigma_lookback_s), (3.0, 0.02, 900.0, 600.0));
    assert_eq!(s.sigma_scale, 1.25);
    assert!(!s.taker_buys_only);

    let d = CheapNp::from_params(&toml::Value::Table(Default::default()));
    assert_eq!(d.mode, CheapNpMode::Hold);
    assert_eq!((d.size, d.theta, d.h, d.sigma_lookback_s), (1.0, THETA, H, SIGMA_LOOKBACK_S));
    assert_eq!(d.sigma_scale, 1.0, "the oracle scale is the default and must stay 1.0");
    assert!(d.taker_buys_only);
    assert_eq!(d.spot_symbol, "");
}

/// The σ knob is a sensitivity dial, so what has to hold is (a) it is INERT at the default and
/// (b) it moves the edge the way σ actually moves it.
///
/// That direction is NOT "bigger σ ⇒ fewer entries", and assuming so is a trap this test exists
/// to pin: σ only ever pulls `p_up` toward 0.5. For a side the model already makes the
/// FAVOURITE (`prob > 0.5`) a bigger σ shrinks the edge; for a side it makes the UNDERDOG a
/// bigger σ GROWS it — and the cheap band `[0.10, 0.35)` is full of underdogs, whose gate
/// clears at `prob > ask + fee + θ ≈ 0.27`, well below 0.5. So a σ change reshuffles WHICH
/// windows qualify rather than uniformly loosening or tightening the gate.
#[test]
fn sigma_scale_is_inert_at_one_and_pulls_the_edge_toward_the_coin_flip() {
    let prints = vec![print_tick(&up(STS), STS + 40, 0.20, false)];
    let series = with_warm_spot(prints, 0.4);

    let base = run(CheapNp::new(SPOT), series.clone());
    let mut unit = CheapNp::new(SPOT);
    unit.sigma_scale = 1.0;
    let same = run(unit, series.clone());
    assert_eq!(same.strategy.signals, base.strategy.signals, "scale 1.0 must change nothing");

    let mut wide = CheapNp::new(SPOT);
    wide.sigma_scale = 4.0;
    let wide = run(wide, series.clone());
    let mut tight = CheapNp::new(SPOT);
    tight.sigma_scale = 0.25;
    let tight = run(tight, series);

    // The spot drifts UP and the entered side is Up, so the model already makes it the
    // favourite: p_up > 0.5 and a bigger σ pulls it DOWN toward the coin flip.
    assert_eq!((base.strategy.entries, wide.strategy.entries), (1, 1));
    let (b, w, t) = (
        base.strategy.signals[0].edge,
        wide.strategy.signals[0].edge,
        tight.strategy.signals[0].edge,
    );
    assert!(w < b, "4x σ must shrink a favourite's edge: {w} vs {b}");
    // Only `>=`, deliberately: this fixture's drift already saturates `p_up` at 1.0, so the
    // edge sits at its CEILING (`1 − ask − fee`) and shrinking σ has nowhere left to push it.
    // Asserting `>` would be asserting a property of the fixture, not of the knob.
    assert!(t >= b, "0.25x σ must not shrink it: {t} vs {b}");
    assert_eq!(
        b,
        1.0 - 0.20 - vike_strategy::fair_value::fee(0.20),
        "the fixture is at the ceiling"
    );
    // ...and the 4x shrink is bounded below by the COIN-FLIP edge, never by zero: σ can only
    // ever pull `prob` to 0.5, which for a 0.20 ask still leaves a fat positive edge. That
    // bound is exactly why "bigger σ ⇒ fewer entries" is false inside the cheap band.
    assert!(w > 0.5 - 0.20 - vike_strategy::fair_value::fee(0.20), "σ can only reach p=0.5: {w}");
}

// -----------------------------------------------------------------------------------------
// the RESTING-ASK gate (opt-in). See `vike_strategy::cheap_np_ask` for why the print price is wrong.
// -----------------------------------------------------------------------------------------

fn ask_gated() -> CheapNp {
    let mut s = CheapNp::new(SPOT);
    s.entry_price = EntryPrice::RestingAsk;
    s
}

/// THE fix, end to end: the print clears θ, the resting ask does not, so there is NO TRADE.
///
/// The fixture's print is 0.20 against a saturated `p_up`, so `prob_wc = 1.0` and the
/// θ-clearing limit sits at ~0.884 — deliberately generous, because the point being pinned is
/// the MECHANISM, not one calibration. The book's only ask is above that limit.
#[test]
fn the_resting_ask_gate_refuses_a_signal_whose_edge_dies_on_the_book() {
    let e = run(ask_gated(), with_book(&[BookLevel::new(0.95, 10_000.0)]));
    assert_eq!(e.strategy.entries, 0, "the ask is above the θ-clearing price: not a trade");
    assert_eq!(e.strategy.ask_rejects, 1);
    assert_eq!(e.strategy.ask_no_book, 0, "there WAS a book — this is a refusal, not a gap");
    assert!(e.strategy.signals.is_empty(), "a refused signal is not recorded as an entry");
    // ...and the very same tape enters under the frozen PRINT gate. That gap is the finding.
    let p = run(CheapNp::new(SPOT), with_book(&[BookLevel::new(0.95, 10_000.0)]));
    assert_eq!(p.strategy.entries, 1);
}

/// The entry is priced at the BOOK's vwap (not the print) and sized to in-edge depth.
#[test]
fn the_resting_ask_gate_prices_at_the_book_and_sizes_to_in_edge_depth() {
    let mut s = ask_gated();
    s.size = 500.0;
    // 40 shares inside any sane limit, then a wall at 0.95 that no in-edge sweep can reach
    let e = run(
        s,
        with_book(&[
            BookLevel::new(0.30, 25.0),
            BookLevel::new(0.32, 15.0),
            BookLevel::new(0.95, 10_000.0),
        ]),
    );
    assert_eq!(e.strategy.entries, 1);
    let sig = e.strategy.signals[0];
    assert_eq!(sig.qty, 40.0, "sized to the resting in-edge depth, not to the 500 requested");
    let want_vwap = (0.30 * 25.0 + 0.32 * 15.0) / 40.0;
    assert!((sig.ask - want_vwap).abs() < 1e-12, "{} vs {want_vwap}", sig.ask);
    assert_eq!(sig.print_px, 0.20, "the print is recorded, and it is NOT the price paid");
    assert!(sig.ask > sig.print_px, "the book is strictly worse than the tape print");
    // the recorded edge is the RE-SCORED one, and it still clears the bar
    assert_eq!(sig.edge, vike_strategy::cheap_np_ask::edge_at(1.0, sig.ask));
    assert!(sig.edge > THETA);
}

/// A book-less run must look like ZERO entries, never like a good backtest: the ask gate has
/// nothing to gate on, so it refuses and says WHY (`ask_no_book`, not `ask_rejects`).
#[test]
fn without_a_book_the_resting_ask_gate_refuses_rather_than_falling_back_to_the_print() {
    let e =
        run(ask_gated(), with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4));
    assert_eq!(e.strategy.entries, 0);
    assert_eq!(e.strategy.ask_no_book, 1, "a missing book is a DATA gap, counted as one");
    assert_eq!(e.strategy.ask_rejects, 0, "...and never reported as 'the edge was gone'");
}

/// DEFAULT-OFF, proven: a book being present changes nothing for the frozen print gate, so
/// every published `cheap_np` number stands.
#[test]
fn the_print_gate_is_the_default_and_a_book_does_not_perturb_it() {
    assert_eq!(CheapNp::new(SPOT).entry_price, EntryPrice::Print);
    assert_eq!(CheapNp::default().entry_price, EntryPrice::Print);
    let bookless = run(
        CheapNp::new(SPOT),
        with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4),
    );
    let booked = run(
        CheapNp::new(SPOT),
        with_book(&[BookLevel::new(0.30, 25.0), BookLevel::new(0.95, 10_000.0)]),
    );
    assert_eq!(booked.strategy.signals, bookless.strategy.signals);
    assert_eq!(booked.strategy.signals[0].ask, 0.20, "still the print's own price");
    assert_eq!(booked.strategy.signals[0].qty, 1.0, "still the requested size");
    assert_eq!((booked.strategy.ask_rejects, booked.strategy.ask_no_book), (0, 0));
}

/// Polymarket's venue-enforced 250 ms taker delay, modelled by composing the two ENGINE seams
/// (`LatencyModelKind::venue_hold_ms` + `FillModelKind::L2Book`) rather than by any code in
/// this strategy: the decision is taken on the book at `T`, the matching engine looks at
/// `T + 250 ms`, and an order that no longer crosses is BOOKED — it rests, which is neither a
/// fill nor a miss.
#[test]
fn the_venue_taker_delay_rests_an_order_the_market_ran_away_from() {
    // A book event and a trade at ms precision — the timeline below turns on sub-second
    // stamps, because 250 ms is SUB-BLOCK: what moves inside the hold is the book, not the
    // tape.
    let book_ms = |ts_ms: i64, asks: Vec<BookLevel>| {
        Tick::Book(vike_model::BookUpdate {
            ts: ts_ms,
            local_ts: ts_ms,
            seq: ts_ms as u64,
            kind: vike_model::BookUpdateKind::Snapshot,
            tick_size: 0.001,
            bids: vec![BookLevel::new(0.05, 1_000.0)],
            asks,
            symbol: up(STS),
        })
    };
    // 0.50 is OUTSIDE the cheap band, so these can never re-arm the gate; they exist only to
    // give the engine a price event on which to run its fill pass.
    let filler_ms = |ts_ms: i64| {
        Tick::Trade(TradeTick {
            ts: ts_ms,
            local_ts: ts_ms,
            price: 0.50,
            size: 1.0,
            is_buyer_maker: false,
            symbol: up(STS),
        })
    };
    let t = (STS + 40) * 1000; // the print, and the decision instant

    let mut series = with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4);
    {
        let ups = series.iter_mut().find(|(s, _)| *s == up(STS)).expect("the up stream");
        // decision book: cheap and deep, well inside the θ-clearing limit
        ups.1.insert(0, book_ms((STS + 20) * 1000, vec![BookLevel::new(0.30, 500.0)]));
        // +100 ms — INSIDE the venue's 250 ms hold, so only a zero-hold order matches here
        ups.1.push(filler_ms(t + 100));
        // +200 ms — still inside the hold, and the market runs away
        ups.1.push(book_ms(t + 200, vec![BookLevel::new(0.99, 10_000.0)]));
        // +300 ms — the hold has expired; this is where a held order is matched
        ups.1.push(filler_ms(t + 300));
    }
    let params = |latency| EngineParams {
        cash: 1000.0,
        fill_model: vike_backtest::engine::FillModelKind::L2Book,
        slippage: 0.0, // the walk IS the slippage
        latency_model: latency,
        ..Default::default()
    };

    // No hold: the order is matched at +100 ms, against the book it was decided on.
    let now = run_with(ask_gated(), series.clone(), params(None));
    assert_eq!(now.strategy.entries, 1, "the gate fired either way");
    assert_eq!(now.core.position_of(&up(STS)).size, 1.0, "filled at the decision book");
    assert_eq!(now.strategy.signals[0].ask, 0.30, "priced at the resting ask, not the print");

    // With the venue's 250 ms hold: the SAME decision at the SAME limit, but the matching
    // engine does not look until +250 ms — by which time the book no longer crosses. The
    // order is booked, i.e. it rests: not a fill, and not a miss.
    let held = run_with(
        ask_gated(),
        series,
        params(Some(vike_backtest::latency::LatencyModelKind::venue_hold_ms(
            vike_backtest::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS,
        ))),
    );
    assert_eq!(held.strategy.entries, 1, "the DECISION is unchanged — only the outcome moves");
    assert_eq!(held.strategy.signals, now.strategy.signals, "...byte-identically so");
    assert_eq!(
        held.core.position_of(&up(STS)).size,
        0.0,
        "booked, not filled: the market ran away inside the venue's hold"
    );
    // and it really is RESTING — still working, not silently dropped
    assert_eq!(held.core.pending_of(&up(STS)).len(), 1, "the order is on the book");
}

#[test]
fn from_params_reads_the_entry_price_knob_and_defaults_to_the_frozen_print_gate() {
    let p = |s: &str| toml::from_str::<toml::Value>(s).unwrap();
    assert_eq!(
        CheapNp::from_params(&p("entry_price = \"resting_ask\"")).entry_price,
        EntryPrice::RestingAsk
    );
    assert_eq!(
        CheapNp::from_params(&p("entry_price = \"RESTING-ASK\"")).entry_price,
        EntryPrice::RestingAsk
    );
    // a typo must NOT silently change which trades a published run takes
    assert_eq!(
        CheapNp::from_params(&p("entry_price = \"restingask\"")).entry_price,
        EntryPrice::Print
    );
    assert_eq!(CheapNp::from_params(&p("size = 1")).entry_price, EntryPrice::Print);
    assert!(CheapNp::from_params(&p("size = 1")).limit_at_edge);
    assert!(!CheapNp::from_params(&p("limit_at_edge = false")).limit_at_edge);
}

#[test]
fn the_gate_the_strategy_fires_on_is_the_shared_cheap_gate() {
    // Whatever the engine-driven entry test above enters on, the pure gate agrees with — this
    // is the link that makes the parity harness (which drives `cheap_gate` directly) a proof of
    // the STRATEGY, not of a second implementation.
    let (ask, s_now, s_open, sigma, t) = (0.20, 60_030.0, 60_000.0, 8.0e-5, 40.0);
    let e = wc(0, ask, s_now, s_open, sigma, t, H);
    assert_eq!(cheap_gate(0, ask, s_now, s_open, sigma, t, THETA, H), Some(e));
    assert!(e > THETA);
}
