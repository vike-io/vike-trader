//! A profile run books a deribit or bybit coin-margined (inverse) contract in its SETTLE COIN, the
//! live and paper law (decision 0258, `run_ledger_coin`), and a binance COIN-M or okx one at the face its
//! committed row states, and refuses what it cannot book there: an inverse contract whose face value only
//! its row states and no committed row does, a run mixing it with a linear symbol or a second coin, and a
//! knob still linear at its site. The worked numbers are the venues' own (the inverse research, Part 1).

use super::*;
use crate::harness::KeptRows;
use crate::harness::strategy_builder::StrategyBuilder;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote, trade};
use vike_model::{ContractValue, OrderDrop};
use vike_simulator::test_support::fixtures::BracketPerSymbol;
use vike_store::HistoryStoreDataFusion;

/// A buy-and-hold profile over one symbol of `venue`, `extra` appended after the strategy table. It
/// states `kind = "free"`: the worked numbers are fee-free, and a profile that states no fee is costed
/// at its venue's table since 2026-11-08.
fn profile_on(
    venue: &str,
    symbol: &str,
    kind: &str,
    cash: f64,
    size: f64,
    extra: &str,
) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "{venue}"
symbols = ["{symbol}"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = {cash:?}
[engine.fee]
kind = "free"

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = {size:?}
symbol = "{symbol}"
{extra}
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategies_check::stand_in)
}

/// A store that holds the series, eleven rows at `px` and a last one at `last`, as bars and as quotes:
/// `buy_hold` buys at `px` and is marked at `last`.
fn store_with(
    venue: &str,
    symbol: &str,
    px: f64,
    last: f64,
) -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let at = |i: i64| if i == 11 { last } else { px };
    let bars: Vec<_> = (0..12).map(|i| flat_bar_zero_volume(i * 1000, at(i))).collect();
    store.append_bars(venue, symbol, "1d", &bars, None).unwrap();
    let quotes: Vec<_> = (0..12).map(|i| quote(i * 1000, at(i), at(i))).collect();
    store.append_quotes(venue, symbol, &quotes, None).unwrap();
    (dir, store)
}

fn close(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() <= tolerance
}

/// What the simulator booked before the coin ledger: an inverse position at the linear default value is
/// wrong by the price, which is why a door with no contract value still refuses one.
#[test]
fn the_linear_default_books_an_inverse_position_wrong_by_the_price() {
    let (qty, price) = (10.0, 65_700.0);
    let live = ContractValue::inverse(1.0);
    assert_eq!(live.order_notional(qty, price), 10.0);
    let linear_default = vike_model::signed_notional(qty, price, 1.0);
    assert_eq!(linear_default, 657_000.0);
    assert_eq!(linear_default / live.signed_notional(qty, price), price, "wrong by the price");
}

/// THE OWNER'S CASE. Deribit `BTC-PERPETUAL`, the research's worked example: 1 BTC of cash, 1,000 USD
/// long from 60,000, marked at 66,000. The venue's books: `1,000·(1/60,000 − 1/66,000)` =
/// 0.0015151515 BTC of PnL on the coin, and the dollars are that equity times the contract's price.
/// It ran on neither lane before (refused by name).
#[test]
fn a_deribit_inverse_contract_books_its_settle_coin_on_both_lanes() {
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    for kind in ["bar", "tick"] {
        let (_dir, store) = store_with("deribit", "BTC-PERPETUAL", 60_000.0, 66_000.0);
        let profile = profile_on("deribit", "BTC-PERPETUAL", kind, 1.0, 1_000.0, "");
        let result = run_backtest(&profile, store).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        assert!(
            close(result.final_equity, 1.0 + coin_pnl, 1e-12),
            "{kind}: {} BTC, want {}",
            result.final_equity,
            1.0 + coin_pnl
        );
        assert_eq!(result.settle_price, Some(66_000.0), "{kind}: valued at the contract's price");
        let report = vike_analytics::report::BacktestReport::from_result(None, &result, 252.0)
            .with_realism(super::super::report::realism_stamp(&profile));
        let dollars = report.in_dollars.expect("a coin ledger says its dollars");
        assert!(close(dollars.final_equity, (1.0 + coin_pnl) * 66_000.0, 1e-6), "{kind}");
        let table = report.to_string();
        assert!(table.contains("account_currency:   BTC"), "{kind}: names its coin:\n{table}");
        assert!(table.contains("in_dollars:    66100.00 (at 66000.00 a coin)"), "{kind}:\n{table}");
        assert!(table.contains("final_equity:  1.00151515"), "{kind}: eight places:\n{table}");
    }
}

