//! Failing-first (TDD) unit tests for the Task-5 execution mapper: `order_to_new_order`
//! (canonical `OrderRequest` → `ProtoOaNewOrderReq`) and `execution_event_to_events`
//! (`ProtoOaExecutionEvent` → canonical `vike_model::events::Event`s). Pure functions, no socket.
//!
//! Volume unit (LIVE-VERIFIED, cTrader demo 2026-07-14): cTrader volume is in centi-units
//! (1/100 of a base unit): `volume = units × 100`. EURUSD `minVolume=stepVolume=100_000`
//! (=0.01 lot), `lotSize=10_000_000` (=1 lot=100k units×100). vike `OrderRequest.qty` is in
//! base-currency UNITS (same as OANDA — `oanda/src/execution/mod.rs`), so `volume = round(qty × 100)`,
//! rounded to `step_volume`, clamped to min/max.
//!
//! Order PRICES: `ProtoOaNewOrderReq.limit_price`/`stop_price` are protobuf `double`s carrying the
//! ABSOLUTE price (e.g. 1.23456) — NOT the 1e5 relative-scaled integer the SPOT/trendbar feed uses.
//! (Only `relative_stop_loss`/`relative_take_profit` are 1e5-scaled i64s.) So the mapper passes the
//! absolute price straight through, and `ProtoOaDeal.execution_price` (also a `double`) is read as
//! an absolute price with no descale.

use vike_ctrader::feed::decode::{
    STOP_LIMIT_LIMIT_OFF_THE_POINTS, STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER,
    STOP_LIMIT_POINT_UNKNOWN, amend_order_req, execution_event_to_events, order_to_new_order,
    stop_limit_slippage,
};
use vike_ctrader::instrument::symbols::SymbolMap;
use vike_ctrader::transport::proto::{
    ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaLightSymbol, ProtoOaOrder,
    ProtoOaOrderType, ProtoOaSymbol, ProtoOaTradeData, ProtoOaTradeSide,
};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// The account's `ProtoOATrader.money_digits` used by every `execution_event_to_events` call in this
/// file — 2 is the LIVE-VERIFIED value (2026-07-14 real demo fill: `commission=-3` raw scales to
/// vike-model's `+0.03` charge at `money_digits=2`; see
/// [`commission_descales_signed_by_money_digits`]).
const MONEY_DIGITS: u32 = 2;

/// EURUSD, symbol_id 1, with the live-verified EURUSD volume grid (centi-units).
fn eurusd_symbols() -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id: 1,
        symbol_name: Some("EURUSD".into()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol {
        symbol_id: 1,
        digits: 5,
        min_volume: Some(100_000),
        step_volume: Some(100_000),
        max_volume: Some(10_000_000_000),
        lot_size: Some(10_000_000),
        ..Default::default()
    }];
    SymbolMap::from_symbols(&light, &full)
}

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "c-1".into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: vike_model::OrderType::from_stored(order_type),
        ..Default::default()
    }
}

#[test]
fn market_buy_maps_type_side_volume_and_labels() {
    let symbols = eurusd_symbols();
    // 1000 units → 100_000 centi-units = min/step (exact).
    let o = order_to_new_order(&req("market", 1, 1000.0), 99, &symbols).expect("mappable");
    assert_eq!(o.ctid_trader_account_id, 99);
    assert_eq!(o.symbol_id, 1);
    assert_eq!(o.order_type, ProtoOaOrderType::Market as i32);
    assert_eq!(o.trade_side, ProtoOaTradeSide::Buy as i32);
    assert_eq!(o.volume, 100_000);
    // order_client_id carried in BOTH label (correlation on the wire) and the clientOrderId field.
    assert_eq!(o.label.as_deref(), Some("c-1"));
    assert_eq!(o.client_order_id.as_deref(), Some("c-1"));
    // market order carries no limit/stop price
    assert_eq!(o.limit_price, None);
    assert_eq!(o.stop_price, None);
}

#[test]
fn sell_side_maps_to_sell() {
    let symbols = eurusd_symbols();
    let o = order_to_new_order(&req("market", -1, 1000.0), 99, &symbols).expect("mappable");
    assert_eq!(o.trade_side, ProtoOaTradeSide::Sell as i32);
}

