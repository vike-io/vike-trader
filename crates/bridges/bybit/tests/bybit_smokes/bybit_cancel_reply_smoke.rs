//! **What a bybit cancel's REST reply says, measured on the DEMO venue** (error-handling plan D5e). Bybit's
//! `/v5/order/cancel` reference calls its answer "the acknowledgement of an cancel order request ... This request is
//! asynchronous so please use the websocket to confirm the order status". These two tests read what the demo venue
//! does after that answer:
//!
//! - `bybit_a_cancel_reply_and_the_streams_cancelled_row`: with the private stream subscribed (the production
//!   `open_bybit_user_data_websocket`, `execution` + `order`), each of three far limits is placed, seen `New` on the
//!   stream, cancelled by the exact request `crates/bridges/bybit/src/execution/rest/mod.rs`'s `cancel_order` sends,
//!   and then read at once by `orderLinkId` (`/v5/order/realtime`); the stream's `Cancelled` row is timed against the
//!   reply (`MEASURED` lines, a negative figure = the row was read before the reply returned) and mapped through the
//!   production `perp_frame_decoder`.
//! - `bybit_a_cancel_sent_before_the_stream_connects`: the grid-wait shape
//!   (`vike_bridge_core::venue_adapter::await_properties` sends a cancel before the exec thread's private stream
//!   exists). A far limit is placed and cancelled with no socket open; the order is read by `orderLinkId` until it
//!   ends, then the stream connects and is listened to for eight seconds (does it say anything about the order?),
//!   and the bridge's own read by order client id (`ConfirmByOrderClientId::confirm_when`) is asked what it makes of
//!   the order.
//!
//! The reply envelopes are printed as `CAPTURED` lines: the fixtures of `crates/bridges/bybit/tests/offline/cancel_requested.rs`.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_cancel_reply_smoke::
//!
//! ORDER-PLACING (linear perp `BTCUSDT.P`, the venue's `BTCUSDT`, the DEMO host only, the account `demo_label` names):
//! four limit BUYs at 90% of the last price, each just over `min_notional`, none reduce-only, each cancelled seconds
//! after it rests. Nothing can fill and nothing touches a position. Only `cancel reply smoke green` (once per test) is
//! a proof; a `SKIP:` line is not.
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
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
    WebsocketSessionUserStream,
};
use vike_bybit::execution::rest::{PATH_ORDER_CANCEL, PATH_ORDER_REALTIME, seed_endpoints};
use vike_bybit::execution::user_data::{open_bybit_user_data_websocket, perp_frame_decoder};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, now_ms};
use vike_secrets::credentials::Credentials;

use crate::bybit_stop_limit_smoke::{
    PerpRest, SYMBOL, build_rest, load_demo_credentials, on_grid, text,
};

/// How many orders the stream test places and cancels, one after the other.
const ROUNDS: usize = 3;
/// How long a socket is read for an order's row before the test gives up on it.
const STREAM_WAIT: Duration = Duration::from_secs(10);

/// The private stream on a thread of its own: every JSON text frame, stamped with the instant it was read.
struct Tap {
    frames: mpsc::Receiver<(Instant, Value)>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    fn open(creds: &Credentials) -> Tap {
        let stop = Arc::new(AtomicBool::new(false));
        let gate = vike_bybit::transport::ratelimit::websocket_rate_gate();
        let url = seed_endpoints(AccountTier::Demo).ws;
        let mut ws = match open_bybit_user_data_websocket(
            &url,
            creds.api_key.expose(),
            creds.api_secret.expose(),
            now_ms,
            &["execution", "order"],
            &stop,
            &gate,
        ) {
            WebsocketSessionUserOpenOutcome::Ready(ws) => ws,
            WebsocketSessionUserOpenOutcome::Transport(e)
            | WebsocketSessionUserOpenOutcome::Refused(e) => {
                panic!("bybit private socket connect failed: {e}")
            }
            WebsocketSessionUserOpenOutcome::Auth(e) => panic!("bybit private socket auth: {e}"),
            WebsocketSessionUserOpenOutcome::Stopped => unreachable!("stop is never raised here"),
        };
        let (sink, frames) = mpsc::channel();
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut pinged = Instant::now();
            while !flag.load(Ordering::Acquire) {
                if pinged.elapsed() > Duration::from_secs(10) {
                    let _ = ws.send_text(&vike_bybit::transport::ws_auth::ping_frame());
                    pinged = Instant::now();
                }
                match ws.recv() {
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
                        println!("FINDING bybit private socket closed: {why}");
                        return;
                    }
                }
            }
        });
        Tap { frames, stop, thread: Some(thread) }
    }

    /// Every frame carrying a row of `order_client_id` read within `within`, stopping at the first row whose
    /// `orderStatus` is `until` (`None`: read the whole window).
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
            let done =
                until.is_some_and(|word| ours.iter().any(|r| text(r, "orderStatus") == word));
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

