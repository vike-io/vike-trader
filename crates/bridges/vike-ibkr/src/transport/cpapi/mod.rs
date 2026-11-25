//! The cpapi (Client Portal Web API) backend: REST place/cancel/modify/resync + a WebSocket push
//! pump, adapted onto the socket-shaped `IbkrTransport` seam (background pump → mpsc → next_recv,
//! exactly like `IbkrTransportSocket`). Assumes a RUNNING, browser-authenticated CP Gateway at
//! `config.cpapi_url` (the tier's `ibkr <type> trade rest cpapi` row, `crate::config::gateway_addresses`).
//! Lifecycle/login is out of scope here.
//!
//! Modules: `endpoints` = REST wrappers (`CpapiRest`); `decode` = pure JSON→IbkrInbound / conId
//! decoders; `reply` = the order reply-confirmation state machine. This file wires them into
//! `IbkrTransportCpapi`.
//!
//! ## The order client id→order_id map trick (keeps the shared `EventMapper` byte-identical)
//! Decoders emit `IbkrInbound` with `order_id: 0` and `order_ref: <cOID>` (the order client id). `IbkrTransportCpapi`
//! keeps a `Numbering` map, populated in `place_order` (the order client id the exec loop bound → the numeric
//! `order_id` arg it allocated). `next_recv`/`request_open_orders` fill the numeric via
//! `number_inbound`/`resolve_numeric` before handing the inbound to the shared `run_execution` loop, so
//! `EventMapper.fill_state` (keyed by numeric order_id) matches exactly like the socket backend —
//! no change needed to the shared layer. An order client id with no allocation (a foreign/prior-session order
//! surfaced by reconnect resync) gets a fresh unique NEGATIVE synthetic id so distinct foreign
//! orders never collide on id 0 in the shared `IdRegistry`.
//!
//! ## Wire shapes — LIVE-VERIFIED against a paper CP Gateway (DUQ186573, 2026-07-15)
//! The submit→accept→cancel round-trip was proven end-to-end via `tests/ibkr_smokes/ibkr_cpapi_smoke.rs`.
//! Ground truth captured that run:
//!   - REST paths under `/v1/api/iserver/...`; place = `POST /iserver/account/{acct}/orders` with
//!     body `{"orders":[{conid, orderType, side, tif, quantity, price}]}`.
//!   - Place returns a **CHAIN** of confirm questions (price-constraint → no-market-data → …), each
//!     answered by `POST /iserver/reply/{id}` `{"confirmed":true}`, until a terminal row
//!     `[{"order_id":"1794719002","order_status":"PreSubmitted"}]`. `place_order` loops the replies
//!     (bounded by `MAX_REPLY_HOPS`, and every call by `PLACE_HOLD`: the `IBKR_CPAPI` row's submit chain, held
//!     under the core's last-resort reject) and captures that `order_id`.
//!   - `orderType` `LMT`, `side` `BUY`, `tif` `GTC` — accepted: the run placed ONE order, a far buy limit.
//!     Nothing else was placed, so `MKT`, `SELL`, `STP` and `STOP_LIMIT` are unmeasured. A stop's trigger rides
//!     `auxPrice` and a stop-limit is `STOP_LIMIT` (`order_json`'s doc quotes IBKR's schema at
//!     `https://api.ibkr.com/gw/api/v3/api-docs`); neither form has ever been placed through a CP Gateway.
//!     ⚠ FOR MONDAY 2026-11-09 OR LATER (no IBKR contact before then): `tests/ibkr_smokes/ibkr_cpapi_stop_kinds_smoke.rs`
//!     places, reads back and cancels one `STP` and one `STOP_LIMIT` on the paper CP Gateway to measure both forms.
//!   - **Cancel-key identity:** cancel/modify address the order by the GATEWAY's own `order_id`
//!     (captured at accept), NOT our allocated numeric — the numeric fails
//!     `{"error":"OrderID 101 doesn't exist"}`. Cancel = `DELETE /iserver/account/{acct}/order/{gw_id}`.
//!   - `secdef/search` returns `conid` as a JSON **string** (`decode.rs::decode_conid`).
//!   - The WS `sor` stream is a best-effort supplement; the accept is emitted directly from the place's REST
//!     reply (`place_order`), and a cancel's end from the order list's row read back after the cancel's REST ack
//!     (`read_cancels_back`: the ack takes the cancel, it is not the order's end), so the lifecycle does not depend
//!     on the WS payload shape.
//!
//! ## ⚠⚠ MEASURED 2026-08-23: the `sor` topic answers NOTHING, so cpapi FILLS ARE INVISIBLE
//!
//! The subscribe frame `pump_loop` sends (`sor+{}`) was carried as a GUESS. It is no longer a
//! guess — it is measured, and it does not work. Against the CI box's live authenticated gateway
//! (DUQ186573, Build 10.46.2d), on the SAME socket and in the SAME session:
//!
//! | subscribe frame | topic frames received |
//! |---|---|
//! | `sor+{}` | **none, ever** |
//! | `str+{}` (trades) | `{"topic":"str","args":[]}` |
//! | `spl+{}` (PnL) | `{"topic":"spl","args":{"DUQ186573.Core":{…}}}` |
//! | `sld+{acct}` (ledger) | full ledger row |
//! | `ssd+{acct}` (summary) | full summary row |
//!
//! So the socket, the auth and the subscribe mechanism are all fine — `sor` alone is inert. Three
//! spellings were tried (`sor+{}` on open, `sor+{}` after the `sts` auth frame with the REST order
//! snapshot primed first, and a bare `sor`); all three answered nothing. A place → accept → cancel
//! cycle driven WHILE subscribed produced `ntf` frames naming that very `orderId` and still no
//! `sor` frame, so the gateway plainly knew about the order.
//!
//! **Why the smoke is green anyway, and why that hid this:** `place_order` synthesizes its `OrderStatus` inbound
//! directly from the REST reply, and a cancel's end is read off the REST order list (`read_cancels_back`), so the
//! whole submit→accept→cancel lifecycle the smoke asserts never touches the WS. The doc line above
//! calls the WS "a best-effort supplement" and is right about accept/cancel — but a FILL is not
//! supplementary and has no other live path:
//!
//! * `decode::decode_websocket_frame` only decodes `topic == "sor"`, so no execution row can ever arrive.
//! * `request_executions` is deliberately NOT overridden here — `IbkrTransport`'s default doc argues
//!   a cpapi fill "structurally cannot strand" because one `sor` row decodes into both halves at
//!   once. That argument is sound and its premise is false: there is no `sor` row.
//! * The only surviving path is `request_open_orders`, which DOES fetch `/iserver/account/trades`
//!   and decode it — but it runs only on `IbkrInbound::StreamResync`, i.e. when the WS happens to
//!   drop or stall and reopens. A fill is therefore recovered by accident, on a reconnect that may never come.
//!
//! **NOT FIXED HERE, deliberately.** The candidate is `str+{}`, which answers — but its row shape is
//! unknown while no fill exists to observe, and inventing a decoder for it would be exactly the
//! guess this section exists to retire. What it needs is a live fill: a marketable order during a
//! trading session (US equities were CLOSED when this was measured — Sunday), after which `str`'s
//! rows can be captured and `decode_websocket_frame` extended against real bytes.

// `pub` (not `pub(crate)`): `transport::mod` re-exports this module further as `cpapi_decode` for
// the no-network fixture test (`tests/ibkr_cpapi_decode.rs`), which needs it visible outside the
// crate too — `pub(crate)` alone cannot be re-exported past the crate boundary (E0365).
pub mod decode;
mod endpoints;
mod reply;

pub use endpoints::CpapiRest;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_bridge_core::websocket_session::user::reconnect::RECONNECT_LOG;
use vike_bridge_core::websocket_session::user::stream_link::{self, VenueLinkStream};
use vike_bridge_core::websocket_session::user::{
    USER_DATA_POLL, WebsocketSessionUserOpenOutcome, WebsocketSessionUserStream, run_user,
};
use vike_transport::websocket::{WsSocket, configure_websocket_stream};
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings;

use super::{AmendOutcome, IbkrInbound, IbkrTransport};
use crate::config::IbkrConfig;
use crate::execution::amend::same;
use crate::execution::event_mapper::IbkrOrderStatus;
use crate::execution::order::{IbkrOrderSpec, OrderStatusKind};
use crate::instrument::contract::IbkrContract;
use crate::transport::error_codes::IbkrError;
use decode::{
    decode_conid, decode_executions, decode_open_orders, decode_websocket_frame, gateway_order_ids,
    order_status_words,
};
use reply::{PlaceOutcome, parse_place_response};

/// Bound on the confirmation questions a single `place_order` call answers, so a pathological gateway
/// that never stops asking questions cannot hang the exec loop's calling thread forever. The answer to
/// the last reply is read like every other: it may be the accept.
const MAX_REPLY_HOPS: usize = 8;

/// The bound on one cpapi REST call: the whole-call timeout of `CpapiRest`'s agent (`vike_transport::http`'s
/// `blocking_agent` and `blocking_agent_loopback_insecure`, both on `DEFAULT_TIMEOUT`).
const CALL_BOUND: Duration = vike_transport::http::DEFAULT_TIMEOUT;

/// **The most one `place_order` may hold the exec thread**, from its top: a call (the order POST, a confirmation reply)
/// is made only if it can end inside it ([`next_call_fits`]), so the place answers by the queue's wait plus this, before
/// the core's last-resort reject. A chain that would run past it is rejected unsent: the gateway places nothing until its
/// last question is answered. Today a place and its usual one to three questions answer in well under a second; the
/// chain this cuts (eight replies of 30 s each) was answered minutes after the core had rejected the order.
const PLACE_HOLD: Duration = Duration::from_secs(35);

