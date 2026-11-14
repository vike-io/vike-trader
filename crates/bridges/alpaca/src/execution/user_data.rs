//! SSE reader for `/v2/events/trades` (v1 is deprecated → 410). Runs on its own thread; each
//! `data: {json}` frame decodes via `execution::event_mapper::decode_trade_event`. Reconnects on drop/timeout/
//! auth failure. Shape adapted from `crates/bridges/oanda/src/execution/user_data.rs`.
//!
//! **Reconnect backfill watermark (verified live 2026-07-14).** Every event carries a top-level
//! `event_id` (a sortable ULID). On reconnect the reader re-opens with `?since_id=<last_event_id>`
//! to backfill events that landed during the gap (OANDA's `sinceid` A3 pattern). Live-verified: the
//! endpoint accepts `since_id=<ULID>` (`200`; a non-ULID like `1` is what returns `400`) and is
//! INCLUSIVE — it re-delivers the boundary event whose id equals `since_id`. So the reader skips
//! exactly that re-delivered boundary event (its `event_id` == the `since_id` it opened with) to
//! avoid re-processing it. This sits on top of two existing idempotency layers, not instead of them:
//! the engine's always-on `seen_trade_ids` guard drops any replayed FILL by `trade_id`, and the
//! order FSM drops replayed lifecycle transitions as invalid — so a boundary re-delivery is safe
//! even if the skip ever misses. First connect (`last_event_id == None`) opens bare = live from now.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::websocket_session::user::reconnect::{
    RECONNECT_LOG, ReconnectBackoff, ReconnectLog, ReconnectLogLine,
};
use vike_bridge_core::websocket_session::user::sleep_unless_stopped;
use vike_execution::EventSender;
use vike_venues::venue_timings::{self, VenueTimingsReconnect};

use crate::config::AlpacaConfig;
use crate::execution::event_mapper::decode_trade_event;
use crate::transport::sign::TokenSource;

/// How long the SSE dial may sit in `connect(2)` before this thread gets its `stop` check back: the
/// exec lane's `dial_timeout` in `vike_venues::venue_timings::ALPACA`.
///
/// It exists because `ureq`'s default is NO connect timeout at all, which makes this thread's stop
/// latency the OS connect timeout (~130 s on Linux) against an unreachable host — see the builder
/// below for the full shutdown argument. Five seconds is ~an order of magnitude over any healthy
/// connect, so a live stream never notices it.
pub(crate) const CONNECT_TIMEOUT: Duration = venue_timings::ALPACA.exec().dial_timeout();

/// The waits before the next try, the exec lane's `reconnect` row, walked by the shared
/// [`ReconnectBackoff`] (owner ruling B-13, one rule for every venue): after a failed open (a token that
/// would not mint, a non-2xx answer, a connect that failed), after a refused one (a 401, whose token is
/// refreshed first) and after a session that ended. The row is a fixed 2 s
/// (`the_execution_reconnect_is_the_fixed_wait_this_driver_reads`), so every one of them is 2 s.
const RECONNECT: VenueTimingsReconnect = venue_timings::ALPACA.exec().reconnect();