#[test]
fn limit_order_carries_absolute_limit_price_unscaled() {
    let symbols = eurusd_symbols();
    let mut r = req("limit", 1, 1000.0);
    r.price = Some(1.23456);
    let o = order_to_new_order(&r, 99, &symbols).expect("mappable");
    assert_eq!(o.order_type, ProtoOaOrderType::Limit as i32);
    // ABSOLUTE double, NOT 1e5-scaled (that would be 123456.0).
    assert_eq!(o.limit_price, Some(1.23456));
    assert_eq!(o.stop_price, None);
}

#[test]
fn stop_order_carries_absolute_stop_price_from_trigger() {
    let symbols = eurusd_symbols();
    let mut r = req("stop", -1, 1000.0);
    r.trigger_price = Some(1.19000);
    let o = order_to_new_order(&r, 99, &symbols).expect("mappable");
    assert_eq!(o.order_type, ProtoOaOrderType::Stop as i32);
    assert_eq!(o.stop_price, Some(1.19000));
    assert_eq!(o.limit_price, None);
}

/// Measured on the CI box (event journal): the gate's tick rounding handed the bridge
/// `0.09000000000000001` and the venue answered `INVALID_REQUEST: Order price =
/// 0.09000000000000001 has more digits than symbol allows. Allowed 5 digits`. The wire price is
/// snapped to the symbol's `digits`, for limit and stop alike.
#[test]
fn noisy_limit_and_stop_prices_are_snapped_to_the_symbol_digits() {
    let symbols = eurusd_symbols();
    let noisy = 0.09000000000000001_f64;
    assert_ne!(noisy, 0.09, "the fixture must actually carry the noise");

    let mut limit = req("limit", 1, 1000.0);
    limit.price = Some(noisy);
    let o = order_to_new_order(&limit, 99, &symbols).expect("mappable");
    assert_eq!(o.limit_price, Some(0.09));
    assert_eq!(format!("{}", o.limit_price.unwrap()), "0.09");

    let mut stop = req("stop", -1, 1000.0);
    let above_119 = f64::from_bits(1.19_f64.to_bits() + 1); // one ulp of noise
    assert_ne!(above_119, 1.19);
    stop.trigger_price = Some(above_119);
    let o = order_to_new_order(&stop, 99, &symbols).expect("mappable");
    assert_eq!(o.stop_price, Some(1.19));

    // Rounds to NEAREST at the 5th decimal (not truncation), and a price already on the grid
    // is untouched bit for bit.
    limit.price = Some(1.234566);
    assert_eq!(order_to_new_order(&limit, 99, &symbols).unwrap().limit_price, Some(1.23457));
    limit.price = Some(1.13986);
    assert_eq!(order_to_new_order(&limit, 99, &symbols).unwrap().limit_price, Some(1.13986));
}

/// The amend path snaps the same way, onto the field the resting order's type names.
#[test]
fn amend_snaps_a_noisy_price_to_the_symbol_digits() {
    let symbols = eurusd_symbols();
    let noisy = 0.09000000000000001_f64;

    let limit = req("limit", 1, 1000.0);
    let a = amend_order_req(&limit, 99, 7, Some(2000.0), Some(noisy), &symbols).unwrap();
    assert_eq!((a.ctid_trader_account_id, a.order_id, a.volume), (99, 7, Some(200_000)));
    assert_eq!(a.limit_price, Some(0.09));
    assert_eq!(a.stop_price, None);

    let stop = req("stop", -1, 1000.0);
    let a =
        amend_order_req(&stop, 99, 7, None, Some(f64::from_bits(1.19_f64.to_bits() + 1)), &symbols)
            .unwrap();
    assert_eq!(a.stop_price, Some(1.19));
    assert_eq!(a.limit_price, None);
    assert_eq!(a.volume, None);

    // A qty-only amend carries no price; an unknown symbol is left untouched.
    let a = amend_order_req(&limit, 99, 7, Some(1000.0), None, &symbols).unwrap();
    assert_eq!((a.limit_price, a.stop_price), (None, None));
    let mut other = req("limit", 1, 1000.0);
    other.symbol = "GBPUSD".into();
    let a = amend_order_req(&other, 99, 7, None, Some(noisy), &symbols).unwrap();
    assert_eq!(a.limit_price, Some(noisy));
}