// The row is this code (`vike_venues::venue_timings::IBKR_CPAPI`'s `submit`, the chain the table's test holds under
// the core's last resort): the exec queue's wait, then the contract-id search, the order POST and up to
// `MAX_REPLY_HOPS` confirmation replies, each at the agent's bound, all inside the hold.
const _: () = assert!(match venue_timings::IBKR_CPAPI.exec().submit {
    Some(chain) => {
        chain.write_by.as_nanos() == vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT.as_nanos()
            && chain.calls.len() == 3
            && chain.calls[2].count as usize == MAX_REPLY_HOPS
            && chain.calls[0].each.as_nanos() == CALL_BOUND.as_nanos()
            && chain.calls[1].each.as_nanos() == CALL_BOUND.as_nanos()
            && chain.calls[2].each.as_nanos() == CALL_BOUND.as_nanos()
            && matches!(chain.hold, Some(hold) if hold.as_nanos() == PLACE_HOLD.as_nanos())
    }
    None => false,
});

/// Can one more call at [`CALL_BOUND`] still end by `held_until`, at `now`?
fn next_call_fits(now: Instant, held_until: Instant) -> bool {
    now + CALL_BOUND <= held_until
}

/// The words of a place the hold cut short (`what` is the call it did not make).
fn out_of_hold(what: &str) -> String {
    format!(
        "this order was NOT placed: {what} could not end inside the {} s a place may take, so it was not made and \
         the gateway holds no order. Send it again if it is still wanted.",
        PLACE_HOLD.as_secs()
    )
}

/// How long after a place of UNKNOWN outcome the one open-orders resync is requested
/// (`IbkrTransportCpapi::place_unknown`): long enough for a received order to be listed.
const AMBIGUOUS_PLACE_RESYNC_AFTER: Duration = Duration::from_secs(2);

/// How long after a cancel the gateway took (or may have taken) the order list is read for its end, and between two
/// such reads (`IbkrTransportCpapi::read_cancels_back`): IB ended a socket cancel in 0.16 s live (the crate's `CLAUDE.md`).
const CANCEL_READ_AFTER: Duration = Duration::from_secs(2);

/// The most reads of the order list one cancel waits for its end (a cancel IB has not yet enacted reads `PendingCancel`),
/// [`CANCEL_READ_AFTER`] apart; past them the order stays live until its own word or the reconcile ends it.
const CANCEL_READS: u32 = 5;

/// What one read of the order list says of a cancel in doubt ([`cancel_read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancelRead {
    /// The row states the cancel's end (`Cancelled`, `ApiCancelled`): the order ends with it.
    Ended,
    /// The row still works or waits for the cancel (`PendingCancel`, `PreCancelled`, a working word), or the list could
    /// not be read: read again.
    Again,
    /// The row states another end (`Filled`, `Inactive`, a word nobody documents), or the list does not name the order:
    /// nothing here settles it; its own word or the reconcile ends it (a cpapi fill has no live path, module doc).
    Elsewhere,
}

/// [`CancelRead`] of one order list read: `listed` = the list was read, `word` = the order's row status word, if listed.
fn cancel_read(listed: bool, word: Option<&str>) -> CancelRead {
    let Some(word) = word else {
        return if listed { CancelRead::Elsewhere } else { CancelRead::Again };
    };
    match OrderStatusKind::from_ibkr(word) {
        OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled => CancelRead::Ended,
        kind if kind.is_active()
            || matches!(kind, OrderStatusKind::PendingCancel | OrderStatusKind::ApiPending) =>
        {
            CancelRead::Again
        }
        _ => CancelRead::Elsewhere,
    }
}

/// The pause between the (at most three) reads of `IbkrTransportCpapi::read_open_orders`: 250 ms keeps
/// them under the ~5 req/s the Client Portal Web API documents for this endpoint family (secondary
/// sources; the repo declares no IBKR rate row, `vike_venues::venue_rate_limits::IBKR` is
/// `NOT_DECLARED`). A literal, not a `venue_timings` row: that table is a shared home.
const OPEN_ORDERS_GAP: Duration = Duration::from_millis(250);

/// The REST `tickle` cadence that keeps the gateway session alive: the exec lane's
/// `session_renew_every` in `vike_venues::venue_timings::IBKR_CPAPI`. It produces no inbound frame
/// on the socket, which is why the table does not call it a keepalive.
const TICKLE_EVERY: Duration = venue_timings::IBKR_CPAPI.exec().session_renew_every();

/// The `tic` frame that keeps the Client Portal socket's session alive: the exec lane's `keepalive_every` in
/// `vike_venues::venue_timings::IBKR_CPAPI`. IBKR answers it with nothing, so it proves nothing about the socket.
const TIC_EVERY: Duration = venue_timings::IBKR_CPAPI.exec().keepalive_every();

// The REST tickle rides the `tic` hook (`run_pump`), so the two cadences must be one.
const _: () = assert!(TIC_EVERY.as_nanos() == TICKLE_EVERY.as_nanos());

/// The longest silence before `pump_loop` calls a session dead and dials again: the exec lane's `idle_threshold` in
/// `vike_venues::venue_timings::IBKR_CPAPI`, six of the gateway's documented 10 s `system` heartbeats.
const IDLE_LIMIT: Duration = venue_timings::IBKR_CPAPI.exec().idle_threshold();

/// The nap after a session that had opened ends, before `pump_loop` dials again: a local 1 s the table does not model,
/// kept as it was before the pump moved onto `run_user`.
const REDIAL_AFTER_DROP: Duration = Duration::from_secs(1);

/// `pump_loop`'s reconnect ladder: the exec lane's fixed `reconnect` in `vike_venues::venue_timings::IBKR_CPAPI` after a
/// failed dial, [`REDIAL_AFTER_DROP`] after a session ended.
fn redial() -> ReconnectBackoff {
    ReconnectBackoff::from_row(&venue_timings::IBKR_CPAPI.exec().reconnect())
        .with_end_wait(REDIAL_AFTER_DROP)
}

/// The cOID→numeric-order_id map plus a synthetic-id counter (see [`resolve_numeric`]). Only the
/// exec-loop thread (which holds `&IbkrTransportCpapi`) touches it — the pump does not — so a plain
/// `Mutex` (no `Arc`) suffices.
struct Numbering {
    map: HashMap<String, i32>,
    /// Our numeric order_id → the GATEWAY's own order id string, captured from the place-accept
    /// response, or read off the order list's `orderId` (a resync, a restored order:
    /// `IbkrTransport::locate_restored`). cpapi cancel/modify address the order by THIS gateway id, NOT our
    /// allocated numeric (live-verified 2026-07-15: cancelling by the numeric → `{"error":"OrderID 101 doesn't
    /// exist"}`); an order with no entry here is refused, nothing sent.
    gw: HashMap<i32, String>,
    /// Next synthetic id for an order client id with no real allocation (a foreign/prior-session order surfaced
    /// by reconnect resync). Starts at -1 and decrements — negative so it never collides with the
    /// exec loop's 101+ allocations.
    synth_next: i32,
}

pub struct IbkrTransportCpapi {
    rest: CpapiRest,
    inbound: Receiver<IbkrInbound>,
    tx: Sender<IbkrInbound>,
    stop: Arc<AtomicBool>,
    /// order client id→numeric-order_id recovery (see [`Numbering`] / [`resolve_numeric`]): own-session orders
    /// are inserted in `place_order`; inbound for an unknown order client id gets a fresh synthetic id so the
    /// shared `EventMapper.fill_state` (keyed by order_id) matches and foreign orders never collide.
    numbering: Mutex<Numbering>,
    /// symbol → resolved conId cache (secdef/search is a network round trip; resolve once per
    /// symbol). ⚠ STILL ASSUMED: that the gateway's conId is stable across a session. The 2026-08-23
    /// live pass confirmed `secdef/search` RETURNS a conId (`265598` for AAPL) and that placing
    /// against it works, but a single session cannot observe a conId CHANGING — only a session that
    /// spans a contract roll or a gateway restart can, so this one is not retired.
    conid_cache: Mutex<HashMap<String, i64>>,
    /// Our numeric id → the reads left, of each order whose cancel the gateway took (its ack is not the order's end) or
    /// may have taken (an outcome it did not state): read back on the order list until it shows how the order stands
    /// ([`IbkrTransportCpapi::read_cancels_back`]).
    cancels_out: HashMap<i32, u32>,
}

impl IbkrTransportCpapi {
    /// `link`: the account's stream link, which the order-update pump writes (`None`: nothing is reported).
    pub fn connect(
        config: &IbkrConfig,
        link: Option<VenueLinkStream>,
    ) -> Result<IbkrTransportCpapi, IbkrError> {
        let rest = CpapiRest::new(&config.cpapi_url, &config.account);
        // Readiness: a tickle must succeed (gateway up + browser-authenticated) or we degrade to
        // paper — the cpapi backend has no in-crate login flow (design doc: lifecycle/login is out
        // of scope for this cluster).
        rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;

        let (tx, rx) = mpsc::channel::<IbkrInbound>();
        // Seed id/account readiness (mirrors IbkrTransportSocket::connect); the cpapi backend has no
        // native nextValidId concept, so seed a floor the IdRegistry will raise as needed.
        let _ = tx.send(IbkrInbound::NextValidId(101));
        let _ = tx.send(IbkrInbound::AccountsReady);

        let stop = Arc::new(AtomicBool::new(false));

        // WS pump + tickle keepalive thread.
        let websocket_url = websocket_url_from(&config.cpapi_url);
        let pump_tx = tx.clone();
        let pump_stop = Arc::clone(&stop);
        let pump_rest_url = config.cpapi_url.clone();
        let pump_account = config.account.clone();
        thread::Builder::new()
            .name("ibkr-cpapi-pump".into())
            .spawn(move || {
                stream_link::report_on_this_thread(link);
                let _span = tracing::info_span!("user_data_pump", venue = "ibkr").entered();
                pump_loop(&websocket_url, &pump_rest_url, &pump_account, &pump_tx, &pump_stop)
            })
            .map_err(|e| IbkrError::Connect(format!("cpapi pump spawn: {e}")))?;

        Ok(IbkrTransportCpapi {
            rest,
            inbound: rx,
            tx,
            stop,
            numbering: Mutex::new(Numbering {
                map: HashMap::new(),
                gw: HashMap::new(),
                synth_next: -1,
            }),
            conid_cache: Mutex::new(HashMap::new()),
            cancels_out: HashMap::new(),
        })
    }