/// The SSE agent, with `connect` as a PARAMETER so the bound is testable.
///
/// ⚠ It is a separate fn purely so `the_sse_agent_gives_up_on_a_dial_that_never_completes` can
/// build the same agent with a short bound and prove the dial actually gives up. Inlined in the
/// builder it was, deleting `.timeout_connect(…)` compiled clean and no test noticed — which is the
/// shape of the bug this fixes, so leaving it unkillable would have been the same mistake twice.
fn sse_agent(connect: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // No redirect: ureq replays every header but `Authorization`/`Cookie` to a 3xx `Location`; a 3xx is a non-2xx arm below.
        .max_redirects(0)
        // The exec lane's `idle_threshold`: the read-idle bound of this long-lived stream.
        .timeout_recv_body(Some(venue_timings::ALPACA.exec().idle_threshold()))
        // ⚠ THE CONNECT BOUND, and it is a SHUTDOWN fix rather than a networking nicety.
        //
        // `ureq`'s `Timeouts::default()` leaves `connect: None`, so without this the dial below
        // parks in the kernel for the OS connect timeout — ~130 s on Linux — with an unreachable
        // host. The `stop` flag is only read BETWEEN calls (the `while` head and the per-line check
        // inside the stream), so for that whole window this thread cannot observe a stop at all.
        // `ExecutionClientActor::stop` (`crates/vike-bridge-core/src/exec_actor.rs`) then joins it with NO
        // timeout, on the core thread, inside `CoreHandle::shutdown_and_join` — i.e. inside
        // `vike_boot::shutdown::run_with_deadline`'s SEQUENTIAL TAIL, which the task counter cannot
        // see. That is how a stop can blow `shutdown_deadline_ms` while the daemon truthfully
        // reports `0 task(s) still in flight`: the thread holding it was never a counted task.
        //
        // The measured condition is not hypothetical — every Alpaca host was TCP-unreachable from
        // the box in the alpaca+ctrader live rehearsal (PR #1407, Evidence 4: DNS resolves, TCP 443
        // times out on BOTH tiers). 5 s is ~an order of magnitude over any healthy connect and well
        // inside `ProfileDaemon::shutdown_deadline`'s 5 s default plus the reconnect backoff, so a
        // stop is observed within seconds of any unreachable host.
        //
        // ⚠ Deliberately NOT `timeout_global`: this request IS a long-lived stream, and a global
        // timeout would cap the stream itself and force a reconnect every N seconds. `recv_body`
        // above is the read-idle bound; this one covers only the dial.
        .timeout_connect(Some(connect))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// Persistent SSE driver: hold `GET {broker}/v2/events/trades`, decode each `data: {json}` frame
/// via [`decode_trade_event`], and emit fills/cancels/accepts via `events` until `stop`.
/// Reconnects on disconnect/read-timeout; on a 401 it force-refreshes the token, then waits like any
/// refused open before retrying. Every wait is the shared [`ReconnectBackoff`] over the exec row's
/// reconnect, and a failed open's line is paced by [`RECONNECT_LOG`] (owner ruling B-13).
/// Blocks — run on a dedicated thread.
pub(crate) fn stream_trade_events(
    config: AlpacaConfig,
    token: Arc<TokenSource>,
    events: EventSender,
    stop: Arc<AtomicBool>,
    symbols: Arc<OrderClientIdSymbols>,
) {
    let base = config.hosts.broker.as_str();
    let agent = sse_agent(CONNECT_TIMEOUT);

    // Reconnect watermark: the ULID of the last event processed. `None` until the first event, so
    // the initial connect opens bare (live from now); every reconnect backfills via `since_id`.
    let mut last_event_id: Option<String> = None;
    let mut backoff = ReconnectBackoff::new(RECONNECT.initial, RECONNECT.max);
    let mut failed = FailedOpens::default();

    while !stop.load(Ordering::Relaxed) {
        let bearer = match token.bearer() {
            Ok(t) => format!("Bearer {t}"),
            Err(_) => {
                failed.say("token mint failed", None);
                sleep_unless_stopped(&stop, backoff.failed());
                continue;
            }
        };
        // Open with `?since_id=<last>` to backfill the reconnect gap. `since_id` is INCLUSIVE, so
        // the server re-delivers the boundary event — skip exactly that one (its id == what we
        // opened with) so it isn't re-processed.
        let url = events_url(base, last_event_id.as_deref());
        let mut skip_boundary = last_event_id.clone();
        let response = agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept", "text/event-stream")
            .call();
        let response = match response {
            Ok(r) if (200..300).contains(&r.status().as_u16()) => r,
            // B-13: a refused open waits like a failed one, where it used to retry at once (a hot
            // loop on a 401 that persists).
            Ok(r) if r.status().as_u16() == 401 => {
                failed.say("401, refreshing token", None);
                let _ = token.force_refresh();
                sleep_unless_stopped(&stop, backoff.refused());
                continue;
            }
            Ok(r) => {
                failed.say("non-2xx response", Some(r.status().as_u16()));
                sleep_unless_stopped(&stop, backoff.failed());
                continue;
            }
            Err(_) => {
                failed.say("connect failed", None);
                sleep_unless_stopped(&stop, backoff.failed());
                continue;
            }
        };
        backoff.opened();
        failed = FailedOpens::default();
        let opened = Instant::now();
        let reader = BufReader::new(response.into_body().into_reader());
        let session = vike_bridge_core::link_history::VenueLinkRecord::of(&events, "trade-events");
        let mut ended = "the stream's body ended".to_string();
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    ended = e.to_string();
                    tracing::warn!(
                        venue = "alpaca",
                        "SSE /v2/events/trades reconnect: read timeout/disconnect"
                    );
                    break; // timeout/disconnect → reconnect
                }
            };
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
                continue;
            }
            let json = match line.strip_prefix("data:") {
                Some(rest) => rest.trim(),
                None => continue,
            };
            if json.is_empty() || json == "[DONE]" {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                // Advance the reconnect watermark and drop the re-delivered `since_id` boundary.
                if let Some(eid) = event_id_of(&v) {
                    let is_boundary = skip_boundary.as_deref() == Some(eid.as_str());
                    last_event_id = Some(eid);
                    if is_boundary {
                        skip_boundary = None; // consume the one boundary re-delivery
                        continue;
                    }
                }
                for event in decode_trade_event(&v, &symbols) {
                    if events.blocking_send(event).is_err() {
                        return; // core gone
                    }
                }
            }
        }
        // Backoff before reconnecting after a mid-stream disconnect. The connect-failure paths
        // above already sleep; this covers the in-stream `break` so a flapping connection (accepts
        // then immediately drops) can't hot-loop reconnects. B-13's one rule: the ladder's next wait,
        // from its start again after a session that lived twice its ceiling.
        session.ended(&ended);
        sleep_unless_stopped(&stop, backoff.ended(opened.elapsed()));
    }
}

