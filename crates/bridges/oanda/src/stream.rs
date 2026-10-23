//! OANDA transactions-stream decode — the delayed LIMIT/STOP fills the order-POST response can't
//! carry. `GET /v3/accounts/{acct}/transactions/stream` delivers one JSON transaction per line
//! (plus HEARTBEATs); `ORDER_FILL` → a fill, `ORDER_CANCEL` → a cancel. Pure decode; the persistent
//! chunked-GET transport is the driver's job (mirrors the WS decode seams).
//!
//! The fill references the order by `orderID` (venue id) and echoes the order's
//! `clientExtensions.id` when set (vike sets it = client_order_id), so both ids are available.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, TradeId};
use vike_venues::venue_timings;

use crate::config::OandaConfig;
use crate::rest::OandaRest;
use crate::symbols::SubmittedSymbols;

const VENUE: &str = "oanda";

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

/// Whether an `ORDER_CANCEL` is the cancel half of an order REPLACE: OANDA stamps the replaced order's
/// cancel with `replacedByOrderID` and the reason `CLIENT_REQUEST_REPLACED`. Either marks it; a
/// plain cancel carries neither.
///
/// ⚠ A replace done OUTSIDE this process (the OANDA web platform, another API client) is skipped the
/// same way, so the platform's order stays resting under its old terms and venue id until reconcile
/// reads the venue's open orders; the alternative, terminating a possibly still-resting order,
/// is the worse error for a replacement that keeps the client id.
pub(crate) fn is_replace_cancel(v: &serde_json::Value) -> bool {
    v.get("replacedByOrderID").is_some()
        || v.get("reason").and_then(|r| r.as_str()) == Some("CLIENT_REQUEST_REPLACED")
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
pub(super) fn fill_from_transaction(
    v: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Option<FillEvent> {
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
    let symbol = symbols.label(&coid, s("instrument").unwrap_or_default());
    Some(FillEvent {
        trade_id,
        client_order_id: coid,
        venue: VENUE.to_string().into(),
        symbol: symbol.into(),
        side: if units >= 0.0 { 1 } else { -1 },
        last_qty: units.abs(),
        last_px: price,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Decode one transaction-stream line into the vike events it implies. An `ORDER_FILL`
/// dual-publishes the bare [`Event::Fill`] (the Account folds it into position/PnL) AND the
/// [`Event::OrderFilled`] wrap (the order FSM applies it) — the same contract the crypto WS
/// mappers honor, so a live fill folds identically to a replayed one. `ORDER_CANCEL` → one
/// [`Event::OrderCanceled`], EXCEPT the cancel half of an order REPLACE (`replacedByOrderID`),
/// which ends nothing: the replacement keeps the client id. HEARTBEAT / echoes → empty.
///
/// A fill carries the symbol its order was SUBMITTED under (`symbols`, see [`crate::symbols`]), the
/// venue's own spelling when nobody recorded the coid.
pub fn decode_transaction_events(v: &serde_json::Value, symbols: &SubmittedSymbols) -> Vec<Event> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    match s("type") {
        Some("ORDER_FILL") => {
            // An id-less fill yields NO events at all — not a half-published pair. The bare Fill
            // and its wrap must stand or fall together (they are the Account's copy and the FSM's
            // copy of one execution), so a refused frame drops both.
            let Some(fill) = fill_from_transaction(v, symbols) else { return Vec::new() };
            let wrap = Event::OrderFilled(OrderFilled {
                client_order_id: fill.client_order_id.clone(),
                fill: fill.clone(),
                ts: fill.ts,
            });
            vec![Event::Fill(fill), wrap]
        }
        // The cancel half of an order REPLACE ends the REPLACED order, not the logical one: the
        // replacement carries the same client id (`crate::exec::build_replace_body`) and the
        // `OrderModified` from the replace reply is what the platform folds. Emitting it as an
        // `OrderCanceled` would terminate an order that is still resting.
        Some("ORDER_CANCEL") if is_replace_cancel(v) => Vec::new(),
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
///
/// ⚠ **Three bounds, three different waits, all read from the exec lane of
/// `vike_venues::venue_timings::OANDA`** (the row carries each one's reason): `dial_timeout`
/// bounds the connect so a black-holed route can never hold the exec event lane for the OS's own
/// SYN ladder; `response_timeout` bounds the wait for the response HEAD, which is NOT the dial (the
/// two are both 10 s today and are separate facts); `idle_threshold` is `timeout_recv_body`, the
/// bound on each read wait of the chunked body (4 missed ~5 s heartbeats), the same mechanism the
/// quote lane's agent in `crate::market_feed` uses.
///
/// ⚠ **The head wait is `timeout_send_request`, NEVER `timeout_recv_response`.** In ureq 3.4.0 the
/// body-read deadline is the minimum of `now + recv_body` and `<time the response head arrived> +
/// recv_response` (`Timeout::RecvBody` lists `RecvResponse` as its preceding check), so a
/// `recv_response` of 10 s kills every successfully opened stream ~10 s after its head despite
/// heartbeats every ~5 s. `Timeout::RecvResponse` also checks `SendRequest` (the instant the
/// request left, plus `send_request`), which bounds the wait for the head and which the body check
/// never consults: the same bound, without capping the body.
fn transactions_agent() -> ureq::Agent {
    let exec = venue_timings::OANDA.exec();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // No redirect: a 3xx on the credentialed open is a non-2xx response, never a second request.
        .max_redirects(0)
        .timeout_connect(Some(exec.dial_timeout()))
        .timeout_send_request(Some(exec.response_timeout()))
        .timeout_recv_body(Some(exec.idle_threshold()))
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

/// Describe a failed stream open (either chunked-GET lane: this one and the pricing stream in
/// `crate::market_feed`) WITHOUT echoing the URL (it carries the account id). A non-2xx is surfaced BY
/// STATUS, a TIMEOUT by the bound that fired and the lane's configured value, and anything else stays the
/// generic transport line.
///
/// Why a timeout is its own sentence: on 2026-10-18 the OANDA practice stream hosts (behind Cloudflare)
/// answered about one open in three and left the rest without a response head until the wait ran out
/// (then an HTTP 520), and the journal said only "transport error on stream open", which hid the cause.
/// ureq 3.4.0 reports the head wait as `Timeout::SendRequest` (`timeout_send_request` is what both agents
/// set, and `RecvResponse` checks it), so both variants read as the head wait; `Connect` is the dial.
/// `lane` is the row the agent was built from, so the number named is the bound that actually applied.
pub(crate) fn describe_dial_error(e: ureq::Error, lane: venue_timings::Lane) -> String {
    match e {
        ureq::Error::StatusCode(code) => {
            format!("HTTP {code} on stream open (401/403: bad or expired token / wrong account)")
        }
        ureq::Error::Timeout(ureq::Timeout::Connect) => format!(
            "no TCP connection within {} s (the OANDA stream host did not accept)",
            lane.dial_timeout().as_secs_f64()
        ),
        ureq::Error::Timeout(ureq::Timeout::SendRequest | ureq::Timeout::RecvResponse) => format!(
            "no response head within {} s (the OANDA stream host did not answer)",
            lane.response_timeout().as_secs_f64()
        ),
        ureq::Error::Timeout(which) => format!("{which} timeout on stream open"),
        _ => "transport error on stream open".to_string(),
    }
}

/// Persistent transactions-stream driver: hold `GET /v3/accounts/{id}/transactions/stream`, decode
/// each line via [`decode_transaction_events`], and emit fills/cancels via `events` until `stop`.
/// Reconnects on disconnect, after the exec row's fixed pause. Blocks — run on a dedicated thread.
/// HEARTBEATs (~5s) keep the read loop ticking so `stop` is honoured; a silently dead connection is
/// bounded by `timeout_recv_body`.
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
pub(crate) fn stream_transactions(
    config: OandaConfig,
    events: EventSender,
    stop: Arc<AtomicBool>,
    last_seen: Arc<AtomicU64>,
    symbols: SubmittedSymbols,
) {
    let agent = transactions_agent();
    let exec = venue_timings::OANDA.exec();
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
                    Err((Some(status), describe_dial_error(ureq::Error::StatusCode(status), exec)))
                }
            }
            // pre-send / connect / TLS / a head that never came: no status exists
            Err(e) => Err((None, describe_dial_error(e, exec))),
        };
        let resp = match opened {
            Ok(r) => {
                terminal_opens = 0; // the token works; any earlier rejection is not standing
                r
            }
            Err((status, cause)) => {
                let kind = open_failure_kind(status);
                if !kind.is_terminal() {
                    // Throttle, venue 5xx, unreachable host: retrying as-is is the right answer,
                    // so this arm keeps looping exactly as before — it is now merely AUDIBLE.
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        error = %cause,
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
                            error = %cause,
                            attempts = terminal_opens,
                            "transactions-stream open REJECTED and not retryable (401/403: bad or \
                             expired token / wrong account) — giving up on this lane. Delayed \
                             LIMIT/STOP fills and venue-side cancels will NOT arrive for the rest \
                             of this session; MARKET fills are unaffected (they ride the order POST \
                             reply). Fix the account's OANDA credentials (API_KEY / ACCOUNT_ID) \
                             and restart."
                        );
                        return;
                    }
                    tracing::warn!(
                        target: "vike_oanda::stream",
                        venue = VENUE,
                        status,
                        kind = %kind,
                        error = %cause,
                        attempts = terminal_opens,
                        max_attempts = MAX_TERMINAL_OPENS,
                        "transactions-stream open rejected; retrying within the bounded budget"
                    );
                }
                // The exec lane's fixed wait between opens (`reconnect.initial`; the row is fixed).
                sleep_unless_stopped(&stop, venue_timings::OANDA.exec().reconnect().initial);
                continue;
            }
        };
        // A3 resync: on a re-open, recover the reconnect gap before reading live. The stream
        // connection is already open (so nothing after it is missed); the backfill + stream
        // overlap is deduped by the core.
        if opened_once && resync_gap(&rest, &config, &events, &last_seen, &symbols).is_break() {
            return; // core gone during backfill
        }
        opened_once = true;
        let reader = BufReader::new(resp.into_body().into_reader());
        let opened_at = Instant::now();
        let mut read_error = None;
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    read_error = Some(e); // recv timeout / disconnect → reconnect
                    break;
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                note_transaction_id(&last_seen, &v); // advance the A3 watermark
                let evs = decode_transaction_events(&v, &symbols);
                if publish(&events, &symbols, evs).is_break() {
                    return; // core gone
                }
            }
        }
        // The read loop ended with `stop` clear: the body ended (`read_error` is `None`) or a read
        // failed. Say which, and how long the stream lived, so a stream cut short is audible; then
        // wait the row's fixed stop-aware pause, so a venue that closes on accept is never
        // redialled hot.
        tracing::info!(
            target: "vike_oanda::stream",
            venue = VENUE,
            lived = ?opened_at.elapsed(),
            error = ?read_error,
            "transactions-stream read loop ended; reopening after the fixed pause"
        );
        sleep_unless_stopped(&stop, venue_timings::OANDA.exec().reconnect().initial);
    }
}