    /// Fill the numeric order_id into an inbound (see [`Numbering`] / [`resolve_numeric`]).
    fn number_inbound(&self, inbound: IbkrInbound) -> IbkrInbound {
        let mut n = self.numbering.lock().unwrap();
        let Numbering { map, synth_next, gw: _ } = &mut *n;
        resolve_numeric(map, synth_next, inbound)
    }

    /// Resolve `contract.con_id` via `/iserver/secdef/search` if absent, caching by symbol. Returns
    /// an owned contract with `con_id` filled when resolution succeeds; on any REST/decode failure
    /// (or an already-populated con_id), returns the contract unchanged so `order_json` still sends
    /// something (a `conid: 0` request the gateway will itself reject — never silently drop the
    /// submit).
    fn resolve_conid(&self, contract: &IbkrContract) -> IbkrContract {
        if contract.con_id.is_some() {
            return contract.clone();
        }
        // ⚠ The search is keyed on symbol + secType ALONE, and `decode_conid` takes the FIRST row
        // it answers with. For a contract whose identity includes an EXPIRY that first row is a
        // different contract — `ES` `FUT` matches every listed quarter — so resolving here would
        // pick a month the operator did not name and place a real order on it. Refuse instead: the
        // contract goes back unchanged, `order_json` sends `conid: 0`, and the gateway rejects it
        // loudly. Narrowing the search is not available without a measured response shape for the
        // expiry-bearing families, and an order path is the wrong place to guess one.
        if contract.conid_search_is_ambiguous() {
            tracing::error!(
                symbol = %contract.symbol,
                sec_type = contract.sec_type.as_ibkr_code(),
                expiry = ?contract.expiry,
                "cpapi: REFUSING to resolve a conId by symbol+secType for an expiry-bearing \
                 contract — the search cannot express the expiry and would pick another month. \
                 Use the socket backend for this contract, which sends the expiry to IBKR."
            );
            return contract.clone();
        }
        if let Some(cached) = self.conid_cache.lock().unwrap().get(&contract.symbol).copied() {
            let mut c = contract.clone();
            c.con_id = Some(cached);
            return c;
        }
        match self.rest.secdef_search(&contract.symbol, contract.sec_type.as_ibkr_code()) {
            Ok(v) => match decode_conid(&v) {
                Some(conid) => {
                    self.conid_cache.lock().unwrap().insert(contract.symbol.clone(), conid);
                    let mut c = contract.clone();
                    c.con_id = Some(conid);
                    c
                }
                None => {
                    tracing::warn!(symbol = %contract.symbol, "cpapi secdef_search returned no conid");
                    contract.clone()
                }
            },
            Err(e) => {
                tracing::warn!(symbol = %contract.symbol, error = %e.msg, "cpapi secdef_search failed");
                contract.clone()
            }
        }
    }
}

impl IbkrTransport for IbkrTransportCpapi {
    /// Every portfolio page, as a reconcile pass reads them. A contract this backend cannot pin reads nothing.
    fn held_position(&mut self, contract: &IbkrContract) -> Result<f64, String> {
        let Some(conid) = self.resolve_conid(contract).con_id else {
            return Err(format!("no contract id is known for {}", contract.symbol));
        };
        let rows = crate::reconcile::cpapi::collect_position_pages(
            |page| self.rest.positions(page).map(|v| v.to_string()).map_err(|e| e.msg),
            conid,
            &contract.symbol,
        )?;
        Ok(rows.iter().map(|row| row.qty).sum())
    }

    fn place_order(&mut self, order_id: i32, spec: &IbkrOrderSpec, contract: &IbkrContract) {
        let held_until = Instant::now() + PLACE_HOLD;
        // Record order client id→numeric so inbound (carrying cOID) recovers the numeric order_id.
        self.numbering.lock().unwrap().map.insert(spec.order_ref.clone(), order_id);
        let resolved = self.resolve_conid(contract);
        let body = match order_json(spec, &resolved) {
            Ok(body) => body,
            Err(why) => {
                self.reject(order_id, &why);
                return;
            }
        };
        // The contract-id search above may have spent the hold: nothing has been sent yet.
        if !next_call_fits(Instant::now(), held_until) {
            self.reject(order_id, &out_of_hold("the order POST"));
            return;
        }
        let mut response = match self.rest.place(&body) {
            Ok(v) => v,
            Err(e) => {
                self.place_failed(order_id, &spec.order_ref, &e);
                return;
            }
        };
        // Bounded reply-confirmation loop (see module doc's MAX_REPLY_HOPS). The live gateway
        // returns a CHAIN of confirm questions (price-constraint → no-market-data → …), each
        // answered reply yielding the next, until a terminal row with `order_id`/`order_status`.
        // Every answer is parsed, the last reply's included.
        let mut replies = 0;
        loop {
            match parse_place_response(&response) {
                PlaceOutcome::Accepted { order_id: gw_id, order_status } => {
                    // Capture the gateway's own order id: cancel/modify must address the order by IT,
                    // not our allocated numeric (the gateway rejects the numeric as "doesn't exist").
                    self.numbering.lock().unwrap().gw.insert(order_id, gw_id);
                    // The row's own word decides (IB-7): a WORKING word (PreSubmitted/Submitted in the capture) is the
                    // accept, emitted directly as that status. Any other (a fill, an end, a word nobody documents, or
                    // none) is no accept this bridge may claim: the order is of unknown state, and the one resync reads it
                    // (its open-order row accepts it, its execution books the fill). The WS 'sor' stream may also deliver
                    // a status; a duplicate accept is a no-op transition in the exec FSM. order_id 0 + order client id →
                    // numbered to our numeric by `next_recv`, exactly like the WS path.
                    match order_status.filter(|w| OrderStatusKind::from_ibkr(w).is_active()) {
                        Some(status) => {
                            let _ = self.tx.send(IbkrInbound::OrderStatus(IbkrOrderStatus {
                                order_id: 0,
                                order_ref: spec.order_ref.clone(),
                                status,
                                filled: 0.0,
                                average_fill_price: 0.0,
                                ..Default::default()
                            }));
                        }
                        None => self.place_unknown(
                            order_id,
                            &spec.order_ref,
                            &format!(
                                "the gateway numbered the order but its row states no working status: {}",
                                response
                            ),
                        ),
                    }
                    return;
                }
                PlaceOutcome::Rejected { reason } => {
                    self.reject(order_id, &reason);
                    return;
                }
                PlaceOutcome::Unrecognized { answer } => {
                    self.place_unknown(
                        order_id,
                        &spec.order_ref,
                        &format!("a 2xx answer this bridge cannot read: {answer}"),
                    );
                    return;
                }
                // The gateway places nothing while a question is unanswered, so this order was not placed.
                PlaceOutcome::NeedsReply { .. } if replies == MAX_REPLY_HOPS => {
                    self.reject(
                        order_id,
                        "cpapi reply-confirmation exceeded MAX_REPLY_HOPS: the last question was left \
                         unanswered, so the order was not placed",
                    );
                    return;
                }
                PlaceOutcome::NeedsReply { reply_id } => {
                    // An unanswered question places nothing, so a reply that could not end in time is not sent.
                    if !next_call_fits(Instant::now(), held_until) {
                        self.reject(order_id, &out_of_hold("the next confirmation reply"));
                        return;
                    }
                    replies += 1;
                    response = match self.rest.reply(&reply_id, true) {
                        Ok(v) => v,
                        Err(e) => {
                            self.place_failed(order_id, &spec.order_ref, &e);
                            return;
                        }
                    };
                }
            }
        }
    }

    fn cancel_order(&mut self, order_id: i32) -> Result<(), VenueApiError> {
        // cpapi addresses the order by the GATEWAY's own id (captured at accept in `place_order`, or
        // read off the order list), never by our numeric: live-verified, cancelling by the numeric
        // fails with "OrderID N doesn't exist"; by the gateway id it succeeds. No such id = NOT sent.
        let Some(key) = self.gateway_key(order_id) else {
            return Err(VenueApiError { code: 0, msg: format!("cpapi cancel {NO_GATEWAY_ID}") });
        };
        // The gateway's `{"msg":"Request was submitted",..}` (the live ack) TOOK the cancel and is not the order's end
        // (IB: it "does not report whether the cancellation can or will ultimately be enacted"), and an outcome it did not
        // state (a timeout, a 5xx, a lost or unreadable reply) may have cancelled it: both are read back by the order list
        // (`read_cancels_back`), never sent again. A refusal it states (a 4xx, or an `error` in a 2xx
        // body: a 2xx with one used to be taken for an ack) and a send that provably never left keep the order live.
        let answer = match self.rest.cancel(&key) {
            // A 2xx's own refusal carries the status as its code, as every cpapi error does (`CPAPI_REPLY`).
            Ok(v) => match stated_error(&v) {
                Some(why) => Err(VenueApiError {
                    code: 200,
                    msg: format!("cpapi cancel refused by the gateway: {why}"),
                }),
                None => Ok(()),
            },
            Err(e) if e.code == E_TIMEOUT_AMBIGUOUS => Err(VenueApiError {
                code: e.code,
                msg: format!("cpapi cancel (key {key}): {}", e.msg),
            }),
            Err(e) if e.code == 0 => {
                Err(VenueApiError { code: 0, msg: format!("cpapi cancel NOT sent: {}", e.msg) })
            }
            // The message is already the gateway's `{"error": …}` sentence (`body_summary`), never its body.
            Err(e) => Err(VenueApiError {
                code: e.code,
                msg: format!("cpapi cancel refused by the gateway ({}): {}", e.code, e.msg),
            }),
        };
        let taken_or_unknown = match &answer {
            Ok(()) => true,
            Err(e) => e.code == E_TIMEOUT_AMBIGUOUS,
        };
        if taken_or_unknown {
            self.cancels_out.insert(order_id, CANCEL_READS);
            self.resync_after(CANCEL_READ_AFTER);
        }
        answer
    }

