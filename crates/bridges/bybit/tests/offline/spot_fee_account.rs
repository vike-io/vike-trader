//! A bybit spot fill's fee, folded into `vike_execution::Account` through the real mapper: the holding is the
//! wallet's coin and the cash is the wallet's, whatever coin the venue charged the fee in.
//!
//! The measured row (the CI box's demo account #4, 2026-11-08): a SPOT market BUY of 0.01 BTC on BTCUSDT
//! charged `execFee 0.00001`, `feeCurrency BTC`, and the wallet received 0.00999 BTC. The account books a
//! linear fill's commission as stated (`vike_execution::Account`'s `commission_in_quote`), so the conversion is
//! the bridge's (`crates/bridges/bybit/src/execution/event_mapper.rs`'s `net_spot_base_fee`, decision 0231): the
//! fill reaches the account as 0.00999 at the price, with the fee's value in USDT.

use serde_json::{Value, json};
use vike_bybit::execution::event_mapper::map_execution;
use vike_execution::{Account, BalanceMode};
use vike_model::events::{Event, FillEvent};

const PRICE_BUY: f64 = 76_000.0;
const PRICE_SELL: f64 = 77_000.0;

fn row(
    category: &str,
    id: &str,
    side: &str,
    qty: &str,
    px: f64,
    fee: &str,
    currency: &str,
) -> Value {
    json!({
        "category": category, "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": id,
        "side": side, "execPrice": px.to_string(), "execQty": qty, "execFee": fee,
        "feeCurrency": currency, "isMaker": false, "execTime": "5", "cumExecQty": qty,
        "orderQty": qty, "execId": id, "orderId": id
    })
}

fn fill_of(row: &Value) -> FillEvent {
    vike_bybit::history::kept_list_for_tests();
    match map_execution(row, "bybit", "BTCUSDT.P").into_iter().next() {
        Some(Event::Fill(f)) => f,
        other => panic!("the row maps to a fill first, got {other:?}"),
    }
}

fn near(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got} != {want}");
}

/// The BUY's holding is the wallet's 0.00999 BTC and its cost is the BTC fee's value in USDT (0.76), not
/// 0.00001 USDT; the SELL of that holding pays its USDT fee as stated and leaves the engine flat with the
/// wallet's USDT. Both hold at a later price: the engine's equity moves as the wallet's value does.
#[test]
fn a_base_coin_fee_leaves_the_holding_and_the_cash_equal_to_the_wallets() {
    let mut account = Account::new(1.0, "bybit", None, BalanceMode::Delta);
    let buy = fill_of(&row("spot", "b1", "Buy", "0.01", PRICE_BUY, "0.00001", "BTC"));
    assert_eq!(buy.symbol.as_str(), "BTCUSDT", "the spot pair's user name (decision 0290)");
    assert!(account.apply_fill(&buy).applied());

    let held = account.positions.values().next().expect("a long").size;
    near(held, 0.01 - 0.000_01);
    let fee = 0.000_01 * PRICE_BUY;
    near(account.fees_paid, fee);
    near(account.balance, -fee);
    let tally: Vec<(&str, f64)> =
        account.fees_by_asset.iter().map(|(a, f)| (a.as_str(), *f)).collect();
    assert_eq!(tally.len(), 1, "one fee asset, the quote: {tally:?}");
    assert_eq!(tally[0].0, "USDT", "no fee is booked as BTC");
    near(tally[0].1, fee);

    // At 80,000 the wallet (+0.00999 BTC, -760 USDT) is worth what the engine's equity moved by.
    let wallet = 0.009_99 * 80_000.0 - 0.01 * PRICE_BUY;
    near(account.balance + held * (80_000.0 - PRICE_BUY), wallet);

    // The SELL of the whole holding: its fee is USDT on the wire and stays as stated.
    let sell_fee = 0.009_99 * PRICE_SELL * 0.001;
    let sell =
        fill_of(&row("spot", "s1", "Sell", "0.00999", PRICE_SELL, &sell_fee.to_string(), "USDT"));
    assert!(account.apply_fill(&sell).applied());
    assert_eq!(
        account.positions.values().next().expect("the row").size,
        0.0,
        "flat, as the wallet"
    );
    // The wallet's USDT moved by -760 + 769.23 - the sell fee; a Delta engine's cash is balance + realized.
    let wallet_usdt = -0.01 * PRICE_BUY + 0.009_99 * PRICE_SELL - sell_fee;
    near(account.balance + account.realized_pnl, wallet_usdt);
    near(account.fees_paid, fee + sell_fee);
}

/// A linear perpetual's USDT fee is booked as stated, bit for bit, and its quantity is never netted.
#[test]
fn a_perpetuals_usdt_fee_is_booked_as_stated() {
    let mut account = Account::new(1.0, "bybit", None, BalanceMode::Delta);
    let perp = fill_of(&row("linear", "p1", "Buy", "0.01", PRICE_BUY, "0.418", "USDT"));
    assert_eq!(perp.symbol.as_str(), "BTCUSDT.P", "the perpetual's user name (decision 0290)");
    assert!(account.apply_fill(&perp).applied());
    assert_eq!(account.fees_paid.to_bits(), 0.418f64.to_bits());
    assert_eq!(account.balance.to_bits(), (-0.418f64).to_bits());
    assert_eq!(account.positions.values().next().expect("a long").size, 0.01);
}
