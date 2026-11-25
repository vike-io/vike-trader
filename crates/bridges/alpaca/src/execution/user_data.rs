//! The exec event lane: the SSE body of `/v2/events/trades` (v1 is deprecated → 410) on the shared private-stream pump
//! (`vike_bridge_core::websocket_session::user`'s `spawn_user` over `run_user`, read as lines by its `LineStream` in
//! `LineFraming::ServerSentEvents`). The pump owns the reconnect ladder (the exec row's), the idle watchdog, the failed-open
//! pace and the order path's state; this file owns the open ([`open_trade_events`]), the cursor and the agent. Each
//! `data: {json}` frame decodes via `execution::event_mapper::decode_trade_event`.
//!
//! **Reconnect backfill cursor (verified live 2026-07-14), inside the open.** Every event carries a top-level
//! `event_id` (a sortable ULID). [`TradeEventsCursor`] keeps the last one the decode saw, and every open after it asks
//! `?since_id=<last_event_id>` to backfill what landed during the gap (OANDA's `sinceid` A3 pattern), so the pump needs
//! no REST replay. Live-verified: the endpoint accepts `since_id=<ULID>` (`200`; a non-ULID like `1` is what returns
//! `400`) and is INCLUSIVE — it re-delivers the boundary event whose id equals `since_id`. So the decode skips exactly
//! that re-delivered boundary event (its `event_id` == the `since_id` the open asked for). This sits on top of two
//! idempotency layers, not instead of them: the engine's always-on `seen_trade_ids` guard drops any replayed FILL by
//! `trade_id`, and the order FSM drops replayed lifecycle transitions as invalid — so a boundary re-delivery is safe even
//! if the skip ever misses. Until the first event (`last_event_id == None`) an open is bare = live from now, so a gap
//! before this process saw any event is not backfilled: reconcile's (the connection row's declared gap).

use std::io::BufReader;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::websocket_session::user::line_stream::{LineFraming, LineStream};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserHandle, WebsocketSessionUserOpenOutcome, WebsocketSessionUserSpec,
    spawn_user,
};
use vike_execution::EventSender;
use vike_utility::mutex::lock;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings::{self, VenueTimingsReconnect};

use crate::config::AlpacaConfig;
use crate::execution::event_mapper::decode_trade_event;
use crate::refusals::AlpacaStreamRefusal;
use crate::transport::sign::TokenSource;

/// How long the SSE dial may sit in `connect(2)` before this thread gets its `stop` check back: the
/// exec lane's `dial_timeout` in `vike_venues::venue_timings::ALPACA`.
///
/// It exists because `ureq`'s default is NO connect timeout at all, which makes this thread's stop
/// latency the OS connect timeout (~130 s on Linux) against an unreachable host —
/// `crates/vike-transport/src/http.rs`'s `stream_agent` doc carries the full shutdown argument. Five seconds is ~an
/// order of magnitude over any healthy
/// connect, so a live stream never notices it.
pub(crate) const CONNECT_TIMEOUT: Duration = venue_timings::ALPACA.exec().dial_timeout();

/// How long one read of the SSE stream may wait for its next byte, the response head's included: the
/// exec lane's `idle_threshold` in `vike_venues::venue_timings::ALPACA`, armed as ureq's
/// `timeout_per_read` AND handed to the pump as its idle watchdog, so the first read that waits it out trips the
/// watchdog (`LineStream`'s module doc).
const READ_IDLE: Duration = venue_timings::ALPACA.exec().idle_threshold();

/// The waits before the next try, the exec lane's `reconnect` row, walked by the pump's
/// [`ReconnectBackoff`] (owner ruling B-13, one rule for every venue): after a failed open (a token that
/// would not mint, a non-2xx answer, a connect that failed), after a refused one (a 401, whose token is
/// refreshed first) and after a session that ended. The row is a fixed 2 s
/// (`the_execution_reconnect_is_the_fixed_wait_the_pump_runs`), so every one of them is 2 s.
const RECONNECT: VenueTimingsReconnect = venue_timings::ALPACA.exec().reconnect();