    /// Amend through the gateway's amend endpoint, addressed by the GATEWAY's order id. Every answer is
    /// explicit: the reply is read with the place-order reply grammar (the endpoint answers in the same
    /// shapes); EVERY answer is `Refused` WITH words (an acceptance as "unconfirmed": [`amend_outcome_of`]). A failed
    /// call is [`AmendOutcome::Failed`], typed: one whose outcome the gateway did not state is read back
    /// ([`IbkrTransportCpapi::lists_terms`]) and is `Acknowledged` only when the order list shows the asked terms. ⚠ The
    /// amend reply's shape is IBKR's documentation, not a capture (the live cpapi smoke places and cancels only), and
    /// a confirmation question is not answered here: it refuses, naming the question, instead of guessing yes.
    fn modify_order(
        &mut self,
        order_id: i32,
        spec: &IbkrOrderSpec,
        contract: &IbkrContract,
    ) -> AmendOutcome {
        use vike_model::OrderRefusalId::{NotSent, VenueCapability};
        let Some(key) = self.gateway_key(order_id) else {
            return AmendOutcome::Refused(format!("cpapi amend {NO_GATEWAY_ID}"), NotSent);
        };
        let resolved = self.resolve_conid(contract);
        let body = match order_json(spec, &resolved) {
            Ok(body) => body,
            Err(why) => return AmendOutcome::Refused(format!("cpapi: {why}"), VenueCapability),
        };
        let response = match self.rest.amend(&key, &body) {
            Ok(v) => v,
            // OUTCOME UNKNOWN (a timeout, a 5xx, a lost or unreadable reply): the amend may have been applied, so the
            // order is read back by our order client id, and only a listing of the asked terms makes it an amend
            // (decision 0303). Never sent again.
            Err(e) if e.code == E_TIMEOUT_AMBIGUOUS => {
                tracing::warn!(order_id, key = %key, error = %e.msg, "cpapi amend outcome UNKNOWN: reading the order back");
                if self.lists_terms(spec, &resolved) {
                    return AmendOutcome::Acknowledged;
                }
                return AmendOutcome::Failed(VenueApiError {
                    code: e.code,
                    msg: format!(
                        "cpapi amend (key {key}): {}; the order list does not show the asked terms, so the \
                         platform keeps the old ones - check it in the client portal",
                        e.msg
                    ),
                });
            }
            Err(e) => {
                tracing::warn!(order_id, key = %key, error = %e.msg, "cpapi amend failed");
                let msg = if e.code == 0 {
                    format!("cpapi amend NOT sent: {}", e.msg)
                } else {
                    format!("cpapi amend refused by the gateway ({}): {}", e.code, e.msg)
                };
                return AmendOutcome::Failed(VenueApiError { code: e.code, msg });
            }
        };
        amend_outcome_of(&response)
    }

    fn request_open_orders(&mut self) {
        let list = self.read_open_orders();
        // Each listed row's status word, by our numeric id: what a cancel in doubt is read back by.
        let words = list.as_ref().map(|v| {
            for inbound in decode_open_orders(v) {
                let _ = self.tx.send(self.number_inbound(inbound));
            }
            // The rows just numbered carry the gateway's own id: a cancel of a resynced order addresses it.
            let mut n = self.numbering.lock().unwrap();
            let Numbering { map, gw, .. } = &mut *n;
            for (order_client_id, id) in gateway_order_ids(v) {
                if let Some(&numeric) = map.get(&order_client_id) {
                    gw.entry(numeric).or_insert(id);
                }
            }
            order_status_words(v)
                .into_iter()
                .filter_map(|(order_client_id, word)| Some((*map.get(&order_client_id)?, word)))
                .collect::<HashMap<i32, String>>()
        });
        self.read_cancels_back(words.as_ref());
        if let Ok(v) = self.rest.executions() {
            for inbound in decode_executions(&v) {
                let _ = self.tx.send(self.number_inbound(inbound));
            }
        }
    }

    /// The restored orders the order list still names, each numbered (a synthetic id, as a resync numbers an order client id
    /// it never placed) with the gateway's own id kept for its cancel and amend. A row with no `orderId` is left
    /// unbound, and so is every order when the list cannot be read: a cancel of one is refused, nothing sent.
    fn locate_restored(&mut self, order_client_ids: &[String]) -> Vec<(i32, String)> {
        let Some(v) = self.read_open_orders() else {
            tracing::warn!(
                orders = order_client_ids.len(),
                "cpapi: the order list could not be read, so the restored orders are not bound here: a \
                 cancel or amend of one is refused and nothing is sent"
            );
            return Vec::new();
        };
        let mut n = self.numbering.lock().unwrap();
        let Numbering { map, gw, synth_next } = &mut *n;
        gateway_order_ids(&v)
            .into_iter()
            .filter(|(order_client_id, _)| order_client_ids.contains(order_client_id))
            .map(|(order_client_id, id)| {
                let numeric = numeric_for(map, synth_next, &order_client_id);
                gw.entry(numeric).or_insert(id);
                (numeric, order_client_id)
            })
            .collect()
    }

    fn next_recv(&mut self, timeout: Duration) -> Option<IbkrInbound> {
        super::recv_inbound(&self.inbound, timeout).map(|inbound| self.number_inbound(inbound))
    }
}

/// Why a cancel or amend of an order whose gateway id is unknown is refused (after "cpapi cancel" / "cpapi amend").
const NO_GATEWAY_ID: &str = "NOT sent: the Client Portal's own id of this order is not known (no accept for it was \
     seen on this connection and the gateway's order list did not name it), and the gateway does not know the order by \
     ours; the order may still rest, check it in the client portal";

impl IbkrTransportCpapi {
    /// The open-orders snapshot, read so that a cold gateway cache is not taken for "no orders": the
    /// first call of a session can answer an empty list (secondary sources), so an EMPTY plain reply
    /// earns ONE `?force=true` call (clears the gateway's cache) and ONE more plain call, at most
    /// three reads, never a loop. `OPEN_ORDERS_GAP` apart, to stay under the ~5 req/s this endpoint
    /// family is documented to allow. A failed read is `None` as before. ⚠ Blocks the exec loop's
    /// thread up to `2 * OPEN_ORDERS_GAP` more, and only when the list really is empty.
    fn read_open_orders(&self) -> Option<serde_json::Value> {
        let has_rows = |v: &serde_json::Value| {
            v.get("orders").and_then(|o| o.as_array()).is_some_and(|o| !o.is_empty())
        };
        let first = self.rest.open_orders().ok()?;
        if has_rows(&first) {
            return Some(first);
        }
        thread::sleep(OPEN_ORDERS_GAP);
        let _ = self.rest.open_orders_forced();
        thread::sleep(OPEN_ORDERS_GAP);
        // A failed last read leaves the (empty) first answer: what decoding it yields is unchanged.
        Some(self.rest.open_orders().unwrap_or(first))
    }

    /// A place/reply REST failure. Only a DEFINITE failure (the request provably never reached the
    /// venue, or the venue refused it) rejects. An `E_TIMEOUT_AMBIGUOUS` one may have left a live order
    /// at IBKR, so it emits NOTHING terminal (the shared ambiguous-submit rule) and asks for ONE
    /// resync `AMBIGUOUS_PLACE_RESYNC_AFTER` later: the exec loop answers `StreamResync` with
    /// `request_open_orders`, whose row for the order client id becomes an `OrderAccepted` (`rebind_open_order`).
    /// Nothing else would look: this venue has no by-order-client-id lookup (the watchdog's `confirm` is a
    /// no-op) and reconcile under `quarantine` folds nothing, so without the resync the core's
    /// stuck-order watchdog rejects a live order after about a minute. A resync that finds no row
    /// (or whose read fails) changes nothing: the watchdog's last resort stays.
    fn place_failed(&self, order_id: i32, order_client_id: &str, e: &VenueApiError) {
        if e.code == E_TIMEOUT_AMBIGUOUS {
            self.place_unknown(order_id, order_client_id, &e.msg);
        } else {
            self.reject(order_id, &e.msg);
        }
    }

    /// A place of UNKNOWN outcome (the order may be live): no terminal reject, and one open-orders resync
    /// after [`AMBIGUOUS_PLACE_RESYNC_AFTER`] so a received order is found, by the order client id the line names.
    fn place_unknown(&self, order_id: i32, order_client_id: &str, why: &str) {
        tracing::warn!(
            order_id,
            order_client_id,
            error = %why,
            "cpapi place outcome unknown (the order may be live): no terminal reject; \
             one open-orders resync follows"
        );
        self.resync_after(AMBIGUOUS_PLACE_RESYNC_AFTER);
    }

    /// One open-orders resync `after` from now: `IbkrInbound::StreamResync`, which the exec loop answers on its own
    /// thread with [`IbkrTransport::request_open_orders`], the read an unknown place and a cancel in doubt wait for.
    fn resync_after(&self, after: Duration) {
        let tx = self.tx.clone();
        let _ = thread::Builder::new().name("cpapi-resync".into()).spawn(move || {
            thread::sleep(after);
            let _ = tx.send(IbkrInbound::StreamResync);
        });
    }

