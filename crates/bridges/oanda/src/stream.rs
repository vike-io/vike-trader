//! OANDA transactions-stream decode — the delayed LIMIT/STOP fills the order-POST response can't
//! carry. `GET /v3/accounts/{acct}/transactions/stream` delivers one JSON transaction per line
//! (plus HEARTBEATs); `ORDER_FILL` → a fill, `ORDER_CANCEL` → a cancel. Pure decode; the persistent
//! chunked-GET transport is the driver's job (mirrors the WS decode seams).
//!
//! The fill references the order by `orderID` (venue id) and echoes the order's
//! `clientExtensions.id` when set (vike sets it = client_order_id), so both ids are available.

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_bridge_core::ErrorKind;
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, TradeId};

use crate::config::OandaConfig;
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// Idle bound on the chunked transactions stream: 4 missed ~5 s heartbeats, the same value and the
/// same `timeout_recv_body` mechanism `crate::market_feed`'s quote lane dials with.
const RECV_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Bounded dial, so a black-holed route can never hold the exec event lane for the OS's own SYN
/// ladder — the ceiling `crate::market_feed` and `vike_bridge_core::pump_spec` both pin.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Fixed stop-aware reconnect delay between opens (walked in [`sleep_unless_stopped`]'s slices).
const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
/// How many opens the venue may reject TERMINALLY, back to back, before this lane gives up.
///
/// A bound rather than stop-on-first because a single 401/403 can be a momentary venue- or
/// edge-side answer, and a bound rather than none because a genuinely bad or revoked token answers
/// the same way forever: unbounded retry is what made this failure invisible. Counted since the
/// last SUCCESSFUL open, NOT since the last failure of any kind — a 500 interleaved between auth
/// rejections says nothing about the token, and resetting on one would let `401, 500, 401, 500, …`
/// loop past the bound, which is the shape being closed.
const MAX_TERMINAL_OPENS: u32 = 3;

fn client_order_id(v: &serde_json::Value) -> String {
    v.pointer("/clientExtensions/id")
        .and_then(|c| c.as_str())
        .or_else(|| v.get("clientOrderID").and_then(|c| c.as_str()))
        .or_else(|| v.get("orderID").and_then(|c| c.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Parse an `ORDER_FILL` transaction into its [`FillEvent`] (shared by the stream and the
/// order-POST response mapper so a delayed fill and an inline fill build identically).
///
/// `None` when the transaction carries no `id`. That field is OANDA's transaction id — unique and
/// monotonic per account, present on every v20 transaction — and it is this venue's ONLY per-fill
/// identity: `orderID` is shared by every fill of a multi-fill order, and `time` is not an identity
/// at all, so there is nothing here to synthesize a replay-stable id FROM. The honest answer is
/// therefore to refuse the frame rather than mint one.
///
/// ⚠ This used to read `s("id").unwrap_or_default()`, i.e. an absent `id` became `""` — and an empty
/// `trade_id` did not dedup badly, it SKIPPED `ExecutionEngine`'s dedup entirely, so every
/// reconnect replay of that fill re-booked its commission and realized PnL. This venue replays by
/// design (the A3 `sinceid` backfill re-decodes the exact missed transactions through this very
/// function), so the id being real is what makes that backfill safe.
pub(super) fn fill_from_transaction(v: &serde_json::Value) -> Option<FillEvent> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    let coid = client_order_id(v);
    let units: f64 = s("units").and_then(|u| u.parse().ok()).unwrap_or(0.0);
    let price: f64 = s("price").and_then(|p| p.parse().ok()).unwrap_or(0.0);
    let commission: f64 = s("commission").and_then(|c| c.parse().ok()).unwrap_or(0.0);
    let trade_id = match TradeId::new(s("id").unwrap_or_default()) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                %coid,
                "ORDER_FILL transaction carries no `id` — dropping the fill rather than folding an \
                 un-dedupable one (it would re-book on every reconnect backfill)"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: coid,
        venue: VENUE.to_string().into(),
        symbol: s("instrument").unwrap_or_default().to_string().into(),
        side: if units >= 0.0 { 1 } else { -1 },
        last_qty: units.abs(),
        last_px: price,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    })
}

/// Decode one transaction-stream line into the vike events it implies. An `ORDER_FILL`
/// dual-publishes the bare [`Event::Fill`] (the Account folds it into position/PnL) AND the
/// [`Event::OrderFilled`] wrap (the order FSM applies it) — the same contract the crypto WS
/// mappers honor, so a live fill folds identically to a replayed one. `ORDER_CANCEL` → one
/// [`Event::OrderCanceled`]. HEARTBEAT / echoes → empty.
pub fn decode_transaction_events(v: &serde_json::Value) -> Vec<Event> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    match s("type") {
        Some("ORDER_FILL") => {
            // An id-less fill yields NO events at all — not a half-published pair. The bare Fill
            // and its wrap must stand or fall together (they are the Account's copy and the FSM's
            // copy of one execution), so a refused frame drops both.
            let Some(fill) = fill_from_transaction(v) else { return Vec::new() };
            let wrap = Event::OrderFilled(OrderFilled {
                client_order_id: fill.client_order_id.clone(),
                fill: fill.clone(),
                ts: fill.ts,
            });
            vec![Event::Fill(fill), wrap]
        }
        Some("ORDER_CANCEL") => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id(v),
            reason: s("reason").unwrap_or_default().to_string().into(),
            ts,
        })],
        _ => Vec::new(), // HEARTBEAT, ORDER_CREATE echoes, etc.
    }
}

