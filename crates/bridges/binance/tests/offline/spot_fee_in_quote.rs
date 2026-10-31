//! A SPOT fill's fee is booked in QUOTE units, on all three legs that can deliver the same fill.
//!
//! Binance spot charges the fee in the coin a fill RECEIVES: a BUY pays in the base coin (BTC), a SELL
//! in the quote (USDT). The core books `FillEvent::commission` as quote units on every linear
//! contract whatever the asset says, so before `family/spot_fee.rs` every BUY booked its BTC fee as
//! that many USDT (1/80,000 of its value). These tests go through the PUBLIC mappers, so they are red
//! on the commit before the rule: the live executionReport (`map_binance_private`), the resync replay
//! (`map_binance_history`) and the reconcile fill parser (`parse_spot_my_trades`).
//!
//! The same fee leaves the wallet holding `qty - fee` of the coin, so a BUY's `last_qty` is NET of a
//! base-coin fee on all three legs (and the engine's long matches the wallet); a SELL, a quote fee, a
//! fee in a third asset (BNB, paid from the BNB balance) and the perpetual keep the quantity as stated.
//! A BNB fee is valued through a rate source the venue registers (`register_rates`); these tests
//! register theirs under a venue string of their own, because the registry is process-wide and a
//! second test that maps a BNB fee for plain `binance` expects the unpriced answer.
//!
//! The money fixtures are MEASURED, not invented: `fixtures/captured/ws_fill.json` is a real demo
//! BUY/SELL pair, and `LEDGER_BUYS` is every distinct spot BUY the the CI box datahub `exec_fill` ledger
//! holds for `binance` `BTCUSDT` (read-only `clickhouse-local`, 2026-10-26, v0.1.91). The audit
//! counted 121 BUY rows; they are 24 trades, because the journal wrote a fill once per delivery before
//! v0.1.91 made one execution one record.

use serde_json::{Value, json};
use vike_binance::event_mapper::map_binance_private;
use vike_binance::history::map_binance_history;
use vike_binance::perp_mapper::map_binance_perp;
use vike_binance::recon_client::{parse_perp_user_trades, parse_spot_my_trades};
use vike_exec::{Account, BalanceMode};
use vike_model::events::{Event, FillEvent};

const WS_FILL: &str = include_str!("../fixtures/captured/ws_fill.json");

/// (venue trade id, qty, price, fee in BTC) of the 24 distinct BUYs in the ledger: all taker, all
/// charged `0.1 %` of the quantity in BTC.
const LEDGER_BUYS: [(i64, f64, f64, f64); 24] = [
    (328738454, 0.001, 80984.92, 0.000001),
    (328835118, 0.00015, 80571.92, 1.5e-7),
    (328835155, 0.00015, 80571.92, 1.5e-7),
    (328835161, 0.00015, 80571.92, 1.5e-7),
    (328835228, 0.00015, 80557.79, 1.5e-7),
    (328835483, 0.00015, 80554.15, 1.5e-7),
    (328835512, 0.00015, 80550.01, 1.5e-7),
    (328835531, 0.00015, 80550.01, 1.5e-7),
    (328835547, 0.00015, 80550.01, 1.5e-7),
    (328835561, 0.00015, 80550.01, 1.5e-7),
    (328835573, 0.00015, 80550.01, 1.5e-7),
    (328835582, 0.00015, 80550.01, 1.5e-7),
    (328835600, 0.00015, 80550.01, 1.5e-7),
    (328835611, 0.00015, 80550.01, 1.5e-7),
    (328835635, 0.00015, 80557.88, 1.5e-7),
    (328835647, 0.00015, 80557.88, 1.5e-7),
    (328835664, 0.00015, 80557.88, 1.5e-7),
    (328835712, 0.00015, 80557.88, 1.5e-7),
    (328835969, 0.00015, 80550.23, 1.5e-7),
    (328836298, 0.00015, 80583.11, 1.5e-7),
    (328836306, 0.00015, 80583.11, 1.5e-7),
    (328836309, 0.00015, 80583.11, 1.5e-7),
    (328836314, 0.00015, 80583.11, 1.5e-7),
    (328836318, 0.00015, 80583.11, 1.5e-7),
];

fn captured(i: usize) -> Value {
    serde_json::from_str::<Value>(WS_FILL).unwrap()["frames"][i].clone()
}

fn bare_fill(events: &[Event]) -> FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.clone()),
            _ => None,
        })
        .expect("a bare Event::Fill")
}