/// EURUSD (five digits), USDJPY (three) and a symbol the handshake read no digits for. No volume grid:
/// 1000 units go out as 100_000 centi-units.
fn stop_limit_symbols() -> SymbolMap {
    let light = [(1, "EURUSD"), (2, "USDJPY"), (3, "NODIGITS")].map(|(symbol_id, name)| {
        ProtoOaLightSymbol { symbol_id, symbol_name: Some(name.into()), ..Default::default() }
    });
    let full = [(1, 5), (2, 3)].map(|(symbol_id, digits)| ProtoOaSymbol {
        symbol_id,
        digits,
        ..Default::default()
    });
    SymbolMap::from_symbols(&light, &full)
}

fn stop_limit(symbol: &str, side: i32, trigger: f64, limit: f64) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        price: Some(limit),
        trigger_price: Some(trigger),
        ..req("stop_limit", side, 1000.0)
    }
}

/// A stop-limit is cTrader's `STOP_LIMIT`: `stopPrice` the trigger (snapped like a stop's), its limit as
/// `slippageInPoints` past the stop in the order's direction, counted in the symbol's points (`10^-digits`),
/// and no `limitPrice` (the venue takes that on a LIMIT only).
#[test]
fn a_stop_limit_rests_at_its_trigger_with_its_limit_as_slippage_points() {
    let symbols = stop_limit_symbols();
    let noisy_trigger = f64::from_bits(1.1_f64.to_bits() + 1);
    // (symbol, side, trigger, limit) -> (stop_price, slippage_in_points)
    let cases = [
        ("EURUSD", 1, 1.10000, 1.10003, 1.1, 3),
        ("EURUSD", -1, 1.10000, 1.09995, 1.1, 5),
        ("EURUSD", 1, 1.10000, 1.10000, 1.1, 0), // a limit AT the trigger
        ("EURUSD", 1, noisy_trigger, 1.10003, 1.1, 3), // f64 noise is not off the grid
        ("USDJPY", 1, 150.000, 150.012, 150.0, 12), // the point is 0.001 at three digits
        ("USDJPY", -1, 150.000, 149.9, 150.0, 100),
    ];
    for (symbol, side, trigger, limit, stop, points) in cases {
        let o = order_to_new_order(&stop_limit(symbol, side, trigger, limit), 99, &symbols)
            .unwrap_or_else(|| panic!("{symbol} {side} {trigger} {limit}: mappable"));
        assert_eq!(o.order_type, ProtoOaOrderType::StopLimit as i32);
        let want_side = if side > 0 { ProtoOaTradeSide::Buy } else { ProtoOaTradeSide::Sell };
        assert_eq!(o.trade_side, want_side as i32);
        assert_eq!(
            (o.stop_price, o.slippage_in_points, o.limit_price),
            (Some(stop), Some(points), None),
            "{symbol} {side} {trigger} {limit}"
        );
        assert_eq!((o.volume, o.client_order_id.as_deref()), (100_000, Some("c-1")));
    }
}

/// What the venue cannot carry is refused BY NAME and never built: a limit short of the trigger (a buy's
/// below, a sell's above), a distance that is not a whole number of points (or does not fit `int32`, or is
/// not a number), a symbol whose digits the handshake did not read, and a stop-limit missing a price.
#[test]
fn a_stop_limit_the_venue_cannot_carry_is_refused_by_name_and_never_built() {
    let symbols = stop_limit_symbols();
    let cases = [
        ("EURUSD", 1, 1.10000, 1.09999, STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER),
        ("EURUSD", -1, 1.10000, 1.10001, STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER),
        ("EURUSD", 1, 1.10000, 1.100035, STOP_LIMIT_LIMIT_OFF_THE_POINTS),
        ("USDJPY", 1, 150.000, 150.0125, STOP_LIMIT_LIMIT_OFF_THE_POINTS),
        ("EURUSD", 1, 1.10000, f64::NAN, STOP_LIMIT_LIMIT_OFF_THE_POINTS),
        ("EURUSD", 1, 1.0, 1.0e6, STOP_LIMIT_LIMIT_OFF_THE_POINTS), // 1e11 points: not an int32
        ("NODIGITS", 1, 1.10000, 1.10003, STOP_LIMIT_POINT_UNKNOWN),
    ];
    for (symbol, side, trigger, limit, why) in cases {
        let id = symbols.id_of(symbol).expect("a known symbol");
        assert_eq!(
            stop_limit_slippage(side, trigger, limit, id, &symbols),
            Err(why),
            "{symbol} {side} {trigger} {limit}"
        );
        assert!(
            order_to_new_order(&stop_limit(symbol, side, trigger, limit), 99, &symbols).is_none(),
            "{symbol} {side} {trigger} {limit}: must not be built"
        );
    }
    for (price, trigger_price) in [(None, Some(1.1)), (Some(1.1), None)] {
        let r = OrderRequest { price, trigger_price, ..stop_limit("EURUSD", 1, 0.0, 0.0) };
        assert!(order_to_new_order(&r, 99, &symbols).is_none(), "{price:?} {trigger_price:?}");
    }
}

