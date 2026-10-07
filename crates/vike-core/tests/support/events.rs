//! Event, report, quote and bar builders.

use vike_exec::QuoteUpdate;
use vike_model::events::{Event, FillEvent, LiquiditySide, TradeId};
use vike_model::{Bar, FillReport, QuoteTick};

/// A bare external fill on the sim venue (empty coid) — the journaled `Ingest::Event` that folds
/// into position/pnl and the `seen_trade_ids` dedup set without any client involvement, so replay
/// reproduces it exactly.
///
/// DEFAULTS: venue `"sim"`, symbol `"BTCUSDT"`, `side 1` (a buy), no commission, `"taker"`,
/// `ts 1`, `mark_price: Some(px)`, position side `"BOTH"`. No mark reaches the account from a fill
/// (fills carry none into the account), so `margin_call_journal.rs` lets a CLOSED BAR set the mark
/// its margin sweep values against. A test that reads the mark, the timestamp or the trade-id bytes
/// builds its fill itself (`mark_slot_semantics.rs`'s and `runtime_smoke.rs`'s carry no mark).
pub(crate) fn sim_bare_fill(tid: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        // `&str`: restart_restore's watchdog-replay test mints `w{i}` per iteration, so this cannot be
        // `&'static`. A literal id builds the same `TradeId` value through either constructor.
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    })
}

/// A venue fill report for an EXTERNAL order — one with no local coid — which a reconcile pass
/// must classify against local state rather than match to an order it placed.
///
/// DEFAULTS: venue `"binance"`, symbol `"BTCUSDT"`, trade id `"t1"`, venue order id `"v9"`,
/// `side 1`, 1.0 @ 100.0, no commission in `"USDT"`, taker, `ts 5`.
pub(crate) fn external_fill_report() -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v9".into(),
        client_order_id: None, // external order — no local coid
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

/// A sim/BTCUSDT quote whose bid and ask are both `px`, at `ts`.
///
/// DEFAULTS: `local_ts` equals `ts`, both sizes are `1.0`, and the tick carries the symbol
/// `"BTCUSDT"`. A test that reads a size or quotes a spread builds its quote itself.
pub(crate) fn sim_quote(px: f64, ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: ts,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        },
    }
}

/// A BTCUSDT quote tick at `ts`, 99 bid over 101 ask.
///
/// DEFAULTS: `local_ts` is `0` ("not stamped"), both sizes are `1.0`, symbol `"BTCUSDT"`.
pub(crate) fn quote_tick(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "BTCUSDT".into(),
    }
}

/// A symbol-less OHLC bar at `ts` — the five-argument shape `vike_marketdata::test_support`'s module
/// doc lists as having no builder there.
///
/// DEFAULTS: `volume` is `1.0`, and `funding`, `bid`, `ask` and `symbol` are all `None` (the shape a
/// feed hands the engine before dispatch stamps a symbol). A flat bar of volume `1.0` is
/// `vike_marketdata::test_support::flat_bar_unit_volume`; a bar that carries a symbol is built by
/// hand.
pub(crate) fn ohlc_bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
