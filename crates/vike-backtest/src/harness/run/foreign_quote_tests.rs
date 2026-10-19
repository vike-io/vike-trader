//! A backtest keeps ONE ledger in dollars and holds no conversion-rate series (decision 0183, item
//! 13), so a series whose symbol NAME says another fiat quote is refused in words, never run with its
//! yen or pounds booked as dollars.
//!
//! The figures are the decision's: USDJPY, 100,000 units, closed 0.10 higher, is 10,000 JPY, which a
//! dollar account books as 10,000 "USD" against 10,000 / 150.10 = 66.62 USD in truth; EURGBP,
//! 100,000 units at 0.85, is 85,000 GBP, which is 114,750 USD at 1.35 and not 85,000.

use super::*;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_store::DataFusionHist;

const VENUE: &str = "oanda";

fn profile(symbol: &str, kind: &str) -> BacktestProfile {
    let toml = format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{symbol}"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 100000.0
symbol = "{symbol}"
"#
    );
    BacktestProfile::from_toml_str(&toml).unwrap()
}

/// A store holding `symbol` as three daily bars and two quotes, the last 0.10 above the first. The
/// directory is returned beside it: it must outlive the store.
fn store_with(symbol: &str, px: f64) -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, px),
        flat_bar_zero_volume(1000, px + 0.05),
        flat_bar_zero_volume(2000, px + 0.10),
    ];
    store.append_bars(VENUE, symbol, "1d", &bars, None).unwrap();
    let quotes = vec![quote(0, px, px + 0.01), quote(1000, px + 0.10, px + 0.11)];
    store.append_quotes(VENUE, symbol, &quotes, None).unwrap();
    (dir, store)
}

fn refusal(symbol: &str, px: f64, kind: &str) -> String {
    let (_dir, store) = store_with(symbol, px);
    match run_backtest(&profile(symbol, kind), store) {
        Err(HarnessError::Data(m)) => m,
        Err(other) => panic!("{symbol}/{kind}: expected a data refusal, got {other:?}"),
        Ok(r) => panic!(
            "{symbol}/{kind}: the run was NOT refused and booked its quote currency as dollars \
             (final equity {}): a 0.10 move on 100,000 units is 10,000 \"USD\" here and 66.62 USD \
             in truth",
            r.final_equity
        ),
    }
}

#[test]
fn a_yen_quoted_bar_series_is_refused_in_words() {
    let m = refusal("USDJPY", 150.0, "bar");
    assert!(m.contains("USDJPY (JPY)"), "names the symbol and its currency: {m}");
    assert!(m.contains("USD"), "and the ledger it would be booked in: {m}");
}

#[test]
fn a_pound_quoted_tick_series_is_refused_in_words() {
    let m = refusal("EURGBP", 0.85, "tick");
    assert!(m.contains("EURGBP (GBP)"), "{m}");
}

/// A dollar-quoted series is untouched: the run proceeds and books exactly as before.
#[test]
fn a_dollar_quoted_series_runs_as_before() {
    for kind in ["bar", "tick"] {
        let (_dir, store) = store_with("EURUSD", 1.08);
        let result = run_backtest(&profile("EURUSD", kind), store)
            .unwrap_or_else(|e| panic!("EURUSD/{kind} must still run: {e:?}"));
        assert_ne!(result.final_equity, 1_000_000.0, "{kind}: buy_hold traded");
    }
}

/// The walk-forward door hands the harness bars it already holds: it is refused the same way.
#[test]
fn bars_a_caller_already_holds_are_refused_the_same_way() {
    let (_dir, store) = store_with("USDJPY", 150.0);
    let store: Arc<dyn HistStore + Send + Sync> = store;
    let bars = vec![("USDJPY".to_string(), vec![flat_bar_zero_volume(0, 150.0)])];
    let err = run_backtest_over_bars(&profile("USDJPY", "bar"), &store, bars)
        .expect_err("a yen-quoted window is refused");
    assert!(matches!(&err, HarnessError::Data(m) if m.contains("USDJPY (JPY)")), "{err:?}");
}
