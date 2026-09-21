//! Shared venue-mapper tails that were byte-identical across every crypto event_mapper. No
//! Python twin — this is the dual-publish contract itself (see the venue mappers' `//!` docs, e.g.
//! `vike_binance::family::event_mapper`): on a TRADE/fill report a venue emits BOTH a bare
//! `Event::Fill` (the `Account` folds it) AND the wrapping `OrderPartiallyFilled`/`OrderFilled`
//! (the FSM registry applies it). The `is_filled` PREDICATE stays per-venue (Binance reads
//! `X=="FILLED"`, OKX compares accFillSz vs sz, Bybit reads leavesQty==0, Deribit reads
//! `state=="filled"`); this helper only owns the terminal-vs-partial branch + the `vec![Fill, wrap]`
//! shape once the predicate is resolved.

use vike_model::events::{Event, FillEvent, OrderFilled, OrderPartiallyFilled};

/// The dual-publish tail every crypto `event_mapper` shares: given a resolved `is_filled`
/// predicate, wrap `fill` in `OrderFilled` (terminal) or `OrderPartiallyFilled` (partial) and
/// return `[Event::Fill(fill), wrap]`. `coid` is moved into the wrap; the caller has already
/// cloned it onto `fill.client_order_id`.
pub fn terminal_events(coid: String, fill: FillEvent, ts: i64, is_filled: bool) -> Vec<Event> {
    let wrap = if is_filled {
        Event::OrderFilled(OrderFilled { client_order_id: coid, fill: fill.clone(), ts })
    } else {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid,
            fill: fill.clone(),
            ts,
        })
    };
    vec![Event::Fill(fill), wrap]
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::LiquiditySide;

    fn sample_fill() -> FillEvent {
        FillEvent {
            trade_id: "T1".into(),
            client_order_id: "C1".to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 42,
            mark_price: None,
            position_side: "BOTH".to_string().into(),
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
            Event::OrderFilled(OrderFilled { client_order_id: "C1".to_string(), fill, ts: 42 })
        );
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
                client_order_id: "C1".to_string(),
                fill,
                ts: 42
            })
        );
    }
}
