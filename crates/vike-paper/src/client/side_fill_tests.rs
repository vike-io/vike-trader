//! Paper fills like the real venue, never at the mid (the owner, 2026-11-16): on a bar folded from
//! quotes the book judges every order on its SIDE's path (`vike_fills::fill_model::side_bar`), a buy
//! on the ask's and a sell on the bid's. The owner's four examples, each with its short-side mirror,
//! and beside each what the mid bar alone (every paper fill before the ruling) does.

use super::test_support::{bar, drain};
use super::*;
use vike_execution::ExecutionClient;
use vike_execution::testing::rejection;
use vike_marketdata::BarSides;
use vike_model::events::RejectCause;
use vike_model::{ContractValue, OrderRequest, OrderType, TimeInForce};

/// The window's quotes `(bid, ask)`, in order, as the node's tick-to-bar synthesizer folds them: each
/// side's open, high, low and close, stamped as the bar.
fn sides(ts: i64, quotes: &[(f64, f64)]) -> BarSides {
    let path = |px: Vec<f64>| {
        let high = px.iter().copied().fold(f64::MIN, f64::max);
        let low = px.iter().copied().fold(f64::MAX, f64::min);
        bar(ts, px[0], high, low, *px.last().expect("one quote at least"))
    };
    BarSides {
        bid: path(quotes.iter().map(|q| q.0).collect()),
        ask: path(quotes.iter().map(|q| q.1).collect()),
    }
}

fn order(
    order_client_id: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    kind: OrderType,
    price: Option<f64>,
) -> OrderRequest {
    let trigger = kind == OrderType::Stop;
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: if symbol == TOKEN { "polymarket" } else { "ctrader" }.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: kind,
        price: price.filter(|_| !trigger),
        trigger_price: price.filter(|_| trigger),
        ..Default::default()
    }
}

const TOKEN: &str = "SCRIPT_OUTCOME_TOKEN";

fn book(symbol: &str) -> ExecutionClientPaper {
    let venue = if symbol == TOKEN { "polymarket" } else { "ctrader" };
    ExecutionClientPaper::new(venue, symbol, 0.0, 0.0, 0.0)
}

/// The prices this book filled at.
fn fill_prices(c: &ExecutionClientPaper) -> Vec<f64> {
    c.fills.lock().unwrap().iter().map(|f| f.price).collect()
}

/// Polymarket 0.48 / 0.52: a market buy of 100 costs 52.00 (the mid's 50.00), a sell gets 48.00.
#[test]
fn a_polymarket_market_buy_pays_the_ask_and_a_sell_gets_the_bid() {
    let mid = bar(60_000, 0.50, 0.50, 0.50, 0.50);
    for (side, price, cost) in [(1, 0.52, 52.0), (-1, 0.48, 48.0)] {
        let mut c = book(TOKEN);
        c.submit(&order("m", TOKEN, side, 100.0, OrderType::Market, None));
        c.on_bar_sides(&mid, Some(&sides(60_000, &[(0.48, 0.52)])));
        let fills = c.fills.lock().unwrap().clone();
        assert_eq!(fills.len(), 1, "side {side}");
        assert_eq!((fills[0].price, fills[0].price * fills[0].size), (price, cost), "side {side}");

        let mut before = book(TOKEN);
        before.submit(&order("m", TOKEN, side, 100.0, OrderType::Market, None));
        before.on_bar(&mid);
        assert_eq!(fill_prices(&before), [0.50], "the mid alone, side {side}: 50.00");
    }
}

/// cTrader EURUSD 1.08495 / 1.08505: a market buy of 100 000 fills at 1.08505 (the mid's 1.08500), a
/// sell at 1.08495.
#[test]
fn a_ctrader_market_buy_fills_at_the_ask_and_a_sell_at_the_bid() {
    let mid = bar(60_000, 1.08500, 1.08500, 1.08500, 1.08500);
    for (side, price) in [(1, 1.08505), (-1, 1.08495)] {
        let mut c = book("EURUSD");
        c.submit(&order("m", "EURUSD", side, 100_000.0, OrderType::Market, None));
        c.on_bar_sides(&mid, Some(&sides(60_000, &[(1.08495, 1.08505)])));
        assert_eq!(fill_prices(&c), [price], "side {side}");
    }
}

