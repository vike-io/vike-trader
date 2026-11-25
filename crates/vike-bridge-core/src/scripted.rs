//! The shared **scripted stream doubles** for the three WS seams ([`WebsocketSessionUserStream`], [`WebsocketSessionMarketStream`],
//! [`WebsocketSessionDepthStream`]): the ONE home for them, so no consumer hand-rolls a private
//! `Scripted`/`WebsocketSessionMarketStreamScript`/`CannedStream` copy. [`one_session`] and [`LaneStatus`] drive a venue's
//! market lane through its `connect` seam for exactly one session.
//!
//! Compiled only under `test` or the `test-support` feature:
//! in-crate unit tests see it; the crate's own `tests/` and downstream crates enable `test-support`
//! as a dev-dependency (`vike-bridge-core = { …, features = ["test-support"] }`). Never in a normal
//! build.
//!
//! Behavior pins:
//! - exhausting the script is a stream-closed error, `WebsocketSessionUserError::Closed("script exhausted")`, the
//!   shape a real disconnect takes, so a driver returning `Err` at the end of a script is expected;
//! - `send_text` records what the driver sent (subscribe frames, keepalive pings) into a log read
//!   back via [`WebsocketSessionMarketStreamScript::sent`] (shareable across sessions via
//!   [`WebsocketSessionMarketStreamScript::with_sent_log`]);
//! - the optional shared `clock` advances `advance_ms` before EVERY read, so consuming the script
//!   ages an ack/freshness watchdog with zero real sleeps;
//! - `since_last_frame` reports a configurable `stall` (`ZERO` default = never idle), and a
//!   [`ScriptStep::TimeoutWithStall`] escalates it the moment it is popped (one synchronous session
//!   has no reentry point for a test to mutate the stream mid-call, so the step carries it);
//! - the user-data double plays [`UserScriptStep`]s: a read as it is, a venue's close frame in a real socket's
//!   words, and a silence that ticks on the pump clock (`crate::pump_clock`), so under a virtual clock a 60 s
//!   stall costs nothing; it logs what the pump sent and its pongs ([`UserScriptHeard`]). The same double is a
//!   market-driver stream too ([`WebsocketSessionMarketStream`]): its idle clock is the pump clock's, so the market
//!   driver's idle watchdog and keepalive cadence run a row's real durations under a virtual clock.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::feed_status::DataClientStatus;

use crate::depth::WebsocketSessionDepthStream;
use crate::pump_clock;
use crate::websocket::server_closed;
use crate::websocket_session::market::{
    WebsocketSessionMarketOptions, WebsocketSessionMarketStream,
};
use crate::websocket_session::user::{
    USER_DATA_POLL, WebsocketSessionUserError, WebsocketSessionUserMessage,
    WebsocketSessionUserStream,
};

/// A market lane's `connect` that opens ONE scripted session: `frames`, then the script's end (a
/// closed stream), with what the lane sends logged into `sent`; a second dial fails the test. With a
/// [`LaneStatus`] whose `wake` stops the lane at its first fault, the lane runs that one session,
/// discloses it, and returns.
pub fn one_session(
    frames: &[&str],
    sent: &Rc<RefCell<Vec<String>>>,
) -> impl FnMut(&WebsocketSessionMarketOptions<'_>) -> Result<WebsocketSessionMarketStreamScript, String>
+ use<> {
    let mut frames: Option<Vec<String>> = Some(frames.iter().map(|f| (*f).to_string()).collect());
    let sent = Rc::clone(sent);
    move |_opts: &WebsocketSessionMarketOptions<'_>| match frames.take() {
        Some(frames) => {
            Ok(WebsocketSessionMarketStreamScript::from_texts(frames)
                .with_sent_log(Rc::clone(&sent)))
        }
        None => panic!("a second dial: the lane did not stop at its first fault"),
    }
}

