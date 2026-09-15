//! `cheap_np` through a TOML `BacktestProfile` — the port backlog's G4/G6/G7 acceptance gate.
//!
//! `cheap_np` already ran end-to-end via the hand-written `cheap_np_run` driver (#642). This file
//! proves the SAME window, over the SAME store, produces a BIT-IDENTICAL `BacktestResult` when it
//! is expressed as a profile and dispatched through `harness::run_backtest` instead — which is the
//! whole point of the widening: composability with the harness (`run_backtest`, sweeps, the
//! `backtest` bin) without a second, divergent way to configure a run.
//!
//! The driver-shaped reference here is `cheap_np_run::run_window` transcribed verbatim: one
//! `StrategyEngine` per 5-minute window, the SPOT stream registered FIRST (`run_ticks` breaks an
//! equal-`ts` tie by stream order, so a same-millisecond spot sample must reach the strategy before
//! the print it informs), the token streams sorted `#0` then `#1`, `cash = 1000`, no fees, no
//! slippage, and a `resolution` closure that pays `1.0` to the winning outcome from the window
//! close onward with `resolution_end_ts` pinned to that close.
//!
//! Three divergences are DELIBERATE and are asserted to be inert rather than assumed away:
//! - the loader forces `fill_model = FillModelKind::Tick` (the driver leaves the `Bar` default).
//!   A `Tick::Trade` projects to a bar with NO bid/ask, and `TickFillModel` delegates to
//!   `order_fill_price` in exactly that case — so the two agree on every fill this strategy makes.
//! - the loader forces `default_venue = Some(venue)` (the driver leaves it `None`). With no bar
//!   seeding there are no bars to tag, and both `properties` and the session gate are off.
//! - the driver scans each token with `TsRange::all()`; the profile has ONE range for the whole
//!   slice. The profile's range is therefore set to cover the whole tape, and the test asserts the
//!   resulting event counts match.

// Builds a concrete temp-dir `DataFusionHist` fixture, so it needs the concrete backend
// (`datafusion-store`), not just the trait-only `hist-replay`.
#![cfg(feature = "datafusion-store")]

use std::sync::Arc;

use vike_backtest::cheap_np::WINDOW_SECS;
use vike_backtest::engine::{EngineParams, StrategyEngine, Tick};
use vike_backtest::harness::{BacktestProfile, run_backtest};
use vike_backtest::{BacktestResult, CheapNp, TokenId};
use vike_data::{DataFusionHist, HistStore};
use vike_model::{Bar, FeeSchedule, QuoteTick, TradeTick};

const SPOT_VENUE: &str = "spot";
const SPOT: &str = "BTCUSDT";
const POLY_VENUE: &str = "polymarket";
/// A real 5-minute grid point (`sts % 300 == 0`), so `TokenId::parse` accepts the slug.
const STS: i64 = 1_772_323_200;
/// Outcome 0 (Up) wins this window — the spot series drifts up.
const WINNING_INDEX: u8 = 0;

fn up() -> String {
    format!("btc-updown-5m-{STS}#0")
}
fn dn() -> String {
    format!("btc-updown-5m-{STS}#1")
}

/// One-second spot samples with a bid/ask straddling a rising mid — the shape `scan_quotes`
/// returns for an ingested `data_history.spot_1s` series.
fn spot_quotes() -> Vec<QuoteTick> {
    (0..200i64)
        .map(|i| {
            let mid = 60_000.0 * (1.0 + 0.4 * 1e-4 * i as f64) + if i % 2 == 0 { 0.5 } else { 0.0 };
            QuoteTick {
                ts: (STS - 110 + i) * 1000,
                local_ts: 0,
                bid: mid - 0.5,
                ask: mid + 0.5,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SPOT.to_string(),
            }
        })
        .collect()
}

fn print_at(ts_s: i64, price: f64) -> TradeTick {
    TradeTick {
        ts: ts_s * 1000,
        local_ts: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(), // the store keys the series; the loader re-tags on scan
    }
}

/// The UP token's taker tape: an out-of-time print, the first qualifying one, then later prints
/// that must NOT re-enter (Hold), and a post-close print so the token has a fill candidate after
/// its own entry.
fn up_prints() -> Vec<TradeTick> {
    vec![
        print_at(STS + 10, 0.20), // tte 290 -> outside [15, 270]
        print_at(STS + 40, 0.20), // the entry
        print_at(STS + 60, 0.21), // the fill lands here (next print of this token)
        print_at(STS + 80, 0.22),
        print_at(STS + 200, 0.30),
    ]
}

fn dn_prints() -> Vec<TradeTick> {
    vec![print_at(STS + 40, 0.80), print_at(STS + 100, 0.78)]
}

/// A store holding exactly the three series a `cheap_np` window needs, each under its own venue.
fn seed_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    store.append_quotes(SPOT_VENUE, SPOT, &spot_quotes(), None).unwrap();
    store.append_trades(POLY_VENUE, &up(), &up_prints(), None).unwrap();
    store.append_trades(POLY_VENUE, &dn(), &dn_prints(), None).unwrap();
    (dir, store)
}

