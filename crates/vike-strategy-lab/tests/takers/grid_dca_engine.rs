//! Copied from `crates/vike-strategy/tests/grid_dca_engine.rs`, driving the shipped
//! `strategies/grid_dca/grid.rs` and `strategies/grid_dca/dca_accumulate.rs`.
//!
//! `Grid` / `DcaAccumulate` LADDER-BEHAVIOUR gate: the two portable reference strategies folded
//! through the REAL [`StrategyEngine`] / [`OrderBrokerSimulator`].
//!
//! A ladder only observably rests, fills, re-arms and halts against a broker, and the engine sits
//! ABOVE `vike-strategy`, so these run here (the pure `from_params` tests stay beside the
//! strategies). Each strategy is built through `from_params`, the PRODUCTION path (the file's
//! `build`); the private runtime state is read through `anchor()`,
//! `is_halted()`, `leg_is_armed()`, `filled_size()` and `avg_entry()`.

use super::dca_accumulate::DcaAccumulate;
use super::grid::Grid;
use vike_marketdata::{Bar, TickQuote};
use vike_model::params::Params;
use vike_model::{Fill, OrderKind, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYMBOL: &str = "BTCUSDT";

/// A flat OHLC bar tagged with `SYMBOL` (so a manually-driven handler resolves the symbol without
/// a `symbol` override).
fn bar(timestamp: i64, price: f64) -> Bar {
    Bar {
        timestamp,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(SYMBOL.into()),
    }
}

/// A crafted OHLC bar (for the round-trip / band-exit engine runs). Symbol is irrelevant here —
/// `StrategyEngine::new` re-tags every stored bar to the bare series symbol.
fn ohlc(timestamp: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar { timestamp, open, high, low, close, ..bar(timestamp, close) }
}

fn fill(side: i32, size: f64, price: f64) -> Fill {
    Fill {
        side,
        size,
        price,
        fee: 0.0,
        timestamp: 0,
        is_maker: true,
        symbol: SYMBOL.to_string(),
        tag: None,
    }
}

fn quote(timestamp: i64, bid: f64, ask: f64) -> TickQuote {
    TickQuote {
        timestamp,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: SYMBOL.into(),
    }
}

/// Parse a params table from TOML text (serde, straight into [`Params`]).
fn params(src: &str) -> Params {
    toml::from_str(src).expect("params table")
}

/// Build a `Grid` from a params table — the registry's own construction path.
fn grid(src: &str) -> Grid {
    Grid::from_params(&params(src))
}

/// Build a `DcaAccumulate` from a params table — the registry's own construction path.
fn dca(src: &str) -> DcaAccumulate {
    DcaAccumulate::from_params(&params(src))
}

/// An engine over one symbol seeded with `bars` (so `SYMBOL` is registered for order routing).
fn engine<S: Strategy<OrderBrokerSimulator>>(strategy: S, bars: Vec<Bar>) -> StrategyEngine<S> {
    StrategyEngine::new(vec![(SYMBOL.to_string(), bars)], strategy, StrategyEngineConfig::default())
}

/// (side, price) of every resting limit order on `SYMBOL`.
fn resting_limits<S: Strategy<OrderBrokerSimulator>>(
    engine: &StrategyEngine<S>,
) -> Vec<(i32, f64)> {
    engine.core.symbol[0]
        .pending
        .iter()
        .filter(|o| o.kind == OrderKind::Limit)
        .map(|o| (o.side, o.price.unwrap()))
        .collect()
}

// ---- Grid ----

#[test]
fn grid_rests_the_ladder_around_the_anchor() {
    // step 1, 2 rungs/side around anchor 100 ⇒ buys @99,@98 and sells @101,@102, size 1.
    let g = grid("step = 1.0\nrungs = 2\nsize = 1.0\nband = 10.0\n");
    let mut engine_under_test = engine(g, vec![bar(0, 100.0)]);
    // One bar: fill phase (empty) → on_bar arms the ladder. Nothing fills (no later bar).
    engine_under_test.run();
    assert_eq!(engine_under_test.strategy.anchor(), Some(100.0));
    let mut rung = resting_limits(&engine_under_test);
    rung.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    assert_eq!(rung, vec![(1, 98.0), (1, 99.0), (-1, 101.0), (-1, 102.0)]);
}

#[test]
fn grid_round_trip_rearms_the_rung() {
    // bar0@100 arms; bar1 dips low=99 → buy@99 fills → TP sell@100 rests; bar2 rises high=100
    // → TP sell@100 fills → the @99 rung re-arms. Round-trip closes flat at +1 gross.
    let g = grid("step = 1.0\nrungs = 2\nsize = 1.0\nband = 10.0\n");
    let bars = vec![
        ohlc(0, 100.0, 100.0, 100.0, 100.0),
        ohlc(1, 100.0, 100.0, 99.0, 99.5),
        ohlc(2, 99.5, 100.0, 99.5, 100.0),
    ];
    let mut engine_under_test = engine(g, bars);
    engine_under_test.run();
    // Flat after the round-trip, one closed trade of +1 gross (fee 0).
    assert_eq!(engine_under_test.core.position_of(SYMBOL).size, 0.0);
    assert_eq!(engine_under_test.core.trades.len(), 1);
    assert!(
        (engine_under_test.core.trades[0].pnl - 1.0).abs() < 1e-9,
        "pnl {}",
        engine_under_test.core.trades[0].pnl
    );
    // The @99 rung re-armed (back to Armed) and a fresh buy@99 rests (exactly one).
    assert!(engine_under_test.strategy.leg_is_armed(0), "the filled rung must return to Armed");
    let buys_at_99 = resting_limits(&engine_under_test)
        .iter()
        .filter(|&&(s, p)| s == 1 && (p - 99.0).abs() < 1e-9)
        .count();
    assert_eq!(buys_at_99, 1, "the re-armed entry, not a duplicate");
}

#[test]
fn grid_band_exit_flattens_and_halts_trading() {
    // band 2 around anchor 100 ⇒ stop outside [98,102]. bar1 spikes to 103: the sells fill
    // (short −2), then on_bar halts + flattens; bar2 fills the flatten back to 0.
    let g = grid("step = 1.0\nrungs = 2\nsize = 1.0\nband = 2.0\n");
    let bars = vec![
        ohlc(0, 100.0, 100.0, 100.0, 100.0),
        ohlc(1, 100.0, 103.0, 100.0, 103.0),
        ohlc(2, 103.0, 103.0, 103.0, 103.0),
    ];
    let mut engine_under_test = engine(g, bars);
    engine_under_test.run();
    assert!(engine_under_test.strategy.is_halted(), "band exit must halt");
    assert_eq!(engine_under_test.core.position_of(SYMBOL).size, 0.0, "band exit must flatten");
    // Halted ⇒ a further fill re-arms / places NOTHING (trading has stopped).
    let before = engine_under_test.core.symbol[0].pending.len();
    engine_under_test.strategy.on_fill(&mut engine_under_test.core, &fill(1, 1.0, 99.0));
    assert_eq!(engine_under_test.core.symbol[0].pending.len(), before, "halted: no new orders");
}

#[test]
fn grid_zero_rungs_is_inert() {
    // OFF/byte-identical shape: an empty grid rests nothing (a default that doesn't select it
    // changes no behavior).
    let g = grid("rungs = 0\nsymbol = \"BTCUSDT\"\n");
    let mut engine_under_test = engine(g, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 100.0));
    assert!(resting_limits(&engine_under_test).is_empty());
}

