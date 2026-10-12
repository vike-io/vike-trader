//! Helpers shared by the `client` unit-test modules: the OHLC `bar` builder and `drain`.

use vike_exec::ExecutionClient;
use vike_model::Bar;
use vike_model::events::Event;

use super::PaperExecutionClient;

/// A symbol-less OHLC bar: no volume, no funding, no bid/ask.
pub(super) fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Drain every queued event, in emit order.
pub(super) fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
    std::iter::from_fn(|| c.poll_events()).collect()
}