/// The pump's ladder over `RECONNECT`, with a refusal budget of ONE: the 401 that refreshed the token is a reopen
/// (`Connecting`, the shared pump's refusal rule), and a second 401 in a row, refused to the fresh token, is a failure.
/// On the fixed row the budget moves no wait.
///
/// **And a silence is spared** (`ReconnectBackoff::with_silence_spared`, the shared pump's stall rule): a session that
/// waits out `READ_IDLE` is ended and reopened on the same wait, from the cursor, but reads `Connecting` at `info`,
/// never `Error`. Alpaca sends `:heartbeat` comment lines (which `LineStream` reads as proof of life) and documents no
/// cadence for them: the SSE page shows them only in its samples (<https://docs.alpaca.markets/docs/sse-events>), the
/// Broker API FAQ says only that a "heartbeat" lets partners know the connection is alive
/// (<https://docs.alpaca.markets/docs/broker-api-faq>), and Alpaca's own guide says "the cadence isn't documented, so
/// measure it rather than hardcoding a timeout"
/// (<https://github.com/alpacahq/alpaca-skills/blob/main/skills/broker-api/sse-events/SKILL.md>). Unmeasured, an idle
/// account's quiet stream would read as an outage every 30 s.
// shortcut: the silence is spared on an unmeasured cadence; measure the heartbeat on the sandbox, and once it is known to
// come well inside READ_IDLE drop the mode, so a silent stream is a stall again.
#[doc(hidden)]
pub fn trade_events_backoff() -> ReconnectBackoff {
    ReconnectBackoff::from_row(&RECONNECT).with_refusal_budget(1).with_silence_spared()
}

/// The stream the pump reads: the response body's lines as server-sent events (a read that waits out
/// `timeout_per_read` is the pump's idle tick, `LineStream`'s module doc).
type TradeEvents = LineStream<BufReader<ureq::BodyReader<'static>>>;

/// The SSE agent: `crates/vike-transport/src/http.rs`'s `stream_agent` (its doc carries the contract: the per-read
/// idle bound, the connect bound as a shutdown fix, no total, no redirect) with this lane's choices: no head bound
/// (the per-read bound caps that wait too) and 4xx/5xx as `Err(StatusCode)`, which [`open_trade_events`]' 401 arm
/// reads. `connect` and `read_idle` are parameters so the tests build the same agent with short bounds.
fn sse_agent(connect: Duration, read_idle: Duration) -> ureq::Agent {
    vike_transport::http::stream_agent(
        connect,
        None,
        read_idle,
        vike_transport::http::StatusPolicy::Error,
    )
}

/// The reconnect cursor of the event stream (the module doc): the last event id the decode saw, and the boundary
/// event the open in progress will have re-delivered. Shared by the open and the decode of one pump.
#[derive(Debug, Default)]
pub struct TradeEventsCursor {
    last: Option<String>,
    boundary: Option<String>,
}

impl TradeEventsCursor {
    /// The URL of the next open from `base` (the broker host), arming the skip of the boundary event it re-delivers:
    /// bare until an event was seen, `?since_id=<last event id>` after.
    pub fn open_url(&mut self, base: &str) -> String {
        self.boundary.clone_from(&self.last);
        events_url(base, self.last.as_deref())
    }

    /// Whether `frame` is to be decoded: it moves the cursor to its `event_id`, and the one re-delivered boundary event
    /// of the open is dropped. A frame with no `event_id` is decoded and moves nothing.
    pub fn admit(&mut self, frame: &Value) -> bool {
        let Some(id) = event_id_of(frame) else { return true };
        let boundary = self.boundary.as_deref() == Some(id.as_str());
        if boundary {
            self.boundary = None;
        }
        self.last = Some(id);
        !boundary
    }
}

