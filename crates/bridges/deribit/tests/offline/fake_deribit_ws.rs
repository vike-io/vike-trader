//! A local `ws://127.0.0.1:<port>` Deribit stand-in — the shared harness behind
//! `recon_client_redial.rs` (the recon socket's lifecycle), `exec_ambiguous_submit.rs` (the order
//! socket's audit-T1 behaviour) and `exec_resync_redial.rs` (the audit-A3 resync socket's
//! lifecycle). No venue, no credentials, no `#[ignore]`.
//!
//! It is testable offline only because `crate::transport::DeribitOrderTransport::new` takes the
//! `ws_url` as a CONSTRUCTOR PARAMETER. The fake is a plain `TcpListener` + `tungstenite::accept`
//! answering the JSON-RPC frames the transport really sends, driven by a fixed script of
//! [`Session`]s — one per accepted connection. When the script is exhausted the listener is
//! DROPPED, so any further dial is refused; that is how a permanently-dead venue is built.
//!
//! Two things are RECORDED, and they are what the tests assert on:
//!
//! * [`FakeDeribit::connections`] — a re-dial is a second accepted connection, and its ABSENCE is
//!   what proves a live socket was not needlessly replaced.
//! * [`FakeDeribit::methods`] — every non-auth JSON-RPC method received, in order, across ALL
//!   connections. This is the order-safety assertion: after a socket dies mid-submit the log must
//!   still contain exactly ONE `private/buy`. A resent order would show up here as a second one.

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

/// The instrument every test mounts.
pub const SYMBOL: &str = "BTC-PERPETUAL";
/// The venue order id the fake reports for a submitted / re-queried / open order.
pub const ORDER_ID: &str = "146062";
/// The `label` (= coid) on the scripted A3 user-trades row — the FILL half of the replay.
pub const RESYNC_FILL_COID: &str = "vike-resync-fill";
/// The `label` on the scripted A3 order-history row that CANCELLED — the non-fill terminal half.
pub const RESYNC_CANCEL_COID: &str = "vike-resync-cancel";
/// The scripted fill's `trade_id`. Load-bearing rather than cosmetic: `map_deribit_trade` DROPS a
/// row carrying no trade id (it would be un-dedupable against the live pump), so a fixture without
/// one would silently replay nothing and every assertion below would pass for the wrong reason.
pub const RESYNC_TRADE_ID: &str = "t-resync-1";
/// Server-side read timeout, so the fake's thread polls its stop flag instead of parking forever.
const TICK: Duration = Duration::from_millis(50);

/// What one accepted connection does AFTER the `public/auth` handshake.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// Answer `n` private calls, then send a Close frame and drop the socket — the venue-side
    /// close the the CI box incident started with, landing BETWEEN requests.
    OkThenClose(usize),
    /// Answer every private call until the test drops the server.
    OkForever,
    /// Answer every private call with a JSON-RPC *error object* — the venue ANSWERING with a
    /// refusal, which is a live socket, not a dead one.
    VenueErrorForever,
    /// Read the first private call and CLOSE WITHOUT ANSWERING IT. The request frame is on the
    /// wire and the venue may or may not have acted on it: the ambiguous shape audit T1 exists
    /// for, and the one this crate used to resolve as a definite `OrderRejected`.
    CloseBeforeAnswering,
    /// Answer every private call, but report NO order for a `get_order_state_by_label` re-query —
    /// the venue confirming the order never landed.
    OkForeverOrderAbsent,
}

/// A scripted Deribit stand-in. Dropping it stops the server thread and joins it.
pub struct FakeDeribit {
    pub url: String,
    connections: Arc<AtomicUsize>,
    methods: Arc<Mutex<Vec<String>>>,
    listener_closed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeDeribit {
    pub fn spawn(script: Vec<Session>) -> FakeDeribit {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("ws://{}", listener.local_addr().expect("local addr"));
        listener.set_nonblocking(true).expect("nonblocking listener");

        let connections = Arc::new(AtomicUsize::new(0));
        let methods = Arc::new(Mutex::new(Vec::new()));
        let listener_closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (conns, seen, closed, stopped) =
            (connections.clone(), methods.clone(), listener_closed.clone(), stop.clone());

        let handle = std::thread::spawn(move || {
            for session in script {
                let accepted = loop {
                    if stopped.load(Ordering::Relaxed) {
                        break None;
                    }
                    match listener.accept() {
                        Ok((s, _)) => break Some(s),
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(TICK);
                        }
                        Err(_) => break None,
                    }
                };
                let Some(stream) = accepted else { break };
                conns.fetch_add(1, Ordering::Relaxed);
                stream.set_nonblocking(false).expect("blocking accepted stream");
                let Ok(mut ws) = tungstenite::accept(stream) else { continue };
                let _ = ws.get_ref().set_read_timeout(Some(TICK));
                serve(&mut ws, session, &seen, &stopped);
            }
            drop(listener);
            closed.store(true, Ordering::Relaxed);
        });

        FakeDeribit { url, connections, methods, listener_closed, stop, handle: Some(handle) }
    }

    /// Accepted connections. A re-dial shows up here as a second one.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    /// Every non-auth JSON-RPC method received, in order, across all connections.
    pub fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }

    /// How many times `method` was received — the order-safety counter.
    pub fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    /// Block (bounded) until the script is exhausted and the listener is gone, so a later dial is
    /// deterministically REFUSED rather than racing the server thread's teardown.
    pub fn await_listener_closed(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.listener_closed.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(TICK);
        }
        panic!("fake venue never released its listener");
    }
}

