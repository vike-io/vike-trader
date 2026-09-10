//! Gate for the opt-in [`EngineParams::funding_source`] seam: a perp held across a funding window
//! with a `funding_source` supplied is CHARGED funding; the same run WITHOUT the source charges
//! ZERO (the pre-seam behavior, where `Bar::funding` is the only source and no production data path
//! sets it). Also pins the precedence rule: a recorded `Bar::funding` WINS and the source is not
//! consulted for that step. Mirrors the run-construction pattern in `properties_fills.rs`.
use std::sync::Arc;

use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_model::{Bar, Strategy};

/// The `EngineParams::funding_source` closure type — `(venue, symbol, ts_ms) -> Some(rate)`.
type FundingSource = Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>;

const BASE_TS: i64 = 1_700_000_000_000;
const STEP_MS: i64 = 60_000;

/// Flat-price bars (so the only equity mover is funding), with an optional per-index `Bar::funding`.
fn mk_bars(symbol: &str, n: usize, price: f64, bar_funding: &[(usize, f64)]) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let funding = bar_funding.iter().find(|(idx, _)| *idx == i).map(|(_, r)| *r);
            Bar {
                ts: BASE_TS + i as i64 * STEP_MS,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 0.0,
                funding,
                bid: None,
                ask: None,
                symbol: Some(symbol.to_string()),
            }
        })
        .collect()
}

/// Opens a long of `size` at bar `open_at` (fills at the following bar's open), then closes at
/// bar `close_at`.
struct OpenHoldClose {
    symbol: String,
    open_at: usize,
    close_at: usize,
    size: f64,
}

impl Strategy<SimBroker> for OpenHoldClose {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        let idx = ctx.index;
        if idx == self.open_at {
            ctx.submit(&self.symbol, 1, self.size, 0.0, true, None);
        } else if idx == self.close_at {
            ctx.submit_close(&self.symbol);
        }
    }
}

fn params(funding_source: Option<FundingSource>) -> EngineParams {
    EngineParams {
        cash: 10_000.0,
        default_venue: Some("TEST".into()),
        funding_source,
        ..Default::default()
    }
}

fn run(bar_funding: &[(usize, f64)], source: Option<f64>, source_at_ts: i64) -> f64 {
    let sym = "PERP0";
    // idx0: submit open (fills idx1). Held over idx1..=idx3. idx3: submit close (fills idx4).
    let bars = mk_bars(sym, 5, 100.0, bar_funding);
    let series = vec![(sym.to_string(), bars)];
    let strat = OpenHoldClose { symbol: sym.to_string(), open_at: 0, close_at: 3, size: 2.0 };
    let src: Option<FundingSource> = source.map(|rate| {
        let f: FundingSource =
            Arc::new(move |_v, _s, ts| if ts == source_at_ts { Some(rate) } else { None });
        f
    });
    StrategyEngine::new(series, strat, params(src)).run().final_equity
}

/// A long held across a single funding window: WITH a `funding_source` it pays funding; WITHOUT it
/// pays nothing (byte-identical to the pre-seam engine). Flat price + zero fees means the entire
/// equity gap is the funding charge.
#[test]
fn funding_source_charges_held_perp_and_absence_is_zero() {
    // The funding event is stamped at the third bar's ts (idx2), a step the 2.0-long is held.
    let funding_ts = BASE_TS + 2 * STEP_MS;
    // funding_charge(size=2.0, mark=100.0, rate=0.01, mult=1.0) = 2.0 * 100 * 0.01 = 2.0.
    let expected_charge = 2.0 * 100.0 * 0.01;

    let no_source = run(&[], None, funding_ts);
    let with_source = run(&[], Some(0.01), funding_ts);

    // No source -> flat price, no fees, no funding -> equity is exactly the starting cash.
    assert!((no_source - 10_000.0).abs() < 1e-9, "unfunded equity={no_source}");
    // Source supplied -> the long paid one funding charge.
    assert!(
        (with_source - (10_000.0 - expected_charge)).abs() < 1e-9,
        "funded equity={with_source}, expected={}",
        10_000.0 - expected_charge
    );
    assert!(with_source < no_source, "funded run must have LOWER equity than the unfunded run");
}

/// Precedence: a recorded `Bar::funding` is the on-the-bar truth and WINS — the source is not
/// consulted for that step. Here the bar carries 0.02 at idx2 and the source would return 0.01;
/// the charge reflects the BAR rate, proving the bar wins.
#[test]
fn bar_funding_wins_over_source() {
    let funding_ts = BASE_TS + 2 * STEP_MS;
    // Bar carries rate 0.02 at idx2; the source would offer 0.01 at the same ts.
    let with_bar_and_source = run(&[(2, 0.02)], Some(0.01), funding_ts);
    // funding_charge(2.0, 100.0, 0.02, 1.0) = 4.0 -> the BAR rate, not the source's 2.0.
    let expected = 10_000.0 - (2.0 * 100.0 * 0.02);
    assert!(
        (with_bar_and_source - expected).abs() < 1e-9,
        "equity={with_bar_and_source}, expected={expected} (bar rate must win)"
    );
}

/// The net funding cashflow is surfaced DISTINCTLY on [`BacktestResult::funding_paid`] (the backtest
/// twin of `vike_exec::Account.funding_paid`), not merely folded into `final_equity`. A long paying a
/// positive-rate funding charge shows a NEGATIVE `funding_paid` equal to the equity it cost; and with
/// NO source `funding_paid` is exactly `0.0` (the byte-identical no-funding path).
#[test]
fn funding_paid_is_surfaced_on_the_result() {
    let sym = "PERP0";
    let funding_ts = BASE_TS + 2 * STEP_MS;
    let build = |src: Option<FundingSource>| {
        let bars = mk_bars(sym, 5, 100.0, &[]);
        let series = vec![(sym.to_string(), bars)];
        let strat = OpenHoldClose { symbol: sym.to_string(), open_at: 0, close_at: 3, size: 2.0 };
        StrategyEngine::new(series, strat, params(src)).run()
    };

    let src: FundingSource =
        Arc::new(move |_v, _s, ts| if ts == funding_ts { Some(0.01) } else { None });
    let funded = build(Some(src));
    // funding_charge(2.0, 100.0, 0.01, 1.0) = 2.0; the long PAYS it → funding_paid = -2.0.
    let expected_charge = 2.0 * 100.0 * 0.01;
    assert!(
        (funded.funding_paid - (-expected_charge)).abs() < 1e-9,
        "funding_paid={}, expected {}",
        funded.funding_paid,
        -expected_charge
    );
    // Flat price + zero fees ⇒ funding is the WHOLE equity gap: funding_paid == final_equity − cash0.
    assert!(
        (funded.funding_paid - (funded.final_equity - 10_000.0)).abs() < 1e-9,
        "funding_paid must equal the equity gap"
    );

    // Absent a source, funding_paid is EXACTLY 0.0 (no funding charged, byte-identical path).
    let unfunded = build(None);
    assert_eq!(unfunded.funding_paid, 0.0, "no source ⇒ funding_paid is exactly 0.0");
}