#[test]
fn grid_bounded01_skips_walled_rungs() {
    // A 0..1 market (tick 0.02) anchored near the lower wall at 0.05, step 0.02, 3 rungs/side.
    // Buy rungs would be 0.03 / 0.01 / -0.01 — the last two fall on/past the tick wall and are
    // SKIPPED; the sell rungs 0.07 / 0.09 / 0.11 all stay. So 4 rungs rest, all inside the band.
    let g = grid(
        "step = 0.02\nrungs = 3\nsize = 1.0\nband = 0.5\nbounded01 = true\ntick = 0.02\nsymbol = \"BTCUSDT\"\n",
    );
    let mut engine_under_test = engine(g, vec![bar(0, 100.0)]); // seed bar price is irrelevant (override set)
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 0.05));
    let rung = resting_limits(&engine_under_test);
    assert_eq!(rung.len(), 4, "two walled buy rungs are skipped");
    for (_, p) in rung {
        assert!(p > 0.02, "rung {p} must sit strictly inside the [tick, 1-tick] band");
    }
}

#[test]
fn grid_arms_on_the_quote_tick_path() {
    // The tick path shares `drive`: a quote at mid 100 arms the same ladder as a bar at 100,
    // proving the portable strategy is deterministic over a tick series too.
    let g = grid("step = 1.0\nrungs = 1\nsize = 1.0\nband = 10.0\n");
    let mut engine_under_test = engine(g, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_quote_tick(&mut engine_under_test.core, &quote(0, 99.5, 100.5));
    assert_eq!(engine_under_test.strategy.anchor(), Some(100.0));
    let mut rung = resting_limits(&engine_under_test);
    rung.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    assert_eq!(rung, vec![(1, 99.0), (-1, 101.0)]);
}

// ---- DcaAccumulate ----

#[test]
fn dca_scales_in_on_the_ladder() {
    // long, step 1, 3 rungs around anchor 100 ⇒ buy limits @99,@98,@97, size 1.
    let d = dca("step = 1.0\nrungs = 3\nsize = 1.0\n");
    let mut engine_under_test = engine(d, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 100.0));
    let mut rung = resting_limits(&engine_under_test);
    rung.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    assert_eq!(rung, vec![(1, 97.0), (1, 98.0), (1, 99.0)]);
}