/// The bridge sends no `timeInForce`, so a stop-limit asking any lifetime but GTC would rest GTC
/// without a word: it is never built (the exec client refuses it by name,
/// `STOP_LIMIT_LIFETIME_UNSUPPORTED`).
#[test]
fn a_stop_limit_with_a_lifetime_but_gtc_is_never_built() {
    use vike_model::TimeInForce;
    let symbols = stop_limit_symbols();
    let gtc = stop_limit("EURUSD", 1, 1.10000, 1.10003);
    assert!(order_to_new_order(&gtc, 99, &symbols).is_some());
    for tif in [TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Gtd, TimeInForce::Day] {
        let r = OrderRequest { time_in_force: tif, ..gtc.clone() };
        assert!(order_to_new_order(&r, 99, &symbols).is_none(), "{tif:?}");
    }
}

/// A stop-limit's amend moves its LIMIT as `slippageInPoints` from its trigger and restates
/// `stopPrice` (the proto does not say what an absent amend field means); a size-only amend restates
/// the current distance; a new limit the venue cannot carry is refused by name, as at submit.
#[test]
fn a_stop_limit_amend_moves_its_limit_as_slippage_points_and_restates_its_stop() {
    let symbols = stop_limit_symbols();
    let resting = stop_limit("EURUSD", 1, 1.10000, 1.10003);
    let a = amend_order_req(&resting, 99, 7, None, Some(1.10010), &symbols).unwrap();
    assert_eq!(
        (a.stop_price, a.slippage_in_points, a.limit_price, a.volume),
        (Some(1.1), Some(10), None, None)
    );
    let a = amend_order_req(&resting, 99, 7, Some(2000.0), None, &symbols).unwrap();
    assert_eq!(
        (a.stop_price, a.slippage_in_points, a.limit_price, a.volume),
        (Some(1.1), Some(3), None, Some(200_000))
    );
    let sell = stop_limit("USDJPY", -1, 150.000, 149.990);
    let a = amend_order_req(&sell, 99, 7, None, Some(149.950), &symbols).unwrap();
    assert_eq!((a.stop_price, a.slippage_in_points), (Some(150.0), Some(50)));

    for (limit, why) in [
        (1.09990, STOP_LIMIT_LIMIT_SHORT_OF_THE_TRIGGER),
        (1.100105, STOP_LIMIT_LIMIT_OFF_THE_POINTS),
    ] {
        let got = amend_order_req(&resting, 99, 7, None, Some(limit), &symbols);
        assert_eq!(got.unwrap_err(), why, "{limit}");
    }
}

/// The venue's `ORDER_REPLACED` of a stop-limit reports its LIMIT (the engine's column for a
/// stop-limit's modified price), rebuilt from `stopPrice` and `slippageInPoints` in the order's
/// direction; never the stop price. Without the slippage it reports no price at all.
#[test]
fn a_replaced_stop_limit_reports_its_limit_not_its_stop() {
    let symbols = eurusd_symbols();
    let replaced = |side: ProtoOaTradeSide, slippage: Option<i64>| {
        let mut order = order_ref(555, "c-1");
        order.order_type = ProtoOaOrderType::StopLimit as i32;
        order.trade_data.trade_side = side as i32;
        order.stop_price = Some(1.1);
        order.slippage_in_points = slippage;
        let event = ProtoOaExecutionEvent {
            ctid_trader_account_id: 99,
            execution_type: ProtoOaExecutionType::OrderReplaced as i32,
            order: Some(order),
            ..Default::default()
        };
        match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
            Event::OrderModified(m) => m.new_price,
            other => panic!("expected OrderModified, got {other:?}"),
        }
    };
    assert_eq!(replaced(ProtoOaTradeSide::Buy, Some(3)), Some(1.10003));
    assert_eq!(replaced(ProtoOaTradeSide::Sell, Some(5)), Some(1.09995));
    assert_eq!(replaced(ProtoOaTradeSide::Buy, None), None);
}