/// A buy limit at 1.08490 does NOT fill while the ask is 1.08495, though the mid touched 1.08490; it
/// fills once the ask reaches it. Its mirror: a sell limit at 1.08510 waits for the BID.
#[test]
fn a_limit_fills_only_once_its_side_reaches_it() {
    let dip_mid = bar(60_000, 1.08500, 1.08500, 1.08490, 1.08490);
    let mut before = book("EURUSD");
    before.submit(&order("l", "EURUSD", 1, 100_000.0, OrderType::Limit, Some(1.08490)));
    before.on_bar(&dip_mid);
    assert_eq!(fill_prices(&before), [1.08490], "the mid alone fills it");

    let mut c = book("EURUSD");
    c.submit(&order("l", "EURUSD", 1, 100_000.0, OrderType::Limit, Some(1.08490)));
    c.on_bar_sides(&dip_mid, Some(&sides(60_000, &[(1.08495, 1.08505), (1.08485, 1.08495)])));
    assert!(fill_prices(&c).is_empty(), "the ask (1.08495) never reached 1.08490");
    let ask_there = sides(120_000, &[(1.08485, 1.08495), (1.08480, 1.08490)]);
    c.on_bar_sides(&bar(120_000, 1.08490, 1.08490, 1.08485, 1.08485), Some(&ask_there));
    assert_eq!(fill_prices(&c), [1.08490], "the ask did");

    let rally_mid = bar(60_000, 1.08500, 1.08510, 1.08500, 1.08510);
    let mut sell = book("EURUSD");
    sell.submit(&order("l", "EURUSD", -1, 100_000.0, OrderType::Limit, Some(1.08510)));
    sell.on_bar_sides(&rally_mid, Some(&sides(60_000, &[(1.08495, 1.08505), (1.08505, 1.08515)])));
    assert!(fill_prices(&sell).is_empty(), "the bid (1.08505) never reached 1.08510");
}

/// A sell stop at 1.08480 fires when the BID reaches 1.08480 (the mid is then 1.08485, and the mid
/// alone fires nothing yet); its mirror, a buy stop at 1.08520, fires when the ASK reaches it.
#[test]
fn a_stop_triggers_on_its_side() {
    let cases = [
        (
            -1,
            1.08480,
            [(1.08495, 1.08505), (1.08480, 1.08490)],
            [1.08500, 1.08500, 1.08485, 1.08485],
        ),
        (
            1,
            1.08520,
            [(1.08495, 1.08505), (1.08510, 1.08520)],
            [1.08500, 1.08515, 1.08500, 1.08515],
        ),
    ];
    for (side, stop, quotes, [o, h, l, cl]) in cases {
        let mid = bar(60_000, o, h, l, cl);
        let mut before = book("EURUSD");
        before.submit(&order("s", "EURUSD", side, 100_000.0, OrderType::Stop, Some(stop)));
        before.on_bar(&mid);
        assert!(fill_prices(&before).is_empty(), "side {side}: the mid alone is half a pip late");

        let mut c = book("EURUSD");
        c.submit(&order("s", "EURUSD", side, 100_000.0, OrderType::Stop, Some(stop)));
        c.on_bar_sides(&mid, Some(&sides(60_000, &quotes)));
        assert_eq!(fill_prices(&c), [stop], "side {side}");
    }
}

/// A trailing sell submitted after a quoted bar trails from the BID's close, as its stop is judged
/// on the bid: from 1.08495 by 0.0010 it sits at 1.08395, where the mid's close would put it at
/// 1.08400 and fire it half a pip early.
#[test]
fn a_trailing_sell_trails_from_the_bid() {
    let mut c = book("EURUSD");
    let mid = bar(60_000, 1.08500, 1.08500, 1.08500, 1.08500);
    c.on_bar_sides(&mid, Some(&sides(60_000, &[(1.08495, 1.08505)])));
    c.submit(&OrderRequest {
        trail: Some(0.0010),
        ..order("t", "EURUSD", -1, 100_000.0, OrderType::Trailing, None)
    });
    let near = [(1.08495, 1.08505), (1.08398, 1.08408)];
    c.on_bar_sides(&bar(120_000, 1.085, 1.085, 1.08403, 1.08403), Some(&sides(120_000, &near)));
    assert!(fill_prices(&c).is_empty(), "the bid 1.08398 is above its stop 1.08395");
    let through = [(1.08398, 1.08408), (1.08390, 1.08400)];
    c.on_bar_sides(
        &bar(180_000, 1.08403, 1.08403, 1.08395, 1.08395),
        Some(&sides(180_000, &through)),
    );
    assert_eq!(fill_prices(&c), [1.08495 - 0.0010]);
}

/// A post-only order is judged against its side: after a quoted bar of 0.48 / 0.52 a post-only buy
/// at 0.51 rests below the ask (the mid's 0.50 would refuse it), and one at the ask is refused.
#[test]
fn a_post_only_order_is_judged_against_its_side() {
    let post_only = |id: &str, price| OrderRequest {
        time_in_force: TimeInForce::PostOnly,
        ..order(id, TOKEN, 1, 10.0, OrderType::Limit, Some(price))
    };
    let mid = bar(60_000, 0.50, 0.50, 0.50, 0.50);
    let mut c = book(TOKEN);
    c.on_bar_sides(&mid, Some(&sides(60_000, &[(0.48, 0.52)])));
    c.submit(&post_only("rests", 0.51));
    assert_eq!(rejection(&drain(&mut c)), None, "below the ask: it rests");
    c.submit(&post_only("lifts", 0.52));
    let events = drain(&mut c);
    assert!(
        events.iter().any(|e| matches!(e, Event::OrderRejected(r)
            if r.cause == Some(RejectCause::PostOnlyWouldCross))),
        "at the ask: refused, {events:?}"
    );

    let mut before = book(TOKEN);
    before.on_bar(&mid);
    before.submit(&post_only("rests", 0.51));
    assert!(rejection(&drain(&mut before)).is_some(), "the mid alone refused it");
}

