//! Shared **scripted stream doubles** for the three WS seams ([`UserStream`], [`MarketStream`],
//! [`DepthStream`]) — testing-arch Phase 4c (test-support consolidation).
//!
//! Before this module, every consumer hand-rolled the same two doubles: the user-data pump tests
//! (binance's `r6_binance_userdata.rs`, aster's "Ported from binance's" clone, the
//! `bridge_conformance` harness) each carried a private `Scripted(VecDeque<…>)`, and the
//! market/depth drivers plus polymarket's pump/raw-tap and vike-backfill's re-parse each carried a
//! private `ScriptedStream`/`CannedStream` replaying canned text frames. One drift-prone shape,
//! ~8 copies. This module is the ONE home; the copies are deleted and re-pointed here.
//!
//! Gated `#[cfg(any(test, feature = "test-support"))]`: in-crate unit tests see it always; the
//! crate's own `tests/` and downstream crates enable the `test-support` cargo feature as a
//! dev-dependency (`vike-bridge-core = { …, features = ["test-support"] }`). Never compiled into
//! a normal build.
//!
//! Behavior pins (verbatim from the copies this replaces):
//! - exhausting the script is a stream-closed error — `StreamError::Closed("script exhausted")`,
//!   the same shape a real disconnect takes, so a driver returning `Err` at the end of a script
//!   is expected, not a test failure;
//! - `send_text` records what the driver sent (subscribe frame, keepalive pings) into a log the
//!   test reads back via [`ScriptedStream::sent`] (shareable across sessions via
//!   [`ScriptedStream::with_sent_log`], the market-pump reconnect-test shape);
//! - the optional shared `clock` advances `advance_ms` before EVERY read, so consuming the script
//!   ages an ack/freshness watchdog with zero real sleeps (the depth/market driver test seam);
//! - `since_last_frame` reports a configurable `stall` (`ZERO` default = never idle), and a
//!   [`ScriptStep::TimeoutWithStall`] escalates it the moment it's popped — polymarket's
//!   `push_timeout_with_stall` shape (one synchronous session has no reentry point for a test to
//!   mutate the stream mid-call, so the step itself carries the escalation).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use crate::depth::DepthStream;
use crate::market_pump::MarketStream;
use crate::user_data::{StreamError, StreamMsg, UserStream};

/// One scripted read outcome for a text-frame stream ([`MarketStream`]/[`DepthStream`]).
pub enum ScriptStep {
    /// A text frame the driver decodes.
    Text(String),
    /// A read-timeout tick (no frame) — the stop-flag / ack- / idle-watchdog poll shape
    /// (`StreamError::Timeout`).
    Timeout,
    /// A read-timeout tick that ALSO escalates `since_last_frame`'s reported stall to the carried
    /// duration, the moment it's popped by `read_frame`.
    TimeoutWithStall(Duration),
}

/// Replays a canned queue of read outcomes over the text-frame seams — implements BOTH
/// [`MarketStream`] and [`DepthStream`] (the traits share one shape; a test picks its seam by the
/// driver it hands the stream to). See the module doc for the behavior pins.
///
/// Deliberately `Rc`-based (single-threaded, like every scripted-driver test) — the doubles model
/// one synchronous session loop, never a cross-thread stream.
pub struct ScriptedStream {
    steps: VecDeque<ScriptStep>,
    sent: Rc<RefCell<Vec<String>>>,
    stall: Duration,
    clock: Rc<Cell<i64>>,
    advance_ms: i64,
}

impl ScriptedStream {
    /// A stream replaying `steps` with a private send log.
    pub fn from_steps(steps: Vec<ScriptStep>) -> Self {
        ScriptedStream {
            steps: steps.into(),
            sent: Rc::new(RefCell::new(Vec::new())),
            stall: Duration::ZERO,
            clock: Rc::new(Cell::new(0)),
            advance_ms: 0,
        }
    }

    /// A stream of text frames only (each becomes a [`ScriptStep::Text`]).
    pub fn from_texts<I, T>(texts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        Self::from_steps(texts.into_iter().map(|t| ScriptStep::Text(t.into())).collect())
    }

    /// A stream of JSON frames (each serialized to its compact text form) — the polymarket
    /// scripted-pump constructor shape.
    pub fn from_json<I>(frames: I) -> Self
    where
        I: IntoIterator<Item = serde_json::Value>,
    {
        Self::from_texts(frames.into_iter().map(|v| v.to_string()))
    }

