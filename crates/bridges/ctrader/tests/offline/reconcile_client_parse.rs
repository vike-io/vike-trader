//! Pure parse tests for the cTrader `ReconcileClient` report parsers (ReconFactory seam, wave-2 task
//! 6) — NO network: builds the already-decoded prost structs directly (cTrader is protobuf, not
//! REST/JSON, so there is no wire body to feed a JSON parser) and feeds them straight to
//! `vike_ctrader::reconcile`'s pure `parse_*` functions. Asserts the tricky bits the module doc
//! calls out: unsigned-volume-plus-tradeSide -> signed qty, the order-client-id fallback to
//! `tradeData.label`, fill-progress-derived `PARTIALLY_FILLED`, and the commission
//! descale-and-negate. The one wire body is a deal-list frame carrying the deal's `label`, which the
//! generated types cannot say (`deal_order_client_ids`).

use std::collections::HashMap;

use prost::Message;
use vike_ctrader::execution::positions::{PositionsById, reconcile_rows};
use vike_ctrader::instrument::symbols::SymbolMap;
use vike_ctrader::reconcile::{
    RawPosition, deal_order_client_ids, parse_balance, parse_fills, parse_orders,
    parse_raw_positions, position_reports,
};
use vike_ctrader::test_support::deal_list_frame;
use vike_ctrader::transport::proto::{
    ProtoOaDeal, ProtoOaDealListRes, ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType,
    ProtoOaPosition, ProtoOaPositionStatus, ProtoOaTradeData, ProtoOaTradeSide,
};
use vike_model::events::{LiquiditySide, PositionSide};

/// One of our v2 ids: a strategy's (`S`) close-long stop, strategy `00A3`, account `07`.
const V2: &str = "S00A307k3x9q2m7d1p4CLSL";
/// One of our ids minted before v2 (session hex, then the sequence).
const V1: &str = "deadbeef17";

fn symbols() -> SymbolMap {
    use vike_ctrader::transport::proto::{ProtoOaLightSymbol, ProtoOaSymbol};
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

/// An ACCEPTED order of `trade_data`, every other field default.
fn accepted(trade_data: ProtoOaTradeData) -> ProtoOaOrder {
    ProtoOaOrder {
        trade_data,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        ..Default::default()
    }
}

// --- parse_orders --------------------------------------------------------------------------

#[test]
fn parses_resting_order_every_field() {
    let order = ProtoOaOrder {
        order_id: 42,
        trade_data: ProtoOaTradeData {
            // A label reads as an order client id only when it is one of our ids (`label_order_client_id`): a v2 id here.
            label: Some(V2.to_string()),
            ..trade_data(1, 1000, ProtoOaTradeSide::Buy)
        },
        order_type: ProtoOaOrderType::Limit as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        execution_price: Some(1.0950),
        executed_volume: Some(0),
        utc_last_update_timestamp: Some(1_700_000_000_000),
        ..Default::default()
    };
    let r = parse_orders(&[order], &symbols(), None);
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "ctrader");
    assert_eq!(o.symbol, "EURUSD");
    assert_eq!(o.venue_order_id.as_str(), "42");
    assert_eq!(o.order_client_id.as_deref(), Some(V2), "falls back to trade_data.label");
    assert_eq!(o.side, 1, "Buy -> +1");
    assert_eq!(o.order_type, vike_model::OrderType::Limit);
    assert_eq!(o.qty, 10.0, "1000 cents -> 10.00 units");
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.average_price, 1.0950);
    assert_eq!(o.status, vike_model::OrderStatus::Accepted);
    assert_eq!(o.timestamp, 1_700_000_000_000);
}

#[test]
fn order_client_id_field_takes_precedence_over_label() {
    let order = ProtoOaOrder {
        client_order_id: Some("dedicated-order-client-id".to_string()),
        ..accepted(ProtoOaTradeData {
            label: Some("fallback-label".to_string()),
            ..trade_data(1, 100, ProtoOaTradeSide::Sell)
        })
    };
    let r = parse_orders(&[order], &symbols(), None);
    assert_eq!(r[0].order_client_id.as_deref(), Some("dedicated-order-client-id"));
    assert_eq!(r[0].side, -1, "Sell -> -1");
}

#[test]
fn absent_order_client_id_and_label_normalizes_to_none() {
    let order = accepted(trade_data(1, 100, ProtoOaTradeSide::Buy));
    let r = parse_orders(&[order], &symbols(), None);
    assert_eq!(r[0].order_client_id, None);
}

