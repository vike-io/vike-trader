//! The scripted serve loop: framed replies to every handshake and order request, the reject modes.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;

use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    ProtoMessage, ProtoOaAccountAuthReq, ProtoOaAccountAuthRes, ProtoOaApplicationAuthRes,
    ProtoOaAsset, ProtoOaAssetClass, ProtoOaAssetClassListRes, ProtoOaAssetListRes,
    ProtoOaCtidTraderAccount, ProtoOaDeal, ProtoOaErrorRes, ProtoOaExecutionEvent,
    ProtoOaExecutionType, ProtoOaGetAccountListByAccessTokenRes, ProtoOaGetTrendbarsReq,
    ProtoOaGetTrendbarsRes, ProtoOaLightSymbol, ProtoOaNewOrderReq, ProtoOaOrder, ProtoOaOrderType,
    ProtoOaReconcileRes, ProtoOaSpotEvent, ProtoOaSubscribeSpotsRes, ProtoOaSymbol,
    ProtoOaSymbolByIdRes, ProtoOaSymbolCategory, ProtoOaSymbolCategoryListRes,
    ProtoOaSymbolsListRes, ProtoOaTradeData, ProtoOaTradeSide, ProtoOaTrader, ProtoOaTraderRes,
    ProtoOaTrendbar, pt,
};

use super::fake::AccountsScript;

/// A `Read`/`Write` stream that flags EOF (a `read` returning `Ok(0)`) so the serve loop can break
/// instead of busy-spinning on a closed socket — a test-local duplicate of `conn::EofStream`
/// (that one is `pub(crate)`, unreachable from `tests/`).
pub(super) struct EofStream<S> {
    inner: S,
    eof: Arc<AtomicBool>,
}

impl<S> EofStream<S> {
    pub(super) fn new(inner: S) -> (Self, Arc<AtomicBool>) {
        let eof = Arc::new(AtomicBool::new(false));
        (Self { inner, eof: eof.clone() }, eof)
    }
}

impl<S: Read> Read for EofStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.eof.store(true, Ordering::SeqCst);
        }
        Ok(n)
    }
}

impl<S: Write> Write for EofStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The `clientOrderId`/`label` the scripted reconnect `ReconcileRes` reports as a pending order —
/// the reconcile test asserts the actor's coid→orderId map resolves this coid after reconnect.
pub const RECONCILE_PENDING_COID: &str = "recon-coid-1";
/// The venue `orderId` paired with [`RECONCILE_PENDING_COID`] in the scripted `ReconcileRes`.
pub const RECONCILE_PENDING_ORDER_ID: i64 = 777_001;

/// Which request `serve_inner` diverts to a scripted `ERROR_RES` instead of the normal happy-path
/// reply — used to script the venue-reject integration tests (`tests/exec_reject.rs`,
/// `tests/data_bars.rs`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RejectMode {
    /// Normal scripted replies for everything.
    None,
    /// `NEW_ORDER_REQ` gets an `ERROR_RES` instead of accepted/filled execution events.
    Orders,
    /// `GET_TRENDBARS_REQ` gets an `ERROR_RES` instead of `GET_TRENDBARS_RES`.
    Trendbars,
    /// `APPLICATION_AUTH_REQ` gets an auth-rejection `ERROR_RES` instead of `APPLICATION_AUTH_RES` —
    /// the PERMANENT connect-retry case (`ConnError::Venue`, which must fail fast, never retry).
    AppAuth,
    /// The three broker reference-list requests get an `ERROR_RES` — a broker that declines them.
    ReferenceLists,
}

/// The access token a TOKEN-GATED fake admits at `ACCOUNT_AUTH_REQ`, switchable while it runs, plus
/// the log of every token a client presented. A request carrying any other token is answered with
/// the `ERROR_RES` a venue sends for a superseded token (`CH_ACCESS_TOKEN_INVALID`) — the shape the
/// handshake classifies as an AUTH failure. Lets a test model "a sibling rotated the grant, and the
/// venue now honours only the new access token" without a network or an OAuth endpoint.
#[derive(Clone)]
pub struct AcceptedToken {
    accepted: Arc<Mutex<String>>,
    presented: Arc<Mutex<Vec<String>>>,
}