/// The transactions-stream agent. Hand-built for the same reason [`crate::market_feed`]'s is: a
/// chunked GET cannot live under `vike_bridge_core::http::blocking_agent`'s global request timeout.
///
/// ⚠ **`http_status_as_error(false)` is this lane's own decision, stated here beside it** — the
/// two chunked-GET lanes each state their agent policy rather than sharing code, and this one used
/// to differ: without it a 401/403 arrives as a transport ERROR indistinguishable from a dead
/// route, which is how a rejected open came to be retried forever with nothing logged. Non-2xx as
/// a RESPONSE is what lets [`open_failure_kind`] tell "the token is wrong" from "the network
/// blinked", and only the first of those is worth giving up on.
fn transactions_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(RECV_IDLE_TIMEOUT))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// Classify one failed stream open into the SHARED taxonomy
/// (`vike_bridge_core::transport::ErrorKind`) rather than a vocabulary local to this venue:
/// `Some(status)` is the venue's own answer, `None` a pre-send transport failure (the `code == 0`
/// case that taxonomy already spells [`ErrorKind::Network`]).
///
/// The verdict this lane acts on is [`ErrorKind::is_terminal`] — "re-issuing as-is cannot help".
/// 401/403 → [`ErrorKind::Auth`] is terminal and bounded by [`MAX_TERMINAL_OPENS`]; 429/5xx and a
/// dead route are retryable and loop unchanged. An unmapped status falls to `Unknown`, which that
/// taxonomy makes terminal BY DESIGN — the safe posture is to stop and say so, never to spin.
fn open_failure_kind(status: Option<u16>) -> ErrorKind {
    match status {
        Some(s) => ErrorKind::from_http_status(s),
        None => ErrorKind::Network,
    }
}

