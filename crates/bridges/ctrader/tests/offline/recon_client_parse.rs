//! Pure parse tests for the cTrader `ReconClient` report parsers (ReconFactory seam, wave-2 task
//! 6) — NO network: builds the already-decoded prost structs directly (cTrader is protobuf, not
//! REST/JSON, so there is no wire body to feed a JSON parser) and feeds them straight to
//! `vike_ctrader::recon_client`'s pure `parse_*` functions. Asserts the tricky bits the module doc
//! calls out: unsigned-volume-plus-tradeSide -> signed qty, the client-order-id fallback to
//! `tradeData.label`, fill-progress-derived `PARTIALLY_FILLED`, and the commission
//! descale-and-negate.

use vike_ctrader::proto::{
    ProtoOaDeal, ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition,
    ProtoOaTradeData, ProtoOaTradeSide,
};
use vike_ctrader::recon_client::{
    parse_fills, parse_orders, parse_positions, parse_raw_positions, RawPosition,
};
use vike_ctrader::symbols::SymbolMap;
use vike_model::events::{LiquiditySide, PositionSide};

fn symbols() -> SymbolMap {
    use vike_ctrader::proto::{ProtoOaLightSymbol, ProtoOaSymbol};
    SymbolMap::from_symbols(
        &[
            ProtoOaLightSymbol {
                symbol_id: 1,
                symbol_name: Some("EURUSD".to_string()),
                enabled: Some(true),
                ..Default::default()
            },
            ProtoOaLightSymbol {
                symbol_id: 2,
                symbol_name: Some("GBPUSD".to_string()),
                enabled: Some(true),
                ..Default::default()
            },
        ],
        &[
            ProtoOaSymbol { symbol_id: 1, digits: 5, pip_position: 4, ..Default::default() },
            ProtoOaSymbol { symbol_id: 2, digits: 5, pip_position: 4, ..Default::default() },
        ],
    )
}

fn trade_data(symbol_id: i64, volume: i64, side: ProtoOaTradeSide) -> ProtoOaTradeData {
    ProtoOaTradeData { symbol_id, volume, trade_side: side as i32, ..Default::default() }
}

// --- parse_orders --------------------------------------------------------------------------

#[test]
fn parses_resting_order_every_field() {
    let order = ProtoOaOrder {
        order_id: 42,
        trade_data: ProtoOaTradeData {
            label: Some("client-coid-1".to_string()),
            ..trade_data(1, 1000, ProtoOaTradeSide::Buy)
        },
        order_type: ProtoOaOrderType::Limit as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        execution_price: Some(1.0950),
        executed_volume: Some(0),
        utc_last_update_timestamp: Some(1_700_000_000_000),
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols());
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "ctrader");
    assert_eq!(o.symbol, "EURUSD");
    assert_eq!(o.venue_order_id.as_str(), "42");
    assert_eq!(
        o.client_order_id.as_deref(),
        Some("client-coid-1"),
        "falls back to trade_data.label"
    );
    assert_eq!(o.side, 1, "Buy -> +1");
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.qty, 10.0, "1000 cents -> 10.00 units");
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.avg_px, 1.0950);
    assert_eq!(o.status, "ACCEPTED");
    assert_eq!(o.ts, 1_700_000_000_000);
}

#[test]
fn client_order_id_field_takes_precedence_over_label() {
    let order = ProtoOaOrder {
        client_order_id: Some("dedicated-coid".to_string()),
        trade_data: ProtoOaTradeData {
            label: Some("fallback-label".to_string()),
            ..trade_data(1, 100, ProtoOaTradeSide::Sell)
        },
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols());
    assert_eq!(r[0].client_order_id.as_deref(), Some("dedicated-coid"));
    assert_eq!(r[0].side, -1, "Sell -> -1");
}

#[test]
fn absent_client_order_id_and_label_normalizes_to_none() {
    let order = ProtoOaOrder {
        trade_data: trade_data(1, 100, ProtoOaTradeSide::Buy),
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols());
    assert_eq!(r[0].client_order_id, None);
}

#[test]
fn accepted_with_partial_fill_progress_derives_partially_filled() {
    let order = ProtoOaOrder {
        trade_data: trade_data(1, 1000, ProtoOaTradeSide::Buy),
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        executed_volume: Some(400),
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols());
    assert_eq!(r[0].status, "PARTIALLY_FILLED");
    assert_eq!(r[0].filled_qty, 4.0);
}

#[test]
fn every_terminal_status_normalizes() {
    for (status, want) in [
        (ProtoOaOrderStatus::OrderStatusFilled, "FILLED"),
        (ProtoOaOrderStatus::OrderStatusRejected, "REJECTED"),
        (ProtoOaOrderStatus::OrderStatusExpired, "EXPIRED"),
        (ProtoOaOrderStatus::OrderStatusCancelled, "CANCELED"),
    ] {
        let order = ProtoOaOrder {
            trade_data: trade_data(1, 100, ProtoOaTradeSide::Buy),
            order_status: status as i32,
            ..Default::default()
        };
        let r = parse_orders(&[order], &symbols());
        assert_eq!(r[0].status, want, "{status:?}");
    }
}

#[test]
fn parse_orders_resolves_symbol_name_from_the_symbol_map() {
    let order = ProtoOaOrder {
        trade_data: trade_data(2, 100, ProtoOaTradeSide::Buy),
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols());
    assert_eq!(r[0].symbol, "GBPUSD");
}

// --- parse_positions -----------------------------------------------------------------------