/// Publish what this lane decoded, in order. An event of an order whose submit the exec thread has not
/// answered yet is kept back ([`SubmittedSymbols::park`]) and goes out behind the exec thread's own
/// events for it: OANDA writes a market order's `ORDER_FILL` before the POST's reply is read, and a
/// fill that reaches the engine ahead of the `OrderAccepted` meets a `Submitted` order, which the FSM
/// refuses (`dropped_terminal_on_live`). `Break` when the core is gone.
fn publish(
    events: &EventSender,
    symbols: &SubmittedSymbols,
    evs: Vec<Event>,
) -> std::ops::ControlFlow<()> {
    for ev in evs {
        // `settle` runs per event, as the event goes out: a parked fill must not drop its order's
        // symbol before the exec thread's own copy of the fill has been labelled with it.
        let Some(ev) = symbols.park(ev) else { continue };
        symbols.settle(std::slice::from_ref(&ev));
        if events.blocking_send(ev).is_err() {
            return std::ops::ControlFlow::Break(());
        }
    }
    std::ops::ControlFlow::Continue(())
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
    symbols: &SubmittedSymbols,
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
            let evs = crate::history::map_transactions_since(&resp, symbols);
            if publish(events, symbols, evs).is_break() {
                return std::ops::ControlFlow::Break(());
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

#[path = "stream_tests.rs"]
#[cfg(test)]
mod stream_tests;
