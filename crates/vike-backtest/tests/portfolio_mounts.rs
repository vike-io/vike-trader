//! **A `[[mounts]]` PORTFOLIO through `run_backtest`** (decision 0300): several strategy files over one simulated
//! account, dispatched in mount order. Each mount's row of the record is its own run's figures when the mounts do
//! not meet (parity with each file's `[strategy]` run, the account the sum of both); two mounts racing one `[risk]`
//! budget fill in mount order (swapped, the other fills); a mount's leverage cap refuses that mount's opening order
//! by the live word while another mount trades, and `engine.max_mount_leverage` caps each mount, never their sum;
//! a mount's `max_loss` latches that mount alone and its row says when (`latched_at`); mounts that disagree on a
//! cost or a header law are refused by name; and a `[strategy]` report keeps its bytes (no `mounts` key).
//!
//! Needs no concrete store: a minimal `HistoryStore` hands every series the same bars (rising, falling for the
//! latch), and the mounts are `.rhai` files whose text the test hands in as a door would have read it.

use std::sync::Arc;

use vike_analytics::TradingRecord;
use vike_analytics::results::{TradingResults, TradingResultsMounts};
use vike_backtest::run::{StrategySource, profile::ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::Bar;

/// Every series this store is asked for: the same bars.
struct SameBarsStore(Vec<Bar>);

impl HistoryStore for SameBarsStore {
    fn load_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Ok(self.0.clone())
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

const HOUR: i64 = 3_600_000;
const CASH: f64 = 1_000.0;

/// 24 hourly bars from 0, the `i`th priced `price(i)` throughout.
fn hourly_bars(price: impl Fn(f64) -> f64) -> Vec<Bar> {
    (0..24)
        .map(|i| {
            let price = price(i as f64);
            Bar {
                timestamp: i * HOUR,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// Buy `size` every fourth bar and sell it two bars later: one winning round trip each four bars.
const ROUND_TRIPS: &str = r#"
let size = param("size", 1.0);
fn on_bar(strategy) {
    let i = index();
    if i % 4 == 0 { strategy.order("in", OrderSide::Long).size(size); }
    if i % 4 == 2 { strategy.order("out", OrderSide::Short).size(size); }
}"#;

/// Buy `size` on the first bar and hold it.
const BUY_ONCE: &str = r#"
let size = param("size", 1.0);
fn on_bar(strategy) {
    if index() == 0 { strategy.order("in", OrderSide::Long).size(size); }
}"#;

/// The `[data]` and `[engine]` of every profile here: `symbols` of one venue, 24 hourly bars, `engine` extra lines.
fn head(symbols: &str, engine: &str) -> String {
    format!(
        r#"
[data]
venue = "test"
symbols = [{symbols}]
kind = "bar"
interval = "1h"
from = "0"
to = "{to}"

[engine]
cash = {CASH}
{engine}
"#,
        to = 23 * HOUR
    )
}

/// One `[[mounts]]` entry on `symbols`, its `extra` keys, its file and its params.
fn mount(symbols: &str, extra: &str, file: &str, params: &str) -> String {
    format!(
        "\n[[mounts]]\nsymbols = [{symbols}]\n{extra}\n[mounts.strategy]\nfile = \"{file}\"\n\
         [mounts.strategy.params]\n{params}\n"
    )
}

/// `text` loaded, each strategy table (`[strategy]`'s, or every mount's in order) handed `scripts`' text as a
/// door would have read it.
fn loaded(text: &str, scripts: &[&str]) -> ProfileBacktest {
    let mut p = ProfileBacktest::from_toml_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    for (strategy, script) in p.strategies_mut().zip(scripts) {
        strategy.source = Some(StrategySource::of((*script).to_string()));
    }
    p
}

/// `p` run over bars priced `100 + i`: every long a script holds gains.
fn run(p: &ProfileBacktest) -> Result<TradingRecord, vike_backtest::HarnessError> {
    run_on(p, hourly_bars(|i| 100.0 + i))
}

fn run_on(
    p: &ProfileBacktest,
    bars: Vec<Bar>,
) -> Result<TradingRecord, vike_backtest::HarnessError> {
    run_backtest(p, Arc::new(SameBarsStore(bars)))
}

fn rows(record: &TradingRecord) -> &TradingResultsMounts {
    record.mounts.as_ref().expect("a portfolio's record carries its mounts")
}

/// **Parity**: two mounts on two slots of one account, each trading as its own `[strategy]` run does. Each mount's
/// row is that run's figures (its closed trades, win %, PnL), the account's trades are their union and its PnL their
/// sum, and nothing is unattributed.
#[test]
fn two_mounts_on_their_own_slots_are_each_their_own_run() {
    let commission = "commission = 0.1";
    let portfolio = loaded(
        &format!(
            "{}{}{}",
            head("\"AAAUSDT\", \"BBBUSDT\"", ""),
            mount("\"AAAUSDT\"", "", "test/a.rhai", &format!("size = 2.0\n{commission}")),
            mount("\"BBBUSDT\"", "", "test/b.rhai", &format!("size = 3.0\n{commission}")),
        ),
        &[ROUND_TRIPS, ROUND_TRIPS],
    );
    let solo = |symbol: &str, size: f64| {
        let text = format!(
            "{}\n[strategy]\nfile = \"test/solo.rhai\"\n[strategy.params]\nsize = {size:?}\n{commission}\n",
            head(&format!("\"{symbol}\""), "")
        );
        run(&loaded(&text, &[ROUND_TRIPS])).expect("a [strategy] run")
    };
    let (a, b) = (solo("AAAUSDT", 2.0), solo("BBBUSDT", 3.0));
    let record = run(&portfolio).expect("a portfolio runs");
    let mounts = rows(&record);
    assert_eq!(mounts.rows.len(), 2);
    for (row, alone) in mounts.rows.iter().zip([&a, &b]) {
        let closed = alone.closed_trades.as_ref().expect("closed trades on by default");
        assert!(closed.all.trades > 0, "the script trades: {row:?}");
        assert_eq!(row.n_trades, closed.all.trades, "{row:?}");
        assert_eq!(row.win_rate, closed.all.percent_profitable, "{row:?}");
        assert!(
            (row.pnl - (alone.final_equity - CASH)).abs() < 1e-9,
            "{row:?} vs {}",
            alone.final_equity
        );
    }
    assert_eq!(record.n_trades, a.n_trades + b.n_trades, "the account's trades are both mounts'");
    let both = (a.final_equity - CASH) + (b.final_equity - CASH);
    assert!((record.final_equity - CASH - both).abs() < 1e-9, "the account is the sum of both");
    assert!(mounts.unattributed_pnl.abs() < 1e-9, "every fill is a mount's: {mounts:?}");
    let report = TradingResults::from_record(None, &record, 252.0);
    assert!(
        report.to_string().contains("mounts:\n  [0] test/a.rhai (AAAUSDT): n_trades"),
        "{report}"
    );
    // A `[strategy]` report keeps its bytes: no `mounts` key, no table.
    let alone = TradingResults::from_record(None, &a, 252.0);
    assert!(!serde_json::to_string(&alone).unwrap().contains("\"mounts\""));
    assert!(!alone.to_string().contains("mounts:"));
}

/// **Mount order is the drain's**: two mounts on ONE slot racing one `[risk]` budget. The first listed fills and
/// the second is refused by the account's gate; swapped, the other fills; with no `[risk]` both fill (the race is
/// real, not an artefact of the sizes).
#[test]
fn two_mounts_racing_one_budget_fill_in_mount_order() {
    let profile = |risk: &str, first: f64, second: f64| {
        loaded(
            &format!(
                "{}{}{}{risk}",
                head("\"AAAUSDT\"", ""),
                mount("\"AAAUSDT\"", "", "test/first.rhai", &format!("size = {first:?}")),
                mount("\"AAAUSDT\"", "", "test/second.rhai", &format!("size = {second:?}")),
            ),
            &[BUY_ONCE, BUY_ONCE],
        )
    };
    let filled = |record: &TradingRecord| -> Vec<bool> {
        rows(record).rows.iter().map(|r| r.pnl > 0.0).collect()
    };
    let risk = "\n[risk]\nmax_leverage = 1.0\n";
    let raced = run(&profile(risk, 8.0, 6.0)).expect("runs");
    assert_eq!(filled(&raced), [true, false], "{:?}", rows(&raced));
    let dropped = &raced.simulator().expect("a backtest").dropped;
    assert_eq!(dropped.len(), 1, "the second mount's order is refused: {dropped:?}");
    let swapped = run(&profile(risk, 6.0, 8.0)).expect("runs");
    assert_eq!(filled(&swapped), [true, false], "the first listed fills: {:?}", rows(&swapped));
    assert!(rows(&swapped).rows[0].pnl < rows(&raced).rows[0].pnl, "the 6 filled, not the 8");
    let free = run(&profile("", 8.0, 6.0)).expect("runs");
    assert_eq!(filled(&free), [true, true], "without [risk] both fill");
}

/// **A mount's leverage cap refuses that mount alone**, by the live word (`over-mount-leverage`), while another
/// mount trades; and `engine.max_mount_leverage` caps EACH mount, never their sum (two mounts each at 1x of the
/// account's equity both fill under a 1.5 cap).
#[test]
fn a_mounts_cap_refuses_its_own_opening_order_and_the_engine_figure_caps_each_mount() {
    let capped = loaded(
        &format!(
            "{}{}{}",
            head("\"AAAUSDT\", \"BBBUSDT\"", ""),
            mount("\"AAAUSDT\"", "", "test/a.rhai", "size = 1.0"),
            mount("\"BBBUSDT\"", "max_leverage = 1.0", "test/b.rhai", "size = 25.0"),
        ),
        &[BUY_ONCE, BUY_ONCE],
    );
    let record = run(&capped).expect("runs");
    let [a, b] = [&rows(&record).rows[0], &rows(&record).rows[1]];
    assert!(a.pnl > 0.0 && a.leverage_refusals == 0, "{a:?}");
    assert_eq!((b.pnl, b.leverage_refusals, b.max_leverage), (0.0, 1, Some(1.0)), "{b:?}");
    let dropped = &record.simulator().expect("a backtest").dropped;
    assert!(
        dropped.iter().any(|d| d.reason.starts_with(vike_execution::risk::MOUNT_LEVERAGE_REFUSAL)),
        "{dropped:?}"
    );

    let each = loaded(
        &format!(
            "{}{}{}",
            head("\"AAAUSDT\", \"BBBUSDT\"", "max_mount_leverage = 1.5"),
            mount("\"AAAUSDT\"", "", "test/a.rhai", "size = 9.0"),
            mount("\"BBBUSDT\"", "", "test/b.rhai", "size = 9.0"),
        ),
        &[BUY_ONCE, BUY_ONCE],
    );
    let record = run(&each).expect("runs");
    for row in &rows(&record).rows {
        assert!(row.pnl > 0.0 && row.leverage_refusals == 0, "each under 1.5x: {row:?}");
        assert_eq!(row.max_leverage, Some(1.5), "the engine figure is each mount's default");
    }
}

/// **A mount's `max_loss` latches that mount alone, and its row says when**: the key reaches the simulator's budget
/// sweep, which latches the mount at the close its loss passes the budget and (the default action, `flatten`) closes
/// its book at the next open, so it stops losing; the row's `latched_at` and the human table say when, and a sibling
/// with no budget rides the fall to the last bar.
#[test]
fn a_mounts_max_loss_latches_it_alone_and_its_row_says_when() {
    let max_loss = 5.5;
    let profile = loaded(
        &format!(
            "{}{}{}",
            head("\"AAAUSDT\", \"BBBUSDT\"", ""),
            mount("\"AAAUSDT\"", &format!("max_loss = {max_loss:?}"), "test/a.rhai", "size = 1.0"),
            mount("\"BBBUSDT\"", "", "test/b.rhai", "size = 1.0"),
        ),
        &[BUY_ONCE, BUY_ONCE],
    );
    // Every long loses 1 a bar.
    let record = run_on(&profile, hourly_bars(|i| 200.0 - i)).expect("runs");
    let [latched, sibling] = [&rows(&record).rows[0], &rows(&record).rows[1]];
    let at = latched.latched_at.unwrap_or_else(|| panic!("never latched: {latched:?}"));
    assert!(0 < at && at <= 24 * HOUR, "latched inside the run: {latched:?}");
    assert_eq!(latched.max_loss, Some(max_loss), "{latched:?}");
    assert!(
        latched.pnl < -max_loss && latched.pnl > -(max_loss + 3.0),
        "flattened just after its loss passed {max_loss}: {latched:?}"
    );
    assert_eq!((sibling.latched_at, sibling.max_loss), (None, None), "{sibling:?}");
    assert!(sibling.pnl < -20.0, "no budget, it rides the fall: {sibling:?}");
    let table = TradingResults::from_record(None, &record, 252.0).to_string();
    assert!(table.contains("latched 1970-01-01 ") && table.contains("latched never"), "{table}");
}

/// **One run has one cost and one header law**: mounts charging different commissions, or whose files fill a
/// tagged plain limit by different laws, are refused by name before anything runs.
#[test]
fn mounts_that_disagree_on_a_cost_or_a_header_law_are_refused() {
    let two = |second: &str| {
        format!(
            "{}{}{}",
            head("\"AAAUSDT\", \"BBBUSDT\"", ""),
            mount("\"AAAUSDT\"", "", "test/a.rhai", "commission = 0.1"),
            mount("\"BBBUSDT\"", "", "test/b.rhai", second),
        )
    };
    let costs = run(&loaded(&two("commission = 0.2"), &[BUY_ONCE, BUY_ONCE])).unwrap_err();
    assert!(costs.message().contains("different commissions"), "{costs}");
    let maker =
        format!("//! ```toml\n//! [strategy]\n//! optimistic_quotes = true\n//! ```\n{BUY_ONCE}");
    let law = run(&loaded(&two("commission = 0.1"), &[BUY_ONCE, &maker])).unwrap_err();
    assert!(law.message().contains("different laws"), "{law}");
}