/// Bybit `BTCUSD`, the venue's own example: 1,000 contracts (dollars) long from 5,000 to 5,500 is
/// 0.01818 BTC, and its 0.055% taker fee is `(1,000 / 5,000) × 0.055%` = 0.00011 BTC.
#[test]
fn a_bybit_inverse_perpetual_books_the_venues_coin_pnl_and_fee() {
    let (_dir, store) = store_with("bybit", "BTCUSD", 5_000.0, 5_500.0);
    let mut profile = profile_on("bybit", "BTCUSD", "bar", 1.0, 1_000.0, "");
    profile.engine.fee = None;
    profile.engine.fee_rate = 0.000_55;
    let result = run_backtest(&profile, store).expect("bybit inverse runs");
    let coin_pnl = 1_000.0 * (1.0 / 5_000.0 - 1.0 / 5_500.0);
    assert!(close(coin_pnl, 0.018_18, 1e-5), "the venue's 0.01819: {coin_pnl}");
    assert!(close(result.fees_paid, 0.000_11, 1e-15), "fee in BTC: {}", result.fees_paid);
    assert!(
        close(result.final_equity, 1.0 - 0.000_11 + coin_pnl, 1e-12),
        "{}",
        result.final_equity
    );
}

/// A caller holding its bars books them by the profile's ledger too: the door the walk-forward's search
/// goes through.
#[test]
fn bars_a_caller_already_holds_book_the_coin_too() {
    let (_dir, store) = store_with("deribit", "BTC-PERPETUAL", 60_000.0, 66_000.0);
    let store: Arc<dyn HistoryStore + Send + Sync> = store;
    let bars = vec![(
        "BTC-PERPETUAL".to_string(),
        (0..12)
            .map(|i| flat_bar_zero_volume(i * 1000, if i == 11 { 66_000.0 } else { 60_000.0 }))
            .collect(),
    )];
    let profile = profile_on("deribit", "BTC-PERPETUAL", "bar", 1.0, 1_000.0, "");
    let result = run_backtest_over_bars(&profile, &store, bars, &[], &[], &Arc::default()).unwrap();
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(result.final_equity, 1.0 + coin_pnl, 1e-12), "{}", result.final_equity);
}

/// ⚠ The kind is by venue: `BTC_USDC-PERPETUAL` is the linear book of the same coin and books
/// `size x price` in dollars, as `BTCUSD` does on a venue where it is a linear CFD.
#[test]
fn a_linear_book_runs_as_before_on_both_lanes() {
    for (venue, symbol) in [("deribit", "BTC_USDC-PERPETUAL"), ("ctrader", "BTCUSD")] {
        for kind in ["bar", "tick"] {
            let (_dir, store) = store_with(venue, symbol, 100.0, 101.0);
            let result =
                run_backtest(&profile_on(venue, symbol, kind, 1_000_000.0, 10.0, ""), store)
                    .unwrap_or_else(|e| panic!("{venue} {symbol} {kind}: {e:?}"));
            assert_eq!(result.final_equity, 1_000_010.0, "{venue} {symbol} {kind}: 10 x 1.0");
            assert_eq!(result.settle_price, None, "a dollar run has no coin to value");
        }
    }
}

