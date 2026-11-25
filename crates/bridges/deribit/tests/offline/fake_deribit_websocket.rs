//! A local `ws://127.0.0.1:<port>` Deribit stand-in — the shared harness behind
//! `reconcile_client_redial.rs` (the recon socket's lifecycle), `execution_ambiguous_submit.rs` (the order
//! socket's audit-T1 behaviour), `execution_resync_redial.rs` (the audit-A3 resync socket's
//! lifecycle) and `order_event_order.rs` (the order's events against the fill pump's, via a
//! [`Hook`]). No venue, no credentials, no `#[ignore]`.
//!
//! It is testable offline only because `crate::transport::DeribitTransport::new` takes the
//! `websocket_url` as a CONSTRUCTOR PARAMETER. The fake is a plain `TcpListener` + `tungstenite::accept`
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

use std::assert_matches;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

/// The instrument every test starts.
pub const SYMBOL: &str = "BTC-PERPETUAL";
/// The venue order id the fake reports for a submitted / re-queried / open order.
pub const ORDER_ID: &str = "146062";
/// The id of the NEW order a fired stop-limit becomes ([`Session::TriggeredStop`]): the stop's own id
/// ([`ORDER_ID`], what the submit answered) no longer exists, the limit it became does.
pub const CHILD_ID: &str = "146099";
/// The `label` (= order client id) on the scripted A3 user-trades row — the FILL half of the replay.
pub const RESYNC_FILL_ORDER_CLIENT_ID: &str = "vike-resync-fill";
/// The `label` on the scripted A3 order-history row that CANCELLED — the non-fill terminal half.
pub const RESYNC_CANCEL_ORDER_CLIENT_ID: &str = "vike-resync-cancel";
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
    /// A trigger order that has FIRED, as the testnet answers it (2026-10-26): the id the submit was
    /// acknowledged with ([`ORDER_ID`]) ceases to exist (`private/cancel` of it answers 10004
    /// `order_not_found`) and a NEW order, [`CHILD_ID`], carries the stop's `label` (a stop-limit's
    /// limit leg, resting). `private/cancel` of [`CHILD_ID`] succeeds.
    TriggeredStop,
    /// The order is already gone and nothing replaced it: `private/cancel` of [`ORDER_ID`] answers 10004
    /// and the label lookup still reports that same id (a terminal order, listed ~31 minutes).
    AlreadyGone,
    /// Complete the `public/auth` handshake, then close with no request ever read: the socket that
    /// died while IDLE, discovered only by the first write after the idle period.
    CloseAfterAuth,
    /// The access token EXPIRES: every `private/*` call answers `13009 unauthorized` (recorded -- the
    /// venue received the frame and refused it BEFORE executing) until a SECOND `public/auth` arrives
    /// on the same connection, after which the session behaves as `OkForever`.
    TokenExpired,
    /// A book spanning several currencies, as MEASURED on the testnet 2026-10-17: the order exists
    /// under its label ONLY in the named settlement currency (`get_order_state_by_label` answers `[]`
    /// with no error for any other valid currency), `get_open_orders` with no currency lists it, and
    /// the positions, open-order and user-trade reads answer rows for the instrument they are asked
    /// about (`currency=any` spans every instrument). Answers every private call forever.
    ForeignBook(&'static str),
    /// The FILL-STREAM venue (`vike_deribit::execution::user_data`'s pump): answers `public/auth` and every
    /// `private/subscribe` (the stops' `user.orders` call, then the fills'), and right after the ack
    /// of the one naming `user.trades` (the last step of the open) pushes each of `frames` as a
    /// notification. With `refuse_orders` the call naming `user.orders` is answered with an ERROR
    /// object instead (the venue refusing the channel). Answers everything else like `OkForever`.
    FillStream { refuse_orders: bool, frames: &'static [&'static str] },
    /// A post-only edit that would cross. With `refused`, `private/edit` answers 11054
    /// `post_only_reject`; otherwise its `result.order` is the order in `order_state`.
    /// `private/get_order_state` answers the order in `order_state`, or an error for `None`. Cancelled
    /// carries the documented `cancel_reason`. (Which of these the venue does is what
    /// `tests/deribit_post_only_smoke.rs` measures.)
    CrossingEdit { refused: bool, order_state: Option<&'static str> },
    /// Answer every private call until `private/edit`, then close WITHOUT answering it: the edit's
    /// reply is lost, the ambiguous shape `edit_order` settles by reading the order back.
    CloseOnEdit,
    /// [`Self::CloseOnEdit`] for `private/buy`: what comes before the frame (a probe, a combo's
    /// registration) is answered.
    CloseOnBuy,
    /// Answer every private call like [`Self::OkForever`], but `private/get_order_state_by_label` with
    /// each of these JSON texts in turn, the last one repeating.
    LabelAnswers(&'static [&'static str]),
    /// `method` alone answered with this venue error object, every other call answered. `absent`: the
    /// venue lists no order under a label. `private/get_order_state` reads the order as [`Self::NoAnswerOn`] does.
    VenueErrorOn { method: &'static str, code: i64, message: &'static str, absent: bool },
    /// Every call naming `instrument_name` = `instrument` answered with this venue error object, every
    /// other call answered like [`Self::OkForever`]: one ADMITTED instrument's reads refused.
    VenueErrorFor { instrument: &'static str, code: i64, message: &'static str },
    /// The venue lists nothing under a label, and `private/get_open_orders` answers this JSON text.
    OpenOrdersAnswer(&'static str),
    /// Answer every private call with a frame carrying its id and neither a `result` nor an `error`.
    NoAnswerForever,
    /// [`Self::NoAnswerForever`] for `method` alone, every other call answered. `absent`: the venue lists no
    /// order under a label. `private/get_order_state` reads the order open at amount 10 and price 51000.
    NoAnswerOn { method: &'static str, absent: bool },
    /// An account with nothing: every `private/get_*` read answers its REAL empty shape.
    EmptyBook,
    /// The testnet book of one phase (`"after"`, `"closed"`) of `deribit_settlement_capture.json`: the account summary
    /// and the positions of the currency each read names, as recorded. Answers everything else like `OkForever`.
    Settlement(&'static str),
    /// Each `(method, JSON text)` answers that method's every call with that text; every other call is
    /// answered like [`Self::OkForever`].
    Scripted(&'static [(&'static str, &'static str)]),
}

/// Runs on the fake's thread with every non-auth `(method, params)`, right AFTER it is recorded and
/// BEFORE the fake answers it: what a test makes happen "at the venue" strictly ahead of the reply
/// (a fill pushed on another socket, say), with no timing in it.
pub type Hook = Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// A scripted Deribit stand-in. Dropping it stops the server thread and joins it.
pub struct FakeDeribit {
    pub url: String,
    connections: Arc<AtomicUsize>,
    methods: Arc<Mutex<Vec<(String, Value)>>>,
    auths: Arc<AtomicUsize>,
    listener_closed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeDeribit {
    pub fn spawn(script: Vec<Session>) -> FakeDeribit {
        Self::spawn_with_hook(script, Arc::new(|_: &str, _: &Value| {}))
    }

    /// [`Self::spawn`], with `hook` run before every non-auth reply (see [`Hook`]).
    pub fn spawn_with_hook(script: Vec<Session>, hook: Hook) -> FakeDeribit {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("ws://{}", listener.local_addr().expect("local addr"));
        listener.set_nonblocking(true).expect("nonblocking listener");

        let connections = Arc::new(AtomicUsize::new(0));
        let auths = Arc::new(AtomicUsize::new(0));
        let methods = Arc::new(Mutex::new(Vec::new()));
        let listener_closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (connection_list, seen, closed, stopped, auth_count) = (
            connections.clone(),
            methods.clone(),
            listener_closed.clone(),
            stop.clone(),
            auths.clone(),
        );

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
                connection_list.fetch_add(1, Ordering::Relaxed);
                stream.set_nonblocking(false).expect("blocking accepted stream");
                let Ok(mut ws) = tungstenite::accept(stream) else { continue };
                let _ = ws.get_ref().set_read_timeout(Some(TICK));
                serve(&mut ws, session, &seen, &stopped, &auth_count, &hook);
            }
            drop(listener);
            closed.store(true, Ordering::Relaxed);
        });

        FakeDeribit {
            url,
            connections,
            auths,
            methods,
            listener_closed,
            stop,
            handle: Some(handle),
        }
    }

    /// Every `public/auth` received (handshakes AND in-place re-auths), across all connections.
    pub fn auths(&self) -> usize {
        self.auths.load(Ordering::Relaxed)
    }

    /// Accepted connections. A re-dial shows up here as a second one.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    /// Every non-auth JSON-RPC method received, in order, across all connections.
    pub fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    /// The `params` object of every received `method` call, in order -- what a test reads to prove
    /// WHICH label a lookup carried or WHICH order id a cancel named.
    pub fn params_of(&self, method: &str) -> Vec<Value> {
        let calls = self.methods.lock().unwrap();
        calls.iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
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
/// `vike_deribit::execution::resync::map_deribit_history` reads directly. One CANCELLED order (the A3
/// replay's non-fill terminal) and one still-OPEN one, which must map to nothing: a test asserting
/// on the event list is then reading the real mapper's fold rather than a row count.
fn order_history_result() -> Value {
    json!([
        {"instrument_name": SYMBOL, "order_id": ORDER_ID, "label": RESYNC_CANCEL_ORDER_CLIENT_ID,
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
            "trade_id": RESYNC_TRADE_ID, "label": RESYNC_FILL_ORDER_CLIENT_ID, "order_id": ORDER_ID,
            "instrument_name": SYMBOL, "price": 50_000.0, "amount": 10.0, "direction": "buy",
            "fee": 0.001, "fee_currency": "BTC", "liquidity": "T",
            "timestamp": 1_700_000_000_005i64, "state": "filled",
        }],
        "has_more": false,
    })
}

/// The foreign-book session's answers (see [`Session::ForeignBook`]): the instruments the account
/// trades are the mounted one, an inverse ETH perpetual and a linear ETH USDC perpetual.
fn foreign_book_result(method: &str, params: &Value, label_currency: &str) -> Option<Value> {
    let instrument = params.get("instrument_name").and_then(Value::as_str).unwrap_or(SYMBOL);
    Some(match method {
        "private/get_order_state_by_label" => {
            if params.get("currency").and_then(Value::as_str) == Some(label_currency) {
                json!([{"order_id": ORDER_ID, "label": "vike-1", "order_state": "open"}])
            } else {
                json!([])
            }
        }
        "private/get_open_orders" => json!([{
            "instrument_name": "ETH_USDC-PERPETUAL", "order_id": ORDER_ID, "label": "vike-1",
            "direction": "buy", "order_type": "limit", "amount": 0.5, "filled_amount": 0.0,
            "average_price": 0.0, "last_update_timestamp": 1_700_000_000_000i64,
        }]),
        "private/get_open_orders_by_instrument" => json!([{
            "instrument_name": instrument, "order_id": ORDER_ID, "label": format!("open-{instrument}"),
            "direction": "buy", "order_type": "limit", "amount": 1.0, "filled_amount": 0.0,
            "average_price": 0.0, "last_update_timestamp": 1_700_000_000_000i64,
        }]),
        "private/get_positions" => {
            let all = params.get("currency").and_then(Value::as_str) == Some("any");
            let mut rows =
                vec![json!({"instrument_name": SYMBOL, "size": 10.0, "average_price": 50_000.0})];
            if all {
                rows.push(json!({"instrument_name": "ETH-PERPETUAL", "size": -5.0, "average_price": 2_500.0}));
                rows.push(json!({"instrument_name": "ETH_USDC-PERPETUAL", "size": 0.5, "average_price": 2_500.0}));
            }
            Value::Array(rows)
        }
        "private/get_user_trades_by_instrument" => json!({
            "trades": [{
                "trade_id": format!("t-{instrument}"), "label": "vike-1", "order_id": ORDER_ID,
                "instrument_name": instrument, "price": 2_500.0, "amount": 1.0, "direction": "buy",
                "fee": 0.001, "fee_currency": "ETH", "liquidity": "T",
                "timestamp": 1_700_000_000_005i64, "state": "filled",
            }],
            "has_more": false,
        }),
        _ => return None,
    })
}

/// The RESULT payload for one method, in the shape the real venue answers with.
fn result_for(method: &str, session: Session, params: &Value) -> Value {
    if let Session::Scripted(answers) = session
        && let Some((_, answer)) = answers.iter().find(|(m, _)| *m == method)
    {
        return serde_json::from_str(answer).expect("a scripted answer is JSON");
    }
    if let Session::OpenOrdersAnswer(answer) = session {
        match method {
            "private/get_order_state_by_label" => return json!([]),
            "private/get_open_orders" => {
                return serde_json::from_str(answer).expect("a scripted answer is JSON");
            }
            _ => {}
        }
    }
    if let Session::ForeignBook(label_currency) = session
        && let Some(result) = foreign_book_result(method, params, label_currency)
    {
        return result;
    }
    if let Session::Settlement(phase) = session
        && let Some(field) = match method {
            "private/get_account_summary" => Some("account_summary"),
            "private/get_positions" => Some("positions"),
            _ => None,
        }
    {
        let capture: Value =
            serde_json::from_str(include_str!("../fixtures/deribit_settlement_capture.json"))
                .expect("the settlement capture parses");
        let currency = params.get("currency").and_then(Value::as_str).unwrap_or("");
        return capture[phase][format!("{field}_{currency}")].clone();
    }
    if session == Session::EmptyBook && method.starts_with("private/get_") {
        return if method == "private/get_user_trades_by_instrument" {
            json!({"trades": [], "has_more": false})
        } else {
            json!([])
        };
    }
    if let Session::CrossingEdit { order_state: Some(state), .. } = session {
        let order = json!({"order_id": ORDER_ID, "label": "vike-1", "order_state": state,
            "cancel_reason": if state == "cancelled" { "edit_post_only_reject" } else { "" }});
        match method {
            "private/edit" => return json!({"order": order, "trades": []}),
            "private/get_order_state" => return order,
            _ => {}
        }
    }
    match method {
        // The audit-A3 resync's two reads — the ONLY methods its socket can carry besides the
        // `public/auth` handshake, which is what licenses that path's re-dial-and-retry.
        "private/get_order_history_by_instrument" => order_history_result(),
        "private/get_user_trades_by_instrument" => user_trades_result(),
        // `{"order": {...}, "trades": [...]}` — `dispatch_submit` reads `result.order.order_id`.
        // A trigger order (it carries `trigger_price`) is held `untriggered`, as the testnet answers it.
        "private/buy" | "private/sell" => json!({
            "order": {"order_id": ORDER_ID, "label": "vike-1",
                      "order_state": if params.get("trigger_price").is_some() { "untriggered" } else { "open" }},
            "trades": [],
        }),
        // The audit-T1 re-query: an ARRAY, empty when the venue never took the order.
        // The edit's real shape: the order as it now rests, under its one id.
        "private/edit" => json!({
            "order": {"order_id": ORDER_ID, "label": "vike-1", "order_state": "open"},
            "trades": [],
        }),
        "private/get_order_state"
            if matches!(session, Session::NoAnswerOn { .. } | Session::VenueErrorOn { .. }) =>
        {
            json!({
                "order_id": ORDER_ID, "label": "vike-1", "order_state": "open",
                "amount": 10.0, "price": 51_000.0,
            })
        }
        "private/get_order_state_by_label" => {
            if session == Session::OkForeverOrderAbsent
                || matches!(
                    session,
                    Session::NoAnswerOn { absent: true, .. }
                        | Session::VenueErrorOn { absent: true, .. }
                )
            {
                json!([])
            } else if session == Session::TriggeredStop {
                json!([{"order_id": CHILD_ID, "label": "vike-1", "order_state": "open"}])
            } else {
                json!([{"order_id": ORDER_ID, "label": "vike-1", "order_state": "open"}])
            }
        }
        "private/cancel" => json!({"order_id": ORDER_ID, "order_state": "cancelled"}),
        // The structure as asked, on a book that already trades (the idempotent create).
        "private/create_combo" => json!({
            "id": "FAKE-COMBO",
            "state": "active",
            "legs": params["trades"].as_array().into_iter().flatten().map(|t| {
                let amount = t["amount"].as_i64().unwrap_or(0);
                json!({
                    "instrument_name": t["instrument_name"],
                    "amount": if t["direction"] == "buy" { amount } else { -amount },
                })
            }).collect::<Vec<_>>(),
        }),
        // The operator's balance read: a wallet per currency, most of them empty.
        "private/get_account_summaries" => json!({"summaries": [
            {"currency": "ETH", "balance": 0.0},
            {"currency": "BTC", "balance": 1.5},
            {"currency": "USDC", "balance": 250.0},
        ]}),
        _ => open_orders_result(),
    }
}

/// Answer JSON-RPC frames by id until the session's script says to stop.
fn serve(
    ws: &mut WebSocket<TcpStream>,
    session: Session,
    seen: &Mutex<Vec<(String, Value)>>,
    stop: &AtomicBool,
    auths: &AtomicUsize,
    hook: &Hook,
) {
    let mut served = 0usize;
    let mut connection_auths = 0usize;
    let mut label_asks = 0usize;
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
                continue;
            }
            Err(_) => return, // the client hung up
        };
        let Message::Text(raw) = msg else { continue };
        let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else { continue };
        let id = frame.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
        let method = frame.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();

        if method == "public/auth" {
            auths.fetch_add(1, Ordering::Relaxed);
            connection_auths += 1;
            // Never recorded: the handshake is not a venue ACTION, and counting it would bury the
            // one number these tests are about.
            let reply = json!({"jsonrpc": "2.0", "id": id,
                "result": {"access_token": "tok", "refresh_token": "ref", "expires_in": 900}});
            let _ = ws.send(Message::Text(reply.to_string().into()));
            if session == Session::CloseAfterAuth {
                let _ = ws.close(None);
                let _ = ws.flush();
                return;
            }
            continue;
        }
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        seen.lock().unwrap().push((method.clone(), params.clone()));
        hook(&method, &params);

        if session == Session::CloseBeforeAnswering
            || (session == Session::CloseOnEdit && method == "private/edit")
            || (session == Session::CloseOnBuy && method == "private/buy")
        {
            // The request is recorded — the venue RECEIVED it — and the socket dies with no reply.
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        let names_orders = method == "private/subscribe"
            && params["channels"].as_array().is_some_and(|c| {
                c.iter().any(|c| c.as_str().is_some_and(|c| c.contains("user.orders")))
            });
        let reply = if let Session::FillStream { refuse_orders: true, .. } = session
            && names_orders
        {
            json!({"jsonrpc": "2.0", "id": id,
               "error": {"code": -32602, "message": "Invalid params"}})
        } else if session == Session::TokenExpired
            && connection_auths < 2
            && method.starts_with("private/")
        {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 13009, "message": "unauthorized"}})
        } else if session == Session::VenueErrorForever {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 10009, "message": "not_enough_funds"}})
        } else if let Session::VenueErrorOn { method: m, code, message, .. } = session
            && m == method
        {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        } else if let Session::VenueErrorFor { instrument, code, message } = session
            && params.get("instrument_name").and_then(Value::as_str) == Some(instrument)
        {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        } else if let Session::LabelAnswers(answers) = session
            && method == "private/get_order_state_by_label"
        {
            let answer = answers[label_asks.min(answers.len() - 1)];
            label_asks += 1;
            let result: Value = serde_json::from_str(answer).expect("a scripted answer is JSON");
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        } else if session == Session::NoAnswerForever
            || matches!(session, Session::NoAnswerOn { method: m, .. } if m == method)
        {
            json!({"jsonrpc": "2.0", "id": id})
        } else if matches!(session, Session::TriggeredStop | Session::AlreadyGone)
            && method == "private/cancel"
            && params.get("order_id").and_then(Value::as_str) == Some(ORDER_ID)
        {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 10004, "message": "order_not_found"}})
        } else if matches!(session, Session::CrossingEdit { refused: true, .. })
            && method == "private/edit"
        {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 11054, "message": "post_only_reject"}})
        } else if matches!(session, Session::CrossingEdit { order_state: None, .. })
            && method == "private/get_order_state"
        {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": 10028, "message": "too_many_requests"}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": result_for(&method, session, &params)})
        };
        let _ = ws.send(Message::Text(reply.to_string().into()));
        if let Session::FillStream { frames, .. } = session
            && method == "private/subscribe"
            && !names_orders
        {
            for frame in frames {
                let _ = ws.send(Message::Text((*frame).to_string().into()));
            }
        }

        served += 1;
        if let Session::OkThenClose(n) = session
            && served >= n
        {
            let _ = ws.close(None); // the venue-side close, landing BETWEEN requests
            let _ = ws.flush();
            return;
        }
    }
}