/// Persistent transactions-stream driver: hold `GET /v3/accounts/{id}/transactions/stream`, decode
/// each line via [`decode_transaction_events`], and emit fills/cancels via `events` until `stop`. Reconnects
/// on disconnect. Blocks — run on a dedicated thread. HEARTBEATs (~5s) keep the read loop ticking so
/// `stop` is honoured; a silently dead connection is bounded by `timeout_recv_body`.
///
/// **A REJECTED open is bounded and audible.** Every failed open is logged with its status and its
/// [`open_failure_kind`] classification; a retryable one (429/5xx/unreachable) backs off and loops
/// as before, while [`MAX_TERMINAL_OPENS`] consecutive terminal ones (401/403 — bad or expired
/// token, wrong account) log at `error!` and END the lane. This function previously retried a
/// rejection forever with no log at all: the exec thread's REST path kept working, so MARKET orders
/// filled normally while every delayed LIMIT/STOP fill and every venue-side cancel silently stopped
/// arriving — a failure no test in this crate can see (its stream coverage is offline decode) and
/// no operator could either.
///
/// Audit A3: the stream carries only transactions from connect-time and never replays, so a
/// terminal (fill/cancel) that lands during a reconnect window is lost. `last_seen` is the shared
/// high-watermark transaction id (also advanced by the exec POST path); on a RE-open (never the
/// first open) the driver backfills `GET /transactions/sinceid?id={last_seen}` — the EXACT missed
/// transactions — through the SAME decode path, and the core's `trade_id`/FSM dedup absorbs the
/// overlap with the freshly-reopened stream. `last_seen == 0` (nothing seen yet) skips the backfill.
pub fn stream_transactions(
    config: OandaConfig,
    events: EventSender,
    stop: Arc<AtomicBool>,
    last_seen: Arc<AtomicU64>,
) {
    let agent = transactions_agent();
    let rest = OandaRest::new(config.api_token.clone()); // bounded (30s global) backfill transport
    let url =
        format!("{}/v3/accounts/{}/transactions/stream", config.stream_base, config.account_id);
    let bearer = format!("Bearer {}", config.api_token);
    let mut opened_once = false;
    let mut terminal_opens = 0_u32;

    while !stop.load(Ordering::Relaxed) {
        let opened = match agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept-Datetime-Format", "UNIX")
            .call()
        {
            // `http_status_as_error(false)`: a rejection arrives HERE, as a response carrying its
            // status, so it is classified rather than blurred into the transport arm below.
            Ok(r) => {
                let status = r.status().as_u16();
                if (200..300).contains(&status) {
                    Ok(r)
                } else {
                    Err(Some(status))
                }
            }
            Err(_) => Err(None), // pre-send / connect / TLS: no status exists
        };
        let resp = match opened {
            Ok(r) => {
                terminal_opens = 0; // the token works; any earlier rejection is not standing
                r
            }
            Err(status) => {
                let kind = open_failure_kind(status);
                if !kind.is_terminal() {
                    // Throttle, venue 5xx, unreachable host: retrying as-is is the right answer,
                    // so this arm keeps looping exactly as before — it is now merely AUDIBLE.
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        "transactions-stream open failed; backing off and retrying"
                    );
                } else {
                    terminal_opens += 1;
                    if terminal_opens >= MAX_TERMINAL_OPENS {
                        // ⚠ The asymmetry is the reason this line is an `error!` and spells the
                        // consequence out: MARKET orders keep working (their fill rides the order
                        // POST reply), so the venue looks healthy from the exec thread while every
                        // resting LIMIT/STOP fill and every venue-side cancel stops arriving and
                        // the orders sit accepted forever.
                        tracing::error!(
                            target: "vike_oanda::stream",
                            venue = VENUE,
                            status,
                            kind = %kind,
                            attempts = terminal_opens,
                            "transactions-stream open REJECTED and not retryable (401/403: bad or \
                             expired token / wrong account) — giving up on this lane. Delayed \
                             LIMIT/STOP fills and venue-side cancels will NOT arrive for the rest \
                             of this session; MARKET fills are unaffected (they ride the order POST \
                             reply). Fix the OANDA_DEMO_* credentials and restart."
                        );
                        return;
                    }
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        attempts = terminal_opens,
                        max_attempts = MAX_TERMINAL_OPENS,
                        "transactions-stream open rejected; retrying within the bounded budget"
                    );
                }
                sleep_unless_stopped(&stop, RECONNECT_BACKOFF);
                continue;
            }
        };
        // A3 resync: on a re-open, recover the reconnect gap before reading live. The stream
        // connection is already open (so nothing after it is missed); the backfill + stream
        // overlap is deduped by the core.
        if opened_once && resync_gap(&rest, &config, &events, &last_seen).is_break() {
            return; // core gone during backfill
        }
        opened_once = true;
        let reader = BufReader::new(resp.into_body().into_reader());
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let line = match line {
                Ok(l) => l,
                Err(_) => break, // recv timeout / disconnect → reconnect
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                note_transaction_id(&last_seen, &v); // advance the A3 watermark
                for ev in decode_transaction_events(&v) {
                    if events.blocking_send(ev).is_err() {
                        return; // core gone
                    }
                }
            }
        }
    }
}

/// Advance the shared A3 watermark to the max of itself and this transaction's `id`.
fn note_transaction_id(last_seen: &Arc<AtomicU64>, v: &serde_json::Value) {
    if let Some(id) = v.get("id").and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()) {
        last_seen.fetch_max(id, Ordering::Relaxed);
    }
}