#[test]
fn volume_rounds_to_step() {
    let symbols = eurusd_symbols();
    // 1200 units → 120_000 centi → round(1.2)=1 step → 100_000
    let o = order_to_new_order(&req("market", 1, 1200.0), 99, &symbols).expect("mappable");
    assert_eq!(o.volume, 100_000);
}

#[test]
fn unknown_symbol_maps_to_none() {
    let symbols = eurusd_symbols();
    let mut r = req("market", 1, 1000.0);
    r.symbol = "GBPUSD".into();
    assert!(order_to_new_order(&r, 99, &symbols).is_none());
}

#[test]
fn quantity_below_min_volume_maps_to_none() {
    let symbols = eurusd_symbols();
    // 100 units → 10_000 centi → round(0.1)=0 steps → 0 < min_volume ⇒ reject (never place a
    // sub-min order; the caller synthesizes a terminal OrderRejected).
    assert!(order_to_new_order(&req("market", 1, 100.0), 99, &symbols).is_none());
}

/// A full ProtoOaOrder for an execution event carrying the given order_client_id + venue order id.
fn order_ref(order_id: i64, order_client_id: &str) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: ProtoOaTradeSide::Buy as i32,
            label: Some(order_client_id.into()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2,
        client_order_id: Some(order_client_id.into()),
        ..Default::default()
    }
}

/// DUAL-PUBLISH venue-contract (mirrors binance's `event_mapper`): a FILLED execution event maps
/// to BOTH `Event::Fill` — the bare fill the core `vike_execution::Account` folds into position/PnL/
/// trades — AND the `Event::OrderFilled` wrap that drives the OMS order FSM, in that ORDER
/// (`Event::Fill` FIRST). Before this fix ctrader emitted ONLY the wrap, so `Account` was blind to
/// ctrader positions; asserting `len()==2` and the ordering pins the fix.
#[test]
fn order_filled_dual_publishes_fill_then_order_filled() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 777,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 1,
            execution_timestamp: 1234,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let evs = execution_event_to_events(&event, &symbols, MONEY_DIGITS);
    assert_eq!(evs.len(), 2, "FILLED must dual-publish [Event::Fill, Event::OrderFilled]");
    // [0] is the bare Fill the Account folds — descaled fields correct for the position/PnL fold.
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.order_client_id, "c-1");
            assert_eq!(fill.trade_id, "777");
            assert_eq!(fill.venue.as_str(), "ctrader");
            assert_eq!(fill.symbol.as_str(), "EURUSD");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0); // 100_000 centi / 100
            assert_eq!(fill.last_price, 1.10); // absolute double, no descale
        }
        other => panic!("expected Event::Fill at [0], got {other:?}"),
    }
    // [1] is the OrderFilled wrap carrying the SAME fill for the OMS FSM.
    match &evs[1] {
        Event::OrderFilled(f) => {
            assert_eq!(f.order_client_id, "c-1");
            assert_eq!(f.fill.trade_id, "777");
            assert_eq!(f.fill.symbol.as_str(), "EURUSD");
            assert_eq!(f.fill.side, 1);
            assert_eq!(f.fill.last_qty, 1000.0);
            assert_eq!(f.fill.last_price, 1.10);
        }
        other => panic!("expected Event::OrderFilled at [1], got {other:?}"),
    }
}