/// A `DeribitTransport` pointed at the fake, dialled and authed. The seam every owner builds
/// from: an owner that RE-DIALS has to hold the transport itself, so a helper that only ever hands
/// back an `Arc<VenueRestDeribit>` cannot serve `vike_deribit::execution::A3Resync`, which owns its socket.
///
/// It holds deribit's kept list first, so every client built on it reads [`SYMBOL`]'s USER name (`BTCUSD.P`)
/// whichever test ran before it: plain `cargo test` runs this binary's tests as threads of ONE process, and a
/// client that found no list would take the venue name for the user name only until a sibling test held one.
pub fn transport_against(fake: &FakeDeribit) -> vike_deribit::transport::client::DeribitTransport {
    vike_deribit::names::hold_deribit_kept_rows();
    let mut transport = vike_deribit::transport::client::DeribitTransport::new(
        &fake.url,
        "client-id",
        "client-secret",
        None,
    );
    transport.connect().expect("fake venue completes the JSON-RPC auth handshake");
    transport
}

/// A `VenueRestDeribit` whose order-WS points at the fake, connected and authed — the same shape
/// `ReconcileClientDeribit::connect` and `vike_deribit::execution` both build.
pub fn rest_against(fake: &FakeDeribit) -> Arc<vike_deribit::execution::rest::VenueRestDeribit> {
    Arc::new(vike_deribit::execution::rest::VenueRestDeribit::new(
        transport_against(fake),
        SYMBOL,
        vike_model::SymbolProperties::default(),
        "BTC",
    ))
}

/// The second of a submit's two events: the emitter split is `[OrderSubmitted, terminal-or-accepted]`.
pub fn outcome(events: Vec<vike_model::events::Event>) -> vike_model::events::Event {
    assert_eq!(events.len(), 2, "the emitter split is [OrderSubmitted, terminal-or-accepted]");
    assert_matches!(
        events[0],
        vike_model::events::Event::OrderSubmitted(_),
        "first event is always OrderSubmitted"
    );
    events.into_iter().nth(1).expect("second event")
}

/// The order's fate stays unknown: `OrderSubmitted` and nothing after it, neither a terminal nor an
/// accept, so the core's stuck-order ladder still holds the order and ends it.
pub fn stays_unknown(events: Vec<vike_model::events::Event>) {
    assert_matches!(
        events.as_slice(),
        [vike_model::events::Event::OrderSubmitted(_)],
        "an unknown order says nothing past OrderSubmitted"
    );
}

/// The reason of an `OrderRejected`; any other event fails the test.
pub fn rejection(event: vike_model::events::Event) -> String {
    match event {
        vike_model::events::Event::OrderRejected(r) => r.reason.to_string(),
        other => panic!("expected a rejection, got {other:?}"),
    }
}