/// Binance COIN-M and okx state their face value only in the contract's row, and the catalog carries
/// the face of every COMMITTED row (`vike_catalog::contract_value`, held equal to each row by the
/// bridges' tests): `BTCUSD_PERP` and `BTC-USD-SWAP` are 100 dollars a contract, so 10 contracts long from
/// 60,000 to 66,000 are the deribit case's 1,000 dollars, `1,000·(1/60,000 − 1/66,000)` BTC on 1 BTC of
/// cash, booked in BTC, the settle coin. Both were refused while only the live row stated the face.
#[test]
fn a_face_a_committed_row_states_books_in_its_settle_coin() {
    let coin_pnl = 10.0 * 100.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    for (venue, symbol) in [("binance", "BTCUSD_PERP.P"), ("okx", "BTC-USD-SWAP")] {
        let ledger = run_ledger_coin([(venue, symbol)], &KeptRows::default())
            .unwrap()
            .expect("a coin ledger");
        assert_eq!(ledger.coin, "BTC", "{symbol} settles in BTC");
        assert_eq!(ledger.values, vec![(symbol.to_string(), ContractValue::inverse(100.0))]);
        for kind in ["bar", "tick"] {
            let (_dir, store) = store_with(venue, symbol, 60_000.0, 66_000.0);
            let profile = profile_on(venue, symbol, kind, 1.0, 10.0, "");
            let result = run_backtest(&profile, store)
                .unwrap_or_else(|e| panic!("{venue} {symbol} {kind}: {e:?}"));
            assert!(
                close(result.final_equity, 1.0 + coin_pnl, 1e-12),
                "{venue} {symbol} {kind}: {} BTC, want {}",
                result.final_equity,
                1.0 + coin_pnl
            );
            assert_eq!(result.settle_price, Some(66_000.0), "{symbol} {kind}");
        }
    }
}

/// A binance COIN-M or okx inverse contract NO committed row states is still refused, on both lanes,
/// never booked at a guessed 100: the face differs by coin (100 on BTC, 10 on most others).
#[test]
fn a_face_value_only_a_row_states_is_still_refused() {
    for (venue, symbol) in [("binance", "SOLUSD_PERP.P"), ("okx", "ETH-USD-SWAP")] {
        for kind in ["bar", "tick"] {
            let (_dir, store) = store_with(venue, symbol, 60_000.0, 66_000.0);
            let profile = profile_on(venue, symbol, kind, 1.0, 100.0, "");
            let Err(HarnessError::Data(m)) = run_backtest(&profile, store) else {
                panic!("{venue} {symbol} {kind} ran without its face value");
            };
            assert!(m.contains(&format!("{symbol} on {venue}")), "names it: {m}");
            assert!(m.contains("face value"), "says why: {m}");
        }
    }
}