/// A spot executionReport TRADE as the venue sends it (strings, as on the wire).
fn report(symbol: &str, side: &str, qty: f64, px: f64, fee: f64, asset: &str, t: i64) -> Value {
    json!({
        "e": "executionReport", "s": symbol, "c": "fee-test", "S": side, "x": "TRADE",
        "X": "FILLED", "t": t, "l": qty.to_string(), "L": px.to_string(), "n": fee.to_string(),
        "N": asset, "m": false, "T": 1_784_710_420_755_i64, "i": 9
    })
}

fn near(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
}

/// THE REAL FRAME: a demo BUY of 0.0001 BTC at 65972.8 paid `n` = 0.0000001 `N` = BTC. Booked in USDT
/// it is `fee x price` = 0.00659728, the very figure its SELL twin was charged in USDT (10 bps).
#[test]
fn the_captured_demo_buy_books_its_btc_fee_at_the_fill_price_in_usdt() {
    let buy = bare_fill(&map_binance_private(&captured(0), "binance", "BTCUSDT"));
    assert_eq!(buy.side, 1);
    near(buy.commission, 0.006_597_28);
    assert_eq!(buy.commission_asset, "USDT");

    let sell = bare_fill(&map_binance_private(&captured(1), "binance", "BTCUSDT"));
    assert_eq!(sell.side, -1);
    near(sell.commission, 0.006_597_28);
    assert_eq!(sell.commission_asset, "USDT", "a SELL already pays in the quote: unchanged");
    near(buy.commission, sell.commission);
}

/// THE REAL FRAME, quantity side: the demo BUY ordered 0.0001 BTC and paid `n` = 0.0000001 BTC out of
/// it, so the wallet received 0.0000999; its SELL pays in USDT and sells the 0.0001 it states.
#[test]
fn the_captured_demo_buy_delivers_its_quantity_net_of_the_btc_fee() {
    let buy = bare_fill(&map_binance_private(&captured(0), "binance", "BTCUSDT"));
    near(buy.last_qty, 0.000_099_9);
    let sell = bare_fill(&map_binance_private(&captured(1), "binance", "BTCUSDT"));
    near(sell.last_qty, 0.0001);
    // the order wrap carries the same net fill the Account folds
    let events = map_binance_private(&captured(0), "binance", "BTCUSDT");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderFilled(w) if w.fill.last_qty == buy.last_qty)),
        "{events:?}"
    );
}

/// The core's side of it: folded into an account, the BUY's fee reaches `balance` and `fees_paid`
/// in USDT. On the commit before the rule `fees_paid` read 0.0000001. And the long it opens is the
/// coin the wallet holds, not the coin that was ordered.
#[test]
fn folded_into_an_account_the_buy_fee_is_a_usdt_charge() {
    let buy = bare_fill(&map_binance_private(&captured(0), "binance", "BTCUSDT"));
    let mut account = Account::new(1.0, "binance", None, BalanceMode::Delta);
    account.apply_fill(&buy);
    near(account.fees_paid, 0.006_597_28);
    near(account.balance, -0.006_597_28);
    near(account.positions.values().next().expect("a long").size, 0.000_099_9);
}

/// All 24 real BUYs: 10 bps of the notional (the account's measured spot rate, 10.0 bps on its SELLs),
/// not the 4.45e-6 the raw BTC figures sum to.
#[test]
fn the_ledgers_buys_book_ten_bps_not_a_hundred_thousandth() {
    let (mut booked, mut raw, mut notional) = (0.0, 0.0, 0.0);
    let (mut ordered, mut delivered) = (0.0, 0.0);
    for (t, qty, px, fee) in LEDGER_BUYS {
        let fill = bare_fill(&map_binance_private(
            &report("BTCUSDT", "BUY", qty, px, fee, "BTC", t),
            "binance",
            "BTCUSDT",
        ));
        assert_eq!(fill.commission_asset, "USDT", "trade {t}");
        near(fill.commission, fee * px);
        booked += fill.commission;
        raw += fee;
        notional += qty * px;
        near(fill.last_qty, qty - fee);
        ordered += qty;
        delivered += fill.last_qty;
    }
    near(booked, 0.358_923_682);
    near(booked / notional, 0.001);
    assert!(raw < 5e-6, "the raw BTC figures this replaces");
    // The KL row's measured gap: the engine's long over the 24 BUYs exceeded the wallet's coin by the
    // sum of their BTC fees (0.00000445 BTC); netted, the two agree.
    near(ordered - delivered, 4.45e-6);
}

