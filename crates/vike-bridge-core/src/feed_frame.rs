//! `FeedFrame` — what ONE text frame on a subscribe-only public market-data socket classifies as,
//! for every venue whose `on_text` closure rides the `market_pump` driver: a data push, the venue's
//! subscribe ACK, its subscribe REJECT, or nothing.
//!
//! The envelope is the venue-neutral half (net-hardening br7: an ack disarms the subscribe-ack
//! watchdog, a reject is an attributable session error, never a silent drop); the payload is the
//! lane's. One generic type rather than an `Ack`/`Error`/`Ignored` triple re-spelled per lane and per
//! venue: bybit's and okx's kline, mark and trades classifiers each carried their own copy of it.
//! The venue still decides what its ack and reject LOOK like on the wire (bybit's `op`/`success`,
//! okx's `event`), so the parse stays in each bridge; only the vocabulary and the one map onto
//! the driver's `FrameOutcome` (`FeedFrame::outcome`) live here.

use vike_marketdata::Bar;

/// One classified frame of a subscribe-only market-data socket, generic over the lane's payload.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedFrame<T> {
    /// A data push the lane emits. Data ALSO confirms the subscribe handshake.
    Data(T),
    /// The venue's subscribe ACK — no data, but it CONFIRMS the subscribe handshake, disarming the
    /// shared driver's subscribe-ack watchdog.
    Ack,
    /// The venue REJECTED the subscribe — an ATTRIBUTABLE error carrying the venue's own message,
    /// surfaced instead of silently dropped. The driver ends the session with it and reconnects.
    Error(String),
    /// Anything else (a keepalive reply, an unrelated topic or channel, another op, a row the lane
    /// cannot read) — dropped, and it does NOT confirm the handshake.
    Ignored,
}

/// A kline lane's payload: one bar and whether the venue marked it FINAL. A closed bar goes to the
/// lossless `close_bar` sink lane, a forming one to the conflating `forming_bar` lane.
#[derive(Debug, Clone, PartialEq)]
pub struct KlineBar {
    /// The bar, built through [`crate::klines::kline_to_bar`].
    pub bar: Bar,
    /// `true` when the venue marked the candle closed (bybit `confirm=true`, okx `confirm=="1"`).
    pub closed: bool,
}

/// A mark lane's payload: the venue's REAL mark price and the epoch-ms stamp it carried. Only a
/// mark that passes [`crate::mark_stream::is_valid_mark`] becomes one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkPx {
    /// The mark price.
    pub px: f64,
    /// The venue's stamp for it (epoch-ms; `0` when the frame carried none).
    pub ts: i64,
}

/// The router of a lane whose data push is a list of rows (bybit's and okx's trades lanes): text
/// that is not JSON (a bare-text `ping`/`pong` keepalive) is [`FeedFrame::Ignored`], the venue's
/// subscribe `envelope` is classified FIRST, and only then are the rows `parse`d; an empty parse is
/// [`FeedFrame::Ignored`], so it never confirms the handshake.
pub fn route_rows<T>(
    text: &str,
    envelope: impl FnOnce(&serde_json::Value) -> Option<FeedFrame<Vec<T>>>,
    parse: impl FnOnce(&serde_json::Value) -> Vec<T>,
) -> FeedFrame<Vec<T>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return FeedFrame::Ignored;
    };
    if let Some(envelope) = envelope(&v) {
        return envelope;
    }
    let rows = parse(&v);
    if rows.is_empty() { FeedFrame::Ignored } else { FeedFrame::Data(rows) }
}

impl<T> FeedFrame<T> {
    /// The ONE map from a classified frame onto the driver's [`crate::market_pump::FrameOutcome`]:
    /// `Data` runs `on_data` (which emits into the sink and answers, normally
    /// [`crate::market_pump::FrameOutcome::Confirm`]), `Ack` confirms, `Error` is fatal with the
    /// venue's message, `Ignored` is ignored.
    pub fn outcome(
        self,
        on_data: impl FnOnce(T) -> crate::market_pump::FrameOutcome,
    ) -> crate::market_pump::FrameOutcome {
        use crate::market_pump::FrameOutcome;
        match self {
            FeedFrame::Data(payload) => on_data(payload),
            FeedFrame::Ack => FrameOutcome::Confirm,
            FeedFrame::Error(msg) => FrameOutcome::Fatal(msg),
            FeedFrame::Ignored => FrameOutcome::Ignore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn envelope(v: &Value) -> Option<FeedFrame<Vec<i64>>> {
        v.get("op").map(|_| FeedFrame::Ack)
    }

    fn parse(v: &Value) -> Vec<i64> {
        v.get("data")
            .and_then(Value::as_array)
            .map_or(Vec::new(), |d| d.iter().filter_map(Value::as_i64).collect())
    }

    /// The trades router bybit and okx each carried: non-JSON is ignored, the envelope answers
    /// BEFORE any rows are read (a frame carrying both is the envelope's), and no rows is ignored.
    #[test]
    fn route_rows_reads_the_envelope_first_and_ignores_text_and_empty_rows() {
        assert_eq!(route_rows("pong", envelope, parse), FeedFrame::Ignored);
        assert_eq!(route_rows(r#"{"op":"subscribe","data":[1]}"#, envelope, parse), FeedFrame::Ack);
        assert_eq!(route_rows(r#"{"data":[]}"#, envelope, parse), FeedFrame::Ignored);
        assert_eq!(route_rows(r#"{"data":[1,2]}"#, envelope, parse), FeedFrame::Data(vec![1, 2]));
    }
}
