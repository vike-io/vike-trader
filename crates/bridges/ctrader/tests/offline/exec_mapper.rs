//! Failing-first (TDD) unit tests for the Task-5 execution mapper: `order_to_new_order`
//! (canonical `OrderRequest` → `ProtoOaNewOrderReq`) and `exec_event_to_events`
//! (`ProtoOaExecutionEvent` → canonical `vike_model::events::Event`s). Pure functions, no socket.
//!
//! Volume unit (LIVE-VERIFIED, cTrader demo 2026-07-14): cTrader volume is in centi-units
//! (1/100 of a base unit): `volume = units × 100`. EURUSD `minVolume=stepVolume=100_000`
//! (=0.01 lot), `lotSize=10_000_000` (=1 lot=100k units×100). vike `OrderRequest.qty` is in
//! base-currency UNITS (same as OANDA — `oanda/src/exec.rs`), so `volume = round(qty × 100)`,
//! rounded to `step_volume`, clamped to min/max.
//!
//! Order PRICES: `ProtoOaNewOrderReq.limit_price`/`stop_price` are protobuf `double`s carrying the
//! ABSOLUTE price (e.g. 1.23456) — NOT the 1e5 relative-scaled integer the SPOT/trendbar feed uses.
//! (Only `relative_stop_loss`/`relative_take_profit` are 1e5-scaled i64s.) So the mapper passes the
//! absolute price straight through, and `ProtoOaDeal.execution_price` (also a `double`) is read as
//! an absolute price with no descale.

use vike_ctrader::event_mapper::{exec_event_to_events, order_to_new_order};
use vike_ctrader::proto::{
    ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaLightSymbol, ProtoOaOrder,
    ProtoOaOrderType, ProtoOaSymbol, ProtoOaTradeData, ProtoOaTradeSide,
};
use vike_ctrader::symbols::SymbolMap;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// The account's `ProtoOATrader.money_digits` used by every `exec_event_to_events` call in this
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
        client_order_id: "c-1".into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: order_type.into(),
        ..Default::default()
    }
}

#[test]
fn market_buy_maps_type_side_volume_and_labels() {
    let syms = eurusd_symbols();
    // 1000 units → 100_000 centi-units = min/step (exact).
    let o = order_to_new_order(&req("market", 1, 1000.0), 99, &syms).expect("mappable");
    assert_eq!(o.ctid_trader_account_id, 99);
    assert_eq!(o.symbol_id, 1);
    assert_eq!(o.order_type, ProtoOaOrderType::Market as i32);
    assert_eq!(o.trade_side, ProtoOaTradeSide::Buy as i32);
    assert_eq!(o.volume, 100_000);
    // client_order_id carried in BOTH label (correlation on the wire) and the clientOrderId field.
    assert_eq!(o.label.as_deref(), Some("c-1"));
    assert_eq!(o.client_order_id.as_deref(), Some("c-1"));
    // market order carries no limit/stop price
    assert_eq!(o.limit_price, None);
    assert_eq!(o.stop_price, None);
}

#[test]
fn sell_side_maps_to_sell() {
    let syms = eurusd_symbols();
    let o = order_to_new_order(&req("market", -1, 1000.0), 99, &syms).expect("mappable");
    assert_eq!(o.trade_side, ProtoOaTradeSide::Sell as i32);
}

#[test]
fn limit_order_carries_absolute_limit_price_unscaled() {
    let syms = eurusd_symbols();
    let mut r = req("limit", 1, 1000.0);
    r.price = Some(1.23456);
    let o = order_to_new_order(&r, 99, &syms).expect("mappable");
    assert_eq!(o.order_type, ProtoOaOrderType::Limit as i32);
    // ABSOLUTE double, NOT 1e5-scaled (that would be 123456.0).
    assert_eq!(o.limit_price, Some(1.23456));
    assert_eq!(o.stop_price, None);
}