/// The status side of a market lane driven over [`one_session`]: the status the lane writes (state
/// and words), its stop flag, and the `wake` it calls after every write. `wake` records each status
/// written into `seen` (a repeat of the last one is folded) and raises `stop` at the first status whose
/// words contain the caller's `needle` (the venue's own fault wording).
pub struct LaneStatus {
    pub status: Arc<Mutex<DataClientStatus>>,
    pub stop: Arc<AtomicBool>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub seen: Arc<Mutex<Vec<DataClientStatus>>>,
}

impl LaneStatus {
    /// A lane status whose `wake` stops the lane at the first status containing `needle`.
    pub fn halting_at(needle: &'static str) -> Self {
        let status = Arc::new(Mutex::new(DataClientStatus::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::<DataClientStatus>::new()));
        let (now, halt, log) = (Arc::clone(&status), Arc::clone(&stop), Arc::clone(&seen));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let now = now.lock().unwrap().clone();
            let mut log = log.lock().unwrap();
            if log.last() != Some(&now) {
                log.push(now.clone());
            }
            if now.text.contains(needle) {
                halt.store(true, Ordering::SeqCst);
            }
        });
        LaneStatus { status, stop, wake, seen }
    }
}

/// One scripted read outcome for a text-frame stream ([`WebsocketSessionMarketStream`]/[`WebsocketSessionDepthStream`]).
pub enum ScriptStep {
    /// A text frame the driver decodes.
    Text(String),
    /// A read-timeout tick, no frame (`WebsocketSessionUserError::Timeout`): the stop-flag / watchdog poll shape.
    Timeout,
    /// A read-timeout tick that ALSO escalates `since_last_frame`'s reported stall to the carried
    /// duration, the moment it's popped by `read_frame`.
    TimeoutWithStall(Duration),
}

/// Replays a canned queue of read outcomes over the text-frame seams: implements BOTH
/// [`WebsocketSessionMarketStream`] and [`WebsocketSessionDepthStream`] (a test picks the seam by the driver it hands the stream
/// to). Behavior pins: module doc. `Rc`-based on purpose: it models one synchronous session loop,
/// never a cross-thread stream.
pub struct WebsocketSessionMarketStreamScript {
    steps: VecDeque<ScriptStep>,
    sent: Rc<RefCell<Vec<String>>>,
    stall: Duration,
    clock: Rc<Cell<i64>>,
    advance_ms: i64,
}

impl WebsocketSessionMarketStreamScript {
    /// A stream replaying `steps` with a private send log.
    pub fn from_steps(steps: Vec<ScriptStep>) -> Self {
        WebsocketSessionMarketStreamScript {
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

    /// A stream of JSON frames, each serialized to its compact text form.
    pub fn from_json<I>(frames: I) -> Self
    where
        I: IntoIterator<Item = serde_json::Value>,
    {
        Self::from_texts(frames.into_iter().map(|v| v.to_string()))
    }

    /// A frame-less stream whose `since_last_frame` reports `stall` — for idle-watchdog tests
    /// (queue [`WebsocketSessionMarketStreamScript::push_timeout`] ticks on top to drive the poll loop).
    pub fn stalled(stall: Duration) -> Self {
        let mut s = Self::from_steps(Vec::new());
        s.stall = stall;
        s
    }

    /// A transport-ALIVE frame-less stream (`stall` under the idle threshold) that shares `clock`
    /// and ages it `advance_ms` per read: the data-freshness seam (the test's injected `now_ms`
    /// reads that SAME clock, so data age is driven purely by consuming the script).
    pub fn stalled_clocked(stall: Duration, clock: Rc<Cell<i64>>, advance_ms: i64) -> Self {
        let mut s = Self::stalled(stall);
        s.clock = clock;
        s.advance_ms = advance_ms;
        s
    }

    /// Builder: record `send_text` into a SHARED log, so a reconnect test can assert across
    /// sessions after the driver consumed each stream.
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
    fn next_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        self.clock.set(self.clock.get() + self.advance_ms);
        match self.steps.pop_front() {
            Some(ScriptStep::Text(t)) => Ok(t),
            Some(ScriptStep::Timeout) => Err(WebsocketSessionUserError::Timeout),
            Some(ScriptStep::TimeoutWithStall(stall)) => {
                self.stall = stall;
                Err(WebsocketSessionUserError::Timeout)
            }
            None => Err(WebsocketSessionUserError::Closed("script exhausted".into())),
        }
    }

    fn record_sent(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        self.sent.borrow_mut().push(s.to_string());
        Ok(())
    }
}

impl WebsocketSessionMarketStream for WebsocketSessionMarketStreamScript {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        self.next_frame()
    }
    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        self.record_sent(s)
    }
    fn since_last_frame(&self) -> Duration {
        self.stall
    }
}