#[test]
fn accepted_with_partial_fill_progress_derives_partially_filled() {
    let order = ProtoOaOrder {
        executed_volume: Some(400),
        ..accepted(trade_data(1, 1000, ProtoOaTradeSide::Buy))
    };
    let r = parse_orders(&[order], &symbols(), None);
    assert_eq!(r[0].status, vike_model::OrderStatus::PartiallyFilled);
    assert_eq!(r[0].filled_qty, 4.0);
}

/// B-16 (owner ruling): an ACCEPTED (resting) row whose executed volume reaches its whole volume still
/// rests, so it is `PartiallyFilled`, never `Accepted`: a fill was made, and the end is the venue's
/// `FILLED` status to say.
#[test]
fn accepted_with_whole_fill_progress_is_partially_filled() {
    for executed in [1000, 1200] {
        let order = ProtoOaOrder {
            executed_volume: Some(executed),
            ..accepted(trade_data(1, 1000, ProtoOaTradeSide::Buy))
        };
        let r = parse_orders(&[order], &symbols(), None);
        assert_eq!(r[0].status, vike_model::OrderStatus::PartiallyFilled, "{executed}");
    }
}

#[test]
fn every_terminal_status_normalizes() {
    for (status, want) in [
        (ProtoOaOrderStatus::OrderStatusFilled, vike_model::OrderStatus::Filled),
        (ProtoOaOrderStatus::OrderStatusRejected, vike_model::OrderStatus::Rejected),
        (ProtoOaOrderStatus::OrderStatusExpired, vike_model::OrderStatus::Expired),
        (ProtoOaOrderStatus::OrderStatusCancelled, vike_model::OrderStatus::Canceled),
    ] {
        let order = ProtoOaOrder {
            trade_data: trade_data(1, 100, ProtoOaTradeSide::Buy),
            order_status: status as i32,
            ..Default::default()
        };
        let r = parse_orders(&[order], &symbols(), None);
        assert_eq!(r[0].status, want, "{status:?}");
    }
}

/// **A resting order reports the terms `order_to_new_order` sent**, so a report-rebuilt order keeps
/// them: a LIMIT's `limitPrice`, a STOP's `stopPrice` as its trigger, and a STOP_LIMIT's `stopPrice`
/// as its trigger with its limit rebuilt from `slippageInPoints` in the order's direction. A term the
/// row does not carry is `None`, never zero; nothing on this venue trails.
#[test]
fn a_resting_order_reports_the_terms_it_was_sent_with() {
    use ProtoOaOrderType::{Limit, Market, Stop, StopLimit};
    use ProtoOaTradeSide::{Buy, Sell};
    let terms = |order_type: ProtoOaOrderType,
                 side: ProtoOaTradeSide,
                 limit: Option<f64>,
                 stop: Option<f64>,
                 slippage: Option<i64>| {
        let order = ProtoOaOrder {
            order_type: order_type as i32,
            limit_price: limit,
            stop_price: stop,
            slippage_in_points: slippage,
            ..accepted(trade_data(1, 100_000, side))
        };
        let r = &parse_orders(&[order], &symbols(), None)[0];
        (r.price, r.trigger_price, r.trail, r.trail_percent)
    };
    assert_eq!(terms(Limit, Buy, Some(1.095), None, None), (Some(1.095), None, None, None));
    assert_eq!(terms(Stop, Sell, None, Some(1.0412), None), (None, Some(1.0412), None, None));
    assert_eq!(
        terms(StopLimit, Buy, None, Some(1.1), Some(3)),
        (Some(1.10003), Some(1.1), None, None)
    );
    assert_eq!(
        terms(StopLimit, Sell, None, Some(1.1), Some(5)),
        (Some(1.09995), Some(1.1), None, None)
    );
    // no slippage on the row: the trigger alone, never a limit guessed at the stop price
    assert_eq!(terms(StopLimit, Buy, None, Some(1.1), None), (None, Some(1.1), None, None));
    assert_eq!(terms(Market, Buy, None, None, None), (None, None, None, None));
    // an unset price the venue writes as zero is not a term
    assert_eq!(terms(Limit, Buy, Some(0.0), None, None), (None, None, None, None));
}

#[test]
fn parse_orders_resolves_symbol_name_from_the_symbol_map() {
    let order = accepted(trade_data(2, 100, ProtoOaTradeSide::Buy));
    let r = parse_orders(&[order], &symbols(), None);
    assert_eq!(r[0].symbol, "GBPUSD");
}

// --- position_reports ----------------------------------------------------------------------

