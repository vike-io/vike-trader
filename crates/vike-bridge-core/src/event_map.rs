//! The venue-mapper tail every crypto `event_mapper` shares: the dual-publish contract. On a
//! TRADE/fill report a venue emits BOTH a bare `Event::Fill` (the `Account` folds it) AND the
//! wrapping `OrderPartiallyFilled`/`OrderFilled` (the FSM registry applies it). The `is_filled`
//! PREDICATE stays per-venue (Binance reads `X=="FILLED"`, OKX compares accFillSz vs sz, Bybit
//! reads leavesQty==0, Deribit reads `state=="filled"`); this helper owns only the
//! terminal-vs-partial branch and the `vec![Fill, wrap]` shape once the predicate is resolved.
//!
//! Also the arithmetic of a spot fill whose fee is paid in the base coin ([`spot_base_fee_net`], decisions 0215, 0218,
//! 0231). Whether a fee asset IS the base and how the quote is spelled stay each venue's mapper's.
//!
//! **A fill states an amount the engine can book, or it is refused** ([`fill_is_bookable`], [`fill_report_is_bookable`]):
//! a decoder that read an absent or unreadable price or quantity as `0.0` opened a position at price 0, because the
//! engine's fold drops only a NON-FINITE fill (`vike_execution`'s `fill_drop_reason`). [`terminal_events`] refuses one
//! for every mapper that ends in it; a mapper that sends a bare fill or a reconcile `FillReport` of its own asks the
//! predicate itself. A fill with no execution id needs no check here: `vike_model::TradeId` cannot be empty.

use vike_model::FillReport;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled, OrderPartiallyFilled};

/// A finite quantity above zero and a finite, non-zero price. A price may be NEGATIVE: a combo bought for a credit
/// (deribit's) fills below zero.
fn bookable_amounts(quantity: f64, price: f64) -> bool {
    quantity.is_finite() && quantity > 0.0 && price.is_finite() && price != 0.0
}

/// Whether `fill` states an amount the engine can book (the module doc); says the refusal at ERROR, naming the fill,
/// when it does not. The caller sends nothing for a refused fill, neither the bare fill nor its wrap.
pub fn fill_is_bookable(fill: &FillEvent) -> bool {
    let bookable = bookable_amounts(fill.last_qty, fill.last_price);
    if !bookable {
        tracing::error!(
            target: "vike_bridge_core::event_map",
            venue = %fill.venue,
            symbol = %fill.symbol,
            order_client_id = %fill.order_client_id,
            trade_id = %fill.trade_id,
            quantity = fill.last_qty,
            price = fill.last_price,
            "a fill states no positive quantity or no non-zero price (an absent or unreadable field) — \
             refusing it rather than booking it at 0"
        );
    }
    bookable
}

/// [`fill_is_bookable`] for a reconcile fill row: a refused row is skipped, as a row with no execution id is.
pub fn fill_report_is_bookable(report: &FillReport) -> bool {
    let bookable = bookable_amounts(report.last_qty, report.last_price);
    if !bookable {
        tracing::error!(
            target: "vike_bridge_core::event_map",
            venue = %report.venue,
            symbol = %report.symbol,
            trade_id = %report.trade_id,
            quantity = report.last_qty,
            price = report.last_price,
            "a fill row states no positive quantity or no non-zero price (an absent or unreadable field) — \
             skipping it rather than reporting a fill at 0"
        );
    }
    bookable
}

/// What a venue books for a base-coin fee outside the one case every venue measured or documents (a BUY charged in
/// the base). A venue states its own [`SpotFeeArms`], beside the comment that says what its wallet does there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotFeeArm {
    /// The fill stays as the venue stated it.
    AsStated,
    /// The commission is restated at the fill price; the quantity stays as stated.
    CommissionOnly,
    /// What the wallet does: the quantity moves against the fee, and the commission is restated.
    Net,
}

/// A venue's two [`SpotFeeArm`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotFeeArms {
    /// A SELL charged in the base coin.
    pub sell: SpotFeeArm,
    /// A fee that is no charge, on either side: negative (a rebate paid in the base coin) or zero.
    pub rebate: SpotFeeArm,
}

/// `(quantity the wallet moved, commission in the quote)` of a spot fill whose fee is paid in its base coin, or `None`
/// when the fill stays as the venue stated it. A BUY charged in the base always nets: it receives `qty - fee`, and the
/// commission is that coin at the fill price, so the engine's long is the wallet's coin and a sale of the whole of it
/// is not refused. Both together do not double count (decision 0215).
///
/// Refused on every venue, whatever its arms: a fee that is no finite number, and a fee (or rebate) not smaller than
/// the fill, which is a malformed row and not a fill to rewrite.
#[must_use]
pub fn spot_base_fee_net(
    side: i32,
    qty: f64,
    px: f64,
    fee: f64,
    arms: SpotFeeArms,
) -> Option<(f64, f64)> {
    if !(fee.is_finite() && fee.abs() < qty) {
        return None;
    }
    let arm = match (fee > 0.0, side > 0) {
        (true, true) => SpotFeeArm::Net,
        (true, false) => arms.sell,
        (false, _) => arms.rebate,
    };
    match arm {
        SpotFeeArm::AsStated => None,
        SpotFeeArm::CommissionOnly => Some((qty, fee * px)),
        SpotFeeArm::Net => Some((qty - f64::from(side.signum()) * fee, fee * px)),
    }
}