/// End-to-end venue-contract proof: the dual-published `Event::Fill` folds through the core
/// `vike_execution::Account` and updates the position — the whole reason the bare `Event::Fill` must be
/// emitted. Before this fix `Account` never saw a ctrader fill and tracked no position/PnL.
#[test]
fn dual_published_fill_folds_into_account_position() {
    use vike_execution::{Account, BalanceMode};
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 777,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 1,
            execution_timestamp: 1234,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let evs = execution_event_to_events(&event, &symbols, MONEY_DIGITS);
    let Event::Fill(fill) = &evs[0] else {
        panic!("expected Event::Fill at [0], got {:?}", evs[0]);
    };

    let mut account = Account::new(1.0, "ctrader", None, BalanceMode::Delta);
    account.apply_fill(fill);

    let key: vike_execution::PositionKey = ("ctrader".into(), "EURUSD".into(), fill.position_side);
    let pos = account.positions.get(&key).expect("Account folded the ctrader fill into a position");
    assert_eq!(pos.size, 1000.0, "long 1000 units after the buy fill");
    assert_eq!(pos.average_price, 1.10);
}

/// LIVE-VERIFIED regression (2026-07-14, real cTrader demo fill): `deal.commission` is a SIGNED
/// integer scaled by `10^money_digits` (falling back to the account-level `ProtoOATrader
/// .money_digits` when the deal omits its own `moneyDigits`). A real demo deal returned
/// `commission=-3` at `money_digits=2`. cTrader's wire sign is NEGATIVE for a charged fee, the
/// opposite of `vike_model::events::FillEvent.commission`'s convention (`> 0` = charge/cost,
/// `< 0` = rebate), so the mapper NEGATES it: `-3` raw -> `+0.03` (a charge), matching how
/// `Account::apply_fill` subtracts a positive commission from balance.
#[test]
fn commission_descales_signed_by_money_digits() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 900,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 1,
            execution_timestamp: 1234,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            commission: Some(-3),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Assert on the dual-published `Event::Fill` at [0] — the fill the `Account` actually folds,
    // so commission-descaling correctness matters precisely here.
    match &execution_event_to_events(&event, &symbols, 2)[0] {
        Event::Fill(fill) => {
            assert!(
                (fill.commission - 0.03).abs() < 1e-9,
                "commission={}, expected +0.03",
                fill.commission
            );
            assert!(fill.commission > 0.0, "vike-model convention: a charged fee is positive");
        }
        other => panic!("expected Event::Fill, got {other:?}"),
    }
}

/// Prefers the deal's OWN `money_digits` (`ProtoOADeal.moneyDigits`, tag 17) over the account-level
/// fallback when the deal surfaces it — e.g. a deal reporting `money_digits=3` with the account at
/// `money_digits=2` must descale by `10^3`, not `10^2`.
#[test]
fn commission_prefers_deal_level_money_digits_over_account_fallback() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 901,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 1,
            execution_timestamp: 1234,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            commission: Some(-3),
            money_digits: Some(3),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Account-level money_digits passed in is 2, but the deal's own money_digits=3 must win:
    // -3 / 10^3 -> -0.003, negated -> +0.003 (NOT +0.03, which would be the account fallback).
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::Fill(fill) => {
            assert!(
                (fill.commission - 0.003).abs() < 1e-9,
                "commission={}, expected +0.003 (deal-level money_digits=3, not account's 2)",
                fill.commission
            );
        }
        other => panic!("expected Event::Fill, got {other:?}"),
    }
}