/// One OPEN venue position of EURUSD (symbol id 1), as cTrader's reconcile lists it.
fn leg(
    position_id: i64,
    volume: i64,
    side: ProtoOaTradeSide,
    price: Option<f64>,
    ts: i64,
) -> ProtoOaPosition {
    ProtoOaPosition {
        position_id,
        trade_data: trade_data(1, volume, side),
        position_status: ProtoOaPositionStatus::PositionStatusOpen as i32,
        price,
        utc_last_update_timestamp: Some(ts),
        ..Default::default()
    }
}

/// [`leg`] under another `positionStatus`.
fn with_status(position: ProtoOaPosition, status: ProtoOaPositionStatus) -> ProtoOaPosition {
    ProtoOaPosition { position_status: status as i32, ..position }
}

fn netted(legs: &[ProtoOaPosition]) -> Vec<vike_model::PositionStatusReport> {
    position_reports(legs, 1, "EURUSD").expect("every row is classified")
}

#[test]
fn a_long_position_is_one_both_row_at_its_entry_price_and_stamp() {
    let r = netted(&[leg(1, 500, ProtoOaTradeSide::Buy, Some(1.10), 123)]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, "EURUSD");
    assert_eq!(r[0].position_side, PositionSide::Both);
    assert_eq!(r[0].qty, 5.0);
    assert_eq!(r[0].average_price, 1.10);
    assert_eq!(r[0].timestamp, 123);
}

#[test]
fn a_short_position_is_a_negative_quantity() {
    let r = netted(&[leg(2, 300, ProtoOaTradeSide::Sell, Some(1.2), 1)]);
    assert_eq!(r[0].position_side, PositionSide::Both);
    assert_eq!(r[0].qty, -3.0, "unsigned wire volume + SELL -> negative");
}

#[test]
fn position_reports_of_one_direction_is_one_both_row_with_a_weighted_price() {
    use ProtoOaTradeSide::Buy;
    let r = netted(&[
        leg(1, 100, Buy, Some(1.10), 5),
        leg(2, 300, Buy, Some(1.20), 9),
        // the empty position a pending order makes
        with_status(leg(3, 0, Buy, Some(9.99), 99), ProtoOaPositionStatus::PositionStatusCreated),
    ]);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(
        (r[0].symbol.as_str(), r[0].position_side, r[0].qty),
        ("EURUSD", PositionSide::Both, 4.0)
    );
    assert!((r[0].average_price - 1.175).abs() < 1e-12, "{}", r[0].average_price);
    assert_eq!(r[0].timestamp, 9, "the newest leg's");
}

#[test]
fn position_reports_of_shorts_carries_the_sign_in_quantity() {
    let r = netted(&[
        leg(1, 100, ProtoOaTradeSide::Sell, Some(1.0), 1),
        leg(2, 100, ProtoOaTradeSide::Sell, Some(2.0), 2),
    ]);
    assert_eq!((r[0].position_side, r[0].qty, r[0].average_price), (PositionSide::Both, -2.0, 1.5));
}

/// A leg with no usable price makes the merged price "unknown" (`0.0`), never a blend with a zero:
/// `resolve` refuses to book a position at an average price of zero.
#[test]
fn position_reports_with_an_unpriced_leg_has_no_price() {
    use ProtoOaTradeSide::Buy;
    let r = netted(&[leg(1, 100, Buy, Some(1.10), 1), leg(2, 100, Buy, None, 2)]);
    assert_eq!((r[0].qty, r[0].average_price), (2.0, 0.0));
}

/// No exposure (nothing listed, or only the empty position a pending order makes) is the flat row cTrader's
/// omission of a closed position calls for, and only the symbol asked about is read.
#[test]
fn position_reports_with_no_exposure_is_the_flat_row() {
    let flat = vec![vike_model::PositionStatusReport::flat("ctrader", "EURUSD")];
    assert_eq!(netted(&[]), flat);
    let pending = with_status(
        leg(1, 0, ProtoOaTradeSide::Buy, None, 1),
        ProtoOaPositionStatus::PositionStatusCreated,
    );
    assert_eq!(netted(&[pending]), flat);
    let other = ProtoOaPosition {
        trade_data: trade_data(2, 100, ProtoOaTradeSide::Buy),
        ..leg(1, 100, ProtoOaTradeSide::Buy, Some(1.2), 1)
    };
    assert_eq!(netted(&[other]), flat, "a GBPUSD position is not EURUSD's");
}