/// The rows of `frame` (any private topic) that name `order_client_id`.
fn rows(frame: &Value, order_client_id: &str) -> Vec<Value> {
    frame["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["orderLinkId"].as_str() == Some(order_client_id))
        .cloned()
        .collect()
}

/// Milliseconds from `reference` to `at`, negative when `at` came first.
fn ms_after(at: Instant, reference: Instant) -> i128 {
    match at.checked_duration_since(reference) {
        Some(later) => later.as_millis() as i128,
        None => -(reference.duration_since(at).as_millis() as i128),
    }
}

/// One limit BUY at 90% of the last price, just over `min_notional`: it rests and cannot fill.
fn far_buy(rest: &PerpRest, order_client_id: &str) -> OrderRequest {
    let last = rest.last_price().expect("tickers");
    assert!(last > 0.0, "a live last price is required");
    let properties = rest.properties;
    let price = on_grid(last * 0.90, properties.tick_size);
    let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / price);
    let qty = (qty / properties.step_size).ceil() * properties.step_size;
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL, "side": 1, "qty": qty,
        "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap()
}

/// The cancel request `VenueRestBybit::cancel_order` sends, byte for byte, with the venue's whole envelope back
/// and the instants it was sent and answered.
fn cancel_request(rest: &PerpRest, order_client_id: &str) -> (Value, Instant, Instant) {
    let params = [
        ("category", json!(rest.category.wire())),
        ("symbol", json!(rest.symbol)),
        ("orderLinkId", json!(order_client_id)),
    ];
    let sent = Instant::now();
    let reply = rest
        .transport
        .signed(&rest.base_url, PATH_ORDER_CANCEL, "POST", &params, &rest.signer)
        .expect("the cancel POST reached the venue");
    (reply, sent, Instant::now())
}

/// One read of the order by `orderLinkId` on `/v5/order/realtime`: its row (`None` = an empty list) and how long
/// the read took.
fn read_once(rest: &PerpRest, order_client_id: &str) -> (Option<Value>, Duration) {
    let started = Instant::now();
    let result = rest
        .call_public(
            PATH_ORDER_REALTIME,
            &[
                ("category", json!(rest.category.wire())),
                ("symbol", json!(rest.symbol)),
                ("orderLinkId", json!(order_client_id)),
            ],
        )
        .expect("the order read");
    (result["list"].as_array().and_then(|l| l.first()).cloned(), started.elapsed())
}

fn status_of(row: Option<&Value>) -> String {
    row.map_or_else(|| "<no row>".to_string(), |r| text(r, "orderStatus"))
}

fn place(rest: &PerpRest, order_client_id: &str) {
    let events = VenueRest::submit_order(rest, &far_buy(rest, order_client_id));
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "{order_client_id}: the demo must ACCEPT the far limit: {events:?}"
    );
}