#[test]
fn stop_order_carries_absolute_stop_price_from_trigger() {
    let syms = eurusd_symbols();
    let mut r = req("stop", -1, 1000.0);
    r.trigger_price = Some(1.19000);
    let o = order_to_new_order(&r, 99, &syms).expect("mappable");
    assert_eq!(o.order_type, ProtoOaOrderType::Stop as i32);
    assert_eq!(o.stop_price, Some(1.19000));
    assert_eq!(o.limit_price, None);
}

#[test]
fn volume_rounds_to_step() {
    let syms = eurusd_symbols();
    // 1200 units → 120_000 centi → round(1.2)=1 step → 100_000
    let o = order_to_new_order(&req("market", 1, 1200.0), 99, &syms).expect("mappable");
    assert_eq!(o.volume, 100_000);
}

#[test]
fn unknown_symbol_maps_to_none() {
    let syms = eurusd_symbols();
    let mut r = req("market", 1, 1000.0);
    r.symbol = "GBPUSD".into();
    assert!(order_to_new_order(&r, 99, &syms).is_none());
}

#[test]
fn qty_below_min_volume_maps_to_none() {
    let syms = eurusd_symbols();
    // 100 units → 10_000 centi → round(0.1)=0 steps → 0 < min_volume ⇒ reject (never place a
    // sub-min order; the caller synthesizes a terminal OrderRejected).
    assert!(order_to_new_order(&req("market", 1, 100.0), 99, &syms).is_none());
}

/// A full ProtoOaOrder for an execution event carrying the given client_order_id + venue order id.
fn order_ref(order_id: i64, coid: &str) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: ProtoOaTradeSide::Buy as i32,
            label: Some(coid.into()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2,
        client_order_id: Some(coid.into()),
        ..Default::default()
    }
}

/// DUAL-PUBLISH venue-contract (mirrors binance's `event_mapper`): a FILLED execution event maps
/// to BOTH `Event::Fill` — the bare fill the core `vike_exec::Account` folds into position/PnL/
/// trades — AND the `Event::OrderFilled` wrap that drives the OMS order FSM, in that ORDER
/// (`Event::Fill` FIRST). Before this fix ctrader emitted ONLY the wrap, so `Account` was blind to
/// ctrader positions; asserting `len()==2` and the ordering pins the fix.
#[test]
fn order_filled_dual_publishes_fill_then_order_filled() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    let evs = exec_event_to_events(&ev, &syms, MONEY_DIGITS);
    assert_eq!(evs.len(), 2, "FILLED must dual-publish [Event::Fill, Event::OrderFilled]");
    // [0] is the bare Fill the Account folds — descaled fields correct for the position/PnL fold.
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "c-1");
            assert_eq!(fill.trade_id, "777");
            assert_eq!(fill.venue.as_str(), "ctrader");
            assert_eq!(fill.symbol.as_str(), "EURUSD");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0); // 100_000 centi / 100
            assert_eq!(fill.last_px, 1.10); // absolute double, no descale
        }
        other => panic!("expected Event::Fill at [0], got {other:?}"),
    }
    // [1] is the OrderFilled wrap carrying the SAME fill for the OMS FSM.
    match &evs[1] {
        Event::OrderFilled(f) => {
            assert_eq!(f.client_order_id, "c-1");
            assert_eq!(f.fill.trade_id, "777");
            assert_eq!(f.fill.symbol.as_str(), "EURUSD");
            assert_eq!(f.fill.side, 1);
            assert_eq!(f.fill.last_qty, 1000.0);
            assert_eq!(f.fill.last_px, 1.10);
        }
        other => panic!("expected Event::OrderFilled at [1], got {other:?}"),
    }
}

