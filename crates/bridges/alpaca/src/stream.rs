//! SSE reader for `/v2/events/trades` (v1 is deprecated → 410). Runs on its own thread; each
//! `data: {json}` frame decodes via `event_mapper::decode_trade_event`. Reconnects on drop/timeout/
//! auth failure. Shape adapted from `crates/bridges/oanda/src/stream.rs`.
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
use std::time::Duration;

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;

use crate::auth::TokenSource;
use crate::config::AlpacaConfig;
use crate::event_mapper::decode_trade_event;

/// Persistent SSE driver: hold `GET {broker}/v2/events/trades`, decode each `data: {json}` frame
/// via [`decode_trade_event`], and emit fills/cancels/accepts via `events` until `stop`.
/// Reconnects on disconnect/read-timeout; on a 401 it force-refreshes the token before retrying.
/// Blocks — run on a dedicated thread.
///
/// How long the SSE dial may sit in `connect(2)` before this thread gets its `stop` check back.
///
/// It exists because `ureq`'s default is NO connect timeout at all, which makes this thread's stop
/// latency the OS connect timeout (~130 s on Linux) against an unreachable host — see the builder
/// below for the full shutdown argument. Five seconds is ~an order of magnitude over any healthy
/// connect, so a live stream never notices it.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The SSE agent, with `connect` as a PARAMETER so the bound is testable.
///
/// ⚠ It is a separate fn purely so `the_sse_agent_gives_up_on_a_dial_that_never_completes` can
/// build the same agent with a short bound and prove the dial actually gives up. Inlined in the
/// builder it was, deleting `.timeout_connect(…)` compiled clean and no test noticed — which is the
/// shape of the bug this fixes, so leaving it unkillable would have been the same mistake twice.
fn sse_agent(connect: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_recv_body(Some(Duration::from_secs(30)))
        // ⚠ THE CONNECT BOUND, and it is a SHUTDOWN fix rather than a networking nicety.
        //
        // `ureq`'s `Timeouts::default()` leaves `connect: None`, so without this the dial below
        // parks in the kernel for the OS connect timeout — ~130 s on Linux — with an unreachable
        // host. The `stop` flag is only read BETWEEN calls (the `while` head and the per-line check
        // inside the stream), so for that whole window this thread cannot observe a stop at all.
        // `ExecActor::stop` (`crates/vike-bridge-core/src/exec_actor.rs`) then joins it with NO
        // timeout, on the core thread, inside `CoreHandle::shutdown_and_join` — i.e. inside
        // `vike_ops::shutdown::run_with_deadline`'s SEQUENTIAL TAIL, which the task counter cannot
        // see. That is how a stop can blow `shutdown_deadline_ms` while the daemon truthfully
        // reports `0 task(s) still in flight`: the thread holding it was never a counted task.
        //
        // The measured condition is not hypothetical — every Alpaca host was TCP-unreachable from
        // the box in the alpaca+ctrader live rehearsal (PR #1407, Evidence 4: DNS resolves, TCP 443
        // times out on BOTH tiers). 5 s is ~an order of magnitude over any healthy connect and well
        // inside `DaemonProfile::shutdown_deadline`'s 5 s default plus the reconnect backoff, so a
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

pub fn stream_trade_events(
    config: AlpacaConfig,
    token: Arc<TokenSource>,
    events: EventSender,
    stop: Arc<AtomicBool>,
) {
    let base = config.hosts.broker;
    let agent = sse_agent(CONNECT_TIMEOUT);

    // Reconnect watermark: the ULID of the last event processed. `None` until the first event, so
    // the initial connect opens bare (live from now); every reconnect backfills via `since_id`.
    let mut last_event_id: Option<String> = None;

    while !stop.load(Ordering::Relaxed) {
        let bearer = match token.bearer() {
            Ok(t) => format!("Bearer {t}"),
            Err(_) => {
                tracing::warn!(
                    venue = "alpaca",
                    "SSE /v2/events/trades reconnect: token mint failed"
                );
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
        };
        // Open with `?since_id=<last>` to backfill the reconnect gap. `since_id` is INCLUSIVE, so
        // the server re-delivers the boundary event — skip exactly that one (its id == what we
        // opened with) so it isn't re-processed.
        let url = events_url(base, last_event_id.as_deref());
        let mut skip_boundary = last_event_id.clone();
        let resp = agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept", "text/event-stream")
            .call();
        let resp = match resp {
            Ok(r) if (200..300).contains(&r.status().as_u16()) => r,
            Ok(r) if r.status().as_u16() == 401 => {
                tracing::warn!(
                    venue = "alpaca",
                    "SSE /v2/events/trades reconnect: 401, refreshing token"
                );
                let _ = token.force_refresh();
                continue;
            }
            Ok(r) => {
                tracing::warn!(
                    venue = "alpaca",
                    status = r.status().as_u16(),
                    "SSE /v2/events/trades reconnect: non-2xx response"
                );
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
            Err(_) => {
                tracing::warn!(venue = "alpaca", "SSE /v2/events/trades reconnect: connect failed");
                sleep_unless_stopped(&stop, Duration::from_secs(2));
                continue;
            }
        };
        let reader = BufReader::new(resp.into_body().into_reader());
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let line = match line {
                Ok(l) => l,
                Err(_) => {
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
                for ev in decode_trade_event(&v) {
                    if events.blocking_send(ev).is_err() {
                        return; // core gone
                    }
                }
            }
        }
        // Backoff before reconnecting after a mid-stream disconnect. The connect-failure paths
        // above already sleep; this covers the in-stream `break` so a flapping connection (accepts
        // then immediately drops) can't hot-loop reconnects.
        sleep_unless_stopped(&stop, Duration::from_secs(1));
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

    /// THE CONNECT BOUND, proven by BEHAVIOUR rather than by the constant being present.
    ///
    /// ⚠ The defect this fixes is precisely "a timeout that is not set, in code that compiles":
    /// `ureq`'s `Timeouts::default()` leaves `connect: None`, so the dial parked in the kernel for
    /// the OS connect timeout (~130 s on Linux) with the `stop` flag unread — and
    /// `ExecActor::stop` joins this thread with no timeout inside `run_with_deadline`'s sequential
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
}