/// **The venue's conversion fee rides on a foreign-quoted CLOSE** (decision 0183): a closing deal carries a
/// `ProtoOAClosePositionDetail` whose `pnlConversionFee` the venue charges to the balance when the symbol's
/// quote asset is not the deposit asset. The fill's commission is the deal's own commission PLUS the fee, in
/// the account's currency (`commission_asset` stays empty), so `Account` (`balance -= commission`) follows the
/// venue; the fee is a COST whichever sign the wire gives it (the page states none), scaled by the detail's own
/// `moneyDigits`; no detail and no fee leave the commission exactly as it was.
#[test]
fn a_closing_deals_conversion_fee_is_part_of_the_fills_commission() {
    use vike_execution::{Account, BalanceMode};
    let symbols = eurusd_symbols();
    let closing = |fee: Option<i64>, detail_digits: Option<u32>, with_detail: bool| {
        let detail =
            with_detail.then(|| vike_ctrader::transport::proto::ProtoOaClosePositionDetail {
                entry_price: 1.10,
                gross_profit: 67,
                commission: -3,
                pnl_conversion_fee: fee,
                money_digits: detail_digits,
                ..Default::default()
            });
        ProtoOaExecutionEvent {
            ctid_trader_account_id: 99,
            execution_type: ProtoOaExecutionType::OrderFilled as i32,
            order: Some(order_ref(555, "c-1")),
            deal: Some(ProtoOaDeal {
                deal_id: 903,
                order_id: 555,
                volume: 100_000,
                filled_volume: 100_000,
                symbol_id: 1,
                execution_timestamp: 1234,
                execution_price: Some(1.10),
                trade_side: ProtoOaTradeSide::Sell as i32,
                commission: Some(-3),
                close_position_detail: detail,
                ..Default::default()
            }),
            ..Default::default()
        }
    };
    let commission =
        |event: &ProtoOaExecutionEvent| match &execution_event_to_events(event, &symbols, 2)[0] {
            Event::Fill(fill) => {
                assert_eq!(fill.commission_asset, "", "the account's own currency");
                fill.commission
            }
            other => panic!("expected Event::Fill, got {other:?}"),
        };
    // -3 raw commission = 0.03; a fee of -7 raw (charged, as the commission's own sign) = 0.07 more.
    assert!((commission(&closing(Some(-7), Some(2), true)) - 0.10).abs() < 1e-9);
    // The same fee stated positive is the same cost: a fee is never a credit.
    assert!((commission(&closing(Some(7), Some(2), true)) - 0.10).abs() < 1e-9);
    // The detail's own exponent wins over the account's (here 3: 7 raw = 0.007).
    assert!((commission(&closing(Some(-7), Some(3), true)) - 0.037).abs() < 1e-9);
    // No fee stated, no detail (an opening deal), a zero fee: the commission alone, as before.
    for event in [
        closing(None, Some(2), true),
        closing(Some(-7), None, false),
        closing(Some(0), Some(2), true),
    ] {
        assert!((commission(&event) - 0.03).abs() < 1e-9);
    }

    // The engine follows the venue: the balance drops by commission + fee.
    let evs = execution_event_to_events(&closing(Some(-7), Some(2), true), &symbols, 2);
    let Event::Fill(fill) = &evs[0] else { panic!("a fill") };
    let mut account = Account::new(1.0, "ctrader", None, BalanceMode::Delta);
    let before = account.balance;
    account.apply_fill(fill);
    assert!((account.fees_paid - 0.10).abs() < 1e-9, "fees paid {}", account.fees_paid);
    assert!(
        ((before - account.balance) - 0.10).abs() < 1e-9,
        "the balance fell by commission + fee"
    );
}

/// Minor regression (Fix 3): a deal with no commission AND no money_digits (both absent, both
/// account-level fallback of 0) must yield exactly `0.0` — no panic, no divide-by-zero (`10f64
/// .powi(0) == 1.0`, so this is a divide-by-1, not divide-by-0).
#[test]
fn commission_absent_and_zero_money_digits_yields_zero_no_panic() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 902,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id: 1,
            execution_timestamp: 1234,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            commission: None,
            money_digits: None,
            ..Default::default()
        }),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, 0)[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.commission, 0.0);
        }
        other => panic!("expected Event::Fill, got {other:?}"),
    }
}

/// DUAL-PUBLISH (partial-fill arm): a PARTIAL_FILL also emits BOTH `Event::Fill` (for the
/// `Account` fold) FIRST and the `Event::OrderPartiallyFilled` wrap (for the OMS FSM) second.
#[test]
fn order_partial_fill_dual_publishes_fill_then_partially_filled() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderPartialFill as i32,
        order: Some(order_ref(555, "c-1")),
        deal: Some(ProtoOaDeal {
            deal_id: 778,
            order_id: 555,
            volume: 100_000,
            filled_volume: 50_000,
            symbol_id: 1,
            execution_price: Some(1.10),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let evs = execution_event_to_events(&event, &symbols, MONEY_DIGITS);
    assert_eq!(
        evs.len(),
        2,
        "PARTIAL_FILL must dual-publish [Event::Fill, Event::OrderPartiallyFilled]"
    );
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.order_client_id, "c-1");
            assert_eq!(fill.last_qty, 500.0); // 50_000 centi / 100
        }
        other => panic!("expected Event::Fill at [0], got {other:?}"),
    }
    match &evs[1] {
        Event::OrderPartiallyFilled(f) => {
            assert_eq!(f.order_client_id, "c-1");
            assert_eq!(f.fill.last_qty, 500.0);
        }
        other => panic!("expected Event::OrderPartiallyFilled at [1], got {other:?}"),
    }
}