/// A combo's legs each trade on their own side: buying a NEAR / FAR spread lifts NEAR's ask and hits
/// FAR's bid, the quotes that stood at the leg bars' close, never the legs' mid bars (which is what
/// the legs' marks were before).
#[test]
fn a_combo_buys_its_bought_leg_at_the_ask_and_sells_its_sold_leg_at_the_bid() {
    use vike_model::{OrderCombo, OrderComboLeg, build_combo};
    let spread = OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            OrderComboLeg { symbol: "NEAR".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "FAR".into(), ratio: -1, limit: None },
        ],
        net_limit: Some(10.0),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let leg =
        |symbol: &str, mid: f64| Bar { symbol: Some(symbol.into()), ..bar(1, mid, mid, mid, mid) };
    let mut c = ExecutionClientPaper::new("deribit", "COMBO", 0.0, 0.0, 0.0);
    c.submit(&build_combo(&spread, "cb").expect("a valid combo"));
    c.on_bar_sides(&leg("NEAR", 99.5), Some(&sides(1, &[(99.0, 100.0)])));
    c.on_bar_sides(&leg("FAR", 94.5), Some(&sides(1, &[(94.0, 95.0)])));
    let fills: Vec<(i32, f64)> =
        c.fills.lock().unwrap().iter().map(|f| (f.side, f.price)).collect();
    assert_eq!(fills, [(1, 100.0), (-1, 94.0)], "NEAR at its ask, FAR at its bid");

    let mut before = ExecutionClientPaper::new("deribit", "COMBO", 0.0, 0.0, 0.0);
    before.submit(&build_combo(&spread, "cb").expect("a valid combo"));
    before.on_bar(&leg("NEAR", 99.5));
    before.on_bar(&leg("FAR", 94.5));
    let mids: Vec<f64> = before.fills.lock().unwrap().iter().map(|f| f.price).collect();
    assert_eq!(mids, [99.5, 94.5], "the mid bars alone: each leg at its mid");
}

/// **An IG paper market buy fills at the ASK, at the epic's contract size** (the owner's ruling of 2026-11-16).
/// Until the engine states the size (`ExecutionClient::on_contract_value`, read over ig's demo login) the book
/// refuses the epic in the owner's words; once stated it takes the order, fills it on the quote's ask and charges
/// its fee on `qty x price x contractSize`. A symbol its NAME values keeps the name's unit whatever is stated.
#[test]
fn an_ig_paper_market_buy_fills_at_the_ask_once_the_epics_size_is_stated() {
    const EPIC: &str = "CS.D.EURUSD.MINI.IP";
    let buy = OrderRequest {
        order_client_id: "m".into(),
        venue: "ig".into(),
        symbol: EPIC.into(),
        side: 1,
        qty: 2.0,
        order_type: OrderType::Market,
        ..Default::default()
    };
    let mid = bar(60_000, 1.08500, 1.08500, 1.08500, 1.08500);
    let quoted = sides(60_000, &[(1.08495, 1.08505)]);

    let mut unstated = ExecutionClientPaper::new("ig", EPIC, 0.0, 0.0, 0.0);
    unstated.submit(&buy);
    let refused = rejection(&drain(&mut unstated)).expect("refused before the size is stated");
    assert!(refused.starts_with("no price: no demo login for ig"), "{refused}");

    let mut c = ExecutionClientPaper::new("ig", EPIC, 0.0, 0.0, 0.0001);
    c.on_contract_value(EPIC, ContractValue::linear(10_000.0));
    c.submit(&buy);
    assert_eq!(rejection(&drain(&mut c)), None, "taken once the size is stated");
    c.on_bar_sides(&mid, Some(&quoted));
    let fills = c.fills.lock().unwrap().clone();
    assert_eq!(fills.len(), 1);
    assert_eq!((fills[0].side, fills[0].size, fills[0].price), (1, 2.0, 1.08505), "the ask");
    let fee = 0.0001 * 2.0 * 1.08505 * 10_000.0;
    assert!((fills[0].fee - fee).abs() < 1e-9, "the fee scales by the size: {}", fills[0].fee);

    let mut fx = ExecutionClientPaper::new("ctrader", "EURUSD", 0.0, 0.0, 0.0);
    fx.on_contract_value("EURUSD", ContractValue::linear(10_000.0));
    assert_eq!(fx.unit("EURUSD"), Ok(ContractValue::default()), "the name's unit stands");
}