/// One open of the event stream (run by the pump, once per session): mint the Bearer, ask from the cursor and hand the
/// pump the body as lines. A 401 force-refreshes the token and is a refused open (the row's wait, then a new try); a
/// token that will not mint, a non-2xx answer and a failed connect are transport failures. Nothing here is fatal: the
/// pump never gives up on this stream.
pub(crate) fn open_trade_events(
    agent: &ureq::Agent,
    base: &str,
    token: &TokenSource,
    cursor: &Mutex<TradeEventsCursor>,
) -> WebsocketSessionUserOpenOutcome<TradeEvents> {
    let bearer = match token.bearer() {
        Ok(bearer) => bearer,
        // The mint's words carry its answer's status, never a credential.
        Err(e) => {
            return WebsocketSessionUserOpenOutcome::Transport(format!("token mint failed: {e}"));
        }
    };
    let url = lock(cursor).open_url(base);
    let response = agent
        .get(&url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Accept", "text/event-stream")
        .call();
    match response {
        Ok(r) if r.status().is_success() => {
            WebsocketSessionUserOpenOutcome::Ready(LineStream::new(
                BufReader::new(r.into_body().into_reader()),
                LineFraming::ServerSentEvents,
            ))
        }
        // ⚠ ureq answers a 4xx or 5xx as `Err(StatusCode)` (its `http_status_as_error` default), never as a response:
        // an `Ok` arm for the 401 was never reached, so an expired token was retried unrefreshed. B-13: a refused open
        // waits like a failed one (the row's wait), where it used to retry at once.
        Err(ureq::Error::StatusCode(401)) => {
            let _ = token.force_refresh();
            WebsocketSessionUserOpenOutcome::Refused(
                AlpacaStreamRefusal::TokenRefreshed.to_string(),
            )
        }
        // A 3xx (no redirect is followed), a 4xx or a 5xx.
        Ok(r) => {
            let status = r.status().as_u16();
            WebsocketSessionUserOpenOutcome::Transport(
                AlpacaStreamRefusal::NonSuccess { status }.to_string(),
            )
        }
        Err(ureq::Error::StatusCode(status)) => WebsocketSessionUserOpenOutcome::Transport(
            AlpacaStreamRefusal::NonSuccess { status }.to_string(),
        ),
        // ureq's words name the host and the cause, never a header: the Bearer is not in them.
        Err(e) => WebsocketSessionUserOpenOutcome::Transport(
            AlpacaStreamRefusal::ConnectFailed { why: e.to_string() }.to_string(),
        ),
    }
}

/// The exec event lane's pump (the module doc): the shared private stream with no REST replay, on its own thread
/// (`alpaca-userdata`, reporting the account's stream link). Its fills and order ends go to `events`, each labelled
/// through `symbols`. The exec thread stops and joins it ([`WebsocketSessionUserHandle::shutdown`]).
pub(crate) fn spawn_trade_events(
    config: &AlpacaConfig,
    token: Arc<TokenSource>,
    events: EventSender,
    symbols: Arc<OrderClientIdSymbols>,
) -> WebsocketSessionUserHandle {
    let agent = sse_agent(CONNECT_TIMEOUT, READ_IDLE);
    let base = config.hosts.broker.to_string();
    let cursor = Arc::new(Mutex::new(TradeEventsCursor::default()));
    let decoded = Arc::clone(&cursor);
    spawn_user(WebsocketSessionUserSpec {
        venue: "alpaca",
        lane: "alpaca",
        // One stream of the whole account: the thread is `alpaca-userdata`.
        symbol: String::new(),
        events,
        // `OrderSubmitted` goes out before the POST, so no fill can precede it (no fence).
        fence: None,
        open: move |_stop: &AtomicBool| open_trade_events(&agent, &base, &token, &cursor),
        decode: move |frame: &Value| {
            if lock(&decoded).admit(frame) {
                decode_trade_event(frame, &symbols)
            } else {
                Vec::new()
            }
        },
        // HTTP has no ping: the read-idle bound is the only liveness check.
        keepalive: None,
        idle_threshold: Some(READ_IDLE),
        backoff: trade_events_backoff(),
        // The cursor re-sends the gap inside the open, and the row pokes no reconcile.
        on_reconnect: || {},
    })
}

/// Build the events-stream URL: bare on first connect, `?since_id=<ulid>` to backfill on reconnect.
/// `since_id` is the sortable ULID `event_id`; the endpoint rejects a non-ULID (`400`).
fn events_url(base: &str, since_id: Option<&str>) -> String {
    match since_id {
        Some(id) => format!("{base}/v2/events/trades?since_id={id}"),
        None => format!("{base}/v2/events/trades"),
    }
}

/// The top-level `event_id` (a ULID) of one `/v2/events/trades` event object, if present.
fn event_id_of(v: &Value) -> Option<String> {
    v.get("event_id").and_then(|x| x.as_str()).map(str::to_string)
}

#[path = "user_data_tests.rs"]
#[cfg(test)]
mod user_data_tests;