/// A face only the row states is read off the contract's KEPT row when the run holds it (the datahub's list,
/// handed in by the door that resolves the profile: `ProfileBacktest::kept`): okx `ETH-USD-SWAP` at the 10 dollars
/// its row states books 10 contracts from 60,000 to 66,000 in ETH, on both lanes, where without the row it is refused.
#[test]
fn a_face_a_kept_row_states_books_in_its_settle_coin() {
    let coin_pnl = 10.0 * 10.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    let (venue, symbol) = ("okx", "ETH-USD-SWAP");
    let swap = vike_model::Contract::Perp(vike_model::ContractPerp {
        margining: vike_model::Margining::Inverse,
        settle: "ETH".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    let row = vike_model::Instrument::new(venue, symbol, "ETH", "USD", swap).with_properties(
        vike_model::SymbolProperties { contract_size: 10.0, ..Default::default() },
    );
    let kept = KeptRows::of(&[row], &[(venue.to_string(), symbol.to_string())]);
    assert!(kept.warnings.is_empty(), "{:?}", kept.warnings);
    for kind in ["bar", "tick"] {
        let (_dir, store) = store_with(venue, symbol, 60_000.0, 66_000.0);
        let mut profile = profile_on(venue, symbol, kind, 1.0, 10.0, "");
        profile.kept = kept.clone();
        let result =
            run_backtest(&profile, store).unwrap_or_else(|e| panic!("{symbol} {kind}: {e:?}"));
        assert!(
            close(result.final_equity, 1.0 + coin_pnl, 1e-12),
            "{symbol} {kind}: {} ETH, want {}",
            result.final_equity,
            1.0 + coin_pnl
        );
    }
}

/// One ledger currency per run: an inverse contract beside a linear symbol, or beside another coin's.
#[test]
fn one_coin_per_run() {
    let mixed = run_ledger_coin(
        [("deribit", "BTC-PERPETUAL"), ("deribit", "BTC_USDC-PERPETUAL")],
        &KeptRows::default(),
    )
    .expect_err("a coin and dollars");
    assert!(
        mixed.message().contains("BTC_USDC-PERPETUAL on deribit in dollars"),
        "{}",
        mixed.message()
    );
    let two =
        run_ledger_coin([("deribit", "BTC-PERPETUAL"), ("bybit", "ETHUSD")], &KeptRows::default())
            .expect_err("two coins");
    assert!(two.message().contains("BTC and ETH"), "{}", two.message());

    let one = run_ledger_coin(
        [("deribit", "BTC-PERPETUAL"), ("bybit", "BTCUSD"), ("deribit", "BTC-PERPETUAL")],
        &KeptRows::default(),
    )
    .unwrap()
    .expect("a coin ledger");
    assert_eq!(one.coin, "BTC");
    assert_eq!(
        one.values,
        vec![
            ("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0)),
            ("BTCUSD".to_string(), ContractValue::inverse(1.0)),
        ]
    );
    assert_eq!(
        run_ledger_coin(
            [("deribit", "BTC_USDC-PERPETUAL"), ("ctrader", "BTCUSD")],
            &KeptRows::default()
        )
        .unwrap(),
        None
    );
}

/// A key still linear at its site is refused on a coin ledger, by name, before anything is read; the
/// walk-forward's pre-flight refuses it too.
#[test]
fn a_knob_the_coin_ledger_does_not_book_is_refused() {
    let (_dir, store) = store_with("deribit", "BTC-PERPETUAL", 60_000.0, 66_000.0);
    let mut profile = profile_on("deribit", "BTC-PERPETUAL", "bar", 1.0, 1_000.0, "");
    profile.engine.account_currency = Some("USD".into());
    profile.engine.multipliers.insert("BTC-PERPETUAL".into(), 10.0);
    let Err(HarnessError::Data(m)) = run_backtest(&profile, store.clone()) else {
        panic!("a fiat ledger label on a coin ledger ran");
    };
    assert!(m.contains("engine.account_currency") && m.contains("engine.multiplier(s)"), "{m}");
    assert!(m.contains("settle coin BTC"), "{m}");

    let mut walk = profile_on(
        "deribit",
        "BTC-PERPETUAL",
        "bar",
        1.0,
        1_000.0,
        "[walkforward]\nn_splits = 2\n",
    );
    walk.engine.multiplier = 10.0;
    let err = crate::walkforward::runner::run_walkforward(&walk, store).expect_err("refused");
    assert!(matches!(&err, HarnessError::Data(m) if m.contains("engine.multiplier")), "{err:?}");
}

/// A store holding `bars` as the series' candles (the bar lane).
fn store_bars(
    venue: &str,
    symbol: &str,
    bars: &[Bar],
) -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    store.append_bars(venue, symbol, "1d", bars, None).unwrap();
    (dir, store)
}

/// The `volume_cap` rows a run dropped.
fn volume_capabilities(r: &BacktestResult) -> Vec<OrderDrop> {
    r.dropped.iter().filter(|d| d.reason == "volume_cap").cloned().collect()
}

/// The rule (`volume_unit`): deribit's, okx's and binance COIN-M's inverse CANDLES count the coin; okx's
/// linear derivative PRINTS count contracts of a `ctVal` a run does not hold; every other series counts the
/// order's unit. No inverse venue's coin ledger refuses the volume keys any more.
#[test]
fn the_volume_unit_of_each_series() {
    use VolumeUnit::{Coin, Order, Unstated};
    let (bar, tick) = (ReplayKind::Bar, ReplayKind::Tick);
    for (venue, symbol, on_bars, on_prints) in [
        ("deribit", "BTC-PERPETUAL", Coin, Order),
        ("deribit", "BTC_USDC-PERPETUAL", Order, Order),
        ("bybit", "BTCUSD", Order, Order),
        ("bybit", "BTCUSDT.P", Order, Order),
        ("okx", "BTC-USD-SWAP", Coin, Order),
        ("okx", "BTC-USDT-SWAP", Order, Unstated),
        ("okx", "ETH-USDT-261225", Order, Unstated),
        ("okx", "BTC-USDT", Order, Order),
        ("hyperliquid", "BTC", Order, Order),
        ("binance", "BTCUSDT.P", Order, Order),
        ("binance", "BTCUSD_PERP.P", Coin, Order),
    ] {
        assert_eq!(volume_unit(venue, symbol, bar), on_bars, "{symbol} on {venue}, candles");
        assert_eq!(volume_unit(venue, symbol, tick), on_prints, "{symbol} on {venue}, prints");
    }
    for (venue, symbol) in
        [("bybit", "BTCUSD"), ("okx", "BTC-USD-SWAP"), ("binance", "BTCUSD_PERP.P")]
    {
        let mut profile = profile_on(venue, symbol, "bar", 1.0, 10.0, "");
        profile.engine.volume_limit = Some(0.1);
        assert!(profile_run_ledger_coin(&profile).is_ok(), "{symbol} on {venue}");
    }
}

/// An okx linear swap's trade prints count contracts of its `ctVal` (0.01 BTC on `BTC-USDT-SWAP`) where
/// an order counts the base, and a run holds no row: the tick lane refuses the volume keys by name, never
/// a cap 100 times too loose. The bar lane runs (its candles store the base), and a tick run without them.
#[test]
fn an_okx_linear_swap_refuses_the_volume_keys_on_its_prints() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    store.append_quotes("okx", "BTC-USDT-SWAP", &[quote(0, 60_000.0, 60_000.0)], None).unwrap();
    let prints: Vec<_> = (1..12).map(|i| trade(i * 1000, 60_000.0, 50.0, false)).collect();
    store.append_trades("okx", "BTC-USDT-SWAP", &prints, None).unwrap();
    let bars: Vec<Bar> =
        (0..12).map(|i| Bar { volume: 0.5, ..flat_bar_zero_volume(i * 1000, 60_000.0) }).collect();
    store.append_bars("okx", "BTC-USDT-SWAP", "1d", &bars, None).unwrap();
    let mut tick = profile_on("okx", "BTC-USDT-SWAP", "tick", 1_000_000.0, 1.0, "");
    let store: Arc<dyn HistoryStore + Send + Sync> = store;
    assert!(run_backtest(&tick, store.clone()).is_ok(), "no volume key: runs as before");
    tick.engine.volume_limit = Some(0.1);
    let Err(HarnessError::Data(m)) = run_backtest(&tick, store.clone()) else {
        panic!("the cap ran against contract-counted prints");
    };
    assert!(m.contains("BTC-USDT-SWAP on okx") && m.contains("ctVal"), "{m}");
    let mut bar = profile_on("okx", "BTC-USDT-SWAP", "bar", 1_000_000.0, 1.0, "");
    bar.engine.volume_limit = Some(0.1);
    let r = run_backtest(&bar, store).expect("the bar lane's candles are in the base");
    let capped = OrderDrop {
        symbol: "BTC-USDT-SWAP".to_string(),
        reason: "volume_cap".to_string(),
        size: 0.95,
        weight: 0.0,
    };
    assert_eq!(
        volume_capabilities(&r),
        vec![capped],
        "10% of a 0.5 BTC candle fills 0.05 of 1 BTC"
    );
}

/// Bybit's inverse candles count CONTRACTS of one dollar, the order's own unit: a cap of 10% of a
/// 1,000-contract bar fills 100 of an order of 1,000 and drops 900, and the coin books those 100. It
/// was refused before; read as the coin, the order (0.2 BTC at 5,000) would have passed the cap whole.
#[test]
fn a_bybit_inverse_volume_limit_capabilities_in_contracts() {
    let px = |i: i64| if i == 11 { 5_500.0 } else { 5_000.0 };
    let bars: Vec<Bar> =
        (0..12).map(|i| Bar { volume: 1_000.0, ..flat_bar_zero_volume(i * 1000, px(i)) }).collect();
    let (_dir, store) = store_bars("bybit", "BTCUSD", &bars);
    let mut profile = profile_on("bybit", "BTCUSD", "bar", 1.0, 1_000.0, "");
    profile.engine.volume_limit = Some(0.1);
    let r = run_backtest(&profile, store).expect("a bybit inverse run with a volume cap runs");
    assert_eq!(
        volume_capabilities(&r),
        vec![OrderDrop {
            symbol: "BTCUSD".to_string(),
            reason: "volume_cap".to_string(),
            size: 900.0,
            weight: 0.0
        }]
    );
    let coin_pnl = 100.0 * (1.0 / 5_000.0 - 1.0 / 5_500.0);
    assert!(close(r.final_equity, 1.0 + coin_pnl, 1e-12), "{}", r.final_equity);
}

/// Deribit's trade PRINTS count dollars (`amount`), the order's own unit: the tick lane caps an order of
/// 1,000 at 10% of a 1,000-dollar print. Read as the coin (0.0167 BTC), it passed whole before.
#[test]
fn a_deribit_inverse_volume_limit_capabilities_the_tick_lane_in_dollars() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    store.append_quotes("deribit", "BTC-PERPETUAL", &[quote(0, 60_000.0, 60_000.0)], None).unwrap();
    let px = |i: i64| if i == 11 { 66_000.0 } else { 60_000.0 };
    let prints: Vec<_> = (1..12).map(|i| trade(i * 1000, px(i), 1_000.0, false)).collect();
    store.append_trades("deribit", "BTC-PERPETUAL", &prints, None).unwrap();
    let mut profile = profile_on("deribit", "BTC-PERPETUAL", "tick", 1.0, 1_000.0, "");
    profile.engine.volume_limit = Some(0.1);
    let r = run_backtest(&profile, store).expect("a deribit tick run with a volume cap runs");
    let capped = OrderDrop {
        symbol: "BTC-PERPETUAL".to_string(),
        reason: "volume_cap".to_string(),
        size: 900.0,
        weight: 0.0,
    };
    assert_eq!(volume_capabilities(&r), vec![capped]);
    let coin_pnl = 100.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(r.final_equity, 1.0 + coin_pnl, 1e-12), "{}", r.final_equity);
}