/// The failed opens of one outage, and its log pace: a line at the onset, then one per growing interval
/// ([`RECONNECT_LOG`], owner ruling B-13), where every failed open used to speak. A successful open starts
/// a new one.
struct FailedOpens {
    log: ReconnectLog,
    since: Instant,
    count: u32,
}

impl Default for FailedOpens {
    fn default() -> Self {
        Self { log: RECONNECT_LOG, since: Instant::now(), count: 0 }
    }
}

impl FailedOpens {
    /// One more failed open, `why` in this lane's words (and the answer's `status` for a non-2xx).
    fn say(&mut self, why: &str, status: Option<u16>) {
        self.count += 1;
        match self.log.tick(self.since.elapsed()) {
            ReconnectLogLine::Onset { next_in } => tracing::warn!(
                venue = "alpaca",
                status,
                next_line_in_s = next_in.as_secs(),
                "SSE /v2/events/trades reconnect: {why}"
            ),
            ReconnectLogLine::Heartbeat { down_for, next_in } => tracing::warn!(
                venue = "alpaca",
                status,
                failed_opens = self.count,
                down_for_s = down_for.as_secs(),
                next_line_in_s = next_in.as_secs(),
                "SSE /v2/events/trades still cannot open — reconnecting: {why}"
            ),
            ReconnectLogLine::Quiet => {}
        }
    }
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
fn event_id_of(v: &serde_json::Value) -> Option<String> {
    v.get("event_id").and_then(|x| x.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_log::loopback::redirecting_origin;

    const BASE: &str = "https://broker-api.sandbox.alpaca.markets";

    #[test]
    fn events_url_bare_on_first_connect() {
        assert_eq!(events_url(BASE, None), format!("{BASE}/v2/events/trades"));
    }

    #[test]
    fn events_url_backfills_with_since_id() {
        let ulid = "01KXFZXK45JRC3RSRXN12SEPRD";
        assert_eq!(
            events_url(BASE, Some(ulid)),
            format!("{BASE}/v2/events/trades?since_id={ulid}")
        );
    }

    #[test]
    fn event_id_extracted_when_present() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"event":"accepted","event_id":"01KXFZXK45JRC3RSRXN12SEPRD","order":{}}"#,
        )
        .unwrap();
        assert_eq!(event_id_of(&v).as_deref(), Some("01KXFZXK45JRC3RSRXN12SEPRD"));
    }

    #[test]
    fn event_id_none_when_absent() {
        let v: serde_json::Value = serde_json::from_str(r#"{"event":"fill","order":{}}"#).unwrap();
        assert_eq!(event_id_of(&v), None);
    }

    /// The SSE agent carries the exec row's two bounds, observed on the BUILT agent: the dial bound
    /// the driver passes in (`CONNECT_TIMEOUT`) and the read-idle bound of the long-lived body. The
    /// 5 s dial is not arbitrary — it is sized against `ExecutionClientActor::stop`'s join of this thread
    /// (the builder's comment) — so the effective value is pinned here as the literal, not as a
    /// restatement of the row.
    #[test]
    fn the_sse_agent_carries_the_execution_rows_bounds() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
        let timeouts = sse_agent(CONNECT_TIMEOUT).config().timeouts();
        assert_eq!(timeouts.connect, Some(Duration::from_secs(5)), "the dial bound");
        assert_eq!(timeouts.recv_body, Some(Duration::from_secs(30)), "the read-idle bound");
    }

    /// Every arm waits on the row's ladder, a FIXED 2 s: a failed open, a refused one (the 401 that
    /// used to retry at once) and the end of a session that lived any time at all.
    #[test]
    fn the_execution_reconnect_is_the_fixed_wait_this_driver_reads() {
        assert!(RECONNECT.is_fixed());
        let mut backoff = ReconnectBackoff::new(RECONNECT.initial, RECONNECT.max);
        let two = Duration::from_secs(2);
        assert_eq!((backoff.failed(), backoff.refused(), backoff.refused()), (two, two, two));
        backoff.opened();
        for lived in [Duration::ZERO, Duration::from_secs(3600)] {
            assert_eq!(backoff.ended(lived), two, "after a session of {lived:?}");
        }
    }

    /// An outage speaks at its onset and then on the shared pace, not on every 2 s retry: two hundred
    /// failed opens inside a few minutes say one line.
    #[test]
    fn an_outage_speaks_once_at_its_onset_and_not_on_every_retry() {
        let lines = vike_log::capture::captured(|| {
            let mut failed = FailedOpens::default();
            for _ in 0..200 {
                failed.say("connect failed", None);
            }
            failed.count
        });
        assert_eq!(lines.0, 200);
        assert_eq!(lines.1.len(), 1, "{:?}", lines.1);
    }

    /// THE CONNECT BOUND, proven by BEHAVIOUR rather than by the constant being present.
    ///
    /// ⚠ The defect this fixes is precisely "a timeout that is not set, in code that compiles":
    /// `ureq`'s `Timeouts::default()` leaves `connect: None`, so the dial parked in the kernel for
    /// the OS connect timeout (~130 s on Linux) with the `stop` flag unread — and
    /// `ExecutionClientActor::stop` joins this thread with no timeout inside `run_with_deadline`'s sequential
    /// TAIL, which the task counter cannot see. So a test that merely asserted `CONNECT_TIMEOUT`
    /// exists would restate the bug's own shape. This one DIALS and asserts the call gives up.
    ///
    /// The address is `203.0.113.1` — TEST-NET-3 (RFC 5737), reserved for documentation and routed
    /// nowhere — so this needs no internet and reaches no third party. Either outcome of a dial to
    /// it is a PASS as long as it is PROMPT: a silent blackhole hits the bound, and a router that
    /// answers "network unreachable" returns sooner. The only way to fail is to hang, which is
    /// exactly the regression. Bound is passed in short so the test costs a fraction of a second.
    #[test]
    fn the_sse_agent_gives_up_on_a_dial_that_never_completes() {
        let bound = Duration::from_millis(300);
        let agent = super::sse_agent(bound);
        let t0 = std::time::Instant::now();
        let outcome = agent.get("http://203.0.113.1:81/v2/events/trades").call();
        let waited = t0.elapsed();
        assert!(outcome.is_err(), "TEST-NET-3 is routed nowhere; a 2xx here means the URL moved");
        assert!(
            waited < bound * 10,
            "the dial took {waited:?} against a {bound:?} connect bound — the bound is not being \
             applied, so this thread cannot observe a stop while it is dialling an unreachable host"
        );
    }

    /// **A 3xx on the credentialed SSE open surfaces as a non-2xx response and the `Location` is
    /// never dialled.** ureq's default follows ten redirects and replays every header but
    /// `Authorization`/`Cookie` to the target.
    ///
    /// MUTATION PROOF: delete `.max_redirects(0)` from `sse_agent` — the 302 is followed and the
    /// second listener sees the replayed request.
    #[test]
    fn the_sse_agent_never_follows_a_redirect() {
        let (base, target_hits) = redirecting_origin();
        let response = sse_agent(CONNECT_TIMEOUT)
            .get(&events_url(&base, None))
            .header("Authorization", "Bearer test-token")
            .header("Accept", "text/event-stream")
            .call();
        assert_eq!(
            target_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the target was dialled"
        );
        assert_eq!(response.expect("a 302 is a response").status().as_u16(), 302);
    }
}