/// Both directions stay two rows, each summed: the book has no key for a pair, and the rows sum to the net.
#[test]
fn position_reports_of_both_directions_is_a_long_row_and_a_short_row() {
    use ProtoOaTradeSide::{Buy, Sell};
    let r = netted(&[
        leg(1, 100, Buy, Some(1.10), 1),
        leg(2, 200, Buy, Some(1.30), 2),
        leg(3, 100, Sell, Some(1.25), 3),
    ]);
    let rows: Vec<_> = r.iter().map(|p| (p.position_side, p.qty, p.timestamp)).collect();
    assert_eq!(rows, [(PositionSide::Long, 3.0, 2), (PositionSide::Short, -1.0, 3)]);
    assert!((r[0].average_price - (1.10 + 2.0 * 1.30) / 3.0).abs() < 1e-12);
    assert_eq!(r.iter().map(|p| p.qty).sum::<f64>(), 2.0);
}

/// **One reading of a reconcile row, three uses** (A15): the exec book's seed (`reconcile_rows`), the report and
/// the flatten list agree that a CLOSED row carries no exposure, whatever volume it still states. The report
/// used to keep every row and the flatten list every row with a volume, so such a row was a position to the
/// report and a flatten target while the book, rightly, did not hold it.
#[test]
fn a_closed_row_with_a_volume_is_exposure_to_no_reading() {
    use ProtoOaPositionStatus::PositionStatusClosed;
    let closed =
        with_status(leg(9, 100, ProtoOaTradeSide::Buy, Some(1.2), 4), PositionStatusClosed);
    let open = leg(1, 200, ProtoOaTradeSide::Buy, Some(1.1), 3);
    let rows = [open, closed];
    let r = netted(&rows);
    assert_eq!((r.len(), r[0].qty, r[0].average_price), (1, 2.0, 1.1), "{r:?}");
    let raw = parse_raw_positions(&rows, &symbols());
    assert_eq!(raw.iter().map(|p| p.position_id).collect::<Vec<_>>(), [1], "{raw:?}");
    let (tracked, unreadable) = reconcile_rows(&rows);
    assert_eq!((tracked.iter().map(|(id, _)| *id).collect::<Vec<_>>(), unreadable), (vec![1], 0));
}

/// A row this build cannot classify (an ERROR position, an OPEN one with no volume) is a hole, never an absence:
/// the symbol's report fails in words (the pass claims nothing about it, as the exec book stops calling itself
/// evidence), another symbol's report is untouched, and the hole is no flatten target.
#[test]
fn a_row_this_build_cannot_classify_fails_its_symbols_report() {
    use ProtoOaPositionStatus::PositionStatusError;
    let error = with_status(leg(5, 100, ProtoOaTradeSide::Buy, Some(1.2), 1), PositionStatusError);
    let empty_open = leg(6, 0, ProtoOaTradeSide::Buy, Some(1.2), 1);
    for hole in [error, empty_open] {
        let rows = [leg(1, 100, ProtoOaTradeSide::Buy, Some(1.1), 1), hole];
        let why = position_reports(&rows, 1, "EURUSD").expect_err("a hole is no absence");
        assert!(why.contains("EURUSD") && why.contains("cannot"), "{why}");
        assert_eq!(
            position_reports(&rows, 2, "GBPUSD").expect("another symbol"),
            vec![vike_model::PositionStatusReport::flat("ctrader", "GBPUSD")]
        );
        let raw = parse_raw_positions(&rows, &symbols());
        assert_eq!(raw.iter().map(|p| p.position_id).collect::<Vec<_>>(), [1], "{raw:?}");
    }
}

// --- parse_raw_positions (close_all input) -------------------------------------------------

#[test]
fn parse_raw_positions_keeps_position_id_side_and_volume() {
    // The whole point vs. position_reports: the raw numeric `position_id` (which the
    // `PositionStatusReport` output has no field for) survives — it is what `close_all` closes by.
    let long = leg(700_001, 100_000, ProtoOaTradeSide::Buy, None, 0);
    let short = leg(700_002, 50_000, ProtoOaTradeSide::Sell, None, 0);
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
    let empty = with_status(
        leg(700_003, 0, ProtoOaTradeSide::Buy, None, 0),
        ProtoOaPositionStatus::PositionStatusCreated,
    );
    assert!(parse_raw_positions(&[empty], &symbols()).is_empty());
}

#[test]
fn parse_raw_positions_resolves_symbol_name() {
    let pos = ProtoOaPosition {
        trade_data: trade_data(2, 10_000, ProtoOaTradeSide::Buy),
        ..leg(700_004, 0, ProtoOaTradeSide::Buy, None, 0)
    };
    assert_eq!(parse_raw_positions(&[pos], &symbols())[0].symbol, "GBPUSD");
}