impl WebsocketSessionDepthStream for WebsocketSessionMarketStreamScript {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        self.next_frame()
    }
    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        self.record_sent(s)
    }
    fn since_last_frame(&self) -> Duration {
        self.stall
    }
}

/// One step of a scripted **user-data** stream ([`WebsocketSessionUserStreamScript`]).
pub enum UserScriptStep {
    /// One `recv` outcome, as it is.
    Read(Result<WebsocketSessionUserMessage, WebsocketSessionUserError>),
    /// The venue ends the session with a close frame: read in the words a real socket reports it in
    /// (`crate::websocket::WebsocketSessionUserStreamTungstenite`'s `recv`).
    Close { code: u16, reason: String },
    /// The socket stays open and silent for this long: a read-timeout tick every [`USER_DATA_POLL`] of the pump clock
    /// (`crate::pump_clock`), as a real socket's read timeout gives them.
    Stall(Duration),
}

/// What the pump did to a scripted user-data stream: [`WebsocketSessionUserStreamScript::logged`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserScriptHeard {
    /// A text frame the pump sent (its keepalive, through [`WebsocketSessionUserStreamScript::send_text`]).
    Text(String),
    /// A pong answering a server ping.
    Pong,
}

/// A scripted user-data stream's log: each [`UserScriptHeard`] with when, on the pump clock. Shared, so a test reads
/// it after the pump has dropped the stream, across sessions and threads.
pub type UserScriptLog = Arc<Mutex<Vec<(Instant, UserScriptHeard)>>>;

/// A scripted **user-data** stream: a queue of [`UserScriptStep`]s the offline seam (`run_user`) consumes. Drained
/// once, then reports `WebsocketSessionUserError::Closed("script exhausted")`, or, [`Self::stopping`], raises the
/// pump's stop; `pong` always succeeds.
pub struct WebsocketSessionUserStreamScript {
    steps: VecDeque<UserScriptStep>,
    stalled_until: Option<Instant>,
    log: UserScriptLog,
    answer_sends: bool,
    /// What an answer to a sent frame reads as: this text frame, or a control frame when `None`.
    answer: Option<String>,
    /// Answers owed to frames the pump sent: each is read next, even inside a stall.
    answers_owed: u32,
    /// When the market seam last read a frame of any kind, on the pump clock (its idle clock).
    last_frame: Option<Instant>,
    stop_at_end: Option<Arc<AtomicBool>>,
}

impl WebsocketSessionUserStreamScript {
    /// A stream replaying `items` in order.
    pub fn new(items: Vec<Result<WebsocketSessionUserMessage, WebsocketSessionUserError>>) -> Self {
        Self::from_steps(items.into_iter().map(UserScriptStep::Read).collect())
    }

    /// A stream playing `steps` in order.
    pub fn from_steps(steps: Vec<UserScriptStep>) -> Self {
        Self {
            steps: steps.into(),
            stalled_until: None,
            log: UserScriptLog::default(),
            answer_sends: false,
            answer: None,
            answers_owed: 0,
            last_frame: None,
            stop_at_end: None,
        }
    }