/// `cheap_np_run::run_window`, transcribed: the hand-written driver this widening must match.
fn driver_run(store: &DataFusionHist, fee_schedule: Option<FeeSchedule>) -> BacktestResult {
    let res_ms = (STS + WINDOW_SECS) * 1000;

    // SPOT first (equal-ts tie-break is stream order), then the tokens sorted `#0` before `#1`.
    let spot: Vec<Tick> = store
        .scan_quotes(SPOT_VENUE, SPOT, vike_data::TsRange::all())
        .unwrap()
        .into_iter()
        .map(|q| Tick::Quote(QuoteTick { symbol: SPOT.to_string(), ..q }))
        .collect();
    let mut series: Vec<(String, Vec<Tick>)> = vec![(SPOT.to_string(), spot)];
    let mut tokens = [up(), dn()];
    tokens.sort();
    for sym in tokens {
        let ticks: Vec<Tick> = store
            .scan_trades(POLY_VENUE, &sym, vike_data::TsRange::all())
            .unwrap()
            .into_iter()
            .map(|t| Tick::Trade(TradeTick { symbol: sym.clone(), ..t }))
            .collect();
        series.push((sym, ticks));
    }

    let resolution = Box::new(move |sym: &str, ts: i64| -> Option<f64> {
        if ts < res_ms {
            return None;
        }
        let tok = TokenId::parse(sym)?;
        if tok.sts != STS {
            return None;
        }
        Some(if tok.oidx == WINNING_INDEX { 1.0 } else { 0.0 })
    });

    let symbols: Vec<(String, Vec<Bar>)> =
        series.iter().map(|(s, _)| (s.clone(), Vec::new())).collect();
    let mut engine = StrategyEngine::new(
        symbols,
        CheapNp::new(SPOT),
        EngineParams {
            cash: 1_000.0,
            fee_rate: 0.0,
            fee_schedule,
            slippage: 0.0,
            resolution: Some(resolution),
            resolution_end_ts: Some(res_ms),
            ..Default::default()
        },
    );
    engine.run_ticks(&series)
}

/// The profile twin of `driver_run`. `extra_engine` injects the `[engine.fee]` table for the G7
/// case; everything else is fixed.
fn profile_toml(extra_engine: &str) -> String {
    // The whole tape, with room on both sides — the driver scans each token with `TsRange::all()`,
    // so the profile's single range must not clip anything (asserted below).
    let from = (STS - 3600) * 1000;
    let to = (STS + 3600) * 1000;
    format!(
        r#"
name = "cheap-np-window-{STS}"

[data]
kind = "tick"
from = "{from}"
to = "{to}"

[[data.series]]
venue = "{SPOT_VENUE}"
symbol = "{SPOT}"
kind = "quote"

[[data.series]]
venue = "{POLY_VENUE}"
symbol = "btc-updown-5m-{STS}#0"
kind = "trade"

[[data.series]]
venue = "{POLY_VENUE}"
symbol = "btc-updown-5m-{STS}#1"
kind = "trade"

[engine]
cash = 1000.0
{extra_engine}

[engine.resolution]
kind = "binary_outcome"
[engine.resolution.winners]
"btc-updown-5m-{STS}" = {WINNING_INDEX}

[strategy]
name = "cheap_catch_updown_fair_value"
[strategy.params]
spot_symbol = "{SPOT}"
"#
    )
}

/// Bit-equality of everything a `BacktestResult` carries. `assert_eq!` on f64 deliberately — a
/// widened tolerance here would hide exactly the wiring bug this file exists to catch.
fn assert_results_identical(what: &str, got: &BacktestResult, want: &BacktestResult) {
    assert_eq!(got.trades.len(), want.trades.len(), "{what}: trade count");
    for (i, (g, w)) in got.trades.iter().zip(&want.trades).enumerate() {
        assert_eq!(g.symbol, w.symbol, "{what}: trade {i} symbol");
        assert_eq!(g.entry_price, w.entry_price, "{what}: trade {i} entry_price");
        assert_eq!(g.exit_price, w.exit_price, "{what}: trade {i} exit_price");
        assert_eq!(g.size, w.size, "{what}: trade {i} size");
        assert_eq!(g.pnl, w.pnl, "{what}: trade {i} pnl");
        assert_eq!(g.entry_ts, w.entry_ts, "{what}: trade {i} entry_ts");
        assert_eq!(g.exit_ts, w.exit_ts, "{what}: trade {i} exit_ts");
        assert_eq!(g.is_long, w.is_long, "{what}: trade {i} is_long");
    }
    assert_eq!(got.final_equity, want.final_equity, "{what}: final_equity");
    assert_eq!(got.equity_curve, want.equity_curve, "{what}: equity_curve");
    assert_eq!(got.equity_ts, want.equity_ts, "{what}: equity_ts");
}