/// Backfill and replay the reconnect gap: `GET /transactions/sinceid?id={last_seen}` (exclusive of
/// `last_seen`, so no duplicate of the watermark), mapped through the live decode path. Returns
/// `Break` if the core went away mid-replay (caller stops). A REST error is logged and skipped —
/// the next reconnect retries. `last_seen == 0` (nothing seen yet) is a no-op.
fn resync_gap(
    rest: &OandaRest,
    config: &OandaConfig,
    events: &EventSender,
    last_seen: &Arc<AtomicU64>,
) -> std::ops::ControlFlow<()> {
    let since = last_seen.load(Ordering::Relaxed);
    if since == 0 {
        return std::ops::ControlFlow::Continue(());
    }
    let path = format!("/v3/accounts/{}/transactions/sinceid", config.account_id);
    let query = format!("id={since}");
    match rest.get(&config.rest_base, &path, &query) {
        Ok(resp) => {
            if let Some(max) = crate::history::max_transaction_id(&resp) {
                last_seen.fetch_max(max, Ordering::Relaxed);
            }
            for ev in crate::history::map_transactions_since(&resp) {
                if events.blocking_send(ev).is_err() {
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_oanda::stream",
                error = %e,
                "A3 reconnect resync backfill failed; retrying on next reconnect"
            );
        }
    }
    std::ops::ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_fill_transaction_dual_publishes_fill_and_wrap() {
        let v = serde_json::json!({
            "type": "ORDER_FILL", "id": "6373", "time": "1478012400.000000000",
            "orderID": "6372", "instrument": "EUR_USD", "units": "-1000", "price": "1.09300",
            "commission": "0.02", "clientExtensions": {"id": "coid-9"}
        });
        // Dual-publish contract: a fill yields BOTH the bare FillEvent (the Account folds
        // it into position/PnL) AND the OrderFilled wrap (the order FSM applies it), both
        // carrying the same FillEvent (same trade_id, so the two dedup sets key correctly).
        let evs = decode_transaction_events(&v);
        assert_eq!(evs.len(), 2, "fill must emit bare Fill + wrap");
        match &evs[0] {
            Event::Fill(fill) => {
                assert_eq!(fill.client_order_id, "coid-9"); // clientExtensions.id preferred
                assert_eq!(fill.trade_id, "6373");
                assert_eq!(fill.side, -1); // negative units → sell
                assert_eq!(fill.last_qty, 1000.0);
                assert_eq!(fill.last_px, 1.093);
                assert_eq!(fill.commission, 0.02);
                assert_eq!(fill.ts, 1_478_012_400_000);
            }
            other => panic!("expected bare Fill first, got {other:?}"),
        }
        match &evs[1] {
            Event::OrderFilled(of) => {
                assert_eq!(of.client_order_id, "coid-9");
                assert_eq!(of.fill.trade_id, "6373"); // same fill on the wrap
                assert_eq!(of.fill.side, -1);
            }
            other => panic!("expected OrderFilled wrap second, got {other:?}"),
        }
    }

    #[test]
    fn order_cancel_transaction_maps_to_single_cancel() {
        let v = serde_json::json!({
            "type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "6372", "reason": "CLIENT_REQUEST"
        });
        let evs = decode_transaction_events(&v);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.reason == "CLIENT_REQUEST"));
    }

    #[test]
    fn heartbeat_is_ignored() {
        assert!(decode_transaction_events(&serde_json::json!({"type": "HEARTBEAT", "time": "1"}))
            .is_empty());
    }

    // --- the rejected open: classified, logged, bounded ---------------------------------------
    //
    // These drive the REAL [`stream_transactions`] against a loopback server that answers a
    // scripted status per open. They live here rather than in `tests/` because that is where the
    // driver is reachable — this crate promotes only its PURE helpers to the crate root — and
    // because the thing under test is the loop, not a decode.

    use std::io::Write;
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::thread;

    /// How long a bounded lane is given to give up. Generous against the real budget
    /// ([`MAX_TERMINAL_OPENS`] opens with [`RECONNECT_BACKOFF`] between them) so a slow runner
    /// cannot flake it, and finite so the pre-fix behaviour — retry FOREVER — fails the test
    /// instead of hanging the suite.
    const LANE_GIVE_UP_BUDGET: Duration = Duration::from_secs(30);

    /// A loopback HTTP server answering each open with the next status in `script`, then closing.
    /// Its join value is how many opens it actually served — the count each test asserts on.
    fn spawn_scripted_server(script: Vec<u16>) -> (SocketAddr, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let handle = thread::spawn(move || {
            let mut served = 0_usize;
            for status in script {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let head = format!(
                    "HTTP/1.1 {status} Rejected\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.flush();
                served += 1;
            }
            served
        });
        (addr, handle)
    }

    /// Run the REAL driver against `addr` on its own thread. The receiver fires exactly once
    /// [`stream_transactions`] RETURNS, which is the property under test.
    fn spawn_lane(addr: SocketAddr, stop: Arc<AtomicBool>) -> mpsc::Receiver<()> {
        let (done_tx, done_rx) = mpsc::channel();
        let config = OandaConfig {
            api_token: "bad-token".to_string(),
            account_id: "101-004-1234567-001".to_string(),
            rest_base: format!("http://{addr}"),
            stream_base: format!("http://{addr}"),
        };
        thread::spawn(move || {
            let (events, _ingest) = vike_exec::event_channel(16);
            stream_transactions(config, events, stop, Arc::new(AtomicU64::new(0)));
            let _ = done_tx.send(());
        });
        done_rx
    }

    /// The shared taxonomy answers, and the split this lane acts on: a rejection that re-issuing
    /// cannot fix is terminal, everything transient keeps looping. Pinned as a unit so a widened
    /// terminal set (which would abandon the lane over a rate limit) cannot land quietly.
    #[test]
    fn open_failures_classify_into_the_shared_taxonomy() {
        for status in [401, 403] {
            let kind = open_failure_kind(Some(status));
            assert_eq!(kind, ErrorKind::Auth, "HTTP {status} on a stream open is an auth failure");
            assert!(kind.is_terminal(), "a bad token answers the same way forever");
        }
        for status in [429, 500, 503] {
            assert!(
                !open_failure_kind(Some(status)).is_terminal(),
                "HTTP {status} is transient — this lane must keep retrying it"
            );
        }
        // No status at all: DNS/connect/TLS, the taxonomy's `code == 0` case. Retryable.
        assert_eq!(open_failure_kind(None), ErrorKind::Network);
        assert!(!open_failure_kind(None).is_terminal());
    }

    /// **THE TRAP, closed.** A venue that rejects the open outright used to be retried forever
    /// with nothing logged. The lane now gives up after a bounded number of terminal rejections —
    /// so this test TERMINATES, which is the whole assertion. (Before the fix it never returned.)
    #[test]
    fn a_permanently_rejected_open_gives_up_after_the_bounded_budget() {
        let (addr, server) = spawn_scripted_server(vec![401; MAX_TERMINAL_OPENS as usize]);
        let stop = Arc::new(AtomicBool::new(false));
        let done = spawn_lane(addr, stop.clone());

        done.recv_timeout(LANE_GIVE_UP_BUDGET)
            .expect("the lane must GIVE UP on a permanently rejected open, not retry forever");
        assert_eq!(
            server.join().expect("server thread joins"),
            MAX_TERMINAL_OPENS as usize,
            "the lane spends exactly its budget of opens before abandoning"
        );
        assert!(!stop.load(Ordering::Relaxed), "it gave up on its own, not because of a stop");
    }

    /// The COMPLEMENT, and the reason the split is not "abandon on any failed open": a transient
    /// rejection must keep looping past the terminal budget. Serving one more 500 than
    /// [`MAX_TERMINAL_OPENS`] proves the lane did not treat it as terminal.
    #[test]
    fn a_transient_rejection_keeps_retrying_past_the_terminal_budget() {
        let opens = MAX_TERMINAL_OPENS as usize + 1;
        let (addr, server) = spawn_scripted_server(vec![500; opens]);
        let stop = Arc::new(AtomicBool::new(false));
        let done = spawn_lane(addr, stop.clone());

        assert_eq!(
            server.join().expect("server thread joins"),
            opens,
            "a 5xx is retryable — the lane must still be dialling after the terminal budget"
        );
        // The listener is gone now, so further opens fail pre-send (also retryable): stop it.
        stop.store(true, Ordering::Relaxed);
        done.recv_timeout(LANE_GIVE_UP_BUDGET).expect("a stopped lane returns");
    }

    /// **The counter resets on a SUCCESSFUL open, not on any non-terminal one** — the rule
    /// [`MAX_TERMINAL_OPENS`] states. Interleaving 5xx between auth rejections must NOT refill the
    /// budget, or `401, 500, 401, 500, …` reopens the forever-loop this fix closes.
    #[test]
    fn an_interleaved_transient_failure_does_not_refill_the_terminal_budget() {
        let script = vec![401, 500, 401, 500, 401];
        let (addr, server) = spawn_scripted_server(script.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let done = spawn_lane(addr, stop.clone());

        done.recv_timeout(LANE_GIVE_UP_BUDGET)
            .expect("the third auth rejection must end the lane despite the 5xx between them");
        assert_eq!(
            server.join().expect("server thread joins"),
            script.len(),
            "it gave up on the THIRD terminal open, having also spent the two transient ones"
        );
    }
}