    /// The cancels the gateway took or may have taken ([`IbkrTransport::cancel_order`]), read back by the order list's
    /// row of each (`words`: its status word by our numeric id; `None`: the list could not be read), per [`cancel_read`].
    /// An end goes to the exec loop as that row's status, as a socket's status would; while a cancel is read again,
    /// one more resync is asked for. Nothing here sends a cancel again.
    fn read_cancels_back(&mut self, words: Option<&HashMap<i32, String>>) {
        if self.cancels_out.is_empty() {
            return;
        }
        let tx = &self.tx;
        let mut again = false;
        self.cancels_out.retain(|&order_id, reads_left| {
            let word = words.and_then(|w| w.get(&order_id));
            match cancel_read(words.is_some(), word.map(String::as_str)) {
                CancelRead::Ended => {
                    // Numeric already, so `resolve_numeric` leaves it and the IdRegistry names the order client id.
                    let _ = tx.send(IbkrInbound::OrderStatus(IbkrOrderStatus {
                        order_id,
                        order_ref: String::new(),
                        status: word.cloned().unwrap_or_default(),
                        ..Default::default()
                    }));
                    false
                }
                CancelRead::Again if *reads_left > 1 => {
                    *reads_left -= 1;
                    again = true;
                    true
                }
                CancelRead::Again => {
                    tracing::warn!(
                        order_id,
                        reads = CANCEL_READS,
                        "cpapi: the order list did not show this cancel's end: the order stays live here until \
                         its own word or the reconcile ends it"
                    );
                    false
                }
                CancelRead::Elsewhere => {
                    tracing::info!(
                        order_id,
                        status = word.map_or("not listed", String::as_str),
                        "cpapi: the order list shows no cancel of this order: its own word or the reconcile ends it"
                    );
                    false
                }
            }
        });
        if again {
            self.resync_after(CANCEL_READ_AFTER);
        }
    }

    /// Whether the order list shows `spec`'s order (by our order client id) holding `spec`'s terms: the read back of an
    /// amend of unknown outcome. A row states a LIMIT order's terms only (its `price` and its size, the reconcile's
    /// `crate::reconcile::cpapi::parse_order_reports`), so any other kind, or a list that cannot be read, proves nothing.
    fn lists_terms(&self, spec: &IbkrOrderSpec, contract: &IbkrContract) -> bool {
        let (Some(conid), "LMT", Some(price)) = (contract.con_id, spec.order_type, spec.lmt_price)
        else {
            return false;
        };
        let Some(list) = self.read_open_orders() else {
            return false;
        };
        let reports = crate::reconcile::cpapi::parse_order_reports(
            &list.to_string(),
            self.rest.account(),
            conid,
            &contract.symbol,
        )
        .unwrap_or_default();
        reports.iter().any(|r| {
            r.order_client_id.as_deref() == Some(spec.order_ref.as_str())
                && same(r.qty, spec.total_qty)
                && r.price.is_some_and(|p| same(p, price))
        })
    }

    /// Synthesize a resolvable `OrderRejected` (no order vanishes). Passes the REAL `order_id` — the
    /// exec loop bound order client id→order_id BEFORE `place_order`, so `EventMapper::on_error` resolves it
    /// deterministically via `IdRegistry` (code 201 = order-rejection class). The id-less path
    /// (`order_id: 0`) is NOT used: it only resolves when exactly one order is unacked and silently
    /// drops the reject otherwise — an order vanishing with a second order in flight.
    fn reject(&self, order_id: i32, msg: &str) {
        let _ =
            self.tx.send(IbkrInbound::Error { code: 201, order_id, msg: format!("cpapi: {msg}") });
    }

    /// The cancel/modify address for our numeric `order_id`: the gateway's own order id, captured at accept or
    /// read off the order list. `None` when neither named it: our numeric (or a resync's synthetic `-1`) is never
    /// sent in its place, the gateway does not know the order by it.
    fn gateway_key(&self, order_id: i32) -> Option<String> {
        self.numbering.lock().unwrap().gw.get(&order_id).cloned()
    }
}

impl Drop for IbkrTransportCpapi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The order-update pump, on the shared private-stream loop (`vike_bridge_core::websocket_session::user`'s
/// `run_user`): each open dials the gateway socket and subscribes to the `sor` topic, and the rest is [`run_pump`]'s.
fn pump_loop(
    websocket_url: &str,
    rest_url: &str,
    account: &str,
    tx: &Sender<IbkrInbound>,
    stop: &AtomicBool,
) {
    let keepalive = CpapiRest::new(rest_url, account);
    run_pump(
        || match connect_websocket(websocket_url) {
            Ok(socket) => {
                // The short read timeout doubles as the stop and keepalive poll; not a venue fact.
                configure_websocket_stream(&socket, Duration::from_millis(200));
                let mut stream = WebsocketSessionUserStreamTungstenite(socket);
                // ⚠ MEASURED 2026-08-23 against a live gateway: THIS FRAME ANSWERS NOTHING. It is left in
                // place rather than deleted because it is the documented spelling and costs nothing, but
                // nothing arrives on it — see this module's doc for the measurement, the sibling topics that
                // DO answer on the same socket, and why cpapi fills are consequently invisible. Do not read
                // a green smoke as evidence this works: the smoke's accept and cancel are both synthesized
                // from the REST acks and never touch this stream.
                let _ = stream.send_text("sor+{}");
                WebsocketSessionUserOpenOutcome::Ready(stream)
            }
            Err(e) => WebsocketSessionUserOpenOutcome::Transport(format!("cpapi ws connect: {e}")),
        },
        |stream: &mut WebsocketSessionUserStreamTungstenite| {
            let _ = stream.send_text("tic");
        },
        move || {
            let _ = keepalive.tickle();
        },
        tx,
        stop,
    );
}

/// [`pump_loop`] over any stream, its socket steps handed in (`open`, and `tic` on the open session) so a test runs
/// it over a scripted stream on a virtual clock. Each frame's inbounds go to the exec loop; every [`TIC_EVERY`] the
/// `tic` goes out and `tickle` renews the REST session ([`TICKLE_EVERY`]); [`IDLE_LIMIT`] of silence ends a session;
/// the waits are [`redial`]'s; and every RE-open sends `StreamResync`, which the exec loop answers with a REST read of
/// the open orders and the trades, the only path a cpapi fill has (module doc). A re-open, not the drop: the read then
/// covers the whole gap, over REST, a connection the socket's drop did not touch. This is the variant the socket
/// backend must NOT use — see `IbkrInbound::StreamDead`.
fn run_pump<S: WebsocketSessionUserStream + 'static>(
    open: impl FnMut() -> WebsocketSessionUserOpenOutcome<S>,
    mut tic: impl FnMut(&mut S) + Send + 'static,
    mut tickle: impl FnMut() + Send + 'static,
    tx: &Sender<IbkrInbound>,
    stop: &AtomicBool,
) {
    // The exec loop's channel carries `IbkrInbound`, not the pump's events, so the decoder hands each frame's to it and
    // the pump none. Nothing reads them once the transport is dropped, and its drop raises `stop`, which ends the pump.
    let decode = |frame: &serde_json::Value| {
        for inbound in decode_websocket_frame(frame) {
            let _ = tx.send(inbound);
        }
        Vec::new()
    };
    let keepalive: Box<dyn FnMut(&mut S) + Send> = Box::new(move |stream: &mut S| {
        tic(stream);
        tickle();
    });
    // No open answers `Auth` (the gateway's login is the browser's), so the pump never ends in one.
    let _ = run_user(
        open,
        decode,
        |_| true,
        stop,
        USER_DATA_POLL,
        redial(),
        Some((TIC_EVERY, keepalive)),
        || {
            let _ = tx.send(IbkrInbound::StreamResync);
        },
        Some(IDLE_LIMIT),
        Some(RECONNECT_LOG),
    );
}

/// `https://127.0.0.1:5000` → `wss://127.0.0.1:5000/v1/api/ws`, by the one http-to-ws rule
/// (`vike_utility::url::websocket_url_from_http`). LIVE-VERIFIED 2026-07-15 against a paper CP Gateway.
pub(crate) fn websocket_url_from(cpapi_url: &str) -> String {
    vike_utility::url::websocket_url_from_http(cpapi_url, "/v1/api/ws")
}

