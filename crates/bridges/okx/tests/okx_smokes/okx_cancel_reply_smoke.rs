//! **What an okx `cancel-order` reply says, measured on the DEMO venue** (error-handling plan D5e). An `sCode` of
//! `0` is the venue's word that it took the cancel request; the final state is what its `orders` channel pushes and
//! what `GET /api/v5/trade/order` reads. These two tests read what the demo venue does after that answer:
//!
//! - `okx_a_cancel_reply_and_the_streams_canceled_row`: with the private socket subscribed to `orders`/`ANY` (the
//!   production `open_okx_user_data_websocket`), each of three far limits is placed, seen resting on the stream,
//!   cancelled by the exact request `crates/bridges/okx/src/execution/rest/mod.rs`'s `cancel_order` sends, and then
//!   read at once by `clOrdId`; the stream's `canceled` row is timed against the reply (`MEASURED` lines, a negative
//!   figure = the row was read before the reply returned) and mapped through the production `map_okx_perp`.
//! - `okx_a_cancel_sent_before_the_stream_connects`: the grid-wait shape
//!   (`vike_bridge_core::venue_adapter::await_properties` sends a cancel before the exec thread's private socket
//!   exists). A far limit is placed and cancelled with no socket open; the order is read by `clOrdId` until it ends,
//!   then the socket connects and is listened to for eight seconds (does it say anything about the order?), and the
//!   bridge's own read by order client id (`ConfirmByOrderClientId::confirm_when`) is asked what it makes of it.
//!
//! The reply envelopes are printed as `CAPTURED` lines: the fixtures of `crates/bridges/okx/tests/offline/cancel_requested.rs`.
//! A stream row is printed by its order fields only (never the frame's `arg`, which names the account).
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_cancel_reply_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`, the user name `BTCUSDT.P`), DEMO ONLY (the simulated-trading header,
//! `crate::demo_swap::build_rest`): four limit BUYs of one minimum lot at 85% of the last price, none reduce-only,
//! each cancelled seconds after it rests. Nothing can fill and nothing touches a position. Only
//! `cancel reply smoke green` (once per test) is a proof; a `SKIP:` line is not.
//!
//! CLEANUP: a `CancelOnDrop` guard per order client id cancels through the placing client on every exit path. A run
//! killed from outside leaves the order resting: cancel the order client ids the log names (`vtcr…`) by hand on the
//! demo account.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
    WebsocketSessionUserStream,
};
use vike_model::events::Event;
use vike_model::{OrderRequest, now_ms};
use vike_okx::execution::event_mapper::map_okx_perp;
use vike_okx::execution::rest::{PATH_ORDER_CANCEL, PATH_ORDER_CREATE, VenueRestOkx};
use vike_okx::execution::user_data::open_okx_user_data_websocket;
use vike_okx::test_support::demo_net;
use vike_okx::transport::ws_auth::PING_TEXT;
use vike_secrets::credentials::Credentials;

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use crate::okx_stop_limit_smoke::{INST_ID, SYMBOL};

type SwapRest = VenueRestOkx<RestTransportJsonUreq>;

/// How many orders the stream test places and cancels, one after the other.
const ROUNDS: usize = 3;
/// How long a socket is read for an order's row before the test gives up on it.
const STREAM_WAIT: Duration = Duration::from_secs(10);
/// The venue's word for an order a cancel ended.
const CANCELED: &str = "canceled";

/// The private socket on a thread of its own: every JSON text frame, stamped with the instant it was read.
struct Tap {
    frames: mpsc::Receiver<(Instant, Value)>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    fn open(creds: &Credentials) -> Tap {
        let passphrase = creds.passphrase.as_ref().expect("OKX needs the API passphrase");
        let stop = Arc::new(AtomicBool::new(false));
        let gate = vike_okx::transport::ratelimit::websocket_rate_gate();
        let mut ws = match open_okx_user_data_websocket(
            &demo_net().private_websocket,
            creds.api_key.expose(),
            creds.api_secret.expose(),
            passphrase.expose(),
            now_ms,
            "orders",
            "ANY",
            &stop,
            &gate,
        ) {
            WebsocketSessionUserOpenOutcome::Ready(ws) => ws,
            WebsocketSessionUserOpenOutcome::Transport(e)
            | WebsocketSessionUserOpenOutcome::Refused(e) => {
                panic!("okx private socket connect failed: {e}")
            }
            WebsocketSessionUserOpenOutcome::Auth(e) => panic!("okx private socket auth: {e}"),
            WebsocketSessionUserOpenOutcome::Stopped => unreachable!("stop is never raised here"),
        };
        let (sink, frames) = mpsc::channel();
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut pinged = Instant::now();
            while !flag.load(Ordering::Acquire) {
                if pinged.elapsed() > Duration::from_secs(15) {
                    let _ = ws.send_text(PING_TEXT);
                    pinged = Instant::now();
                }
                match ws.recv() {
                    // `pong` and every other non-JSON text is skipped
                    Ok(WebsocketSessionUserMessage::Text(text)) => {
                        let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                        if sink.send((Instant::now(), frame)).is_err() {
                            return;
                        }
                    }
                    Ok(WebsocketSessionUserMessage::Ping(payload)) => {
                        let _ = ws.pong(payload);
                    }
                    Ok(WebsocketSessionUserMessage::Other)
                    | Err(WebsocketSessionUserError::Timeout) => {}
                    Err(WebsocketSessionUserError::Closed(why)) => {
                        println!("FINDING okx private socket closed: {why}");
                        return;
                    }
                }
            }
        });
        Tap { frames, stop, thread: Some(thread) }
    }

    /// Every frame carrying a row of `order_client_id` read within `within`, stopping at the first row whose
    /// `state` is `until` (`None`: read the whole window).
    fn rows_of(
        &self,
        order_client_id: &str,
        until: Option<&str>,
        within: Duration,
    ) -> Vec<(Instant, Value)> {
        let deadline = Instant::now() + within;
        let mut seen = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok((at, frame)) = self.frames.recv_timeout(left) else { break };
            let ours = rows(&frame, order_client_id);
            if ours.is_empty() {
                continue;
            }
            let done = until.is_some_and(|word| ours.iter().any(|r| word_of(r, "state") == word));
            seen.push((at, frame));
            if done {
                break;
            }
        }
        seen
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The rows of `frame` that name `order_client_id`.
fn rows(frame: &Value, order_client_id: &str) -> Vec<Value> {
    frame["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["clOrdId"].as_str() == Some(order_client_id))
        .cloned()
        .collect()
}

fn word_of(row: &Value, key: &str) -> String {
    row[key].as_str().map_or_else(|| row[key].to_string(), str::to_string)
}

/// Milliseconds from `reference` to `at`, negative when `at` came first.
fn ms_after(at: Instant, reference: Instant) -> i128 {
    match at.checked_duration_since(reference) {
        Some(later) => later.as_millis() as i128,
        None => -(reference.duration_since(at).as_millis() as i128),
    }
}

/// One limit BUY of one minimum lot at 85% of the last price: it rests and cannot fill.
fn place(rest: &SwapRest, order_client_id: &str) {
    let last = rest.last_price().expect("ticker");
    assert!(last > 0.0, "a live last price is required");
    let tick = rest.properties.tick_size;
    let price = (last * 0.85 / tick).round() * tick;
    let request: OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": 1,
        "qty": rest.properties.min_qty * rest.ct_val, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(rest, &request);
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "{order_client_id}: the demo must ACCEPT the far limit: {events:?}"
    );
}

/// The cancel request `VenueRestOkx::cancel_order` sends first, byte for byte, with the venue's whole envelope
/// back and the instants it was sent and answered.
fn cancel_request(rest: &SwapRest, order_client_id: &str) -> (Value, Instant, Instant) {
    let params = [("instId", json!(INST_ID)), ("clOrdId", json!(order_client_id))];
    let sent = Instant::now();
    let reply = rest
        .transport
        .signed(&rest.base_url, PATH_ORDER_CANCEL, "POST", &params, &rest.signer)
        .expect("the cancel POST reached the venue");
    (reply, sent, Instant::now())
}

/// One read of the order by `clOrdId` (`GET /api/v5/trade/order`): its row and how long the read took.
fn read_once(rest: &SwapRest, order_client_id: &str) -> (Option<Value>, Duration) {
    let started = Instant::now();
    let data = rest
        .call_get(
            PATH_ORDER_CREATE,
            &[("instId", json!(INST_ID)), ("clOrdId", json!(order_client_id))],
        )
        .expect("the order read");
    (data.as_array().and_then(|d| d.first()).cloned(), started.elapsed())
}

fn state_of(row: Option<&Value>) -> String {
    row.map_or_else(|| "<no row>".to_string(), |r| word_of(r, "state"))
}

/// The one row's `sCode` of a `cancel-order` envelope.
fn row_code(reply: &Value) -> Option<String> {
    reply["data"].as_array().and_then(|d| d.first()).map(|row| word_of(row, "sCode"))
}

#[test]
#[ignore = "network + demo creds — places and cancels three real far demo limits — run manually (see module doc)"]
fn okx_a_cancel_reply_and_the_streams_canceled_row() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, INST_ID);
    let tap = Tap::open(&creds);
    for round in 1..=ROUNDS {
        let order_client_id = format!("vtcr{round}s{}", now_ms() % 100_000_000);
        place(&rest, &order_client_id);
        let _cleanup =
            CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let resting = tap.rows_of(&order_client_id, None, Duration::from_secs(3));
        assert!(!resting.is_empty(), "{order_client_id}: the stream never named the resting order");

        let (reply, sent, answered) = cancel_request(&rest, &order_client_id);
        let (row, read_took) = read_once(&rest, &order_client_id);
        println!("CAPTURED okx cancel reply (round {round}): {reply}");
        println!(
            "MEASURED okx round {round}: the reply took {} ms; the read sent right after it ({} ms) says state={} \
             uTime={}",
            answered.duration_since(sent).as_millis(),
            read_took.as_millis(),
            state_of(row.as_ref()),
            row.as_ref().map_or_else(String::new, |r| word_of(r, "uTime")),
        );
        assert_eq!(
            row_code(&reply).as_deref(),
            Some("0"),
            "{order_client_id}: the cancel was refused: {reply}"
        );

        let after = tap.rows_of(&order_client_id, Some(CANCELED), STREAM_WAIT);
        for (at, frame) in &after {
            for r in rows(frame, &order_client_id) {
                println!(
                    "MEASURED okx round {round}: stream state={} cancelSource={} uTime={} read {:+} ms after the \
                     reply returned ({:+} ms after it was sent)",
                    word_of(&r, "state"),
                    word_of(&r, "cancelSource"),
                    word_of(&r, "uTime"),
                    ms_after(*at, answered),
                    ms_after(*at, sent),
                );
            }
        }
        let (_, ended) = after
            .iter()
            .find(|(_, f)| {
                rows(f, &order_client_id).iter().any(|r| word_of(r, "state") == CANCELED)
            })
            .unwrap_or_else(|| panic!("{order_client_id}: the stream never said canceled"));
        let mapped = map_okx_perp(ended, "okx", SYMBOL, rest.ct_val);
        println!("MEASURED okx round {round}: the production mapper maps that row to {mapped:?}");
        assert!(
            mapped.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)
            ),
            "{order_client_id}: the stream's canceled row is the order's OrderCanceled: {mapped:?}"
        );
    }
    tracing::info!(target: "vike_okx", "cancel reply smoke green (stream)");
}

#[test]
#[ignore = "network + demo creds — places and cancels one real far demo limit — run manually (see module doc)"]
fn okx_a_cancel_sent_before_the_stream_connects() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, INST_ID);
    let order_client_id = format!("vtcrb{}", now_ms() % 100_000_000);
    place(&rest, &order_client_id);
    let _cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };

    // No socket is open: the cancel goes out as the grid wait sends it.
    let (reply, sent, answered) = cancel_request(&rest, &order_client_id);
    println!("CAPTURED okx cancel reply (no stream): {reply}");
    assert_eq!(
        row_code(&reply).as_deref(),
        Some("0"),
        "{order_client_id}: the cancel was refused: {reply}"
    );
    let mut reads = Vec::new();
    let ended = loop {
        let (row, took) = read_once(&rest, &order_client_id);
        let state = state_of(row.as_ref());
        reads.push(format!("{state} (+{} ms)", ms_after(Instant::now(), answered)));
        if state == CANCELED {
            break true;
        }
        if answered.elapsed() > STREAM_WAIT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100).saturating_sub(took));
    };
    println!(
        "MEASURED okx no stream: the reply took {} ms; the reads by clOrdId after it said {reads:?}",
        answered.duration_since(sent).as_millis()
    );
    assert!(ended, "{order_client_id}: the venue's read never said canceled: {reads:?}");

    // The socket connects only now, as the exec thread's would after its grid loads.
    let tap = Tap::open(&creds);
    let late = tap.rows_of(&order_client_id, None, Duration::from_secs(8));
    let words: Vec<String> = late
        .iter()
        .flat_map(|(_, f)| rows(f, &order_client_id))
        .map(|r| format!("{}:{}", word_of(&r, "state"), word_of(&r, "uTime")))
        .collect();
    println!(
        "MEASURED okx no stream: after connecting, the stream said {} thing(s) about the order in 8 s: {words:?}",
        words.len()
    );

    // The read the bridge has for it: by order client id, the regular book then the algo book.
    let confirm = rest.confirm_when(&order_client_id, now_ms(), false);
    println!("MEASURED okx no stream: the bridge's read by order client id answers {confirm:?}");
    assert!(
        confirm
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)),
        "{order_client_id}: the read by order client id ends the order: {confirm:?}"
    );
    tracing::info!(target: "vike_okx", "cancel reply smoke green (no stream)");
}