/// One fill, three deliveries (stream, resync replay, reconcile): the engine keeps the FIRST delivery
/// of a trade id for good, so all three must state the same fee in the same asset.
#[test]
fn the_stream_the_replay_and_the_reconcile_book_one_fill_identically() {
    let (t, qty, px, fee) = LEDGER_BUYS[1];
    let live = bare_fill(&map_binance_private(
        &report("BTCUSDT", "BUY", qty, px, fee, "BTC", t),
        "binance",
        "BTCUSDT",
    ));

    let row = json!({
        "symbol": "BTCUSDT", "id": t, "orderId": 7, "price": px.to_string(),
        "qty": qty.to_string(), "quoteQty": (qty * px).to_string(), "commission": fee.to_string(),
        "commissionAsset": "BTC", "time": 5, "isBuyer": true, "isMaker": false
    });
    let orders =
        json!([{ "orderId": 7, "clientOrderId": "fee-test", "status": "FILLED", "updateTime": 6 }]);
    let replayed =
        bare_fill(&map_binance_history(&orders, &json!([row.clone()]), "binance", "BTCUSDT"));
    let reconciled = &parse_spot_my_trades(&json!([row]).to_string()).unwrap()[0];

    near(live.commission, fee * px);
    assert_eq!(live.commission_asset, "USDT");
    assert_eq!(
        (replayed.commission, replayed.commission_asset),
        (live.commission, live.commission_asset)
    );
    assert_eq!(reconciled.commission, live.commission);
    assert_eq!(reconciled.commission_asset, "USDT");
    // The quantity too, bit for bit: it is part of the fill's identity print, so legs that disagreed on
    // it would read as a trade-id collision.
    near(live.last_qty, qty - fee);
    assert_eq!(replayed.last_qty.to_bits(), live.last_qty.to_bits());
    assert_eq!(reconciled.last_qty.to_bits(), live.last_qty.to_bits());
}

/// A SELL (fee in USDT) and a BUY whose fee is already the quote (a venue or promotion that charges
/// it there) are booked as stated, on every leg.
#[test]
fn a_fee_already_in_the_quote_is_unchanged() {
    for (side, is_buyer) in [("SELL", false), ("BUY", true)] {
        let live = bare_fill(&map_binance_private(
            &report("BTCUSDT", side, 0.001, 80970.0, 0.08097, "USDT", 42),
            "binance",
            "BTCUSDT",
        ));
        assert_eq!((live.commission, live.commission_asset.as_str()), (0.08097, "USDT"));
        assert_eq!(live.last_qty, 0.001, "a quote fee takes nothing out of the coin");

        let body = json!([{
            "symbol": "BTCUSDT", "id": 42, "orderId": 7, "price": "80970", "qty": "0.001",
            "commission": "0.08097", "commissionAsset": "USDT", "time": 5,
            "isBuyer": is_buyer, "isMaker": false
        }]);
        let r = &parse_spot_my_trades(&body.to_string()).unwrap()[0];
        assert_eq!((r.commission, r.commission_asset.as_str()), (0.08097, "USDT"));
        assert_eq!(r.last_qty, 0.001);
    }
}

/// A BNB pair's own coin is its base: converted, and a BUY nets it. BNB on any OTHER pair with NO rate
/// source registered for the venue is NOT guessed: it stays the BNB figure under the name BNB and the
/// quantity whole, so a later "fix" with an invented rate has to change this test first.
#[test]
fn bnb_is_converted_on_its_own_pair_and_never_guessed_on_another() {
    let own = bare_fill(&map_binance_private(
        &report("BNBUSDT", "BUY", 2.0, 600.0, 0.002, "BNB", 1),
        "binance",
        "BNBUSDT",
    ));
    near(own.commission, 1.2);
    assert_eq!(own.commission_asset, "USDT");
    near(own.last_qty, 1.998);

    let discounted = bare_fill(&map_binance_private(
        &report("BTCUSDT", "BUY", 0.0001, 65972.8, 0.0000003, "BNB", 2),
        "binance",
        "BTCUSDT",
    ));
    assert_eq!((discounted.commission, discounted.commission_asset.as_str()), (0.0000003, "BNB"));
    assert_eq!(discounted.last_qty, 0.0001);
}

/// A rate source for BNB: the same BNB-discounted BUY, delivered by all three legs, is valued in the
/// pair's quote at the source's price and keeps its quantity whole (the fee came out of the BNB balance,
/// not out of the BTC delivered).
struct Bnb(f64);
impl vike_binance::family::spot_fee::QuoteRates for Bnb {
    fn price(&self, asset: &str, quote: &str) -> Option<f64> {
        (asset == "BNB" && quote == "USDT").then_some(self.0)
    }
}