    /// Builder: log what the pump sends and its pongs into `log`.
    pub fn logged(mut self, log: UserScriptLog) -> Self {
        self.log = log;
        self
    }

    /// Builder: the venue answers every frame the pump sends (its keepalive's ping) with a control frame, read next.
    pub fn answering_sends(mut self) -> Self {
        self.answer_sends = true;
        self
    }

    /// Builder: the venue answers every frame the pump sends with the text frame `answer` (a venue whose pong is a
    /// text frame, as okx's `pong` is), read next.
    pub fn answering_sends_with(mut self, answer: &str) -> Self {
        self.answer_sends = true;
        self.answer = Some(answer.to_string());
        self
    }

    /// Builder: once drained, raise `stop` (the pump's) and read timeouts, so the pump ends without a last close.
    pub fn stopping(mut self, stop: Arc<AtomicBool>) -> Self {
        self.stop_at_end = Some(stop);
        self
    }

    /// Send an app-level text frame: logged, and answered when [`Self::answering_sends`].
    pub fn send_text(&mut self, text: &str) -> Result<(), WebsocketSessionUserError> {
        self.heard(UserScriptHeard::Text(text.to_string()));
        if self.answer_sends {
            self.answers_owed += 1;
        }
        Ok(())
    }

    fn heard(&self, heard: UserScriptHeard) {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push((pump_clock::now(), heard));
    }
}

impl WebsocketSessionUserStream for WebsocketSessionUserStreamScript {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        if self.answers_owed > 0 {
            self.answers_owed -= 1;
            return Ok(match &self.answer {
                Some(text) => WebsocketSessionUserMessage::Text(text.clone()),
                None => WebsocketSessionUserMessage::Other,
            });
        }
        loop {
            if let Some(until) = self.stalled_until {
                let left = until.saturating_duration_since(pump_clock::now());
                if !left.is_zero() {
                    pump_clock::sleep(USER_DATA_POLL.min(left));
                    return Err(WebsocketSessionUserError::Timeout);
                }
                self.stalled_until = None;
            }
            match self.steps.pop_front() {
                Some(UserScriptStep::Read(read)) => return read,
                Some(UserScriptStep::Close { code, reason }) => {
                    return Err(server_closed(code, &reason));
                }
                Some(UserScriptStep::Stall(span)) => {
                    self.stalled_until = Some(pump_clock::now() + span)
                }
                None => {
                    return match &self.stop_at_end {
                        Some(stop) => {
                            stop.store(true, Ordering::SeqCst);
                            Err(WebsocketSessionUserError::Timeout)
                        }
                        None => Err(WebsocketSessionUserError::Closed("script exhausted".into())),
                    };
                }
            }
        }
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        self.heard(UserScriptHeard::Pong);
        Ok(())
    }
}

/// The market-driver seam over the same script: one TEXT frame per read, a server ping answered and a control frame
/// looped past (as `crate::websocket_session::market`'s `WebsocketSessionMarketStreamTungstenite` does), every frame
/// stamping the idle clock on the pump clock, so a [`UserScriptStep::Stall`] is the silence the driver's idle
/// watchdog measures.
impl WebsocketSessionMarketStream for WebsocketSessionUserStreamScript {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        self.last_frame.get_or_insert_with(pump_clock::now);
        loop {
            let read = WebsocketSessionUserStream::recv(self)?;
            self.last_frame = Some(pump_clock::now());
            match read {
                WebsocketSessionUserMessage::Text(text) => return Ok(text),
                WebsocketSessionUserMessage::Ping(payload) => {
                    WebsocketSessionUserStream::pong(self, payload)?;
                }
                WebsocketSessionUserMessage::Other => {}
            }
        }
    }
    fn send_text(&mut self, text: &str) -> Result<(), WebsocketSessionUserError> {
        WebsocketSessionUserStreamScript::send_text(self, text)
    }
    fn since_last_frame(&self) -> Duration {
        self.last_frame.map_or(Duration::ZERO, pump_clock::since)
    }
}