// --- parse_fills ---------------------------------------------------------------------------

fn deal(symbol_id: i64, status: vike_ctrader::transport::proto::ProtoOaDealStatus) -> ProtoOaDeal {
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
    let d = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "ctrader");
    assert_eq!(f.symbol, "EURUSD");
    assert_eq!(f.trade_id.as_str(), "900");
    assert_eq!(f.venue_order_id.as_str(), "42");
    assert_eq!(f.order_client_id, None, "a deal with no label carries no order client id");
    assert_eq!(f.side, 1, "Buy -> +1");
    assert_eq!(f.last_qty, 10.0);
    assert_eq!(f.last_price, 1.095);
    assert_eq!(f.commission, 0.03, "-3 raw / 10^2, negated -> +0.03 cost");
    assert_eq!(f.commission_asset, "");
    assert_eq!(f.liquidity_side, LiquiditySide::Unknown);
    assert_eq!(f.timestamp, 1_700_000_000_000);
}

/// A reconcile fill of a foreign-quoted CLOSE carries the same commission the live fill does: the deal's own
/// plus the venue's conversion fee (`pnlConversionFee`, a cost whichever sign it comes in).
#[test]
fn a_closing_deals_conversion_fee_is_part_of_the_reported_commission() {
    let mut d = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    d.close_position_detail = Some(vike_ctrader::transport::proto::ProtoOaClosePositionDetail {
        pnl_conversion_fee: Some(-7),
        money_digits: Some(2),
        ..Default::default()
    });
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
    assert!((r[0].commission - 0.10).abs() < 1e-12, "{}", r[0].commission);
    assert_eq!(r[0].commission_asset, "", "the account's own currency");
}

#[test]
fn partially_filled_deal_is_kept() {
    let d = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::PartiallyFilled);
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
    assert_eq!(r.len(), 1);
}

#[test]
fn rejected_deals_are_dropped() {
    for status in [
        vike_ctrader::transport::proto::ProtoOaDealStatus::Rejected,
        vike_ctrader::transport::proto::ProtoOaDealStatus::InternallyRejected,
        vike_ctrader::transport::proto::ProtoOaDealStatus::Error,
    ] {
        let d = deal(1, status);
        let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
        assert!(r.is_empty(), "{status:?} must never surface as a fill");
    }
}

#[test]
fn parse_fills_filters_out_other_symbol_ids() {
    let d = deal(2, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
    assert!(r.is_empty());
}

#[test]
fn deal_own_money_digits_takes_precedence_over_account_level() {
    // deal.money_digits = Some(2) above; the account-level fallback (passed as the last param
    // here) is a DIFFERENT value (4) — the deal's own field must win.
    let d = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 4);
    assert_eq!(r[0].commission, 0.03, "deal.money_digits=2 wins over the account-level 4");
}

#[test]
fn falls_back_to_account_level_money_digits_when_deal_omits_it() {
    let mut d = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    d.money_digits = None;
    let r = parse_fills(&[d], &HashMap::new(), 1, "EURUSD", 2);
    assert_eq!(r[0].commission, 0.03);
}

// --- the deal's label: our order client id (`deal_order_client_ids`) ------------------------------------------------

/// Our deal reads back under the order client id its order was placed with; a deal with no label, or an empty one, stays `None`
/// (foreign). The generated decoder skips the label and still reads every deal whole.
#[test]
fn a_deal_list_frame_reads_our_order_client_id_from_the_label_and_a_foreign_deal_stays_none() {
    let ours = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let foreign = ProtoOaDeal { deal_id: 901, ..ours };
    let blank = ProtoOaDeal { deal_id: 902, ..ours };
    let frame = deal_list_frame(&[
        (ours, Some("S00C10Hp0o9i8u7y6t5CLSL")),
        (foreign, None),
        (blank, Some("")),
    ]);
    let res = ProtoOaDealListRes::decode(frame.as_slice()).expect("the generated decoder reads it");
    assert_eq!(res.deal, [ours, foreign, blank], "the label is skipped, every deal read whole");
    let order_client_ids = deal_order_client_ids(&frame, None);
    assert_eq!(order_client_ids, HashMap::from([(900, "S00C10Hp0o9i8u7y6t5CLSL".to_string())]));
    let got: Vec<_> = parse_fills(&res.deal, &order_client_ids, 1, "EURUSD", 2)
        .into_iter()
        .map(|f| (f.trade_id.as_str().to_string(), f.order_client_id))
        .collect();
    assert_eq!(
        got,
        [
            ("900".to_string(), Some("S00C10Hp0o9i8u7y6t5CLSL".to_string())),
            ("901".to_string(), None),
            ("902".to_string(), None),
        ]
    );
}