/// End-to-end venue-contract proof: the dual-published `Event::Fill` folds through the core
/// `vike_exec::Account` and updates the position — the whole reason the bare `Event::Fill` must be
/// emitted. Before this fix `Account` never saw a ctrader fill and tracked no position/PnL.
#[test]
fn dual_published_fill_folds_into_account_position() {
    use vike_exec::{Account, BalanceMode};
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    let evs = exec_event_to_events(&ev, &syms, MONEY_DIGITS);
    let Event::Fill(fill) = &evs[0] else {
        panic!("expected Event::Fill at [0], got {:?}", evs[0]);
    };

    let mut account = Account::new(1.0, "ctrader", None, BalanceMode::Delta);
    account.apply_fill(fill);

    let key: vike_exec::PositionKey = ("ctrader".into(), "EURUSD".into(), fill.position_side);
    let pos = account.positions.get(&key).expect("Account folded the ctrader fill into a position");
    assert_eq!(pos.size, 1000.0, "long 1000 units after the buy fill");
    assert_eq!(pos.avg_px, 1.10);
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
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    match &exec_event_to_events(&ev, &syms, 2)[0] {
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
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
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

/// Minor regression (Fix 3): a deal with no commission AND no money_digits (both absent, both
/// account-level fallback of 0) must yield exactly `0.0` — no panic, no divide-by-zero (`10f64
/// .powi(0) == 1.0`, so this is a divide-by-1, not divide-by-0).
#[test]
fn commission_absent_and_zero_money_digits_yields_zero_no_panic() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    match &exec_event_to_events(&ev, &syms, 0)[0] {
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
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
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
    let evs = exec_event_to_events(&ev, &syms, MONEY_DIGITS);
    assert_eq!(
        evs.len(),
        2,
        "PARTIAL_FILL must dual-publish [Event::Fill, Event::OrderPartiallyFilled]"
    );
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "c-1");
            assert_eq!(fill.last_qty, 500.0); // 50_000 centi / 100
        }
        other => panic!("expected Event::Fill at [0], got {other:?}"),
    }
    match &evs[1] {
        Event::OrderPartiallyFilled(f) => {
            assert_eq!(f.client_order_id, "c-1");
            assert_eq!(f.fill.last_qty, 500.0);
        }
        other => panic!("expected Event::OrderPartiallyFilled at [1], got {other:?}"),
    }
}

#[test]
fn order_accepted_maps_with_venue_order_id() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c-1");
            assert_eq!(a.venue_order_id.as_deref(), Some("555"));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn order_rejected_carries_error_code_reason() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderRejected as i32,
        order: Some(order_ref(555, "c-1")),
        error_code: Some("NOT_ENOUGH_MONEY".into()),
        ..Default::default()
    };
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "c-1");
            assert_eq!(r.reason, "NOT_ENOUGH_MONEY");
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn order_cancelled_maps_to_canceled() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderCancelled as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
        Event::OrderCanceled(c) => assert_eq!(c.client_order_id, "c-1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn order_replaced_maps_to_modified() {
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderReplaced as i32,
        order: Some(order_ref(555, "c-1")),
        ..Default::default()
    };
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "c-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("555"));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

/// `ORDER_REPLACED` must carry the CONFIRMED post-amend terms straight from the referenced
/// order — `trade_data.volume` (centi-units → units) as `new_qty`, `limit_price` (absolute,
/// unscaled) as `new_price` for a LIMIT order — never a hardcoded `None`/`None`.
#[test]
fn order_replaced_carries_confirmed_new_qty_and_price() {
    let syms = eurusd_symbols();
    let mut order = order_ref(555, "c-1");
    order.order_type = ProtoOaOrderType::Limit as i32;
    order.trade_data.volume = 200_000; // centi-units -> 2000 units
    order.limit_price = Some(1.105);
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderReplaced as i32,
        order: Some(order),
        ..Default::default()
    };
    match &exec_event_to_events(&ev, &syms, MONEY_DIGITS)[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "c-1");
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
    let syms = eurusd_symbols();
    let ev = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderCancelRejected as i32,
        order: Some(order_ref(555, "c-1")),
        error_code: Some("ORDER_NOT_FOUND".into()),
        ..Default::default()
    };
    let evs = exec_event_to_events(&ev, &syms, MONEY_DIGITS);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.client_order_id, "c-1");
            assert_eq!(r.reason, "ORDER_NOT_FOUND");
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}