/// Connect the cpapi WS. The Client Portal Gateway is LOOPBACK-only and serves a self-signed (often
/// expired) cert, so — mirroring the REST `blocking_agent_loopback_insecure` path — a loopback URL
/// gets a rustls connector whose verifier accepts any cert. `is_loopback_url` gates it: a
/// non-loopback ws URL always takes the shared bounded dial, with tungstenite's default
/// full-verification path. (Live-verified 2026-07-15: plain `tungstenite::connect` rejected the
/// gateway's expired cert, so no order-update frames ever arrived.)
///
/// The loopback arm takes the shared arm's bound by hand (the shared dial has no knob for this connector):
/// each address's TCP connect and every read of the TLS + WebSocket handshake get `CONNECT_10S`, as in
/// `crates/vike-transport/src/websocket_proxy.rs`'s module doc, so a gateway that accepts and then says
/// nothing fails the open and the pump reads its `stop` before the next one. The window is left on the
/// socket: both callers replace it with their poll (`configure_websocket_stream`) at once.
pub(crate) fn connect_websocket(websocket_url: &str) -> Result<WsSocket, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    use tungstenite::HandshakeError;
    use vike_transport::websocket::is_timeout;
    use vike_utility::url::{is_loopback_url, url_authority};
    if !is_loopback_url(websocket_url) {
        return vike_transport::websocket_proxy::connect_websocket(
            websocket_url,
            None,
            Some(CONNECT_10S),
        );
    }
    // authority = "127.0.0.1:5000" — strip scheme + path for the TcpStream connect.
    let authority = url_authority(websocket_url);
    let mut last = format!("no address for {authority}");
    let mut stream = None;
    for addr in authority.to_socket_addrs().map_err(|e| e.to_string())? {
        match TcpStream::connect_timeout(&addr, CONNECT_10S) {
            Ok(tcp) => {
                stream = Some(tcp);
                break;
            }
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    let stream = stream.ok_or(last)?;
    let _ = stream.set_read_timeout(Some(CONNECT_10S));
    let no_answer = || format!("ws handshake: no answer within {CONNECT_10S:?}");
    // The loopback-only unverified config (`vike_transport::tls::loopback_insecure_client_config`), behind the gate above.
    let connector = tungstenite::Connector::Rustls(Arc::new(
        vike_transport::tls::loopback_insecure_client_config(),
    ));
    let handshake =
        tungstenite::client_tls_with_config(websocket_url, stream, None, Some(connector));
    let (socket, _response) = handshake.map_err(|e| match e {
        // A timed-out read is `WouldBlock` (Interrupted) on Linux and `TimedOut` on Windows.
        HandshakeError::Interrupted(_) => no_answer(),
        HandshakeError::Failure(err) if is_timeout(&err) => no_answer(),
        HandshakeError::Failure(err) => err.to_string(),
    })?;
    Ok(socket)
}

/// Substitute the numeric order_id into a decoder-produced inbound (which carries `order_id: 0` +
/// `order_ref: <cOID>`). Own-session order client ids resolve from `map` (populated at `place_order`); an order client id
/// with no entry (a foreign/prior-session order surfaced by reconnect resync) is assigned a fresh
/// unique negative synthetic id and remembered, so distinct foreign orders never collide on id 0 in
/// the shared `IdRegistry` (and the same order client id always resolves to the same id).
fn resolve_numeric(
    map: &mut HashMap<String, i32>,
    synth_next: &mut i32,
    mut inbound: IbkrInbound,
) -> IbkrInbound {
    fn fill(
        map: &mut HashMap<String, i32>,
        synth_next: &mut i32,
        order_id: &mut i32,
        order_ref: &str,
    ) {
        if *order_id != 0 || order_ref.is_empty() {
            return; // already numbered, or no order client id to key on
        }
        *order_id = numeric_for(map, synth_next, order_ref);
    }
    match &mut inbound {
        IbkrInbound::OrderStatus(st) => {
            let r = st.order_ref.clone();
            fill(map, synth_next, &mut st.order_id, &r);
        }
        IbkrInbound::ExecutionDetails(e) => {
            let r = e.order_ref.clone();
            fill(map, synth_next, &mut e.order_id, &r);
        }
        IbkrInbound::OpenOrder { order_id, order_ref } => {
            let r = order_ref.clone();
            fill(map, synth_next, order_id, &r);
        }
        _ => {}
    }
    inbound
}

/// The refusal a gateway states in a reply body: a non-empty `error` string (the live answer to a cancel by our
/// numeric, 2026-07-15, was `{"error":"OrderID 101 doesn't exist"}`).
fn stated_error(v: &serde_json::Value) -> Option<String> {
    v.get("error").and_then(serde_json::Value::as_str).filter(|w| !w.is_empty()).map(str::to_string)
}

/// The numeric id of `order_client_id`: its allocation, else a fresh negative synthetic one, remembered (see [`resolve_numeric`]).
fn numeric_for(map: &mut HashMap<String, i32>, synth_next: &mut i32, order_client_id: &str) -> i32 {
    *map.entry(order_client_id.to_string()).or_insert_with(|| {
        let s = *synth_next;
        *synth_next -= 1;
        s
    })
}

/// What the gateway's answer to an amend means. ⚠ The reply of the amend endpoint carries the order's id (the
/// place-order grammar, [`parse_place_response`]) and NOT the terms it now holds, and no `openOrder`-style row
/// follows on this backend (the order feed decodes ids and statuses, not prices), so nothing here can PROVE the
/// amend took the requested quantity and price: an accepted reply is reported as what it is - an UNCONFIRMED
/// amend, non-terminal (`Refused`), saying the order may nevertheless have been amended - never as
/// `OrderModified`, which would make the engine believe terms the venue was never seen to hold.
fn amend_outcome_of(response: &serde_json::Value) -> AmendOutcome {
    use vike_model::OrderRefusalId::{OutcomeUnknown, VenueRefused};
    match parse_place_response(response) {
        PlaceOutcome::Accepted { .. } => AmendOutcome::Refused(
            "cpapi amend unconfirmed: the gateway accepted the request but its reply does not carry the \
             order's new terms, so the platform keeps the old ones; the order may nevertheless have been \
             amended - check it in the client portal"
                .to_string(),
            OutcomeUnknown,
        ),
        PlaceOutcome::Rejected { reason } => {
            AmendOutcome::Refused(format!("cpapi amend refused: {reason}"), VenueRefused)
        }
        // The gateway applies nothing while its question is unanswered, and asks it again on a resend.
        PlaceOutcome::NeedsReply { .. } => AmendOutcome::Refused(
            format!(
                "cpapi answered the amend with a confirmation question this bridge does not answer for \
                 amends: {response}"
            ),
            VenueRefused,
        ),
        PlaceOutcome::Unrecognized { answer } => AmendOutcome::Refused(
            format!(
                "cpapi amend unconfirmed: the gateway answered in a shape this bridge cannot read ({answer}), so \
                 the platform keeps the old terms; the order may nevertheless have been amended - check it in the \
                 client portal"
            ),
            OutcomeUnknown,
        ),
    }
}

/// Build the CP Gateway order JSON from the pure spec + contract. `conId` is required by the
/// gateway; `place_order`/`modify_order` resolve it first via `resolve_conid` when absent.
/// Field names LIVE-VERIFIED 2026-08-23: a raw `POST` carrying exactly this body
/// (`conid`/`orderType`/`side`/`tif`/`quantity`/`price`/`cOID`) was accepted by the live gateway and
/// came back as `Buy 1 AAPL Limit 1.00, GTC` with `cOID` echoed as `local_order_id`. See the
/// module doc table in `transport/cpapi/endpoints.rs`.
///
/// The stop prices follow IBKR's own Web API schema (`https://api.ibkr.com/gw/api/v3/api-docs`, "IB REST API" 2.41.0,
/// read 2026-11-08). Its order ticket under `/v1/api/iserver/account/{modelCode}/orders` lists `orderType`
/// `["MKT","LMT","STP","STOP_LIMIT","MIDPRICE","TRAIL","TRAILLMT"]`, `price` "Limit price. Required for `LMT` and
/// `STOP_LIMIT` order types." and `auxPrice` "Stop or trailing price. Required for `STP`, `STOP_LIMIT`, and trailing
/// order types."; the ticket of `/v1/api/iserver/account/{accountId}/orders` (the one placed here) says `auxPrice` is
/// "Additional price value used in certain order types, such as stop orders.". So:
///
/// - a stop's trigger is `auxPrice` (`IbkrOrderSpec::aux_price`, as the socket backend's `build_order` sends it), present
///   ONLY for an order that has one, so the limit/market bodies stay exactly the live-verified ones;
/// - a stop-limit is the TWS code `STP LMT`, which this API spells `STOP_LIMIT`: `price` carries the limit and
///   `auxPrice` the trigger.
///
/// ⚠ Schema, not a capture: no stop or stop-limit was ever placed through a CP Gateway (the live cpapi smoke places a
/// limit order); `tests/ibkr_smokes/ibkr_stop_limit_smoke.rs` speaks the socket. An older Client Portal schema said
/// "For STP this is the stop price" of `price`; the current one above says `auxPrice`, which this body keeps.
///
/// **The wave-2 kinds, read in the same schema 2026-11-08.** The placed ticket's `orderType` is "IB order type
/// identifier." with no list; the one list of ticket order types is the seven above. `MIT`, `LIT`, `MTL`, `MOC` and
/// `LOC` are in neither: `mit`, `lit`, `marketonclose` and `limitonclose` appear only as `/iserver/contract/rules`'
/// `orderTypes` words (what a contract permits), `MTL` nowhere, and no ticket field is a display size
/// (`displaySize` is a contract rule's "Standard display increment rule"). So those kinds and an iceberg are REFUSED
/// here, in words, never sent under a guessed spelling (`Err`, which the callers turn into the order's rejection or
/// the amend's refusal). The ticket's `tif` list is `DAY`, `IOC`, `GTC`, `OPG`, `PAX`: a market- or limit-on-open
/// (`MKT`/`LMT` with `OPG`) is spelled there and is sent.
///
/// **The trailing kinds, read in the same schema 2026-11-08.** The ticket does name `TRAIL` and `TRAILLMT`, with
/// `trailingAmt` "Trailing amount for `TRAIL` / `TRAILLMT` order types." and `trailingType` (`amt`, `%`), but
/// `auxPrice` is "Stop or trailing price. Required for `STP`, `STOP_LIMIT`, and trailing order types.": a
/// trailing order there must state its starting stop, which a vike trailing order does not (its stop starts
/// from the market), and no ticket field is a limit OFFSET (`price` is "Required for `LMT` and `STOP_LIMIT`").
/// An activation has no field either (the socket's price condition). So both kinds are REFUSED here, in words.
///
/// **The pegged order, read in the same schema 2026-11-08.** The ticket's list names no `PEG BEST`, `PEG MID`,
/// `PEG MKT` or `REL` (its `MIDPRICE` is IB's midprice order, which does not re-peg), so a pegged order is REFUSED
/// here by its IB code, with the wave-2 kinds.
///
/// **The attributes wave (decision 0267).** All-or-none is the body's `"allOrNone": true` (**d\***: the Web API's
/// "submit new order" reference, read only as a search snippet of a page that answers 403; built, pinned by a test,
/// never captured). Every other item has no ticket field and is REFUSED by name: a hidden order, sweep-to-fill, a
/// discretionary amount, a percent trail, an adjustable stop, a good-till date (the ticket's `tif` list has no `GTD`)
/// and a release IB would hold (`goodAfterTime`: on this backend the core holds a time release, as it always did, since
/// `crate::config::IbkrConfig::native_time_release` is `false` here, so one reaching here was misrouted); `TRAIL MIT`
/// and `TRAIL LIT` are not in the order-type list, and an opening
/// market-to-limit (`MTL`, TIF `AUC`) is refused by its `MTL`.
fn order_json(spec: &IbkrOrderSpec, contract: &IbkrContract) -> Result<serde_json::Value, String> {
    let order_type = match spec.order_type {
        "STP LMT" => "STOP_LIMIT",
        code @ ("MIT" | "LIT" | "MTL" | "MOC" | "LOC" | "PEG BEST" | "PEG MID" | "PEG MKT"
        | "REL" | "TRAIL MIT" | "TRAIL LIT") => {
            return Err(format!(
                "IBKR's Web API order ticket names no `{code}` order type (its list is MKT, LMT, STP, \
                 STOP_LIMIT, MIDPRICE, TRAIL, TRAILLMT); this order goes through the socket backend only. \
                 Nothing was sent"
            ));
        }
        code @ ("TRAIL" | "TRAIL LIMIT") => {
            return Err(format!(
                "IBKR's Web API order ticket takes a trailing order (`{code}`) only with its starting stop \
                 price in `auxPrice` (required there), which this order does not state, and has no limit \
                 offset field; this order goes through the socket backend only. Nothing was sent"
            ));
        }
        code => code,
    };
    if spec.display_size.is_some() {
        return Err("IBKR's Web API order ticket has no display size field, so an iceberg would rest \
                    with its whole size shown; it goes through the socket backend only. Nothing was sent"
            .to_string());
    }
    let unsent = [
        (spec.hidden, "a hidden order (`hidden`)"),
        (spec.sweep_to_fill, "a sweep-to-fill order (`sweepToFill`)"),
        (spec.discretionary_amt.is_some(), "a discretionary amount (`discretionaryAmt`)"),
        (spec.trailing_percent.is_some(), "a percent trail (`trailingPercent`)"),
        (spec.adjust.is_some(), "an adjustable stop (`triggerPrice`, `adjustedOrderType`)"),
        (
            spec.good_till_date.is_some(),
            "a good-till date (`goodTillDate`; no `GTD` in its `tif` list)",
        ),
        (
            spec.good_after_time.is_some(),
            "a release IB holds (`goodAfterTime`; on this backend the core holds a time release)",
        ),
    ];
    if let Some((_, what)) = unsent.iter().find(|(asked, _)| *asked) {
        return Err(format!(
            "IBKR's Web API order ticket has no field for {what}, so the order would go without it; it goes \
             through the socket backend only. Nothing was sent"
        ));
    }
    let mut body = json!({
        "conid": contract.con_id.unwrap_or(0),
        "orderType": order_type,
        "side": spec.action,
        "tif": spec.tif,
        "quantity": spec.total_qty,
        "price": spec.lmt_price,
        "cOID": spec.order_ref,
    });
    if let Some(trigger) = spec.aux_price {
        body["auxPrice"] = json!(trigger);
    }
    if spec.all_or_none {
        body["allOrNone"] = json!(true);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{
        AmendOutcome, CALL_BOUND, IDLE_LIMIT, IbkrInbound, PLACE_HOLD, TIC_EVERY, TICKLE_EVERY,
        amend_outcome_of, next_call_fits, order_json, out_of_hold, redial, resolve_numeric,
        venue_timings,
    };
    use crate::execution::event_mapper::{IbkrExecutionDetails, IbkrOrderStatus};
    use std::collections::HashMap;
    use std::time::Duration;

    /// The cpapi websocket is the REST row's host with `/v1/api/ws`, by the one http-to-ws rule
    /// (`vike_utility::url::websocket_url_from_http`, drift U-D3): a base with no scheme dials `wss://`.
    #[test]
    fn the_websocket_is_the_rest_rows_host_by_the_one_http_to_websocket_rule() {
        let ws = super::websocket_url_from;
        assert_eq!(ws("https://127.0.0.1:5000"), "wss://127.0.0.1:5000/v1/api/ws");
        assert_eq!(ws("http://127.0.0.1:5000/"), "ws://127.0.0.1:5000/v1/api/ws");
        assert_eq!(ws("127.0.0.1:5000"), "wss://127.0.0.1:5000/v1/api/ws");
    }

    /// `pump_loop`'s cadences and waits, pinned verbatim: the exec row's 30 s REST tickle and 30 s `tic` (one hook), its
    /// 60 s idle limit, a FIXED 2 s after every failed websocket dial, and the local 1 s after a drop mid-stream, the
    /// sequence the pump's own loop ran before it moved onto `run_user`.
    #[test]
    fn the_execution_timings_are_the_rows_tickle_tic_idle_and_redial() {
        assert_eq!(TICKLE_EVERY, Duration::from_secs(30));
        assert_eq!(TIC_EVERY, Duration::from_secs(30));
        assert_eq!(IDLE_LIMIT, Duration::from_secs(60));
        assert!(venue_timings::IBKR_CPAPI.exec().reconnect().is_fixed());
        let mut b = redial();
        let s = Duration::from_secs;
        assert_eq!([b.failed(), b.failed()], [s(2), s(2)], "a failed dial");
        b.opened();
        assert_eq!(b.ended(s(3600)), s(1), "a drop mid-stream, however long the session lived");
        assert_eq!([b.failed(), b.failed()], [s(2), s(2)], "then a failed dial again");
    }

    /// **A place's calls end inside its hold** (the row's `submit`): a call is made only while one more at the agent's
    /// 30 s bound still ends by the hold, 35 s from the top of `place_order`, so the order POST goes only within the
    /// first 5 s (a slow contract-id search spends it) and every reply likewise; the place then answers by the queue's
    /// 20 s plus 35 s, before the core's 60 s last resort. The refusal says nothing was placed.
    #[test]
    fn a_place_makes_a_call_only_while_it_can_end_inside_the_hold() {
        let s = Duration::from_secs;
        assert_eq!((CALL_BOUND, PLACE_HOLD), (s(30), s(35)));
        let top = std::time::Instant::now();
        let held_until = top + PLACE_HOLD;
        assert!(next_call_fits(top, held_until), "the first call at the top");
        assert!(next_call_fits(top + s(5), held_until), "a call that ends exactly at the hold");
        assert!(
            !next_call_fits(top + s(5) + Duration::from_millis(1), held_until),
            "one that would not"
        );
        let words = out_of_hold("the next confirmation reply");
        assert!(words.contains("NOT placed") && words.contains("35 s"), "{words}");
    }

    /// Off loopback the websocket dial is bounded through the handshake, like
    /// `crates/bridges/deribit/tests/offline/dial_bound.rs`'s: a peer that accepts and says nothing would
    /// hold the order-update pump for ever. `0.0.0.0` reaches the listener on Linux and is not a loopback
    /// address to `is_loopback_url`, so this dials the off-loopback arm with no network.
    #[test]
    fn off_loopback_a_silent_peer_cannot_hold_the_websocket_dial() {
        assert!(!vike_utility::url::is_loopback_url("ws://0.0.0.0:5000"));
        vike_bridge_core::silent_peer::dial_silent_peer("0.0.0.0:0", |url| {
            super::connect_websocket(&url).map(drop)
        });
    }

    /// On loopback, the arm production always takes (the Client Portal Gateway is `https://127.0.0.1:5000`), a gateway
    /// that accepts and never answers the TLS ClientHello fails the dial inside the off-loopback arm's `CONNECT_10S`
    /// instead of holding the order-update pump for ever, deaf to its `stop`. Runs about 10 s: the bound is the
    /// production one, `connect_websocket` takes none.
    #[test]
    fn on_loopback_a_gateway_that_never_answers_tls_cannot_hold_the_websocket_dial() {
        let dial = vike_bridge_core::silent_peer::dial_silent_peer("127.0.0.1:0", |url| {
            let url = url.replacen("ws://", "wss://", 1);
            assert!(vike_utility::url::is_loopback_url(&url));
            super::connect_websocket(&url).map(drop)
        });
        assert!(dial.failure.contains("10s"), "the dial spent CONNECT_10S, got: {}", dial.failure);
    }

    fn status(order_ref: &str) -> IbkrInbound {
        IbkrInbound::OrderStatus(IbkrOrderStatus {
            order_id: 0,
            order_ref: order_ref.into(),
            status: "Submitted".into(),
            filled: 0.0,
            average_fill_price: 0.0,
            ..Default::default()
        })
    }

    fn order_id_of(inbound: &IbkrInbound) -> i32 {
        match inbound {
            IbkrInbound::OrderStatus(s) => s.order_id,
            IbkrInbound::ExecutionDetails(e) => e.order_id,
            IbkrInbound::OpenOrder { order_id, .. } => *order_id,
            _ => panic!("no order_id"),
        }
    }

    #[test]
    fn own_session_order_client_id_resolves_to_allocated_id() {
        let mut map = HashMap::from([("order-client-id-A".to_string(), 101)]);
        let mut synth = -1;
        let out = resolve_numeric(&mut map, &mut synth, status("order-client-id-A"));
        assert_eq!(order_id_of(&out), 101);
        assert_eq!(synth, -1, "no synthetic id allocated for a known order client id");
    }

    #[test]
    fn distinct_foreign_order_client_ids_get_distinct_stable_synthetic_ids() {
        let mut map = HashMap::new();
        let mut synth = -1;
        let a = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-1")));
        let b = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-2")));
        assert_ne!(
            a, b,
            "distinct foreign order client ids must not collide (the reviewer's finding #2)"
        );
        assert!(a < 0 && b < 0, "synthetic ids are negative (never collide with 101+ allocations)");
        // The same order client id resolves to the SAME id on a later inbound.
        let a2 = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-1")));
        assert_eq!(a, a2, "an order client id's synthetic id is stable across inbound");
    }

    #[test]
    fn execution_details_numeric_is_filled_too() {
        let mut map = HashMap::from([("order-client-id-X".to_string(), 202)]);
        let mut synth = -1;
        let e = IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
            order_id: 0,
            order_ref: "order-client-id-X".into(),
            exec_id: "e1".into(),
            symbol: "S".into(),
            side_buy: true,
            shares: 1.0,
            price: 1.0,
            ts: 0,
            ..Default::default()
        });
        assert_eq!(order_id_of(&resolve_numeric(&mut map, &mut synth, e)), 202);
    }

    fn spec(
        order_type: &'static str,
        lmt: Option<f64>,
        aux: Option<f64>,
    ) -> crate::execution::order::IbkrOrderSpec {
        crate::execution::order::IbkrOrderSpec {
            action: "BUY",
            total_qty: 5.0,
            order_type,
            lmt_price: lmt,
            aux_price: aux,
            tif: "GTC",
            order_ref: "c1".into(),
            display_size: None,
            lmt_price_offset: None,
            activation: None,
            ..Default::default()
        }
    }

    /// A stop is sent WITH its trigger (`auxPrice`); a limit's body stays the live-verified one, without it.
    #[test]
    fn a_stop_body_carries_its_trigger_and_a_limit_body_does_not() {
        let c = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
        let stop = order_json(&spec("STP", None, Some(180.0)), &c).unwrap();
        assert_eq!(stop["auxPrice"], 180.0);
        assert_eq!(stop["orderType"], "STP");
        let limit = order_json(&spec("LMT", Some(190.0), None), &c).unwrap();
        assert!(limit.get("auxPrice").is_none(), "{limit}");
        assert_eq!(limit["price"], 190.0);
    }

    /// A stop-limit goes out as this API's `STOP_LIMIT`, its limit in `price` and its trigger in `auxPrice`; the other
    /// kinds keep the TWS code they always sent.
    #[test]
    fn a_stop_limit_body_is_stop_limit_with_its_limit_and_its_trigger() {
        let c = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
        let body = order_json(&spec("STP LMT", Some(201.5), Some(200.0)), &c).unwrap();
        assert_eq!(body["orderType"], "STOP_LIMIT", "{body}");
        assert_eq!(body["price"], 201.5, "the limit");
        assert_eq!(body["auxPrice"], 200.0, "the trigger");
        for code in ["MKT", "LMT", "STP"] {
            assert_eq!(order_json(&spec(code, None, None), &c).unwrap()["orderType"], code);
        }
    }

    /// **The wave-2 kinds on the Web API**: a market- or limit-on-open is the ticket's own `tif` `OPG` and is sent; the
    /// kinds the ticket's schema names no order type for (`MIT`, `LIT`, `MTL`, `MOC`, `LOC`, the four pegs) and an iceberg (no display
    /// field) are refused in words, never sent under a guessed spelling.
    #[test]
    fn an_opening_auction_is_sent_and_the_kinds_the_ticket_does_not_name_are_refused() {
        let c = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
        let loo = order_json(
            &crate::execution::order::IbkrOrderSpec {
                tif: "OPG",
                ..spec("LMT", Some(150.0), None)
            },
            &c,
        )
        .unwrap();
        assert_eq!((loo["orderType"].as_str(), loo["tif"].as_str()), (Some("LMT"), Some("OPG")));
        for code in ["MIT", "LIT", "MTL", "MOC", "LOC", "PEG BEST", "PEG MID", "PEG MKT", "REL"] {
            let why = order_json(&spec(code, Some(150.0), Some(149.0)), &c).expect_err(code);
            assert!(
                why.contains(&format!("`{code}`")) && why.contains("Nothing was sent"),
                "{why}"
            );
        }
        let iceberg = crate::execution::order::IbkrOrderSpec {
            display_size: Some(1),
            ..spec("LMT", Some(150.0), None)
        };
        assert!(order_json(&iceberg, &c).expect_err("iceberg").contains("display size"));
    }

    /// **The trailing kinds on the Web API are refused by name** (`order_json`'s doc): the ticket names `TRAIL` and
    /// `TRAILLMT` but wants the stop price in `auxPrice`, which a vike trailing order does not state, and has no limit
    /// offset field; an activation (a socket price condition) has no field either.
    #[test]
    fn the_trailing_kinds_are_refused_on_the_web_api() {
        let c = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
        for code in ["TRAIL", "TRAIL LIMIT"] {
            let why = order_json(&spec(code, None, Some(2.5)), &c).expect_err(code);
            assert!(
                why.contains(&format!("`{code}`"))
                    && why.contains("auxPrice")
                    && why.contains("Nothing was sent"),
                "{why}"
            );
        }
    }

    /// **The attributes wave on the Web API** (`order_json`'s doc): all-or-none is the body's `allOrNone`, and a plain
    /// limit's body is unchanged without it; `TRAIL MIT` / `TRAIL LIT` and every other new item are refused by name.
    #[test]
    fn all_or_none_is_sent_and_every_other_attribute_is_refused_on_the_web_api() {
        use crate::execution::order::{IbkrAdjust, IbkrOrderSpec};
        let c = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
        let limit = spec("LMT", Some(150.0), None);
        let aon = order_json(&IbkrOrderSpec { all_or_none: true, ..limit.clone() }, &c).unwrap();
        assert_eq!(
            (aon["orderType"].as_str(), aon["allOrNone"].as_bool()),
            (Some("LMT"), Some(true))
        );
        assert!(order_json(&limit, &c).unwrap().get("allOrNone").is_none());

        for code in ["TRAIL MIT", "TRAIL LIT"] {
            let why = order_json(&spec(code, None, Some(2.5)), &c).expect_err(code);
            assert!(
                why.contains(&format!("`{code}`")) && why.contains("Nothing was sent"),
                "{why}"
            );
        }
        let adjust = IbkrAdjust {
            trigger_price: 200.0,
            order_type: "STP",
            stop_price: 195.0,
            stop_limit_price: None,
            trailing_amount: None,
            trailing_unit: 0,
        };
        for (one, word) in [
            (IbkrOrderSpec { hidden: true, ..limit.clone() }, "`hidden`"),
            (IbkrOrderSpec { sweep_to_fill: true, ..limit.clone() }, "`sweepToFill`"),
            (IbkrOrderSpec { discretionary_amt: Some(0.5), ..limit.clone() }, "`discretionaryAmt`"),
            (IbkrOrderSpec { trailing_percent: Some(1.0), ..limit.clone() }, "`trailingPercent`"),
            (
                IbkrOrderSpec { adjust: Some(adjust), ..spec("STP", None, Some(180.0)) },
                "`triggerPrice`",
            ),
            (
                IbkrOrderSpec {
                    tif: "GTD",
                    good_till_date: Some("20261109-14:30:00".into()),
                    ..limit.clone()
                },
                "`goodTillDate`",
            ),
            (
                IbkrOrderSpec {
                    good_after_time: Some("20261109-14:30:00".into()),
                    ..limit.clone()
                },
                "`goodAfterTime`",
            ),
        ] {
            let why = order_json(&one, &c).expect_err(word);
            assert!(why.contains(word) && why.contains("Nothing was sent"), "{why}");
        }
    }

    /// **What one read of the order list says of a cancel in doubt**: only the cancel's end ends the order; a row still
    /// working or pending the cancel, or a list that could not be read, is read again; a fill, another end, a word nobody
    /// documents or no row settles nothing here.
    #[test]
    fn a_cancel_is_read_back_ended_only_by_its_end() {
        use super::{CancelRead, cancel_read};
        for word in ["Cancelled", "ApiCancelled", "Canceled"] {
            assert_eq!(cancel_read(true, Some(word)), CancelRead::Ended, "{word}");
        }
        for word in [
            "PendingCancel",
            "PreCancelled",
            "Submitted",
            "PreSubmitted",
            "PendingSubmit",
            "ApiPending",
        ] {
            assert_eq!(cancel_read(true, Some(word)), CancelRead::Again, "{word}");
        }
        assert_eq!(cancel_read(false, None), CancelRead::Again, "the list could not be read");
        for word in [Some("Filled"), Some("Inactive"), Some("SomethingNew"), None] {
            assert_eq!(cancel_read(true, word), CancelRead::Elsewhere, "{word:?}");
        }
    }

    /// The gateway's acceptance of an amend does not carry the new terms: it is never `OrderModified`.
    #[test]
    fn an_accepted_amend_reply_is_unconfirmed_never_acknowledged() {
        let ok = serde_json::json!([{ "order_id": "123", "order_status": "Submitted" }]);
        match amend_outcome_of(&ok) {
            AmendOutcome::Refused(why, refusal) => {
                assert!(why.contains("amend unconfirmed"), "{why}");
                assert_eq!(refusal, vike_model::OrderRefusalId::OutcomeUnknown);
            }
            other => panic!("expected Refused(unconfirmed), got {other:?}"),
        }
        let bad = serde_json::json!({ "error": "no such order" });
        match amend_outcome_of(&bad) {
            AmendOutcome::Refused(why, refusal) => {
                assert!(why.contains("no such order"), "{why}");
                assert_eq!(refusal, vike_model::OrderRefusalId::VenueRefused);
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        let q = serde_json::json!([{ "id": "r1", "message": ["sure?"] }]);
        assert!(matches!(amend_outcome_of(&q), AmendOutcome::Refused(..)));
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod pump_tests;