impl Drop for FakeDeribit {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// One open-order row, shaped like `private/get_open_orders_by_instrument`'s real payload.
fn open_orders_result() -> Value {
    json!([{
        "instrument_name": SYMBOL,
        "order_id": ORDER_ID,
        "label": "vike-1",
        "direction": "buy",
        "order_type": "limit",
        "amount": 10.0,
        "filled_amount": 0.0,
        "average_price": 0.0,
        "last_update_timestamp": 1_700_000_000_000i64,
    }])
}

/// `private/get_order_history_by_instrument` — a BARE array, the shape
/// `vike_deribit::history::map_deribit_history` reads directly. One CANCELLED order (the A3
/// replay's non-fill terminal) and one still-OPEN one, which must map to nothing: a test asserting
/// on the event list is then reading the real mapper's fold rather than a row count.
fn order_history_result() -> Value {
    json!([
        {"instrument_name": SYMBOL, "order_id": ORDER_ID, "label": RESYNC_CANCEL_COID,
         "order_state": "cancelled", "last_update_timestamp": 1_700_000_000_008i64},
        {"instrument_name": SYMBOL, "order_id": ORDER_ID, "label": "vike-resync-open",
         "order_state": "open", "last_update_timestamp": 1_700_000_000_006i64},
    ])
}

/// `private/get_user_trades_by_instrument` — `{trades, has_more}`, which `get_user_trades`
/// unwraps. One FILLED trade, so the replay folds to `[Fill, OrderFilled]` through the same
/// `map_deribit_trade` the live pump uses.
fn user_trades_result() -> Value {
    json!({
        "trades": [{
            "trade_id": RESYNC_TRADE_ID, "label": RESYNC_FILL_COID, "order_id": ORDER_ID,
            "instrument_name": SYMBOL, "price": 50_000.0, "amount": 10.0, "direction": "buy",
            "fee": 0.001, "fee_currency": "BTC", "liquidity": "T",
            "timestamp": 1_700_000_000_005i64, "state": "filled",
        }],
        "has_more": false,
    })
}

/// The RESULT payload for one method, in the shape the real venue answers with.
fn result_for(method: &str, session: Session) -> Value {
    match method {
        // The audit-A3 resync's two reads — the ONLY methods its socket can carry besides the
        // `public/auth` handshake, which is what licenses that path's re-dial-and-retry.
        "private/get_order_history_by_instrument" => order_history_result(),
        "private/get_user_trades_by_instrument" => user_trades_result(),
        // `{"order": {...}, "trades": [...]}` — `dispatch_submit` reads `result.order.order_id`.
        "private/buy" | "private/sell" => json!({
            "order": {"order_id": ORDER_ID, "label": "vike-1", "order_state": "open"},
            "trades": [],
        }),
        // The audit-T1 re-query: an ARRAY, empty when the venue never took the order.
        "private/get_order_state_by_label" => {
            if session == Session::OkForeverOrderAbsent {
                json!([])
            } else {
                json!([{"order_id": ORDER_ID, "label": "vike-1", "order_state": "open"}])
            }
        }
        "private/cancel" => json!({"order_id": ORDER_ID, "order_state": "cancelled"}),
        _ => open_orders_result(),
    }
}

/// Answer JSON-RPC frames by id until the session's script says to stop.
fn serve(
    ws: &mut WebSocket<TcpStream>,
    session: Session,
    seen: &Mutex<Vec<String>>,
    stop: &AtomicBool,
) {
    let mut served = 0usize;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        let msg = match ws.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(ref e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(_) => return, // the client hung up
        };
        let Message::Text(raw) = msg else { continue };
        let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else { continue };
        let id = frame.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
        let method = frame.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();

        if method == "public/auth" {
            // Never recorded: the handshake is not a venue ACTION, and counting it would bury the
            // one number these tests are about.
            let reply = json!({"jsonrpc": "2.0", "id": id,
                "result": {"access_token": "tok", "refresh_token": "ref", "expires_in": 900}});
            let _ = ws.send(Message::Text(reply.to_string().into()));
            continue;
        }
        seen.lock().unwrap().push(method.clone());

        if session == Session::CloseBeforeAnswering {
            // The request is recorded — the venue RECEIVED it — and the socket dies with no reply.
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        let reply = if session == Session::VenueErrorForever {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 10009, "message": "not_enough_funds"}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": result_for(&method, session)})
        };
        let _ = ws.send(Message::Text(reply.to_string().into()));

        served += 1;
        if let Session::OkThenClose(n) = session {
            if served >= n {
                let _ = ws.close(None); // the venue-side close, landing BETWEEN requests
                let _ = ws.flush();
                return;
            }
        }
    }
}

/// A `DeribitOrderTransport` pointed at the fake, dialled and authed. The seam every owner builds
/// from: an owner that RE-DIALS has to hold the transport itself, so a helper that only ever hands
/// back an `Arc<DeribitRest>` cannot serve `vike_deribit::exec::A3Resync`, which owns its socket.
pub fn transport_against(fake: &FakeDeribit) -> vike_deribit::transport::DeribitOrderTransport {
    let mut transport = vike_deribit::transport::DeribitOrderTransport::new(
        &fake.url,
        "client-id",
        "client-secret",
        None,
    );
    transport.connect().expect("fake venue completes the JSON-RPC auth handshake");
    transport
}

/// A `DeribitRest` whose order-WS points at the fake, connected and authed — the same shape
/// `DeribitReconClient::connect` and `vike_deribit::exec` both build.
pub fn rest_against(fake: &FakeDeribit) -> Arc<vike_deribit::client::DeribitRest> {
    Arc::new(vike_deribit::client::DeribitRest::new(
        transport_against(fake),
        SYMBOL,
        vike_model::SymbolProperties::default(),
        "BTC",
    ))
}