/// **A label is ours when the bridge KNOWS it or it reads as one of our v2 ids** (`label_order_client_id`), on a deal and on an
/// order that carries the label alone (no `clientOrderId`). A v2 id is ours to any book. "MyStop" is ours to the book of
/// the bridge that placed it and foreign to one that did not (the same text typed in the cTrader app); so is an id
/// minted before v2, ours once a restore taught it. A lower-case word of that shape ("hedge1"), "my hedge", an `EXT-…`
/// text and an id-shaped text under a letter no source has (`B`) are foreign; no label is `None`.
#[test]
fn a_label_is_ours_when_the_bridge_knows_it_or_it_reads_as_a_v2_id() {
    // (label, ours by its spelling, ours once the bridge knows it)
    let labels = [
        (Some(V2), true, true),
        (Some("MyStop"), false, true),
        (Some(V1), false, true),
        (Some("hedge1"), false, false),
        (Some("my hedge"), false, false),
        (Some("EXT-ctrader-42"), false, false),
        (Some("B00A307k3x9q2m7d1p4CSTP9"), false, false),
        (None, false, false),
    ];
    let mut placed = PositionsById::default();
    placed.know_order_client_id("MyStop");
    placed.know_order_client_id(V1);
    let elsewhere = PositionsById::default();
    let deals: Vec<_> = (900..)
        .zip(labels)
        .map(|(deal_id, (label, ..))| {
            (
                ProtoOaDeal {
                    deal_id,
                    ..deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled)
                },
                label,
            )
        })
        .collect();
    let frame = deal_list_frame(&deals);
    let res = ProtoOaDealListRes::decode(frame.as_slice()).expect("the generated decoder reads it");
    let orders: Vec<_> = labels
        .iter()
        .map(|(label, ..)| {
            accepted(ProtoOaTradeData {
                label: label.map(str::to_string),
                ..trade_data(1, 100, ProtoOaTradeSide::Buy)
            })
        })
        .collect();
    for (book, knows) in [(&placed, true), (&elsewhere, false)] {
        let fills =
            parse_fills(&res.deal, &deal_order_client_ids(&frame, Some(book)), 1, "EURUSD", 2);
        let reports = parse_orders(&orders, &symbols(), Some(book));
        assert_eq!((fills.len(), reports.len()), (labels.len(), labels.len()));
        for ((label, spelled, known), (fill, order)) in
            labels.iter().zip(fills.iter().zip(&reports))
        {
            let want = label.filter(|_| *spelled || (knows && *known)).map(str::to_string);
            assert_eq!(fill.order_client_id, want, "a deal labelled {label:?}, known={knows}");
            assert_eq!(order.order_client_id, want, "an order labelled {label:?}, known={knows}");
        }
    }
}

/// **The bridge KNOWS what it placed and what a restore taught it**: `ExecutionClientCtrader::submit` and `adopt_restored` teach
/// the book the recon client reads, so a deal labelled "MyStop" (submitted through it) or with an id minted before v2
/// (taught) reads as ours. A v2 id is not stored (its spelling says it) and reads as ours all the same.
#[test]
fn what_the_execution_placed_or_a_restore_taught_reads_as_ours() {
    use std::sync::{Arc, Mutex, mpsc};
    use vike_ctrader::execution::ExecutionClientCtrader;
    use vike_ctrader::execution::positions::CloseTracker;
    use vike_ctrader::transport::connection::ConnectionShared;
    use vike_execution::ExecutionClient;
    let book = Arc::new(Mutex::new(PositionsById::default()));
    let (tx, _commands) = mpsc::channel();
    let shared = ConnectionShared {
        tx,
        symbols: Arc::new(symbols()),
        ctid: 99,
        orders: Arc::default(),
        positions: Arc::clone(&book),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    let (events, _rx) = vike_execution::event_channel(64);
    let halt = std::env::temp_dir().join(format!("vike-ctrader-known-{}", std::process::id()));
    let mut exec =
        ExecutionClientCtrader::from_shared(shared, events).with_halt_path(halt.join("HALT"));
    for order_client_id in ["MyStop", V2] {
        exec.submit(&vike_model::OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "ctrader".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(1.05),
            ..Default::default()
        });
    }
    exec.adopt_restored(&[vike_execution::RestoredOrder::new(V1, "EURUSD")]);
    let book = book.lock().expect("the book");
    assert!(book.knows_order_client_id("MyStop") && book.knows_order_client_id(V1));
    assert!(!book.knows_order_client_id(V2), "a v2 id is ours by its spelling and is not stored");
    let filled = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let frame = deal_list_frame(&[
        (filled, Some("MyStop")),
        (ProtoOaDeal { deal_id: 901, ..filled }, Some(V1)),
        (ProtoOaDeal { deal_id: 902, ..filled }, Some(V2)),
        (ProtoOaDeal { deal_id: 903, ..filled }, Some("hedge1")),
    ]);
    assert_eq!(
        deal_order_client_ids(&frame, Some(&book)),
        HashMap::from([(900, "MyStop".to_string()), (901, V1.to_string()), (902, V2.to_string())])
    );
}