impl AcceptedToken {
    pub fn new(initial: &str) -> Self {
        Self {
            accepted: Arc::new(Mutex::new(initial.to_string())),
            presented: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// From now on admit only `token` (the venue ended the previous one).
    pub fn accept_only(&self, token: &str) {
        *self.accepted.lock().expect("accepted lock") = token.to_string();
    }

    /// Every access token presented at `ACCOUNT_AUTH_REQ` so far, in arrival order.
    pub fn presented(&self) -> Vec<String> {
        self.presented.lock().expect("presented lock").clone()
    }

    fn admits(&self, presented: &str) -> bool {
        self.presented.lock().expect("presented lock").push(presented.to_string());
        *self.accepted.lock().expect("accepted lock") == presented
    }
}

pub(super) fn serve_inner(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    script: AccountsScript,
    drop_after: Option<u32>,
    reject: RejectMode,
    reconcile_inflight_fill: bool,
    mute_reconcile: bool,
    gate: Option<AcceptedToken>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                let payload_type = msg.payload_type;
                seen.lock().expect("seen lock").push(payload_type);
                // Mute mode: SILENCE for `RECONCILE_REQ` — no reply of any kind, not even an
                // `ERROR_RES`, while the socket stays open and every other request is answered
                // normally. That is the only shape that costs the client its full reconcile
                // deadline (a reject returns on the spot, a dead socket returns at EOF), so it is
                // the shape a timeout BOUND has to be pinned against.
                if mute_reconcile && payload_type == pt::RECONCILE_REQ {
                    continue;
                }
                // Reject mode: answer the scripted request with an ERROR_RES that echoes the
                // request's clientMsgId, so the client can correlate it (or, for the Trendbars
                // case, prove it correctly does NOT correlate it to an order). Orders/Trendbars use
                // a NON-auth `NOT_ENOUGH_MONEY` code; AppAuth uses an auth-rejection code — the
                // PERMANENT connect-retry case the client must fail fast on (`ConnError::Venue`).
                // Token gate: an `ACCOUNT_AUTH_REQ` whose access token the venue no longer honours
                // is refused with the superseded-token error, whatever the reject mode says.
                let gated = gate.as_ref().is_some_and(|gate| {
                    payload_type == pt::ACCOUNT_AUTH_REQ && {
                        let presented =
                            ProtoOaAccountAuthReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                                .map(|req| req.access_token)
                                .unwrap_or_default();
                        !gate.admits(&presented)
                    }
                });
                let reject_err: Option<(&str, &str)> = match reject {
                    _ if gated => Some(("CH_ACCESS_TOKEN_INVALID", "Access token is not valid")),
                    RejectMode::None => None,
                    RejectMode::Orders => (payload_type == pt::NEW_ORDER_REQ)
                        .then_some(("NOT_ENOUGH_MONEY", "insufficient funds")),
                    RejectMode::Trendbars => (payload_type == pt::GET_TRENDBARS_REQ)
                        .then_some(("NOT_ENOUGH_MONEY", "insufficient funds")),
                    RejectMode::AppAuth => (payload_type == pt::APPLICATION_AUTH_REQ)
                        .then_some(("CH_CLIENT_AUTH_FAILURE", "invalid client credentials")),
                    RejectMode::ReferenceLists => matches!(
                        payload_type,
                        pt::SYMBOL_CATEGORY_REQ | pt::ASSET_CLASS_LIST_REQ | pt::ASSET_LIST_REQ
                    )
                    .then_some(("INVALID_REQUEST", "not offered by this broker")),
                };
                if let Some((error_code, description)) = reject_err {
                    let err = ProtoOaErrorRes {
                        error_code: error_code.to_string(),
                        description: Some(description.to_string()),
                        ..Default::default()
                    };
                    let msg_id = msg.client_msg_id.as_deref().unwrap_or("");
                    let frame = framing::encode(pt::ERROR_RES, &err.encode_to_vec(), msg_id);
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                    continue;
                }
                for (reply_type, body) in script_reply(&msg, script, reconcile_inflight_fill) {
                    let frame = framing::encode(reply_type, &body, "srv");
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                }
                if drop_after == Some(payload_type) {
                    return; // simulate the connection dying right here
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break; // client closed
                }
            }
            Err(_) => break,
        }
    }
}

/// The scripted `SPOT_EVENT`'s `timestamp` (epoch milliseconds) — a live-captured value
/// (~2026-07-14) used so `tests/data_spot.rs` can assert it flows through to `QuoteTick::ts`
/// UNCHANGED (milliseconds in, milliseconds out — no further scaling/conversion).
pub const SPOT_EVENT_TIMESTAMP_MS: i64 = 1_784_017_625_503;

