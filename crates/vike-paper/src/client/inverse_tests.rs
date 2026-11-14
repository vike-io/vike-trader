//! A coin-margined (INVERSE) contract on the paper book. Its fee is the venue's rate on the order's
//! DOLLARS, stated in `USD`. A contract whose dollar value per unit only the venue's instrument row
//! states is refused in words. A linear symbol keeps the fee and the empty currency it always had.

use super::test_support::{bar, drain};
use super::*;
use vike_execution::ExecutionClient;
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

/// Deribit's published taker rate in the worked example of a live `BTC-PERPETUAL` fill: 3.5 bps.
const TAKER: FeeSchedule = FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 3.5 };

fn market(venue: &str, symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "m1".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// Every event one market buy of `qty` on `symbol` produces, filled at the next open `open` with no
/// slippage under [`TAKER`].
fn one_buy(venue: &str, symbol: &str, qty: f64, open: f64) -> (ExecutionClientPaper, Vec<Event>) {
    let mut c = ExecutionClientPaper::with_fee_schedule(venue, symbol, 0.0, TAKER);
    c.submit(&market(venue, symbol, qty));
    c.on_bar(&bar(1, open, open, open, open));
    let events = drain(&mut c);
    (c, events)
}

fn the_fill(events: &[Event]) -> &FillEvent {
    events
        .iter()
        .find_map(|e| if let Event::Fill(f) = e { Some(f) } else { None })
        .expect("the market buy filled")
}

/// **An inverse fill pays the rate on its DOLLARS, at any price, and says `USD`.** Deribit's
/// `BTC-PERPETUAL` amount is dollars (bybit's `BTCUSD` contract is one dollar): a buy of 1,000 at
/// 60,000 owes the venue `0.00035 · 1,000 / 60,000 = 0.0000058333` BTC, which the engine values at
/// the fill price, 0.35. The book booked `1,000 · 60,000 · 0.00035 = 21,000` while it read every
/// symbol as linear, and stated no currency, which the engine would multiply by the price again
/// (`vike_execution::Account::commission_in_quote`).
#[test]
fn an_inverse_fill_pays_the_rate_on_its_dollars_and_says_usd() {
    for (venue, symbol) in [("deribit", "BTC-PERPETUAL"), ("bybit", "BTCUSD")] {
        for open in [60_000.0, 66_000.0] {
            let (c, events) = one_buy(venue, symbol, 1_000.0, open);
            let fill = the_fill(&events);
            assert_eq!(fill.last_qty, 1_000.0, "{symbol}: the quantity is the dollars, as given");
            assert_eq!(fill.last_price, open);
            assert_eq!(fill.commission, TAKER.commission(false, 1_000.0, 1.0), "{symbol} @ {open}");
            assert!(
                (fill.commission - 0.35).abs() < 1e-12,
                "{symbol} @ {open}: {}",
                fill.commission
            );
            assert_eq!(fill.commission_asset.as_str(), "USD", "{symbol}: the fee is dollars");
            assert_eq!(c.fills.lock().unwrap()[0].fee, fill.commission, "the captured fill agrees");
        }
    }
}

/// **A linear symbol keeps its fee and its empty currency**, the coin-settled venues' linear books
/// included: deribit's USDC perpetual is sized in coin, `0.01 · 60,000 · 0.00035`. An ibkr stock and an
/// fxcm currency pair, whose names fix one unit, are linear 1.0 too.
#[test]
fn a_linear_fill_keeps_its_fee_and_its_empty_currency() {
    for (venue, symbol, qty) in [
        ("deribit", "BTC_USDC-PERPETUAL", 0.01),
        ("bybit", "BTCUSDT", 0.01),
        ("binance", "BTCUSDT", 0.01),
        ("ibkr", "AAPL.SMART.USD", 1.0),
        ("fxcm", "EURUSD", 1_000.0),
    ] {
        let (_, events) = one_buy(venue, symbol, qty, 60_000.0);
        let fill = the_fill(&events);
        assert_eq!(fill.commission.to_bits(), TAKER.commission(false, qty, 60_000.0).to_bits());
        assert_eq!(fill.commission_asset.as_str(), "", "{symbol}: no currency, as always");
    }
}

/// **A binance COIN-M or okx inverse contract books the face its committed row states** (the catalog's,
/// held equal to the venue's rows by each bridge's tests): `BTCUSD_PERP` and `BTC-USD-SWAP` are 100
/// dollars a contract, so a buy of 10 at 60,000 is 1,000 dollars and owes the rate on them,
/// `0.00035 · 1,000` = 0.35, stated in `USD`. Both were refused while only the live row stated the face.
#[test]
fn a_coin_m_or_okx_inverse_contract_books_the_face_its_row_states() {
    for (venue, symbol) in [("binance", "BTCUSD_PERP.P"), ("okx", "BTC-USD-SWAP")] {
        let value = paper_contract_value(venue, symbol)
            .unwrap_or_else(|why| panic!("{symbol}: a committed row states its face: {why}"));
        assert_eq!(value, ContractValue::inverse(100.0), "{symbol}");
        assert_eq!(value.order_notional(10.0, 60_000.0), 1_000.0, "{symbol}: 1,000 dollars");
        let (c, events) = one_buy(venue, symbol, 10.0, 60_000.0);
        let fill = the_fill(&events);
        assert_eq!(fill.last_qty, 10.0, "{symbol}: contracts, as given");
        assert!((fill.commission - 0.35).abs() < 1e-12, "{symbol}: {}", fill.commission);
        assert_eq!(fill.commission_asset.as_str(), "USD", "{symbol}: the fee is dollars");
        assert_eq!(c.fills.lock().unwrap().len(), 1, "{symbol}: it filled");
    }
}

/// **An inverse contract no committed row states is refused in words, and never fills.** Binance
/// COIN-M (`contractSize`) and okx (`ctVal`) value a contract at 100 dollars on BTC and 10 on most other
/// coins; a guessed unit books a wrong number by that factor.
#[test]
fn an_inverse_contract_whose_unit_only_its_row_states_is_refused() {
    for (venue, symbol) in [("binance", "SOLUSD_PERP.P"), ("okx", "ETH-USD-SWAP")] {
        let (c, events) = one_buy(venue, symbol, 1.0, 60_000.0);
        let reason = events
            .iter()
            .find_map(|e| {
                if let Event::OrderRejected(r) = e { Some(r.reason.to_string()) } else { None }
            })
            .unwrap_or_else(|| panic!("{venue} {symbol} is refused: {events:?}"));
        assert!(reason.starts_with("paper-inverse-unit"), "{reason}");
        assert!(reason.contains(symbol), "{reason}");
        assert!(c.fills.lock().unwrap().is_empty(), "{symbol}: nothing rests, nothing fills");
    }
}

/// The reason a market buy of one unit of `symbol` on `venue` was refused, after checking nothing filled.
fn refusal_of(venue: &str, symbol: &str) -> String {
    let (c, events) = one_buy(venue, symbol, 1.0, 60_000.0);
    assert!(c.fills.lock().unwrap().is_empty(), "{venue} {symbol}: nothing rests, nothing fills");
    events
        .iter()
        .find_map(
            |e| if let Event::OrderRejected(r) = e { Some(r.reason.to_string()) } else { None },
        )
        .unwrap_or_else(|| panic!("{venue} {symbol} is refused: {events:?}"))
}

/// **A deribit coin-settled option is refused, never booked at one dollar a unit**: its premium is coin, and
/// the live mount refuses it in the same words (`vike_deribit::catalog::coin_option_refusal`). A USDC option
/// is linear 1.0 and is not refused.
#[test]
fn a_deribit_coin_settled_option_is_refused_in_the_mounts_words() {
    for (symbol, coin) in [("BTC-5OCT26-74000-C", "BTC"), ("ETH-5OCT26-2300-P", "ETH")] {
        let reason = refusal_of("deribit", symbol);
        assert!(reason.starts_with("paper-coin-option"), "{reason}");
        assert!(
            reason.contains(&format!(
                "{symbol} is a coin-settled option: its premium is quoted in {coin}"
            )),
            "{reason}"
        );
    }
    assert_eq!(
        paper_contract_value("deribit", "SOL_USDC-12OCT26-96-P"),
        Ok(ContractValue::default())
    );
}

/// **A symbol whose unit only its row states is refused, never booked at 1**: an ibkr future or option, an fxcm
/// CFD, every ig epic. A venue outside the roster (a test's book) keeps the default.
#[test]
fn a_contract_whose_unit_only_its_row_states_is_refused() {
    for (venue, symbol) in [
        ("ibkr", "ES.CME.USD.FUT.20261218"),
        ("ibkr", "SPY.SMART.USD.OPT.20261218.500.C"),
        ("ibkr", "ES"),
        ("fxcm", "XAUUSD"),
        ("fxcm", "US30"),
        ("ig", "CS.D.EURUSD.CFD.IP"),
    ] {
        let reason = refusal_of(venue, symbol);
        assert!(reason.starts_with("paper-contract-unit"), "{venue} {symbol}: {reason}");
        assert!(reason.contains(symbol), "{reason}");
    }
    assert_eq!(paper_contract_value("sim", "ES"), Ok(ContractValue::default()));
}