/// The accept that a fill wrap implies for an order the venue never answered, to be sent AHEAD of the wrap ([`None`] for
/// any event that is not a wrap).
///
/// The engine's order table has no `Submitted -> Filled` edge, so the wrap alone is refused on a `Submitted` order and
/// not kept: the order reads as if it never filled, and a bracket entry never releases its exits. The accept is true,
/// because a fill is the venue saying it holds the order. Two callers: hyperliquid's answer to a place, whose `filled`
/// row is the order's only word, and an order whose POST got no answer and whose re-query failed
/// (`crate::exec_actor::run_loop`'s sentinel, `crate::exec_actor::submit_fence`).
///
/// `names_the_order`: the wrap's trade id IS the venue's order id (hyperliquid's answer, whose `oid` it carries). Every
/// other venue's trade id is no order id, so the accept names none and the first accept that does (a confirm's) is
/// refused as a repeat: the venue's id is then missing from the order's row, and a cancel of what rests goes by the
/// client id on every venue of that path.
pub fn accept_implied_by_fill(event: &Event, names_the_order: bool) -> Option<Event> {
    let (order_client_id, fill, ts) = match event {
        Event::OrderFilled(w) => (&w.order_client_id, &w.fill, w.timestamp),
        Event::OrderPartiallyFilled(w) => (&w.order_client_id, &w.fill, w.timestamp),
        _ => return None,
    };
    Some(Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.clone(),
        venue_order_id: names_the_order.then(|| fill.trade_id.to_string().into()),
        timestamp: ts,
    }))
}

/// The dual-publish tail every crypto `event_mapper` shares: given a resolved `is_filled`
/// predicate, wrap `fill` in `OrderFilled` (terminal) or `OrderPartiallyFilled` (partial) and
/// return `[Event::Fill(fill), wrap]`. `order_client_id` is moved into the wrap; the caller has already
/// cloned it onto `fill.order_client_id`. A fill [`fill_is_bookable`] refuses returns NOTHING: the bare fill and its
/// wrap stand or fall together, or the order would end filled with nothing booked.
pub fn terminal_events(
    order_client_id: String,
    fill: FillEvent,
    ts: i64,
    is_filled: bool,
) -> Vec<Event> {
    if !fill_is_bookable(&fill) {
        return Vec::new();
    }
    let wrap = if is_filled {
        Event::OrderFilled(OrderFilled { order_client_id, fill: fill.clone(), timestamp: ts })
    } else {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id,
            fill: fill.clone(),
            timestamp: ts,
        })
    };
    vec![Event::Fill(fill), wrap]
}