/// The known order client ids are bounded ([`vike_ctrader::execution::positions::KNOWN_ORDER_CLIENT_IDS_CAP`]): past the cap the oldest is forgotten.
#[test]
fn the_known_order_client_ids_forget_the_oldest_past_the_cap() {
    use vike_ctrader::execution::positions::KNOWN_ORDER_CLIENT_IDS_CAP;
    let mut book = PositionsById::default();
    for i in 0..=KNOWN_ORDER_CLIENT_IDS_CAP {
        book.know_order_client_id(&format!("op{i}"));
    }
    assert!(!book.knows_order_client_id("op0"), "the oldest went first");
    assert!(
        book.knows_order_client_id("op1")
            && book.knows_order_client_id(&format!("op{KNOWN_ORDER_CLIENT_IDS_CAP}"))
    );
}

/// An exit of `parent` split over two long positions (`vike_ctrader::execution::exit_legs`), in a book of its own, and its legs.
fn book_with_legs(parent: &str) -> (PositionsById, Vec<String>) {
    use vike_bridge_core::exit::ExitKind;
    use vike_ctrader::execution::positions::PositionTracked;
    use vike_ctrader::instrument::symbols::VolumeGrid;
    let mut book = PositionsById::default();
    let mut free = Vec::new();
    for (open_timestamp, (id, volume)) in [(5, 100_000), (6, 200_000)].into_iter().enumerate() {
        let p = PositionTracked {
            symbol_id: 1,
            side: 1,
            volume,
            open_timestamp: open_timestamp as i64,
        };
        book.upsert(id, p);
        free.push((id, p));
    }
    let grid = VolumeGrid {
        lot_size: 10_000_000,
        min_volume: 100_000,
        step_volume: 100_000,
        max_volume: 0,
    };
    let legs =
        book.split_exit_among(parent, -1, ExitKind::Stop, 250_000, &free, grid).expect("legs");
    let order_client_ids = legs.iter().map(|l| l.order_client_id.clone()).collect();
    (book, order_client_ids)
}

/// **A leg of a split exit reports its exit** (`<parent>L<n>`, `vike_ctrader::execution::exit_legs`): by the exec's table while it
/// holds the group, which comes first (a parent that is no v2 id, an operator's "MyStop", is named by it alone); and,
/// once a gatewayhub restart emptied the table, by its spelling when `<parent>` reads as one of our v2 ids. A
/// leg-shaped label whose parent is no v2 id is foreign without the table.
#[test]
fn a_split_exits_leg_reports_its_exit_by_the_table_or_by_a_v2_parents_spelling() {
    let (_, legs) = book_with_legs(V2);
    assert_eq!(legs, [format!("{V2}L1"), format!("{V2}L2")]);
    let (named, own_legs) = book_with_legs("MyStop");
    let filled = deal(1, vike_ctrader::transport::proto::ProtoOaDealStatus::Filled);
    let frame = deal_list_frame(&[
        (filled, Some(&legs[0])),
        (ProtoOaDeal { deal_id: 901, ..filled }, Some(&own_legs[1])),
        (ProtoOaDeal { deal_id: 902, ..filled }, Some("hedgeL2")),
    ]);
    assert_eq!(
        deal_order_client_ids(&frame, Some(&named)),
        HashMap::from([(900, V2.to_string()), (901, "MyStop".to_string())]),
        "the table first, the v2 parent's spelling beside it"
    );
    let restarted = PositionsById::default();
    let after_restart = HashMap::from([(900, V2.to_string())]);
    assert_eq!(
        deal_order_client_ids(&frame, Some(&restarted)),
        after_restart,
        "the table is empty"
    );
    assert_eq!(deal_order_client_ids(&frame, None), after_restart, "no book at all");
}

