//! **A private stream that is an HTTP body, read line by line, on the shared pump** (connection plan B5): a
//! server-sent-events body (alpaca's `/v2/events/trades`) or a newline-delimited JSON body over chunked HTTP (oanda's
//! transactions stream) is a [`WebsocketSessionUserStream`] here, so its lane can run on [`super::run_user`] (the
//! reconnect ladder, the idle watchdog, the replay, the order path's state) instead of a read loop of its own. The
//! reader is any [`BufRead`]: the lane's `ureq` response body behind a `BufReader`, no second HTTP crate.
//!
//! What one read gives the pump:
//! - a line that carries a payload is [`WebsocketSessionUserMessage::Text`], the payload alone (an SSE `data:` field's
//!   value; a JSON line as it is), which the pump decodes;
//! - any other line (a blank one, an SSE comment or a field other than `data:`, an SSE `[DONE]`) is
//!   [`WebsocketSessionUserMessage::Other`]: proof of life with nothing to decode, as a heartbeat frame is;
//! - a read that waited out the reader's own bound (`TimedOut`, or `WouldBlock` where the platform says that, or a
//!   `ureq` body's per-read timeout, which ureq hands over as kind `Other` wrapping its own `ureq::Error::Timeout`) is
//!   the pump's [`WebsocketSessionUserError::Timeout`], its stop poll, and the idle watchdog judges the silence; a part
//!   line already read is kept for the next read, so a reader that carries on after a timed-out read loses no byte;
//! - the body's end, or any other read error, is [`WebsocketSessionUserError::Closed`] in its words.
//!
//! The lane's agent bounds each read (`ureq`'s `timeout_per_read`): at the pump's poll, so a stop is heard within it
//! and the row's `idle_threshold` is the watchdog's, or at the idle bound itself, whose first timed-out read then
//! trips the watchdog. HTTP has no ping, so [`WebsocketSessionUserStream::pong`] sends nothing.
//!
//! **The market pump reads the same stream** ([`WebsocketSessionMarketStream`]): a market feed that is an HTTP body (a
//! pricing stream over chunked HTTP) runs on `crate::websocket_session::market`'s `run_market_on`, its lane dialling
//! for itself. A payload line is the frame, any other line is proof of life and read past, a timed-out read is the
//! pump's tick, and the time since the last whole line (on `crate::pump_clock`) is what its idle watchdog judges.
//! Nothing can be written to an HTTP body, so a send is refused, never dropped.

use std::io::{BufRead, ErrorKind};
use std::time::{Duration, Instant};

use super::{WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserStream};
use crate::pump_clock;
use crate::websocket_session::market::WebsocketSessionMarketStream;

/// How a line carries its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineFraming {
    /// Server-sent events: a `data:` field's value is the payload; comments (`:`), other fields and blank lines carry
    /// none.
    // shortcut: one `data:` line is one payload, as alpaca sends each event; join the lines up to the blank line when a
    // venue splits one event over several `data:` lines.
    ServerSentEvents,
    /// One JSON document per line (a chunked HTTP body): a non-blank line is the payload.
    NewlineDelimitedJson,
}

impl LineFraming {
    /// The pump's message for one line, its line ending already trimmed.
    fn message(self, line: &str) -> WebsocketSessionUserMessage {
        let payload = match self {
            LineFraming::ServerSentEvents => {
                line.strip_prefix("data:").map(str::trim).filter(|data| *data != "[DONE]")
            }
            LineFraming::NewlineDelimitedJson => Some(line),
        };
        match payload {
            Some(payload) if !payload.is_empty() => {
                WebsocketSessionUserMessage::Text(payload.to_string())
            }
            _ => WebsocketSessionUserMessage::Other,
        }
    }
}

/// An HTTP body read as lines, framed by [`LineFraming`] (the module doc).
pub struct LineStream<R> {
    reader: R,
    framing: LineFraming,
    /// The line being read: kept across a timed-out read, handed out once it is whole.
    line: Vec<u8>,
    /// When the last whole line was read (the stream's birth before the first): the market pump's idle clock.
    last_line: Instant,
}

impl<R: BufRead> LineStream<R> {
    /// The stream of `reader`'s lines under `framing`.
    #[must_use]
    pub fn new(reader: R, framing: LineFraming) -> Self {
        Self { reader, framing, line: Vec::new(), last_line: pump_clock::now() }
    }
}

impl<R: BufRead> WebsocketSessionUserStream for LineStream<R> {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        // `read_until` keeps every byte it consumed before an error in `line`, so a timed-out read loses nothing.
        match self.reader.read_until(b'\n', &mut self.line) {
            Ok(0) if self.line.is_empty() => {
                return Err(WebsocketSessionUserError::Closed(
                    "the stream's body ended".to_string(),
                ));
            }
            Ok(_) => {} // a whole line, or the body's last line without its newline
            Err(e) if timed_out(&e) => return Err(WebsocketSessionUserError::Timeout),
            Err(e) => return Err(WebsocketSessionUserError::Closed(e.to_string())),
        }
        let line = std::mem::take(&mut self.line);
        self.last_line = pump_clock::now();
        Ok(self.framing.message(String::from_utf8_lossy(&line).trim()))
    }

    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        Ok(()) // HTTP has no ping to answer
    }
}

impl<R: BufRead> WebsocketSessionMarketStream for LineStream<R> {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        loop {
            // A line with no payload is proof of life, stamped by `recv`, and read past.
            if let WebsocketSessionUserMessage::Text(payload) = self.recv()? {
                return Ok(payload);
            }
        }
    }

    fn send_text(&mut self, _frame: &str) -> Result<(), WebsocketSessionUserError> {
        Err(WebsocketSessionUserError::Closed(
            "an HTTP body takes no frame: a line stream sends nothing".to_string(),
        ))
    }

    fn since_last_frame(&self) -> Duration {
        pump_clock::since(self.last_line)
    }
}

/// Whether a read error is a read that waited out its bound: `TimedOut`, `WouldBlock`, or ureq's per-read timeout,
/// which ureq 3.5 hands a body's reader as kind `Other` wrapping its own `ureq::Error::Timeout` (read as the body's end,
/// a silent stream would look like a clean close and the idle watchdog would never judge it).
fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
        || e.get_ref()
            .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
}

#[path = "line_stream_tests.rs"]
#[cfg(test)]
mod line_stream_tests;