/// **Every symbol `events` carry, respelled by `user`** (decision 0290: the engine books a venue row under its USER
/// name, and the venue's own spelling lives only inside its bridge): a fill's (bare or wrapped), a liquidation's, a
/// funding settlement's and a fee adjustment's. `user` answers the user name of a venue name; the bridge applies it
/// where events leave it, so the code that reads a fill's venue name (its class, its fee) runs before.
pub fn respell_symbols(events: &mut [Event], user: impl Fn(&str) -> String) {
    for event in events {
        let symbol = match event {
            Event::PositionLiquidated(e) => &mut e.symbol,
            Event::Funding(e) => &mut e.symbol,
            Event::FeeAdjustment(e) => &mut e.symbol,
            other => match other.fill_mut() {
                Some(fill) => &mut fill.symbol,
                None => continue,
            },
        };
        *symbol = user(symbol.as_str()).into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::LiquiditySide;

    fn sample_fill() -> FillEvent {
        FillEvent {
            trade_id: "T1".into(),
            order_client_id: "C1".to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 42,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        }
    }

    const AS_STATED: SpotFeeArms =
        SpotFeeArms { sell: SpotFeeArm::AsStated, rebate: SpotFeeArm::AsStated };
    const COMMISSION: SpotFeeArms =
        SpotFeeArms { sell: SpotFeeArm::CommissionOnly, rebate: SpotFeeArm::CommissionOnly };
    const NET: SpotFeeArms = SpotFeeArms { sell: SpotFeeArm::Net, rebate: SpotFeeArm::Net };

    fn bits(answer: Option<(f64, f64)>) -> Option<(u64, u64)> {
        answer.map(|(qty, commission)| (qty.to_bits(), commission.to_bits()))
    }

    /// The one case every venue measured: `qty - fee`, and the fee at the fill price, whatever the arms.
    #[test]
    fn a_buy_charged_in_the_base_nets_on_every_venue() {
        let want = bits(Some((10.0 - 0.0045, 0.0045 * 37.0)));
        for arms in [AS_STATED, COMMISSION, NET] {
            assert_eq!(bits(spot_base_fee_net(1, 10.0, 37.0, 0.0045, arms)), want);
        }
    }

    /// The arms, on a SELL charged in the base and on a rebate of either side.
    #[test]
    fn a_sell_and_a_rebate_follow_the_venues_arm() {
        for (side, fee, net) in
            [(-1, 0.0045, 10.0045), (1, -0.0045, 10.0045), (-1, -0.0045, 9.9955)]
        {
            assert_eq!(spot_base_fee_net(side, 10.0, 37.0, fee, AS_STATED), None);
            assert_eq!(
                bits(spot_base_fee_net(side, 10.0, 37.0, fee, COMMISSION)),
                bits(Some((10.0, fee * 37.0)))
            );
            assert_eq!(
                bits(spot_base_fee_net(side, 10.0, 37.0, fee, NET)),
                bits(Some((net, fee * 37.0)))
            );
        }
        // the arms are separate: a venue may answer a SELL and a rebate differently
        let sell_only = SpotFeeArms { sell: SpotFeeArm::Net, rebate: SpotFeeArm::AsStated };
        assert!(spot_base_fee_net(-1, 10.0, 37.0, 0.0045, sell_only).is_some());
        assert_eq!(spot_base_fee_net(1, 10.0, 37.0, -0.0045, sell_only), None);
    }

    /// No arm rewrites a fee that is no number or that is not smaller than the fill.
    #[test]
    fn a_malformed_row_is_refused_whatever_the_arms() {
        for arms in [AS_STATED, COMMISSION, NET] {
            for side in [1, -1] {
                for fee in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 10.0, 20.0, -10.0, -20.0] {
                    assert_eq!(spot_base_fee_net(side, 10.0, 37.0, fee, arms), None, "{fee}");
                }
                assert_eq!(spot_base_fee_net(side, f64::NAN, 37.0, 0.0045, arms), None);
                assert_eq!(spot_base_fee_net(side, 0.0, 37.0, 0.0, arms), None);
            }
        }
    }

    #[test]
    fn filled_wraps_in_order_filled() {
        let fill = sample_fill();
        let out = terminal_events("C1".to_string(), fill.clone(), 42, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], Event::Fill(fill.clone()));
        assert_eq!(
            out[1],
            Event::OrderFilled(OrderFilled {
                order_client_id: "C1".to_string(),
                fill,
                timestamp: 42
            })
        );
    }

    /// A fill whose quantity or price a decoder defaulted is neither the bare fill nor its wrap; a negative price (a
    /// combo bought for a credit) is a fill.
    #[test]
    fn a_fill_with_no_quantity_or_no_price_is_refused_with_its_wrap() {
        let with =
            |last_qty: f64, last_price: f64| FillEvent { last_qty, last_price, ..sample_fill() };
        for (qty, price) in [
            (0.0, 100.0),
            (-1.0, 100.0),
            (f64::NAN, 100.0),
            (f64::INFINITY, 100.0),
            (1.0, 0.0),
            (1.0, f64::NAN),
            (1.0, f64::NEG_INFINITY),
        ] {
            let fill = with(qty, price);
            assert!(!fill_is_bookable(&fill), "{qty} @ {price}");
            for is_filled in [true, false] {
                assert!(terminal_events("C1".to_string(), fill.clone(), 42, is_filled).is_empty());
            }
            let report = FillReport {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                trade_id: "T1".into(),
                venue_order_id: "9".into(),
                order_client_id: None,
                side: 1,
                last_qty: qty,
                last_price: price,
                commission: 0.0,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Taker,
                timestamp: 42,
            };
            assert!(!fill_report_is_bookable(&report), "{qty} @ {price}");
            assert!(fill_report_is_bookable(&FillReport {
                last_qty: 1.0,
                last_price: -3.5,
                ..report
            }));
        }
        assert_eq!(terminal_events("C1".to_string(), with(1.0, -3.5), 42, true).len(), 2);
    }

    #[test]
    fn partial_wraps_in_order_partially_filled() {
        let fill = sample_fill();
        let out = terminal_events("C1".to_string(), fill.clone(), 42, false);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], Event::Fill(fill.clone()));
        assert_eq!(
            out[1],
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                order_client_id: "C1".to_string(),
                fill,
                timestamp: 42
            })
        );
    }

    /// A bare fill and the fill its wrap carries are both respelled; an event that carries no symbol is untouched.
    #[test]
    fn every_symbol_an_event_carries_is_respelled() {
        let mut out = terminal_events("C1".to_string(), sample_fill(), 42, true);
        out.push(Event::OrderAccepted(OrderAccepted {
            order_client_id: "C1".to_string(),
            venue_order_id: None,
            timestamp: 42,
        }));
        let accepted = out[2].clone();
        respell_symbols(&mut out, |wire| format!("{wire}.P"));
        assert_eq!(out[0].fill().map(|f| f.symbol.as_str()), Some("BTCUSDT.P"));
        assert_eq!(out[1].fill().map(|f| f.symbol.as_str()), Some("BTCUSDT.P"));
        assert_eq!(out[2], accepted);
    }
}