    /// A frame-less stream whose `since_last_frame` reports `stall` — for idle-watchdog tests
    /// (queue [`ScriptedStream::push_timeout`] ticks on top to drive the poll loop).
    pub fn stalled(stall: Duration) -> Self {
        let mut s = Self::from_steps(Vec::new());
        s.stall = stall;
        s
    }

    /// A transport-ALIVE frame-less stream (`stall` under the idle threshold) that shares `clock`
    /// and ages it `advance_ms` per read — the §B data-freshness test seam (the test's injected
    /// `now_ms` reads that SAME clock, so data-age is driven purely by consuming the script).
    pub fn stalled_clocked(stall: Duration, clock: Rc<Cell<i64>>, advance_ms: i64) -> Self {
        let mut s = Self::stalled(stall);
        s.clock = clock;
        s.advance_ms = advance_ms;
        s
    }

    /// Builder: record `send_text` into a SHARED log (so a feed test can assert across sessions
    /// after the driver consumed each stream) — the market-pump reconnect-test shape.
    pub fn with_sent_log(mut self, sent: Rc<RefCell<Vec<String>>>) -> Self {
        self.sent = sent;
        self
    }

    /// Builder: share `clock` and advance it `advance_ms` before every read.
    pub fn clocked(mut self, clock: Rc<Cell<i64>>, advance_ms: i64) -> Self {
        self.clock = clock;
        self.advance_ms = advance_ms;
        self
    }

    /// Builder: report `stall` from `since_last_frame` (on a stream that still has frames).
    pub fn with_stall(mut self, stall: Duration) -> Self {
        self.stall = stall;
        self
    }

    /// Queue a raw text frame (usable for malformed non-JSON frames a `serde_json::Value` cannot
    /// express).
    pub fn push_text(&mut self, t: &str) {
        self.steps.push_back(ScriptStep::Text(t.to_string()));
    }

    /// Queue a read-timeout tick (no frame).
    pub fn push_timeout(&mut self) {
        self.steps.push_back(ScriptStep::Timeout);
    }

    /// Queue a read-timeout tick that escalates the reported stall to `stall` when popped.
    pub fn push_timeout_with_stall(&mut self, stall: Duration) {
        self.steps.push_back(ScriptStep::TimeoutWithStall(stall));
    }

    /// Everything the driver sent (subscribe frames, keepalives), in order.
    pub fn sent(&self) -> Vec<String> {
        self.sent.borrow().clone()
    }

    /// The shared read: advance the clock, pop the next step. Exhaustion is a stream-closed error
    /// (the real-disconnect shape).
    fn next_frame(&mut self) -> Result<String, StreamError> {
        self.clock.set(self.clock.get() + self.advance_ms);
        match self.steps.pop_front() {
            Some(ScriptStep::Text(t)) => Ok(t),
            Some(ScriptStep::Timeout) => Err(StreamError::Timeout),
            Some(ScriptStep::TimeoutWithStall(stall)) => {
                self.stall = stall;
                Err(StreamError::Timeout)
            }
            None => Err(StreamError::Closed("script exhausted".into())),
        }
    }

    fn record_sent(&mut self, s: &str) -> Result<(), StreamError> {
        self.sent.borrow_mut().push(s.to_string());
        Ok(())
    }
}

impl MarketStream for ScriptedStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        self.next_frame()
    }
    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.record_sent(s)
    }
    fn since_last_frame(&self) -> Duration {
        self.stall
    }
}

impl DepthStream for ScriptedStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        self.next_frame()
    }
    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.record_sent(s)
    }
    fn since_last_frame(&self) -> Duration {
        self.stall
    }
}

/// A scripted **user-data** stream: a queue of `recv` outcomes the offline seam
/// (`run_user_data_forever`) consumes. Drained once, then reports
/// `StreamError::Closed("script exhausted")`; `pong` always succeeds. The one shape the
/// per-venue `r6_*_userdata` tests and the conformance harness all copied.
pub struct ScriptedUserStream(VecDeque<Result<StreamMsg, StreamError>>);

impl ScriptedUserStream {
    /// A stream replaying `items` in order.
    pub fn new(items: Vec<Result<StreamMsg, StreamError>>) -> Self {
        Self(items.into())
    }
}

impl UserStream for ScriptedUserStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError> {
        self.0.pop_front().unwrap_or(Err(StreamError::Closed("script exhausted".into())))
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
        Ok(())
    }
}