/// THE GATE: the profile path reproduces the hand-written driver path, bit for bit.
#[test]
fn the_profile_path_reproduces_the_driver_path() {
    let (_dir, store) = seed_store();

    let want = driver_run(&store, None);
    let profile = BacktestProfile::from_toml_str(&profile_toml("")).unwrap();
    let got = run_backtest(&profile, store.clone()).unwrap();

    // The window actually traded — otherwise "identical" would be two empty runs agreeing.
    assert_eq!(want.trades.len(), 1, "the driver run must enter and settle exactly one position");
    assert_eq!(want.trades[0].symbol, up(), "the UP token is the one that qualified");
    assert_eq!(want.trades[0].exit_price, 1.0, "held to a winning binary resolution");
    // Every replayed event shows up on both sides, so the single profile range clips nothing that
    // the driver's per-token `TsRange::all()` scan saw.
    assert_eq!(
        want.equity_curve.len(),
        spot_quotes().len() + up_prints().len() + dn_prints().len()
    );

    assert_results_identical("baseline", &got, &want);
}

/// G7: the probability-scaled fee reaches the fill. Two claims at once — the profile's
/// `[engine.fee]` selects the same schedule the driver would build by hand, AND the engine now
/// charges the real `qty × 0.072 × p(1−p)` instead of the flat-rate flattening that would have
/// silently priced it at zero.
#[test]
fn the_probability_scaled_fee_is_charged_and_matches_the_driver() {
    let (_dir, store) = seed_store();

    let curve = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.072,
        maker_rate: 0.0,
        maker_rebate_share: 0.0,
    };
    let want = driver_run(&store, Some(curve));
    let fee_toml = "\n[engine.fee]\nkind = \"probability_scaled\"\ntaker_rate = 0.072\n";
    let profile = BacktestProfile::from_toml_str(&profile_toml(fee_toml)).unwrap();
    let got = run_backtest(&profile, store.clone()).unwrap();
    assert_results_identical("prob-scaled fee", &got, &want);

    // ...and the cost is REAL: exactly `0.072·p·(1−p)` at the entry fill's own price, with the
    // settlement leg free (a payout of 1.0 sits at the curve's zero, and `settle_at_payout` is
    // fee-free anyway). The fee-free baseline is the control.
    //
    // `Trade::fees` is the EXACT carrier of the charge and is what is asserted. The
    // equity DIFFERENCE is not: subtracting two ~1e3 equities to recover a ~1e-2 fee is
    // catastrophic cancellation, so it is checked to an explicit 1-ULP-of-the-equity bound —
    // a float-representation bound, not a tolerance on the fee itself.
    let free = driver_run(&store, None);
    assert_eq!(free.trades[0].fees, 0.0, "the control run must charge nothing");
    let entry_px = free.trades[0].entry_price;
    let expected_fee = curve.commission(false, free.trades[0].size, entry_px);
    assert!(expected_fee > 0.0, "the fixture's entry price must be inside the curve's support");
    assert_eq!(
        want.trades[0].fees, expected_fee,
        "the p(1−p) curve is charged once, at the entry price"
    );
    assert_eq!(got.trades[0].fees, expected_fee, "...and the profile path charges the same");
    let equity_delta = free.final_equity - want.final_equity;
    assert!(
        (equity_delta - expected_fee).abs() <= f64::EPSILON * free.final_equity,
        "the fee is the ONLY difference between the two runs: {equity_delta} vs {expected_fee}"
    );
}

/// The trap G7 names: handing the engine a `ProbabilityScaled` schedule USED to flatten through
/// `maker_taker_rates()` to `(0.0, 0.0)` and charge nothing while looking configured. Prove the
/// flattening is gone — and that the zero-rate `POLYMARKET_PROB_CURVE` still charges zero, so
/// every existing consumer of that constant is numerically unchanged.
#[test]
fn the_zero_rate_polymarket_curve_still_charges_nothing() {
    let (_dir, store) = seed_store();
    let free = driver_run(&store, None);
    let zero_curve = driver_run(&store, Some(vike_model::POLYMARKET_PROB_CURVE));
    assert_results_identical("POLYMARKET_PROB_CURVE (all-zero rates)", &zero_curve, &free);
}

/// The committed example profile must stay loadable — a schema change that breaks it should fail
/// here, not on a user's next run.
#[test]
fn the_committed_example_profile_parses_and_validates() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("profiles")
        .join("cheap_np_window.toml");
    let profile = BacktestProfile::from_path(&path).unwrap();
    assert_eq!(profile.strategy.name, "cheap_catch_updown_fair_value");
    let series = profile.data.resolved_series();
    assert_eq!(series.len(), 3, "one spot reference series + both outcome tokens");
    assert!(profile.data.is_cross_venue(), "the example is the cross-venue case");
    // The resolution source builds against the example's own symbols (coverage check included).
    let symbols: Vec<String> = series.into_iter().map(|s| s.symbol).collect();
    assert!(
        profile
            .engine
            .resolution
            .as_ref()
            .expect("the example configures settlement")
            .build(profile.base_dir.as_deref(), &symbols)
            .is_ok()
    );
}