/// **The resting legs of one split exit are ONE open-order row under the exit's order client id** (`parse_orders`): its size is
/// every leg's, its fills theirs summed at the size-weighted price, its terms the exit's, its venue order id the first
/// leg's and its `ts` the newest. A restore keeps one row per order client id, so one leg's row alone would understate what the
/// exit still covers. The same by the table and, after a gatewayhub restart, by the v2 spelling; a leg named by its
/// label alone joins too. Two orders that are no legs keep their own rows, even under one order client id, as before.
#[test]
fn the_resting_legs_of_one_exit_are_one_row_for_the_exit() {
    let (book, legs) = book_with_legs(V2);
    let stop = |order_id: i64,
                order_client_id: Option<&str>,
                label: &str,
                volume: i64,
                executed: i64,
                ts: i64| {
        ProtoOaOrder {
            order_id,
            client_order_id: order_client_id.map(str::to_string),
            order_type: ProtoOaOrderType::Stop as i32,
            stop_price: Some(1.08),
            executed_volume: Some(executed),
            execution_price: (executed > 0).then_some(1.0625),
            utc_last_update_timestamp: Some(ts),
            ..accepted(ProtoOaTradeData {
                label: Some(label.to_string()),
                ..trade_data(1, volume, ProtoOaTradeSide::Sell)
            })
        }
    };
    let orders = [
        stop(11, Some(&legs[0]), &legs[0], 100_000, 0, 1_000),
        stop(12, None, &legs[1], 150_000, 50_000, 2_000),
        stop(13, Some("dup"), "dup", 100_000, 0, 1_000),
        stop(14, Some("dup"), "dup", 100_000, 0, 1_000),
    ];
    use vike_model::OrderStatus::{Accepted, PartiallyFilled};
    for book in [Some(&book), None] {
        let got: Vec<_> = parse_orders(&orders, &symbols(), book)
            .into_iter()
            .map(|r| {
                let id = r.venue_order_id.as_str().to_string();
                (
                    r.order_client_id,
                    id,
                    r.qty,
                    r.filled_qty,
                    r.average_price,
                    r.status,
                    r.timestamp,
                    r.trigger_price,
                )
            })
            .collect();
        let row = |order_client_id: &str, id: &str, qty, filled, px, status, ts| {
            (
                Some(order_client_id.to_string()),
                id.to_string(),
                qty,
                filled,
                px,
                status,
                ts,
                Some(1.08),
            )
        };
        assert_eq!(
            got,
            [
                row(V2, "11", 2500.0, 500.0, 1.0625, PartiallyFilled, 2_000),
                row("dup", "13", 1000.0, 0.0, 0.0, Accepted, 1_000),
                row("dup", "14", 1000.0, 0.0, 0.0, Accepted, 1_000),
            ],
            "book: {}",
            book.is_some()
        );
    }
}

fn trader(
    balance: i64,
    money_digits: Option<u32>,
) -> vike_ctrader::transport::proto::ProtoOaTrader {
    vike_ctrader::transport::proto::ProtoOaTrader { balance, money_digits, ..Default::default() }
}

#[test]
fn parse_balance_descales_by_the_traders_own_money_digits() {
    assert_eq!(parse_balance(&trader(1_000_067, Some(2)), Some("USD")), Ok(10_000.67));
    assert_eq!(parse_balance(&trader(1_000_067, Some(4)), Some("usd")), Ok(100.0067));
    assert_eq!(parse_balance(&trader(-250, Some(2)), Some("USD")), Ok(-2.5), "a debit stays one");
}

/// The vendor documents no default for `moneyDigits`: an absent one is a refusal in words, never a scale
/// of 0 (which would read a true 2 a hundred times too large).
#[test]
fn parse_balance_refuses_an_absent_money_digits() {
    let refusal = parse_balance(&trader(1_000_067, None), Some("USD")).expect_err("no scale");
    assert!(refusal.contains("moneyDigits"), "{refusal}");
}

/// The balance is a bare number: only the currency the engine's ledger is guaranteed to share is adopted.
#[test]
fn parse_balance_refuses_a_deposit_it_cannot_call_usd() {
    let eur = parse_balance(&trader(1_000_067, Some(2)), Some("EUR")).expect_err("EUR");
    assert!(eur.contains("deposit currency is EUR"), "{eur}");
    let unknown = parse_balance(&trader(1_000_067, Some(2)), None).expect_err("unnamed");
    assert!(unknown.contains("deposit currency is unknown"), "{unknown}");
}