/// Deribit's inverse CANDLES count the coin: the `market_impact` slippage prices an order of 6,000 dollars at
/// 60,000 as 0.1 BTC against them, the charge the linear book of the same coin pays for 0.1 BTC over
/// the same candles. The entry each run paid is read back off its final equity (no trade closes).
/// It was refused before; compared as 6,000 against a 10-coin volume, the charge is over 100%.
#[test]
fn deribit_inverse_impact_charges_what_a_linear_order_of_the_same_dollars_pays() {
    let close_price = |i: i64| match i {
        11 => 66_000.0,
        i if i % 2 == 1 => 60_600.0,
        _ => 60_000.0,
    };
    // Every bar opens at 60,000 (the fill price) and closes on a path that gives the window a sigma.
    let bars: Vec<Bar> = (0..12)
        .map(|i| {
            let c: f64 = close_price(i);
            Bar {
                open: 60_000.0,
                high: c.max(60_000.0),
                low: c.min(60_000.0),
                volume: 10.0,
                ..flat_bar_zero_volume(i * 1000, c)
            }
        })
        .collect();
    let impact = "[engine.slippage]\nmodel = \"market_impact\"\nwindow = 3\n";
    let run = |symbol: &str, cash: f64, size: f64| {
        let (_dir, store) = store_bars("deribit", symbol, &bars);
        let mut profile = profile_on("deribit", symbol, "bar", cash, size, impact);
        profile.data.warmup = Some("4bars".into());
        let r = run_backtest(&profile, store).unwrap_or_else(|e| panic!("{symbol}: {e:?}"));
        assert_eq!(r.impact_unpriced, 0, "{symbol}: the window priced the fill");
        r.final_equity
    };
    let linear = run("BTC_USDC-PERPETUAL", 1_000_000.0, 0.1);
    let entry_linear = 66_000.0 - (linear - 1_000_000.0) / 0.1;
    let inverse = run("BTC-PERPETUAL", 1.0, 6_000.0);
    let entry_inverse = 1.0 / ((inverse - 1.0) / 6_000.0 + 1.0 / 66_000.0);
    assert!(entry_linear > 60_001.0, "the linear order paid an impact: {entry_linear}");
    assert!(
        close(entry_inverse, entry_linear, 1e-6),
        "inverse {entry_inverse} against linear {entry_linear}"
    );
}