#[test]
fn a_bnb_fee_is_valued_through_the_registered_rate_on_every_leg() {
    const VENUE: &str = "binance-bnb-rate-test";
    vike_binance::family::spot_fee::register_rates(VENUE, std::sync::Arc::new(Bnb(612.5)));
    let (qty, px, fee) = (0.0001, 65972.8, 0.000_000_3);

    let live = bare_fill(&map_binance_private(
        &report("BTCUSDT", "BUY", qty, px, fee, "BNB", 2),
        VENUE,
        "BTCUSDT",
    ));
    near(live.commission, fee * 612.5);
    assert_eq!(live.commission_asset, "USDT");
    assert_eq!(live.last_qty, qty);

    let row = json!({
        "symbol": "BTCUSDT", "id": 2, "orderId": 7, "price": px.to_string(), "qty": qty.to_string(),
        "commission": fee.to_string(), "commissionAsset": "BNB", "time": 5, "isBuyer": true,
        "isMaker": false
    });
    let orders =
        json!([{ "orderId": 7, "clientOrderId": "fee-test", "status": "FILLED", "updateTime": 6 }]);
    let replayed =
        bare_fill(&map_binance_history(&orders, &json!([row.clone()]), VENUE, "BTCUSDT"));
    let reconciled =
        &vike_binance::family::recon::parse_spot_my_trades(&json!([row]).to_string(), VENUE)
            .unwrap()[0];
    for (commission, asset, last_qty) in [
        (replayed.commission, replayed.commission_asset.as_str(), replayed.last_qty),
        (reconciled.commission, reconciled.commission_asset.as_str(), reconciled.last_qty),
    ] {
        assert_eq!((commission, asset, last_qty), (live.commission, "USDT", live.last_qty));
    }

    // the pair the source cannot price (BNB against BTC) is still left as stated, not guessed
    let other = bare_fill(&map_binance_private(
        &report("ETHBTC", "BUY", 1.0, 0.05, 0.001, "BNB", 3),
        VENUE,
        "ETHBTC",
    ));
    assert_eq!((other.commission, other.commission_asset.as_str()), (0.001, "BNB"));
}

/// A coin-quoted pair converts into THAT coin, and the BUY nets the coin it paid.
#[test]
fn the_quote_is_whatever_the_symbol_says() {
    let fill = bare_fill(&map_binance_private(
        &report("ETHBTC", "BUY", 1.0, 0.05, 0.001, "ETH", 3),
        "binance",
        "ETHBTC",
    ));
    near(fill.commission, 0.000_05);
    assert_eq!(fill.commission_asset, "BTC");
    near(fill.last_qty, 0.999);
}

/// A SELL that a venue charges in the base coin would not be netted (the coin left the wallet in full):
/// the rule keys on the SIDE, so the quantity of a SELL is its stated one whatever the fee asset.
#[test]
fn a_sell_never_nets_its_quantity() {
    let fill = bare_fill(&map_binance_private(
        &report("BTCUSDT", "SELL", 0.001, 80970.0, 0.000_001, "BTC", 4),
        "binance",
        "BTCUSDT",
    ));
    near(fill.commission, 0.000_001 * 80970.0);
    assert_eq!(fill.last_qty, 0.001);
}

/// The perpetual is untouched: its fee is the venue's USDT figure, and even a row that names a
/// base-looking asset is not routed through the spot rule.
#[test]
fn the_perpetual_keeps_the_venues_figure() {
    let frame = json!({
        "e": "ORDER_TRADE_UPDATE", "T": 200,
        "o": {
            "s": "BTCUSDT", "c": "perp-1", "x": "TRADE", "X": "FILLED", "S": "BUY",
            "l": "0.5", "L": "64000.0", "n": "0.0000001", "N": "BTC", "t": 777, "m": false,
            "ps": "BOTH"
        }
    });
    let fill = bare_fill(&map_binance_perp(&frame, "binance", "BTCUSDT"));
    assert_eq!(fill.symbol.as_str(), "BTCUSDT.P");
    assert_eq!((fill.commission, fill.commission_asset.as_str()), (0.0000001, "BTC"));
    assert_eq!(fill.last_qty, 0.5, "a perpetual's quantity is a position delta, never netted");

    let body = json!([{
        "symbol": "BTCUSDT", "id": 9, "orderId": 7, "price": "64000", "qty": "0.5",
        "commission": "0.0000001", "commissionAsset": "BTC", "time": 5, "side": "BUY",
        "maker": false, "positionSide": "BOTH"
    }]);
    let r = &parse_perp_user_trades(&body.to_string()).unwrap()[0];
    assert_eq!((r.commission, r.commission_asset.as_str()), (0.0000001, "BTC"));
    assert_eq!(r.last_qty, 0.5);
}
