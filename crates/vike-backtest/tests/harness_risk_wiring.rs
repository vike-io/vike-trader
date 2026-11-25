//! `[risk]` → `StrategyEngineConfig.risk_limits` → a REAL `OrderBrokerSimulator` denial, end-to-end through
//! `run_backtest` (runprofile-wiring-step2). The unit tests in `run/profile.rs` already prove
//! the TOML parses into `vike_model::ProfileRisk`; this file proves the wiring actually reaches the
//! engine — "the field is populated" is not "the check runs" (the failure class a prior session
//! shipped tests that didn't actually pin). The account's `[engine] margin_mode` is proven the same
//! way: a spot (`"cash"`) account's oversell refused by the real engine, `"cross"` and absent alike on
//! a pair whose name says nothing spot, and absent on a spot pair taking the spot law by its name.
//!
//! Needs no concrete `HistoryStoreDataFusion`: a minimal local `HistoryStore` impl
//! hands `run_backtest` a fixed bar series directly, so this compiles and runs at DEFAULT
//! features — the FAST half (the roster lane's default build, linted single-package by `ci.yml`'s
//! `datahub-all` job), not a store-backed one.

use std::sync::Arc;

use vike_analytics::TradingRecord;
use vike_backtest::run::{StrategySource, profile::ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::Bar;

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";

/// A `HistoryStore` that hands back ONE fixed bar series for [`HistoryStore::load_bars`] regardless of
/// the requested `(venue, symbol, interval, range)` — everything else is an inert stub. Not
/// `HistoryStoreMemory`, which stores bars but answers only for the exact `(venue, symbol, interval)`
/// triple appended to it, while its tick scans stay empty — this test needs REAL bars flowing
/// through `run_backtest`'s bar-mode branch into a real `StrategyEngine`, whatever key it asks.
struct FixedBarsStore(Vec<Bar>);

impl HistoryStore for FixedBarsStore {
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
    // append_funding / scan_funding / append_chain_snapshot / scan_chain / chain_as_of* /
    // properties_as_of / list_series / inventory / series_gaps all have DEFAULTS on the trait — no
    // override needed. ⚠ "empty/no-op" is what that read until the funding and chain pairs were
    // made to REFUSE, and this list now splits THREE ways rather than two — ⚠ read the VARIANT off
    // the method in `crates/vike-data/src/store/hist.rs`, never off whichever neighbour is nearest:
    //   * `HistoryStoreError::Unsupported` — append_funding, scan_funding, append_chain_snapshot,
    //     scan_chain, and chain_as_of* derived over the last of those.
    //   * `HistoryStoreError::Query`       — list_series, inventory. They refuse for the same reason and in
    //     an older variant: their defaults predate `Unsupported` and nothing swept them.
    //   * an empty `Ok`            — properties_as_of (derived over this double's own
    //     `scan_symbol_properties`) and series_gaps.
    // Nothing here calls any of them, which is why this double still needs no arm — but a test that
    // starts calling one from either refusing group gets an `Err`, not a `[]`, and a `matches!`
    // written off the wrong group is a false negative rather than a failure.
}

const HOUR: i64 = 3_600_000;

/// One flat bar per hour, `n` bars starting at `start`, all priced `price`.
fn flat_bars(start: i64, n: i64, price: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            timestamp: start + i * HOUR,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

const CASH: f64 = 1_000_000.0;

fn profile_toml(risk_section: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1h"
from = "0"
to = "36000000"

[engine]
cash = {CASH}

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1000.0
commission = 0.1

{risk_section}
"#
    )
}

/// `buy_hold` never closes (`Trade` — entry+exit — only exists for a ROUND TRIP), so a filled-but-
/// still-open position never shows up in `TradingRecord::trades`/`n_trades`. `commission > 0` in
/// [`profile_toml`] gives a fill an observable side effect that DOES survive an open position:
/// `OrderBrokerSimulator::fold_fill` debits the transaction fee from cash immediately
/// (`sim_broker/fills.rs`'s `apply_fill`: `self.cash -= fee`), so a filled order strictly LOWERS
/// `final_equity` below `CASH` on this flat-price tape, while a denied order (no fill at all)
/// leaves it at EXACTLY `CASH`. This is the "did a real fill happen" signal these tests read,
/// alongside the RiskGate's own `dropped` reason string.
fn filled(result: &TradingRecord) -> bool {
    result.final_equity < CASH
}

/// A `[risk]` section with a `max_notional_per_order` cap far below what `buy_hold`'s single
/// market order would cost (`1000 units × 100.0 = 100_000` notional vs. a `250.0` cap) MUST deny
/// the order through the real `OrderBrokerSimulator` gate — not merely populate a struct field. Asserts on
/// `TradingRecord` (`dropped` carries the live `RiskGate`'s own reason string; `final_equity`
/// stays untouched because no fill, hence no fee, ever happened): the engine's observable
/// behavior, per the task's own "assert on behaviour, not the struct" requirement.
#[test]
fn risk_limits_from_profile_deny_a_violating_order() {
    let toml = profile_toml("[risk]\nmax_notional_per_order = 250.0");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(profile.risk.is_some(), "the `[risk]` section must parse");

    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(
        !filled(&result),
        "the over-cap order must never fill (final_equity {} should equal cash {CASH}): {result:?}",
        result.final_equity
    );
    assert!(
        result.simulator().unwrap().dropped.iter().any(|d| d.reason == "over-max-notional"),
        "the RiskGate's own denial reason must be surfaced in `dropped`: {:?}",
        result.simulator().unwrap().dropped
    );
}

/// The exact same profile MINUS `[risk]` must let the order through — the byte-identical-default
/// claim proven against the SAME strategy/bars/cash, not just "risk_limits is None" in isolation.
#[test]
fn no_risk_section_is_byte_identical_to_today() {
    let toml = profile_toml("");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(profile.risk.is_none(), "no `[risk]` section -> None");

    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(
        filled(&result),
        "with no gate armed the buy_hold order must fill (final_equity {} should be < cash \
         {CASH}): {result:?}",
        result.final_equity
    );
    assert!(
        result.simulator().unwrap().dropped.is_empty(),
        "nothing should be gate-dropped: {:?}",
        result.simulator().unwrap().dropped
    );
}

/// A generous cap (well above the order's notional) must likewise let the order through — proves
/// the gate is a real pass/deny judge, not a knob that always denies once armed.
#[test]
fn risk_limits_from_profile_admit_a_compliant_order() {
    let toml = profile_toml("[risk]\nmax_notional_per_order = 1000000.0");
    let profile = ProfileBacktest::from_toml_str(&toml)
        .expect("profile parses")
        .with_stand_in(vike_strategy_lab::stand_in);

    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(filled(&result), "a compliant order must still fill: {result:?}");
    assert!(
        result.simulator().unwrap().dropped.is_empty(),
        "nothing should be gate-dropped: {:?}",
        result.simulator().unwrap().dropped
    );
}

/// A profile whose strategy buys 1 on the first bar and sells 2 on the next (one past the long),
/// on an account stated by `engine_line` (`margin_mode = …`, or nothing).
fn oversell_toml(engine_line: &str) -> String {
    oversell_toml_on(VENUE, SYMBOL, engine_line)
}

/// [`oversell_toml`] on `venue`'s `symbol`.
fn oversell_toml_on(venue: &str, symbol: &str, engine_line: &str) -> String {
    format!(
        r#"
[data]
venue = "{venue}"
symbols = ["{symbol}"]
kind = "bar"
interval = "1h"
from = "0"
to = "36000000"

[engine]
cash = {CASH}
{engine_line}

[strategy]
file = "other/oversell.rhai"
"#
    )
}

/// The script [`oversell_toml`]'s file holds: a buy of one, then a sell of two.
const OVERSELL_SCRIPT: &str = r#"
fn on_bar(strategy) {
    if index() == 0 { strategy.order("buy", OrderSide::Long).size(1.0); }
    if index() == 1 { strategy.order("sell", OrderSide::Short).size(2.0); }
}"#;

/// `toml` parsed, its `.rhai` file's text the one a door would have read ([`OVERSELL_SCRIPT`]).
fn oversell_profile(toml: &str) -> Result<ProfileBacktest, vike_backtest::HarnessError> {
    let mut profile =
        ProfileBacktest::from_toml_str(toml)?.with_stand_in(vike_strategy_lab::stand_in);
    profile.strategy.as_mut().unwrap().source =
        Some(StrategySource::of(OVERSELL_SCRIPT.to_string()));
    Ok(profile)
}

fn run_oversell(engine_line: &str) -> TradingRecord {
    let profile = oversell_profile(&oversell_toml(engine_line)).expect("parses");
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    run_backtest(&profile, store).expect("run_backtest succeeds")
}

/// `[engine] margin_mode = "cash"`, a SPOT account, through a real run: the sell past the long is
/// refused at the submit in the live engine's own words (`vike_execution::risk::cash_oversell_refusal`),
/// and lands in `dropped`; a margin account sells it and goes short.
#[test]
fn a_cash_account_refuses_the_oversell_in_a_real_run() {
    let result = run_oversell("margin_mode = \"cash\"");
    let refusal = "a spot account cannot sell more than it holds: it has 1, this sells 2; nothing \
                   was sent";
    let reasons: Vec<&str> =
        result.simulator().unwrap().dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(
        reasons,
        vec![refusal],
        "the sell of 2 against a long of 1: {:?}",
        result.simulator().unwrap().dropped
    );
    assert_eq!(result.simulator().unwrap().dropped[0].symbol, SYMBOL);
    assert!(
        run_oversell("").simulator().unwrap().dropped.is_empty(),
        "a margin account refuses nothing"
    );
}

/// **A pair whose NAME says spot refuses the oversell with no key at all** (the spot rule, owner 2026-11-11: the
/// backtest, paper and live apply one spot law), and the stamp says `cash`; `margin_mode = "cross"` written out
/// keeps the old margin account, which sells it and goes short.
#[test]
fn a_spot_pair_by_name_refuses_the_oversell_unless_cross_is_written() {
    let run = |engine_line: &str| {
        let toml = oversell_toml_on("binance", "BTCUSDT", engine_line);
        let profile = oversell_profile(&toml).expect("parses");
        let store: Arc<dyn HistoryStore + Send + Sync> =
            Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
        let stamp = vike_backtest::run::report::realism_stamp(&profile);
        (run_backtest(&profile, store).expect("run_backtest succeeds"), stamp)
    };
    let (absent, stamp) = run("");
    let reasons: Vec<&str> =
        absent.simulator().unwrap().dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(
        reasons,
        vec![
            "a spot account cannot sell more than it holds: it has 1, this sells 2; nothing was sent"
        ],
        "a spot pair by name: {:?}",
        absent.simulator().unwrap().dropped
    );
    assert_eq!(stamp.values.get("engine.margin_mode").map(String::as_str), Some("cash"));
    let (cross, stamp) = run("margin_mode = \"cross\"");
    assert!(
        cross.simulator().unwrap().dropped.is_empty(),
        "cross written out keeps the margin account"
    );
    assert_eq!(stamp.values.get("engine.margin_mode").map(String::as_str), Some("cross"));
}

/// **A venue that books only cash judges every symbol by the spot law with no key** (E9: polymarket's outcome
/// tokens, whose names say nothing spot, were a margin account a sell past the long turned short, which live
/// refuses), and `margin_mode = "cross"` written out keeps the margin account there too.
#[test]
fn an_outcome_token_takes_the_spot_law_unless_cross_is_written() {
    let stamp = |engine_line: &str| {
        let profile =
            oversell_profile(&oversell_toml_on("polymarket", "TKN", engine_line)).expect("parses");
        vike_backtest::run::report::realism_stamp(&profile)
            .values
            .get("engine.margin_mode")
            .cloned()
    };
    assert_eq!(stamp("").as_deref(), Some("cash"), "absent: the spot law");
    assert_eq!(stamp("margin_mode = \"cross\"").as_deref(), Some("cross"));
}

/// On a pair whose NAME says nothing spot (`TESTUSDT` on `test`), absent and `"cross"` are the same
/// account, and the same run as before the key existed, to the last bit of every field of the result.
#[test]
fn an_absent_margin_mode_and_cross_run_byte_identical() {
    let absent = format!("{:?}", run_oversell(""));
    assert_eq!(absent, format!("{:?}", run_oversell("margin_mode = \"cross\"")));
    assert_ne!(absent, format!("{:?}", run_oversell("margin_mode = \"cash\"")), "the key acts");
}

/// `isolated` is a mode vike does not run, and a word that is no margin mode is no default: both
/// are refused at load, in words.
#[test]
fn isolated_and_an_unknown_margin_mode_are_refused_at_load() {
    let refused =
        |word: &str| match oversell_profile(&oversell_toml(&format!("margin_mode = \"{word}\""))) {
            Err(vike_backtest::HarnessError::Validation(msg)) => msg.to_string(),
            other => panic!("{word}: expected a validation refusal, got {other:?}"),
        };
    assert_eq!(
        refused("isolated"),
        "engine.margin_mode \"isolated\": vike does not run isolated margin yet"
    );
    for word in ["spot", "Cash", ""] {
        assert!(refused(word).starts_with("unknown engine.margin_mode"), "{word}");
    }
}