/// The truncating leverage cap sizes a coin ledger in dollars by the one sizing law: 1 BTC at 60,000 on
/// 1x leaves room for 60,000 dollar contracts, so an order of 100,000 is cut to 60,000 (it was refused
/// on a coin ledger before `ContractValue::units_worth`).
#[test]
fn the_leverage_clamp_capabilities_a_coin_ledger_in_dollars() {
    let (_dir, store) = store_with("deribit", "BTC-PERPETUAL", 60_000.0, 66_000.0);
    let mut profile = profile_on("deribit", "BTC-PERPETUAL", "bar", 1.0, 100_000.0, "");
    profile.engine.leverage = Some(1.0);
    profile.engine.clamp_to_leverage = true;
    let r = run_backtest(&profile, store).expect("a coin ledger with the clamp runs");
    let coin_pnl = 60_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(r.final_equity, 1.0 + coin_pnl, 1e-12), "{}", r.final_equity);
}

/// A profile's risk sizer sizes a coin ledger by the contract's coin PnL: `bracket_per_symbol` buys with a
/// stop 3% under the close (58,200 from 60,000) and `max_risk_percent` risks 1% of 1 BTC. One dollar contract
/// loses `1/58,200 − 1/60,000` BTC to the stop, so the sizer opens 19,400 contracts, whose loss at the
/// stop is 0.01 BTC, and the coin gains `19,400·(1/60,000 − 1/66,000)` at 66,000. It was refused on a
/// coin ledger before (`[engine.sizer]`).
#[test]
fn a_risk_sizer_sizes_a_coin_ledger_by_its_coin_pnl() {
    let (_dir, store) = store_with("deribit", "BTC-PERPETUAL", 60_000.0, 66_000.0);
    let mut profile = ProfileBacktest::from_toml_str(
        r#"
[data]
venue = "deribit"
symbols = ["BTC-PERPETUAL"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1.0
[engine.fee]
kind = "free"

[engine.sizer]
kind = "max_risk_percent"
percent = 0.01

[strategy]
file = "test_doubles/bracket_per_symbol.rs"
"#,
    )
    .unwrap();
    // The simulator's own fixture, built here: no shipped file is a bracket per symbol.
    profile.strategy.builder = Some(StrategyBuilder::new(|_| Ok(Box::new(BracketPerSymbol))));
    let r = run_backtest(&profile, store).expect("a coin ledger with a sizer runs");
    let qty = 19_400.0;
    assert!(close(qty * (1.0 / 58_200.0 - 1.0 / 60_000.0), 0.01, 1e-12), "1% of the coin");
    let coin_pnl = qty * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(r.final_equity, 1.0 + coin_pnl, 1e-9), "{}", r.final_equity);
}

/// Every inverse symbol a picker door refuses is named once, each with its own venue, and the linear
/// ones are not.
#[test]
fn the_refusal_names_each_inverse_symbol_once_with_its_venue() {
    let err = refuse_inverse_contracts([
        ("deribit", "BTC-PERPETUAL"),
        ("bybit", "BTCUSD.P"),
        ("deribit", "BTC-PERPETUAL"),
        ("bybit", "BTCUSDT.P"),
        ("ctrader", "BTCUSD"),
    ])
    .expect_err("two inverse symbols");
    let m = err.message();
    assert!(m.contains("BTC-PERPETUAL on deribit, BTCUSD.P on bybit is"), "{m}");
    assert_eq!(m.matches("BTC-PERPETUAL on deribit").count(), 1, "{m}");
    assert!(!m.contains("BTCUSDT") && !m.contains("ctrader"), "{m}");
    assert!(m.contains("Run it from a profile"), "the way out: {m}");

    assert!(refuse_inverse_contracts([("binance", "BTCUSDT.P"), ("okx", "BTC-USDT-SWAP")]).is_ok());
    assert!(refuse_inverse_contracts([]).is_ok());
}