#[test]
fn parses_long_position() {
    let pos = ProtoOaPosition {
        position_id: 1,
        trade_data: trade_data(1, 500, ProtoOaTradeSide::Buy),
        price: Some(1.10),
        utc_last_update_timestamp: Some(123),
        ..Default::default()
    };
    let r = parse_positions(&[pos], &symbols());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "EURUSD");
    assert_eq!(r[0].position_side, PositionSide::Long);
    assert_eq!(r[0].qty, 5.0);
    assert_eq!(r[0].avg_px, 1.10);
    assert_eq!(r[0].ts, 123);
}

#[test]
fn parses_short_position_as_negative_qty() {
    let pos = ProtoOaPosition {
        position_id: 2,
        trade_data: trade_data(1, 300, ProtoOaTradeSide::Sell),
        ..Default::default()
    };
    let r = parse_positions(&[pos], &symbols());
    assert_eq!(r[0].position_side, PositionSide::Short);
    assert_eq!(r[0].qty, -3.0, "unsigned wire volume + SELL -> negative");
}

// --- parse_raw_positions (close_all input) -------------------------------------------------

#[test]
fn parse_raw_positions_keeps_position_id_side_and_volume() {
    // The whole point vs. parse_positions: the raw numeric `position_id` (which the
    // `PositionStatusReport` output has no field for) survives — it is what `close_all` closes by.
    let long = ProtoOaPosition {
        position_id: 700_001,
        trade_data: trade_data(1, 100_000, ProtoOaTradeSide::Buy),
        ..Default::default()
    };
    let short = ProtoOaPosition {
        position_id: 700_002,
        trade_data: trade_data(1, 50_000, ProtoOaTradeSide::Sell),
        ..Default::default()
    };
    let r = parse_raw_positions(&[long, short], &symbols());
    assert_eq!(
        r,
        vec![
            RawPosition { position_id: 700_001, symbol: "EURUSD".into(), side: 1, volume: 100_000 },
            RawPosition { position_id: 700_002, symbol: "EURUSD".into(), side: -1, volume: 50_000 },
        ]
    );
}

#[test]
fn parse_raw_positions_drops_zero_volume_placeholder() {
    // A CREATED placeholder (a pending order's empty position) has zero volume — nothing to flatten.
    let empty = ProtoOaPosition {
        position_id: 700_003,
        trade_data: trade_data(1, 0, ProtoOaTradeSide::Buy),
        ..Default::default()
    };
    assert!(parse_raw_positions(&[empty], &symbols()).is_empty());
}

#[test]
fn parse_raw_positions_resolves_symbol_name() {
    let pos = ProtoOaPosition {
        position_id: 700_004,
        trade_data: trade_data(2, 10_000, ProtoOaTradeSide::Buy),
        ..Default::default()
    };
    assert_eq!(parse_raw_positions(&[pos], &symbols())[0].symbol, "GBPUSD");
}

// --- parse_fills ---------------------------------------------------------------------------

fn deal(symbol_id: i64, status: vike_ctrader::proto::ProtoOaDealStatus) -> ProtoOaDeal {
    ProtoOaDeal {
        deal_id: 900,
        order_id: 42,
        symbol_id,
        volume: 1000,
        filled_volume: 1000,
        trade_side: ProtoOaTradeSide::Buy as i32,
        deal_status: status as i32,
        execution_price: Some(1.095),
        execution_timestamp: 1_700_000_000_000,
        commission: Some(-3),
        money_digits: Some(2),
        ..Default::default()
    }
}

#[test]
fn parses_filled_deal_every_field() {
    let d = deal(1, vike_ctrader::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], 1, "EURUSD", 2);
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "ctrader");
    assert_eq!(f.symbol, "EURUSD");
    assert_eq!(f.trade_id.as_str(), "900");
    assert_eq!(f.venue_order_id.as_str(), "42");
    assert_eq!(f.client_order_id, None, "ProtoOADeal carries no client-order-id field");
    assert_eq!(f.side, 1, "Buy -> +1");
    assert_eq!(f.last_qty, 10.0);
    assert_eq!(f.last_px, 1.095);
    assert_eq!(f.commission, 0.03, "-3 raw / 10^2, negated -> +0.03 cost");
    assert_eq!(f.commission_asset, "");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.ts, 1_700_000_000_000);
}

#[test]
fn partially_filled_deal_is_kept() {
    let d = deal(1, vike_ctrader::proto::ProtoOaDealStatus::PartiallyFilled);
    let r = parse_fills(&[d], 1, "EURUSD", 2);
    assert_eq!(r.len(), 1);
}

#[test]
fn rejected_deals_are_dropped() {
    for status in [
        vike_ctrader::proto::ProtoOaDealStatus::Rejected,
        vike_ctrader::proto::ProtoOaDealStatus::InternallyRejected,
        vike_ctrader::proto::ProtoOaDealStatus::Error,
    ] {
        let d = deal(1, status);
        let r = parse_fills(&[d], 1, "EURUSD", 2);
        assert!(r.is_empty(), "{status:?} must never surface as a fill");
    }
}

#[test]
fn parse_fills_filters_out_other_symbol_ids() {
    let d = deal(2, vike_ctrader::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], 1, "EURUSD", 2);
    assert!(r.is_empty());
}

#[test]
fn deal_own_money_digits_takes_precedence_over_account_level() {
    // deal.money_digits = Some(2) above; the account-level fallback (passed as the last param
    // here) is a DIFFERENT value (4) — the deal's own field must win.
    let d = deal(1, vike_ctrader::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], 1, "EURUSD", 4);
    assert_eq!(r[0].commission, 0.03, "deal.money_digits=2 wins over the account-level 4");
}

#[test]
fn falls_back_to_account_level_money_digits_when_deal_omits_it() {
    let mut d = deal(1, vike_ctrader::proto::ProtoOaDealStatus::Filled);
    d.money_digits = None;
    let r = parse_fills(&[d], 1, "EURUSD", 2);
    assert_eq!(r[0].commission, 0.03);
}