/// Map a request payload type to its scripted reply frame(s) `(payload_type, encoded_body)`.
/// Most requests get exactly one reply; `SUBSCRIBE_SPOTS_REQ` gets its `SUBSCRIBE_SPOTS_RES` PLUS
/// an unsolicited `SPOT_EVENT` (EURUSD id=1, bid=113911/ask=113912, timestamp=
/// `SPOT_EVENT_TIMESTAMP_MS` — mirrors real cTrader, which pushes a technical spot event with the
/// current price right after a subscription is accepted) so `tests/data_spot.rs` can assert the
/// actor decodes+routes it to a `LiveDataSink`.
pub(super) fn script_reply(
    msg: &ProtoMessage,
    script: AccountsScript,
    reconcile_inflight_fill: bool,
) -> Vec<(u32, Vec<u8>)> {
    match msg.payload_type {
        pt::APPLICATION_AUTH_REQ => {
            vec![(pt::APPLICATION_AUTH_RES, ProtoOaApplicationAuthRes::default().encode_to_vec())]
        }
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ => {
            let accounts = match script {
                AccountsScript::OneDemo => vec![ProtoOaCtidTraderAccount {
                    ctid_trader_account_id: 99,
                    is_live: Some(false),
                    trader_login: Some(1234),
                    ..Default::default()
                }],
                AccountsScript::OnlyLive => vec![ProtoOaCtidTraderAccount {
                    ctid_trader_account_id: 77,
                    is_live: Some(true),
                    trader_login: Some(5678),
                    ..Default::default()
                }],
            };
            let res = ProtoOaGetAccountListByAccessTokenRes {
                access_token: "token".to_string(),
                ctid_trader_account: accounts,
                ..Default::default()
            };
            vec![(pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES, res.encode_to_vec())]
        }
        pt::ACCOUNT_AUTH_REQ => {
            let res = ProtoOaAccountAuthRes { ctid_trader_account_id: 99, ..Default::default() };
            vec![(pt::ACCOUNT_AUTH_RES, res.encode_to_vec())]
        }
        pt::TRADER_REQ => {
            let res = ProtoOaTraderRes {
                ctid_trader_account_id: 99,
                trader: ProtoOaTrader {
                    ctid_trader_account_id: 99,
                    money_digits: Some(FAKE_MONEY_DIGITS),
                    ..Default::default()
                },
                ..Default::default()
            };
            vec![(pt::TRADER_RES, res.encode_to_vec())]
        }
        // The three broker reference lists, shaped as the demo broker publishes them
        // (`tests/fixtures/ctrader_catalog_capture.json`): category 1 `Default Category` -> asset
        // class 1 `Forex`; assets EUR (5) and USD (16).
        pt::SYMBOL_CATEGORY_REQ => {
            let res = ProtoOaSymbolCategoryListRes {
                ctid_trader_account_id: 99,
                symbol_category: vec![ProtoOaSymbolCategory {
                    id: 1,
                    asset_class_id: 1,
                    name: "Default Category".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::SYMBOL_CATEGORY_RES, res.encode_to_vec())]
        }
        pt::ASSET_CLASS_LIST_REQ => {
            let res = ProtoOaAssetClassListRes {
                ctid_trader_account_id: 99,
                asset_class: vec![ProtoOaAssetClass {
                    id: Some(1),
                    name: Some("Forex".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::ASSET_CLASS_LIST_RES, res.encode_to_vec())]
        }
        pt::ASSET_LIST_REQ => {
            let asset = |asset_id, name: &str| ProtoOaAsset {
                asset_id,
                name: name.to_string(),
                ..Default::default()
            };
            let res = ProtoOaAssetListRes {
                ctid_trader_account_id: 99,
                asset: vec![asset(5, "EUR"), asset(16, "USD")],
                ..Default::default()
            };
            vec![(pt::ASSET_LIST_RES, res.encode_to_vec())]
        }
        pt::SYMBOLS_LIST_REQ => {
            let res = ProtoOaSymbolsListRes {
                ctid_trader_account_id: 99,
                symbol: vec![ProtoOaLightSymbol {
                    symbol_id: 1,
                    symbol_name: Some("EURUSD".to_string()),
                    enabled: Some(true),
                    symbol_category_id: Some(1),
                    base_asset_id: Some(5),
                    quote_asset_id: Some(16),
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::SYMBOLS_LIST_RES, res.encode_to_vec())]
        }
        pt::SYMBOL_BY_ID_REQ => {
            let res = ProtoOaSymbolByIdRes {
                ctid_trader_account_id: 99,
                symbol: vec![ProtoOaSymbol {
                    symbol_id: 1,
                    digits: 5,
                    pip_position: 4,
                    ..Default::default()
                }],
                ..Default::default()
            };
            vec![(pt::SYMBOL_BY_ID_RES, res.encode_to_vec())]
        }
        pt::SUBSCRIBE_SPOTS_REQ => {
            let res = ProtoOaSubscribeSpotsRes { ctid_trader_account_id: 99, ..Default::default() };
            let sub_res = (pt::SUBSCRIBE_SPOTS_RES, res.encode_to_vec());
            let spot_event = ProtoOaSpotEvent {
                ctid_trader_account_id: 99,
                symbol_id: 1,
                bid: Some(113911),
                ask: Some(113912),
                timestamp: Some(SPOT_EVENT_TIMESTAMP_MS),
                ..Default::default()
            };
            vec![sub_res, (pt::SPOT_EVENT, spot_event.encode_to_vec())]
        }
        pt::NEW_ORDER_REQ => {
            // Decode the incoming order so the scripted execution events echo the client's real
            // clientOrderId/symbol/volume (mirrors cTrader: a NewOrder acks with ORDER_ACCEPTED
            // then, for a market order, ORDER_FILLED). Malformed → no reply.
            match ProtoOaNewOrderReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                Ok(order) => new_order_exec_events(&order),
                Err(_) => Vec::new(),
            }
        }
        pt::GET_TRENDBARS_REQ => {
            // Echo the request's symbolId/period back (F2 historical seed, `tests/data_bars.rs`).
            // Malformed → no reply (matches every other request's malformed-decode handling here).
            match ProtoOaGetTrendbarsReq::decode(msg.payload.as_deref().unwrap_or(&[])) {
                Ok(req) => vec![(pt::GET_TRENDBARS_RES, get_trendbars_res(&req).encode_to_vec())],
                Err(_) => Vec::new(),
            }
        }
        pt::RECONCILE_REQ => {
            // F3: answer the reconnect reconcile with ONE pending LIMIT order carrying the known
            // coid (in both the dedicated `clientOrderId` field and the `label`) + venue orderId,
            // so the client can rebuild its coid→orderId map from it.
            let order = ProtoOaOrder {
                order_id: RECONCILE_PENDING_ORDER_ID,
                trade_data: ProtoOaTradeData {
                    symbol_id: 1,
                    volume: 100,
                    trade_side: ProtoOaTradeSide::Buy as i32,
                    label: Some(RECONCILE_PENDING_COID.to_string()),
                    ..Default::default()
                },
                order_type: ProtoOaOrderType::Limit as i32,
                order_status: 1, // pending/accepted — irrelevant to the coid→orderId rebuild
                client_order_id: Some(RECONCILE_PENDING_COID.to_string()),
                limit_price: Some(1.10),
                ..Default::default()
            };
            let res = ProtoOaReconcileRes {
                ctid_trader_account_id: 99,
                order: vec![order],
                position: vec![],
                ..Default::default()
            };
            let mut frames = Vec::new();
            if reconcile_inflight_fill {
                // F3 fix 1: an EXECUTION_EVENT (a full fill) that lands DURING the reconcile
                // window, BEFORE the RECONCILE_RES — the actor must buffer + replay it, not drop
                // it. Sent first so it is read while `read_reconcile_res` is still waiting.
                frames.push((pt::EXECUTION_EVENT, inflight_fill_event().encode_to_vec()));
            }
            frames.push((pt::RECONCILE_RES, res.encode_to_vec()));
            frames
        }
        _ => Vec::new(), // heartbeats etc. — record but don't reply
    }
}

/// The two scripted trendbars `GET_TRENDBARS_REQ` gets back, returned in REVERSE-chronological
/// order (newer bar first) so `tests/data_bars.rs` can prove `conn::on_get_trendbars_res` sorts
/// ascending before calling `seed_bars` — cTrader's actual wire order is not documented/
/// guaranteed. Descaled (÷[`event_mapper::RELATIVE_PRICE_SCALE`], i.e. 1e5) OHLC:
/// - older (`utc_timestamp_in_minutes=1000`): low=1.139, open=1.13905, close=1.13912, high=1.1392,
///   volume=42.
/// - newer (`utc_timestamp_in_minutes=1005`): low=1.1395, open=1.13953, close=1.13975,
///   high=1.1398, volume=17.
pub const SEED_BAR_OLDER_MINUTES: u32 = 1000;
pub const SEED_BAR_NEWER_MINUTES: u32 = 1005;

fn get_trendbars_res(req: &ProtoOaGetTrendbarsReq) -> ProtoOaGetTrendbarsRes {
    let newer = ProtoOaTrendbar {
        volume: 17,
        period: Some(req.period),
        low: Some(113_950),
        delta_open: Some(3),
        delta_close: Some(25),
        delta_high: Some(30),
        utc_timestamp_in_minutes: Some(SEED_BAR_NEWER_MINUTES),
    };
    let older = ProtoOaTrendbar {
        volume: 42,
        period: Some(req.period),
        low: Some(113_900),
        delta_open: Some(5),
        delta_close: Some(12),
        delta_high: Some(20),
        utc_timestamp_in_minutes: Some(SEED_BAR_OLDER_MINUTES),
    };
    ProtoOaGetTrendbarsRes {
        ctid_trader_account_id: req.ctid_trader_account_id,
        period: req.period,
        trendbar: vec![newer, older], // deliberately newest-first
        symbol_id: Some(req.symbol_id),
        ..Default::default()
    }
}

/// The venue order id the fake assigns to a placed order (echoed in both execution events so the
/// client's coid→orderId map is populated and `venue_order_id` flows through).
pub const FAKE_ORDER_ID: i64 = 555_001;
/// The fill price the fake reports on the scripted `ORDER_FILLED` (absolute double — cTrader order
/// prices/deal prices are NOT 1e5-scaled, unlike the spot feed).
pub const FAKE_FILL_PRICE: f64 = 1.13950;
/// The `ProtoOATrader.money_digits` the fake's `TRADER_RES` reports — the LIVE-VERIFIED value
/// (2026-07-14 real demo account) used everywhere in this crate's tests that descale a fill's
/// commission.
pub const FAKE_MONEY_DIGITS: u32 = 2;

/// Build the scripted `ORDER_ACCEPTED` then `ORDER_FILLED` execution events for a placed order,
/// echoing its `clientOrderId`/`symbolId`/`volume`. The FILLED event carries a `deal` with the
/// full filled volume at [`FAKE_FILL_PRICE`], matching a market order that fills immediately.
fn new_order_exec_events(order: &ProtoOaNewOrderReq) -> Vec<(u32, Vec<u8>)> {
    let coid = order.client_order_id.clone().or_else(|| order.label.clone());
    let order_ref = ProtoOaOrder {
        order_id: FAKE_ORDER_ID,
        trade_data: ProtoOaTradeData {
            symbol_id: order.symbol_id,
            volume: order.volume,
            trade_side: order.trade_side,
            label: order.label.clone(),
            ..Default::default()
        },
        order_type: order.order_type,
        order_status: 1, // ACCEPTED
        client_order_id: coid.clone(),
        ..Default::default()
    };
    let accepted = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        order: Some(order_ref.clone()),
        ..Default::default()
    };
    let mut filled_order = order_ref;
    filled_order.order_status = 2; // FILLED
    let filled = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(filled_order),
        deal: Some(ProtoOaDeal {
            deal_id: 900_001,
            order_id: FAKE_ORDER_ID,
            volume: order.volume,
            filled_volume: order.volume,
            symbol_id: order.symbol_id,
            execution_timestamp: 1_784_017_625_777,
            execution_price: Some(FAKE_FILL_PRICE),
            trade_side: order.trade_side,
            ..Default::default()
        }),
        ..Default::default()
    };
    vec![
        (pt::EXECUTION_EVENT, accepted.encode_to_vec()),
        (pt::EXECUTION_EVENT, filled.encode_to_vec()),
    ]
}

/// The coid the in-flight `ORDER_FILLED` (buffered during the reconcile window) fills — the F3
/// fix-1 test asserts an `Event::OrderFilled` for this coid reaches the ingest lane.
pub const INFLIGHT_FILL_COID: &str = "inflight-fill-coid-1";
/// The venue `orderId`/`dealId` on that in-flight fill.
pub const INFLIGHT_FILL_ORDER_ID: i64 = 888_002;
pub const INFLIGHT_FILL_DEAL_ID: i64 = 900_888;
/// The absolute fill price on that in-flight fill.
pub const INFLIGHT_FILL_PRICE: f64 = 1.14200;

/// Build the in-flight `ORDER_FILLED` execution event (EURUSD id=1) the reconnect fake sends
/// BEFORE its `RECONCILE_RES`, to prove the reconcile path buffers + replays it rather than
/// dropping it. A full fill (`ORDER_FILLED`) of the whole volume at [`INFLIGHT_FILL_PRICE`].
fn inflight_fill_event() -> ProtoOaExecutionEvent {
    let order = ProtoOaOrder {
        order_id: INFLIGHT_FILL_ORDER_ID,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100,
            trade_side: ProtoOaTradeSide::Buy as i32,
            label: Some(INFLIGHT_FILL_COID.to_string()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Market as i32,
        order_status: 2, // FILLED
        client_order_id: Some(INFLIGHT_FILL_COID.to_string()),
        ..Default::default()
    };
    ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(order),
        deal: Some(ProtoOaDeal {
            deal_id: INFLIGHT_FILL_DEAL_ID,
            order_id: INFLIGHT_FILL_ORDER_ID,
            volume: 100,
            filled_volume: 100,
            symbol_id: 1,
            execution_timestamp: 1_784_017_626_000,
            execution_price: Some(INFLIGHT_FILL_PRICE),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Human-readable payload-type name for `assert_saw` diagnostics.
pub(super) fn pt_name(payload_type: u32) -> String {
    let name = match payload_type {
        pt::HEARTBEAT_EVENT => "HEARTBEAT_EVENT",
        pt::APPLICATION_AUTH_REQ => "APPLICATION_AUTH_REQ",
        pt::APPLICATION_AUTH_RES => "APPLICATION_AUTH_RES",
        pt::ACCOUNT_AUTH_REQ => "ACCOUNT_AUTH_REQ",
        pt::ACCOUNT_AUTH_RES => "ACCOUNT_AUTH_RES",
        pt::TRADER_REQ => "TRADER_REQ",
        pt::TRADER_RES => "TRADER_RES",
        pt::SYMBOL_CATEGORY_REQ => "SYMBOL_CATEGORY_REQ",
        pt::SYMBOL_CATEGORY_RES => "SYMBOL_CATEGORY_RES",
        pt::ASSET_CLASS_LIST_REQ => "ASSET_CLASS_LIST_REQ",
        pt::ASSET_CLASS_LIST_RES => "ASSET_CLASS_LIST_RES",
        pt::ASSET_LIST_REQ => "ASSET_LIST_REQ",
        pt::ASSET_LIST_RES => "ASSET_LIST_RES",
        pt::SYMBOLS_LIST_REQ => "SYMBOLS_LIST_REQ",
        pt::SYMBOLS_LIST_RES => "SYMBOLS_LIST_RES",
        pt::SYMBOL_BY_ID_REQ => "SYMBOL_BY_ID_REQ",
        pt::SYMBOL_BY_ID_RES => "SYMBOL_BY_ID_RES",
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ => "GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ",
        pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES => "GET_ACCOUNTS_BY_ACCESS_TOKEN_RES",
        pt::SUBSCRIBE_SPOTS_REQ => "SUBSCRIBE_SPOTS_REQ",
        pt::SUBSCRIBE_SPOTS_RES => "SUBSCRIBE_SPOTS_RES",
        pt::UNSUBSCRIBE_SPOTS_REQ => "UNSUBSCRIBE_SPOTS_REQ",
        pt::SPOT_EVENT => "SPOT_EVENT",
        pt::SUBSCRIBE_LIVE_TRENDBAR_REQ => "SUBSCRIBE_LIVE_TRENDBAR_REQ",
        pt::UNSUBSCRIBE_LIVE_TRENDBAR_REQ => "UNSUBSCRIBE_LIVE_TRENDBAR_REQ",
        pt::GET_TRENDBARS_REQ => "GET_TRENDBARS_REQ",
        pt::GET_TRENDBARS_RES => "GET_TRENDBARS_RES",
        pt::NEW_ORDER_REQ => "NEW_ORDER_REQ",
        pt::CANCEL_ORDER_REQ => "CANCEL_ORDER_REQ",
        pt::AMEND_ORDER_REQ => "AMEND_ORDER_REQ",
        pt::CLOSE_POSITION_REQ => "CLOSE_POSITION_REQ",
        pt::RECONCILE_REQ => "RECONCILE_REQ",
        pt::RECONCILE_RES => "RECONCILE_RES",
        pt::EXECUTION_EVENT => "EXECUTION_EVENT",
        other => return format!("PT_{other}"),
    };
    name.to_string()
}