#[test]
fn order_accepted_maps_with_venue_order_id() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, "c-1");
            assert_eq!(a.venue_order_id.as_deref(), Some("555"));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn order_rejected_carries_error_code_reason() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderRejected as i32,
        order: Some(order_ref(555, "c-1")),
        error_code: Some("NOT_ENOUGH_MONEY".into()),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::OrderRejected(r) => {
            assert_eq!(r.order_client_id, "c-1");
            assert_eq!(r.reason, "NOT_ENOUGH_MONEY");
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn order_cancelled_maps_to_canceled() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderCancelled as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::OrderCanceled(c) => assert_eq!(c.order_client_id, "c-1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn order_replaced_maps_to_modified() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderReplaced as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.order_client_id, "c-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("555"));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

/// `ORDER_REPLACED` must carry the CONFIRMED post-amend terms straight from the referenced
/// order — `trade_data.volume` (centi-units → units) as `new_qty`, `limit_price` (absolute,
/// unscaled) as `new_price` for a LIMIT order — never a hardcoded `None`/`None`.
#[test]
fn order_replaced_carries_confirmed_new_quantity_and_price() {
    let symbols = eurusd_symbols();
    let mut order = order_ref(555, "c-1");
    order.order_type = ProtoOaOrderType::Limit as i32;
    order.trade_data.volume = 200_000; // centi-units -> 2000 units
    order.limit_price = Some(1.105);
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderReplaced as i32,
        order: Some(order),
        ..Default::default()
    };
    match &execution_event_to_events(&event, &symbols, MONEY_DIGITS)[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.order_client_id, "c-1");
            assert_eq!(m.new_qty, Some(2000.0));
            assert_eq!(m.new_price, Some(1.105));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

/// `ORDER_CANCEL_REJECTED` must not be silently dropped — the FSM models it as a NON-TERMINAL
/// event so a caller learns its cancel request failed and the order is still live.
#[test]
fn order_cancel_rejected_maps_to_cancel_rejected_event() {
    let symbols = eurusd_symbols();
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderCancelRejected as i32,
        order: Some(order_ref(555, "c-1")),
        error_code: Some("ORDER_NOT_FOUND".into()),
        ..Default::default()
    };
    let evs = execution_event_to_events(&event, &symbols, MONEY_DIGITS);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.order_client_id, "c-1");
            assert_eq!(r.reason, "ORDER_NOT_FOUND");
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}

/// The amend's size goes through the SAME volume grid a new order's does: an off-grid amend is refused by the
/// venue (`TRADING_BAD_VOLUME`), so it is stepped here. Unlike a submit (nearest step) it steps DOWN: the nearest
/// step can lie above the quantity the pre-trade gate judged.
#[test]
fn an_amended_size_is_stepped_onto_the_symbols_volume_grid() {
    let symbols = eurusd_symbols();
    let limit = req("limit", 1, 1000.0);
    // 2400 units → 240_000 centi → 2 steps → 200_000
    let a = amend_order_req(&limit, 99, 7, Some(2400.0), None, &symbols).unwrap();
    assert_eq!(a.volume, Some(200_000));
    // 2600 units → 2.6 steps → DOWN to 2 steps (a submit would round to 3)
    let a = amend_order_req(&limit, 99, 7, Some(2600.0), None, &symbols).unwrap();
    assert_eq!(a.volume, Some(200_000));
    // an exact multiple is untouched
    let a = amend_order_req(&limit, 99, 7, Some(1000.0), None, &symbols).unwrap();
    assert_eq!(a.volume, Some(100_000));
}

/// A size below the symbol's minimum is REFUSED (the caller answers `OrderModifyRejected`), never put on the
/// wire as a sub-minimum volume; and a price-only amend never consults the grid.
#[test]
fn an_amended_size_below_the_minimum_is_refused_not_sent() {
    let symbols = eurusd_symbols();
    let limit = req("limit", 1, 1000.0);
    let why = amend_order_req(&limit, 99, 7, Some(100.0), None, &symbols).unwrap_err();
    assert!(why.contains("minimum"), "{why}");
    assert!(amend_order_req(&limit, 99, 7, None, Some(1.1), &symbols).is_ok(), "no size, no grid");
}