#[test]
#[ignore = "network + demo creds — places and cancels three real far demo limits — run manually (see module doc)"]
fn bybit_a_cancel_reply_and_the_streams_cancelled_row() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds);
    let tap = Tap::open(&creds);
    let decode = perp_frame_decoder(SYMBOL);
    for round in 1..=ROUNDS {
        let order_client_id = format!("vtcr{round}s{}", now_ms() % 100_000_000);
        place(&rest, &order_client_id);
        let _cleanup =
            CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let resting = tap.rows_of(&order_client_id, Some("New"), STREAM_WAIT);
        assert!(!resting.is_empty(), "{order_client_id}: the stream never said New");

        let (reply, sent, answered) = cancel_request(&rest, &order_client_id);
        let (row, read_took) = read_once(&rest, &order_client_id);
        println!("CAPTURED bybit cancel reply (round {round}): {reply}");
        println!(
            "MEASURED bybit round {round}: the reply took {} ms; the read sent right after it ({} ms) says \
             orderStatus={} updatedTime={}",
            answered.duration_since(sent).as_millis(),
            read_took.as_millis(),
            status_of(row.as_ref()),
            row.as_ref().map_or_else(String::new, |r| text(r, "updatedTime")),
        );
        assert_eq!(
            reply["retCode"].as_i64(),
            Some(0),
            "{order_client_id}: the cancel was refused: {reply}"
        );

        let after = tap.rows_of(&order_client_id, Some("Cancelled"), STREAM_WAIT);
        for (at, frame) in &after {
            for r in rows(frame, &order_client_id) {
                println!(
                    "MEASURED bybit round {round}: stream topic={} orderStatus={} cancelType={} updatedTime={} \
                     frame creationTime={} read {:+} ms after the reply returned ({:+} ms after it was sent)",
                    text(frame, "topic"),
                    text(&r, "orderStatus"),
                    text(&r, "cancelType"),
                    text(&r, "updatedTime"),
                    text(frame, "creationTime"),
                    ms_after(*at, answered),
                    ms_after(*at, sent),
                );
            }
        }
        let (_, ended) = after
            .iter()
            .find(|(_, f)| {
                rows(f, &order_client_id).iter().any(|r| text(r, "orderStatus") == "Cancelled")
            })
            .unwrap_or_else(|| {
                panic!("{order_client_id}: the stream never said Cancelled: {after:?}")
            });
        let mapped = decode(ended);
        println!(
            "MEASURED bybit round {round}: the production decoder maps that row to {mapped:?}"
        );
        assert!(
            mapped.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)
            ),
            "{order_client_id}: the stream's Cancelled row is the order's OrderCanceled: {mapped:?}"
        );
    }
    tracing::info!(target: "vike_bybit", "cancel reply smoke green (stream)");
}

#[test]
#[ignore = "network + demo creds — places and cancels one real far demo limit — run manually (see module doc)"]
fn bybit_a_cancel_sent_before_the_stream_connects() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds);
    let order_client_id = format!("vtcrb{}", now_ms() % 100_000_000);
    place(&rest, &order_client_id);
    let _cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };

    // No socket is open: the cancel goes out as the grid wait sends it.
    let (reply, sent, answered) = cancel_request(&rest, &order_client_id);
    println!("CAPTURED bybit cancel reply (no stream): {reply}");
    assert_eq!(
        reply["retCode"].as_i64(),
        Some(0),
        "{order_client_id}: the cancel was refused: {reply}"
    );
    let mut reads = Vec::new();
    let ended = loop {
        let (row, took) = read_once(&rest, &order_client_id);
        let status = status_of(row.as_ref());
        reads.push(format!("{status} (+{} ms)", ms_after(Instant::now(), answered)));
        if status == "Cancelled" {
            break true;
        }
        if answered.elapsed() > STREAM_WAIT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100).saturating_sub(took));
    };
    println!(
        "MEASURED bybit no stream: the reply took {} ms; the reads by orderLinkId after it said {reads:?}",
        answered.duration_since(sent).as_millis()
    );
    assert!(ended, "{order_client_id}: the venue's read never said Cancelled: {reads:?}");

    // The stream connects only now, as the exec thread's would after its grid loads.
    let tap = Tap::open(&creds);
    let late = tap.rows_of(&order_client_id, None, Duration::from_secs(8));
    let words: Vec<String> = late
        .iter()
        .flat_map(|(_, f)| rows(f, &order_client_id))
        .map(|r| format!("{}:{}", text(&r, "orderStatus"), text(&r, "updatedTime")))
        .collect();
    println!(
        "MEASURED bybit no stream: after connecting, the stream said {} thing(s) about the order in 8 s: {words:?}",
        words.len()
    );

    // The read the bridge has for it: by order client id, realtime then history.
    let confirm = rest.confirm_when(&order_client_id, now_ms(), false);
    println!("MEASURED bybit no stream: the bridge's read by order client id answers {confirm:?}");
    assert!(
        confirm
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)),
        "{order_client_id}: the read by order client id ends the order: {confirm:?}"
    );
    tracing::info!(target: "vike_bybit", "cancel reply smoke green (no stream)");
}