#[test]
fn dca_accumulates_average_and_exits_at_take_profit() {
    let d = dca("step = 1.0\nrungs = 3\nsize = 1.0\ntp = 0.1\n");
    let mut engine_under_test = engine(d, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 100.0)); // arm the ladder
    // Two entries fill: avg = (99 + 98) / 2 = 98.5, size 2.
    engine_under_test.strategy.on_fill(&mut engine_under_test.core, &fill(1, 1.0, 99.0));
    engine_under_test.strategy.on_fill(&mut engine_under_test.core, &fill(1, 1.0, 98.0));
    assert_eq!(engine_under_test.strategy.filled_size(), 2.0);
    assert_eq!(engine_under_test.strategy.avg_entry(), 98.5);
    // TP target = 98.5 * 1.1 = 108.35; a bar at 109 fires the single aggregate close.
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(1, 109.0));
    let closes: Vec<_> = engine_under_test.core.symbol[0]
        .pending
        .iter()
        .filter(|o| o.kind == OrderKind::Market)
        .map(|o| (o.side, o.size))
        .collect();
    assert_eq!(closes, vec![(-1, 2.0)], "one market close for the whole position");
    // The close fill resets state for the next cycle.
    engine_under_test.strategy.on_fill(&mut engine_under_test.core, &fill(-1, 2.0, 109.0));
    assert_eq!(engine_under_test.strategy.filled_size(), 0.0);
    assert_eq!(engine_under_test.strategy.anchor(), None, "re-anchors for a fresh cycle");
}

#[test]
fn dca_short_ladders_above_the_anchor() {
    // short: entries step ABOVE the anchor (sell to accumulate a short).
    let d = dca("side = \"short\"\nstep = 1.0\nrungs = 2\nsize = 1.0\nsymbol = \"BTCUSDT\"\n");
    let mut engine_under_test = engine(d, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 100.0));
    let mut rung = resting_limits(&engine_under_test);
    rung.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    assert_eq!(rung, vec![(-1, 101.0), (-1, 102.0)]);
}

#[test]
fn dca_zero_rungs_is_inert() {
    // OFF/byte-identical shape: no ladder ⇒ nothing rests.
    let d = dca("rungs = 0\nsymbol = \"BTCUSDT\"\n");
    let mut engine_under_test = engine(d, vec![bar(0, 100.0)]);
    engine_under_test.strategy.on_bar(&mut engine_under_test.core, &bar(0, 100.0));
    assert!(resting_limits(&engine_under_test).is_empty());
}
