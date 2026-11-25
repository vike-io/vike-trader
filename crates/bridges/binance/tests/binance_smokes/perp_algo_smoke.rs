//! LIVE DEMO drill for Binance USDⓈ-M CONDITIONAL orders on the Algo Service (network + demo creds —
//! run by hand, never in a loop; NOT in `.github/workflows/live-smokes.yml`):
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture perp_algo_smoke::
//!
//! `crates/bridges/binance/src/execution/rest/algo.rs` moved the bridge's stop from `POST /fapi/v1/order`
//! (`-4120` since 2025-12-09) to `POST /fapi/v1/algoOrder`. Four facts about the DEMO venue are not
//! documented and this drill MEASURES them (it prints a `FINDING` line for each):
//!
//! * **row 14** — is the resting stop listed by `GET /fapi/v1/openOrders`, by `GET /fapi/v1/openAlgoOrders`,
//!   or both? (The report path, `ReconcileClient::fetch_order_status_reports`, reads both either way.)
//! * **row 18** — when a stop triggers, is the CHILD order's `ORDER_TRADE_UPDATE.o.c` the stop's
//!   `clientAlgoId`, and is `o.i` the `ALGO_UPDATE.ai`? (Measured 2026-10-19: yes to both, and the
//!   child fill arrives BEFORE `TRIGGERED`; `crates/bridges/binance/src/execution/rest/algo.rs`'s module doc has the
//!   answers, so no fill needs re-keying. Re-run this when the venue changes anything.)
//! * **row 20 / 26** — does the demo host behave like the live one: the OLD call answers `-4120`, the
//!   algo calls work, and what does the algo endpoint answer for an order it does not know?
//!
//! ## What it does (ETHUSDT, a minimal position, far-from-market levels)
//!
//! 0. refuses to run unless the base URL is the DEMO host and the symbol is flat with nothing resting;
//! 1. listens on the user-data stream the code really uses (the seed's demo `user` row,
//!    `crate::demo_credentials::demo_hosts`'s `fapi_websocket`) and records every frame;
//! 2. opens a minimal long with a market BUY (our `VenueRestBinancePerp`);
//! 3. (a) a raw `POST /fapi/v1/algoOrder` STOP_MARKET far away — the ack shape; our `submit_order` for a second;
//! 4. (b) the `ALGO_UPDATE NEW` frame, if any; (c) `openOrders` vs `openAlgoOrders`, our confirm / report path;
//! 5. (d) cancel by `clientAlgoId` through our code (the non-member fallback AND the member path) and
//!    the `ALGO_UPDATE CANCELED` frame; the answers for an unknown order on both endpoints;
//! 6. (e) the OLD call: `POST /fapi/v1/order` `type=STOP_MARKET` must answer `-4120`;
//! 7. (f) a stop one tick under the market that TRIGGERS: `TRIGGERING/TRIGGERED/FINISHED`, the child
//!    `ORDER_TRADE_UPDATE`, then flat. If the market does not move in time the stop is cancelled instead;
//! 8. ALWAYS leaves the account flat: a `Drop` guard cancels every algo and regular order on the symbol
//!    and closes any position with a reduce-only market order, even when an assertion fails.
//!
//! **DEMO ONLY.** Refuses any base URL but the seed's demo USDⓈ-M host (`crate::demo_credentials::demo_hosts`, asserted by
//! `crate::perp_algo_venue::Venue`). Credentials come from the smoke store
//! (`vike_secrets::smoke_directory`) and are never printed, logged or put on argv.
//!
//! ## The stop-limit drills (`OrderType::StopLimit`), same account, same fence, nothing can fill
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 perp_algo_smoke::perp_stop_limit
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 perp_algo_smoke::spot_stop_limit
//!
//! * [`perp_stop_limit_on_the_demo_algo_service`]: a far Algo Service `STOP` (no position, not
//!   reduce-only) placed through our `submit_order`, read back on every read the bridge makes (the
//!   `openAlgoOrders` row, `ALGO_UPDATE NEW`, our confirm, the report path), refused
//!   a modify, cancelled; then, if the market crosses a near trigger inside the window, a TRIGGERED
//!   stop-limit whose limit rests 3% away from the market is confirmed live, cancelled through our
//!   code (the drill asserts its resting child is gone), and confirmed again as ended (`FINDING
//!   SL/...` lines carry what the venue answered).
//! * [`spot_stop_limit_on_the_demo_host`]: a far BUY `STOP_LOSS_LIMIT` and a far BUY `STOP_LOSS` on spot
//!   through our `VenueRestBinanceSpot`, read back (the venue's open-order row, our confirm, the report
//!   path), the stop-limit refused a modify, both cancelled; then a near BUY stop-limit whose limit
//!   rests 3% under the market, which (if the market crosses it inside the window) must be the SAME
//!   order after the trigger and stay listed by `openOrders` and the report, cancelled through our
//!   code. Its `Drop` guard cancels every spot order on the symbol.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::demo_credentials::demo_hosts;
use crate::perp_algo_venue::{LeaveFlat, SYMBOL, Venue, order};
use serde_json::Value;
use vike_binance::execution::rest::perp::{
    PATH_EXCHANGE_INFO, VenueRestBinancePerp, parse_binance_perp_instruments,
};
use vike_binance::execution::rest::spot::{self, VenueRestBinanceSpot, parse_symbol_properties};
use vike_binance::execution::user_data::perp::{LISTENKEY_PATH, ListenKeyAuthenticationHeader};
use vike_binance::family::execution::user_data::open_user_data_websocket;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
    WebsocketSessionUserStream,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// Everything below must finish well inside this.
pub(crate) const BUDGET: Duration = Duration::from_secs(300);

/// The demo spot host, the seed's demo row (the spot drills sign against it alone).
pub(crate) fn demo_spot() -> &'static str {
    demo_hosts().spot_rest
}

// ---- the venue, raw ------------------------------------------------------------------------------

impl Venue {
    pub(crate) fn open_order_client_ids(&self, path: &str, key: &str) -> Vec<String> {
        let rows = self.call("GET", path, &[self.symbol()]).unwrap_or_else(|e| {
            println!("FINDING row 26: GET {path} answered {} {} on the demo host", e.code, e.msg);
            panic!("{path}: {e}")
        });
        rows.as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get(key)?.as_str().map(str::to_string))
            .collect()
    }
}

// ---- the user-data stream, raw -------------------------------------------------------------------

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<Value>>>);

impl Frames {
    fn all(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }

    fn of(&self, e: &str) -> Vec<Value> {
        self.all().into_iter().filter(|f| f["e"] == e).collect()
    }

    /// The `ALGO_UPDATE` frames, whichever key the algo object rides under, with their `X`.
    fn algo(&self, order_client_id: &str) -> Vec<(String, Value)> {
        self.of("ALGO_UPDATE")
            .into_iter()
            .filter_map(|f| {
                let o = f.get("o").or_else(|| f.get("ao"))?.clone();
                (o["caid"].as_str() == Some(order_client_id))
                    .then(|| (o["X"].as_str().unwrap_or("?").to_string(), f.clone()))
            })
            .collect()
    }

    fn wait(&self, what: &str, within: Duration, mut done: impl FnMut(&Frames) -> bool) -> bool {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if done(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        println!("WAIT timed out after {within:?}: {what}");
        false
    }
}

fn listen(creds: &Credentials, stop: Arc<AtomicBool>) -> (Frames, std::thread::JoinHandle<()>) {
    let auth = ListenKeyAuthenticationHeader::new(creds.api_key.expose().clone());
    let never = AtomicBool::new(false);
    let hosts = demo_hosts();
    let mut ws = match open_user_data_websocket(
        hosts.fapi_rest,
        LISTENKEY_PATH,
        hosts.fapi_websocket,
        "",
        &auth,
        &never,
    ) {
        WebsocketSessionUserOpenOutcome::Ready(ws) => ws,
        WebsocketSessionUserOpenOutcome::Transport(e)
        | WebsocketSessionUserOpenOutcome::Refused(e) => panic!("user-data connect failed: {e}"),
        WebsocketSessionUserOpenOutcome::Auth(e) => panic!("user-data auth failed: {e}"),
        WebsocketSessionUserOpenOutcome::Stopped => unreachable!(),
    };
    let frames = Frames::default();
    let sink = frames.clone();
    let reader = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match ws.recv() {
                Ok(WebsocketSessionUserMessage::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        let e = v["e"].as_str().unwrap_or("?").to_string();
                        if e == "ALGO_UPDATE" || e == "ORDER_TRADE_UPDATE" {
                            println!("FRAME {t}");
                        } else {
                            println!("FRAME-TYPE {e}");
                        }
                        sink.0.lock().unwrap().push(v);
                    }
                }
                Ok(WebsocketSessionUserMessage::Ping(p)) => {
                    let _ = ws.pong(p);
                }
                Ok(WebsocketSessionUserMessage::Other)
                | Err(WebsocketSessionUserError::Timeout) => {}
                Err(WebsocketSessionUserError::Closed(why)) => {
                    println!("FRAME-STREAM closed: {why}");
                    break;
                }
            }
        }
    });
    (frames, reader)
}

// ---- the drill -----------------------------------------------------------------------------------

pub(crate) fn accepted(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderAccepted(a) => Some(a.venue_order_id.as_deref().unwrap_or("").to_string()),
        _ => None,
    })
}

pub(crate) fn finding(row: &str, text: impl std::fmt::Display) {
    println!("FINDING row {row}: {text}");
}

/// The default binance DEMO account's credentials, or `None` after a `SKIP` line when they are absent
/// or the account is one the owner fenced off.
pub(crate) fn drill_credentials() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return None;
    };
    // The owner's fence: these account ids are never driven by a drill, whatever their tier says.
    const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("DRILL account id {id:?} (default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return None;
    }
    Some(creds)
}

/// Our perp client on the demo host for `SYMBOL`, on the venue's own grid.
pub(crate) fn demo_rest(
    venue: &Venue,
    creds: &Credentials,
) -> (VenueRestBinancePerp<BinanceHmacSigner, UreqTransport>, SymbolProperties) {
    let info = venue
        .transport
        .public(venue.host, PATH_EXCHANGE_INFO, &[venue.symbol()])
        .expect("exchangeInfo");
    let properties_value = parse_binance_perp_instruments(&info)[SYMBOL].properties;
    let rest = VenueRestBinancePerp {
        link_id: None,
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance")
            .with_rate_gate(vike_binance::transport::ratelimit::perp_rest_gate()),
        base_url: venue.host.to_string(),
        ty: demo_hosts().ty,
        margining: vike_binance::execution::rest::coin_m::margining_of(SYMBOL),
        symbol: SYMBOL.to_string(),
        properties: properties_value,
        leverage: 2.0,
        algo_order_client_ids: Default::default(),
    };
    (rest, properties_value)
}

/// The value the bridge's own builder put on the wire for `key` (what a venue echo is judged against).
pub(crate) fn sent(params: &[(&'static str, String)], key: &str) -> Option<f64> {
    params.iter().find(|(k, _)| *k == key).and_then(|(_, v)| v.parse().ok())
}

/// A venue row's decimal string as a number (the venue pads its own zeros).
pub(crate) fn number(v: &Value) -> Option<f64> {
    v.as_str().and_then(|s| s.parse().ok())
}

#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO orders, flattens - run by hand (see module doc)"]
fn perp_conditional_orders_on_the_demo_algo_service() {
    vike_log::test_init();
    let started = Instant::now();
    let Some(creds) = drill_credentials() else { return };
    let venue = Venue::new(&creds);

    // 0. the symbol must be flat with nothing resting, or this drill touches nothing
    let (pos, regular, algo) = (
        venue.position(SYMBOL),
        venue.open_order_client_ids("/fapi/v1/openOrders", "clientOrderId"),
        venue.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId"),
    );
    if pos != 0.0 || !regular.is_empty() || !algo.is_empty() {
        println!(
            "SKIP: {SYMBOL} is not flat/clean on the demo account (position {pos}, {} regular, {} algo resting)",
            regular.len(),
            algo.len()
        );
        return;
    }
    let flat_guard = LeaveFlat { creds: creds.clone() };

    // row 20 (what the cutover did to conditional orders that already existed): nothing can be placed
    // on the old endpoint any more, so the one thing measurable is the account's standing census -
    // any conditional order resting ACCOUNT-WIDE, read from both lists (symbols and kinds only).
    let census = |path: &str| -> String {
        match venue.call("GET", path, &[]) {
            Ok(rows) => {
                let kinds: Vec<String> = rows
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        format!(
                            "{}/{}",
                            r["symbol"].as_str().unwrap_or("?"),
                            r["orderType"].as_str().or(r["type"].as_str()).unwrap_or("?")
                        )
                    })
                    .collect();
                format!("{} resting {kinds:?}", kinds.len())
            }
            Err(e) => format!("read failed: {} {}", e.code, e.msg),
        }
    };
    finding(
        "20",
        format!(
            "account-wide at start: openOrders {}; openAlgoOrders {}",
            census("/fapi/v1/openOrders"),
            census("/fapi/v1/openAlgoOrders")
        ),
    );

    let (rest, properties) = demo_rest(&venue, &creds);

    // 1. the stream the code really uses
    let stop_reader = Arc::new(AtomicBool::new(false));
    let (frames, reader) = listen(&creds, stop_reader.clone());
    std::thread::sleep(Duration::from_secs(3));

    // 2. a minimal long
    let last = venue.last_price(SYMBOL);
    let want = (properties.min_notional.max(5.0) * 1.5 / last).max(properties.min_qty);
    let qty = (want / properties.step_size).ceil() * properties.step_size;
    let tag = now_ms() % 100_000_000;
    let open = rest.submit_order(&order(&format!("vtalgoO{tag}"), 1, qty, OrderType::Market, None));
    println!(
        "OPEN market BUY {} @ ~{last}: accepted={:?}",
        format_to_step_f(qty, properties.step_size),
        accepted(&open)
    );
    assert!(accepted(&open).is_some(), "the entry must be accepted: {open:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while venue.position(SYMBOL) < qty * 0.99 {
        assert!(Instant::now() < deadline, "the entry never showed in positionRisk");
        std::thread::sleep(Duration::from_millis(500));
    }

    // 3. (a) a raw New Algo Order far below the market: the ack shape
    let tick = properties.tick_size;
    let far = |pct: f64| ((last * pct) / tick).floor() * tick;
    let a1 = format!("vtalgoA{tag}");
    let mut placed = None;
    for pct in [0.85, 0.95] {
        let req = order(&a1, -1, qty, OrderType::Stop, Some(far(pct)));
        let params = rest.build_algo_params(&req);
        match venue.call("POST", "/fapi/v1/algoOrder", &params) {
            Ok(ack) => {
                println!("ACK algoOrder POST (trigger {pct}x): {ack}");
                placed = Some(ack);
                break;
            }
            Err(e) => println!("ACK algoOrder POST (trigger {pct}x) refused: {e}"),
        }
    }
    let ack = placed.expect("a far STOP_MARKET must be accepted on the algo endpoint");
    assert!(ack.get("algoId").is_some(), "the ack carries algoId: {ack}");
    finding(
        "10",
        format!(
            "ack keys: {:?}; has orderId: {}",
            ack.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            ack.get("orderId").is_some()
        ),
    );

    // and the same through our code (a member of the set from here on)
    let a2 = format!("vtalgoB{tag}");
    let ours = rest.submit_order(&order(&a2, -1, qty, OrderType::Stop, Some(far(0.85))));
    println!("OURS submit_order stop: {ours:?}");
    let a2_algo_id = accepted(&ours).expect("our stop submit must be accepted");
    assert!(!a2_algo_id.is_empty(), "OrderAccepted carries the algoId");

    // (b) ALGO_UPDATE NEW on the stream?
    let saw_new = frames.wait("ALGO_UPDATE NEW for both stops", Duration::from_secs(8), |f| {
        f.algo(&a1).iter().any(|(x, _)| x == "NEW") && f.algo(&a2).iter().any(|(x, _)| x == "NEW")
    });
    let fapi_websocket = demo_hosts().fapi_websocket;
    finding(
        "15/host",
        format!(
            "ALGO_UPDATE NEW pushed on {fapi_websocket}: {saw_new}; frame types seen: {:?}",
            frames
                .all()
                .iter()
                .map(|f| f["e"].as_str().unwrap_or("?").to_string())
                .collect::<std::collections::BTreeSet<_>>()
        ),
    );

    // (c) openOrders vs openAlgoOrders
    let in_regular = venue.open_order_client_ids("/fapi/v1/openOrders", "clientOrderId");
    let in_algo = venue.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId");
    finding(
        "14",
        format!(
            "resting stops in openOrders: {:?}; in openAlgoOrders: {:?}",
            [&a1, &a2].map(|c| in_regular.contains(c)),
            [&a1, &a2].map(|c| in_algo.contains(c))
        ),
    );
    assert!(
        in_algo.contains(&a1) && in_algo.contains(&a2),
        "openAlgoOrders must list both stops: {in_algo:?}"
    );
    // our confirm (a1 is not in the client's set: regular lookup absent -> algo fallback) and the
    // reconcile read production runs
    let c1 = rest.confirm_order(&a1, now_ms());
    println!("OURS confirm_order a1 (non-member): {c1:?}");
    assert!(accepted(&c1).is_some(), "a resting stop must confirm live, not absent: {c1:?}");
    let reports = vike_binance::recon_client(&creds, &format!("{SYMBOL}.P"), demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    println!(
        "OURS report path: {} rows, order client ids {:?}",
        reports.len(),
        reports.iter().map(|r| r.order_client_id.clone()).collect::<Vec<_>>()
    );
    for order_client_id in [&a1, &a2] {
        assert!(
            reports.iter().any(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str())),
            "the report path lists both stops: {order_client_id} missing"
        );
    }

    // (d) cancel: what the regular DELETE answers for an algo clientAlgoId, the unknown-order words of
    // both endpoints, then OUR cancel of a non-member (a1) and a member (a2)
    let regular_del = venue.call(
        "DELETE",
        "/fapi/v1/order",
        &[venue.symbol(), ("origClientOrderId", a1.clone())],
    );
    finding(
        "7/regular",
        format!(
            "DELETE /fapi/v1/order of an algo clientAlgoId: {}",
            regular_del
                .as_ref()
                .map_or_else(|e| format!("{} {}", e.code, e.msg), |v| format!("ACCEPTED {v}"))
        ),
    );
    assert!(regular_del.is_err(), "the regular endpoint must not cancel an algo order");
    let ghost = format!("vtalgoZ{tag}");
    let ghost_del = venue.call("DELETE", "/fapi/v1/algoOrder", &[("clientAlgoId", ghost.clone())]);
    let ghost_get = venue.call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", ghost.clone())]);
    finding(
        "11/unknown",
        format!(
            "DELETE algoOrder of an unknown id: {:?}; GET algoOrder of an unknown id: {:?}",
            ghost_del.as_ref().map_err(|e| (e.code, e.msg.clone())),
            ghost_get.as_ref().map_err(|e| (e.code, e.msg.clone()))
        ),
    );
    assert!(
        ghost_del.as_ref().err().is_some_and(vike_binance::execution::rest::algo::is_unknown_order),
        "the algo DELETE of an unknown id must answer -2011/-2013 in words `is_unknown_order` knows, or it must learn them: {ghost_del:?}"
    );

    assert!(rest.cancel_order(&a1).is_ok(), "non-member cancel falls back to the algo endpoint");
    assert!(rest.cancel_order(&a2).is_ok(), "member cancel goes straight to the algo endpoint");
    let canceled = frames.wait("ALGO_UPDATE CANCELED for both", Duration::from_secs(8), |f| {
        f.algo(&a1).iter().any(|(x, _)| x == "CANCELED")
            && f.algo(&a2).iter().any(|(x, _)| x == "CANCELED")
    });
    finding("15/cancel", format!("ALGO_UPDATE CANCELED seen for both: {canceled}"));
    let after = venue.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId");
    assert!(!after.contains(&a1) && !after.contains(&a2), "both stops are gone: {after:?}");
    // The stream's second ack: the captured NEW frame, through the bridge's mapper, is an OrderAccepted
    // carrying the SAME algo id the REST ack returned (so a lost ack is rescued by it).
    let new_frame = frames.algo(&a1).into_iter().find(|(x, _)| x == "NEW").map(|(_, f)| f);
    let rescued = new_frame.map(|f| {
        vike_binance::execution::event_mapper::perp::map_binance_perp_opts(
            &f,
            "binance",
            "ETHUSDT.P",
            false,
            None,
        )
    });
    let ack_id = ack["algoId"].to_string();
    finding("M1", format!("NEW frame mapped to {rescued:?}; REST ack algoId {ack_id}"));
    match rescued.as_deref() {
        Some([Event::OrderAccepted(a)]) => {
            assert_eq!(a.venue_order_id.as_deref(), Some(ack_id.as_str()));
        }
        other => panic!("the NEW frame must map to one OrderAccepted: {other:?}"),
    }
    // The post-reconnect resync (and the fill sentinel) reads allAlgoOrders: both cancelled stops must
    // come back as terminal events (the read can lag a moment, like the order queries).
    let replayed = |event: &[Event], order_client_id: &str| {
        event
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id))
    };
    let until = Instant::now() + Duration::from_secs(8);
    let history = loop {
        let h = vike_binance::execution::perp_history_events(&rest, &format!("{SYMBOL}.P"));
        if (replayed(&h, &a1) && replayed(&h, &a2)) || Instant::now() > until {
            break h;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    finding(
        "M2",
        format!(
            "resync replay after the cancels: a1 canceled {}, a2 canceled {}, {} events",
            replayed(&history, &a1),
            replayed(&history, &a2),
            history.len()
        ),
    );
    assert!(
        replayed(&history, &a1) && replayed(&history, &a2),
        "allAlgoOrders must replay both cancelled stops: {history:?}"
    );
    // The query endpoint may lag the push: record what it says first and how long until CANCELED.
    let status_of = |v: &Venue| {
        v.call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", a1.clone())])
            .map_or_else(|e| format!("{} {}", e.code, e.msg), |b| b["algoStatus"].to_string())
    };
    let lag_from = Instant::now();
    let first_status = status_of(&venue);
    let mut canceled_after = None;
    while lag_from.elapsed() < Duration::from_secs(8) {
        if status_of(&venue).contains("CANCELED") {
            canceled_after = Some(lag_from.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    finding(
        "12",
        format!(
            "GET algoOrder right after the cancel push: {first_status}; reads CANCELED after {canceled_after:?}"
        ),
    );
    let c2 = rest.confirm_order(&a1, now_ms());
    println!("OURS confirm_order a1 after cancel: {c2:?}");

    // (e) the OLD call must be refused. The params are on the venue's grid (a float printed raw trips
    // `-1111 Precision`, which the venue checks BEFORE the type and would hide the answer measured here).
    let old = venue.call(
        "POST",
        "/fapi/v1/order",
        &[
            venue.symbol(),
            ("side", "SELL".into()),
            ("type", "STOP_MARKET".into()),
            ("quantity", format_to_step_f(qty, properties.step_size)),
            ("stopPrice", format_to_step_f(far(0.85), tick)),
            ("reduceOnly", "true".into()),
            ("newClientOrderId", format!("vtalgoX{tag}")),
        ],
    );
    finding(
        "26",
        format!(
            "POST /fapi/v1/order type=STOP_MARKET on the demo host: {}",
            old.as_ref()
                .map_or_else(|e| format!("{} {}", e.code, e.msg), |v| format!("ACCEPTED {v}"))
        ),
    );
    if old.is_ok() {
        let _ = venue.call(
            "DELETE",
            "/fapi/v1/order",
            &[venue.symbol(), ("origClientOrderId", format!("vtalgoX{tag}"))],
        );
    }
    assert_eq!(
        old.as_ref().err().map(|e| e.code),
        Some(-4120),
        "the demo host must refuse a conditional type on /order: {old:?}"
    );

    // (f) a stop one tick under the market that TRIGGERS
    let near = format!("vtalgoN{tag}");
    let mut near_events = None;
    for ticks in [2.0, 6.0, 20.0] {
        let now = venue.last_price(SYMBOL);
        let trigger = ((now / tick).floor() - ticks) * tick;
        let event = rest.submit_order(&order(&near, -1, qty, OrderType::Stop, Some(trigger)));
        println!("NEAR stop trigger {trigger} (last {now}, {ticks} ticks under): {event:?}");
        if accepted(&event).is_some() {
            near_events = Some(event);
            break;
        }
    }
    if near_events.is_some() {
        let triggered = frames.wait(
            "the near stop to TRIGGER",
            Duration::from_secs(90).min(BUDGET.saturating_sub(started.elapsed())),
            |f| f.algo(&near).iter().any(|(x, _)| x == "TRIGGERED" || x == "FINISHED"),
        );
        let statuses: Vec<String> = frames.algo(&near).into_iter().map(|(x, _)| x).collect();
        finding("18/lifecycle", format!("near stop ALGO_UPDATE statuses in order: {statuses:?}"));
        if triggered {
            std::thread::sleep(Duration::from_secs(3));
            let ai: Vec<String> = frames
                .algo(&near)
                .iter()
                .filter_map(|(_, f)| {
                    let o = f.get("o").or_else(|| f.get("ao"))?;
                    let ai = o.get("ai")?;
                    Some(ai.to_string())
                })
                .collect();
            let aid = frames.algo(&near).first().and_then(|(_, f)| {
                Some(f.get("o").or_else(|| f.get("ao"))?.get("aid")?.to_string())
            });
            let trades: Vec<Value> = frames
                .of("ORDER_TRADE_UPDATE")
                .into_iter()
                .filter(|f| f["o"]["s"] == SYMBOL && f["o"]["x"] == "TRADE")
                .collect();
            let child = trades.last().map(|f| (f["o"]["c"].clone(), f["o"]["i"].clone()));
            finding(
                "18",
                format!(
                    "clientAlgoId={near}; algoId={aid:?}; ALGO_UPDATE.ai values={ai:?}; the child TRADE frame (c, i)={child:?}; child c equals clientAlgoId: {}",
                    child.as_ref().is_some_and(|(c, _)| c.as_str() == Some(near.as_str()))
                ),
            );
            finding(
                "18/ap",
                format!(
                    "ALGO_UPDATE ap/aq/act after the trigger: {:?}",
                    frames.algo(&near).last().map(|(_, f)| {
                        let o = f.get("o").or_else(|| f.get("ao")).cloned().unwrap_or_default();
                        (o["ap"].clone(), o["aq"].clone(), o["act"].clone())
                    })
                ),
            );
            let flat_deadline = Instant::now() + Duration::from_secs(15);
            while venue.position(SYMBOL) != 0.0 && Instant::now() < flat_deadline {
                std::thread::sleep(Duration::from_millis(500));
            }
            assert_eq!(
                venue.position(SYMBOL),
                0.0,
                "a triggered reduce-only stop flattens the position"
            );
        } else {
            println!(
                "NEAR stop never triggered inside the window; cancelling it (the market did not move)"
            );
            assert!(rest.cancel_order(&near).is_ok());
            finding("18", "NOT MEASURED: the market never crossed the trigger in the window");
        }
    }

    // 8. flat and clean, whatever the path
    stop_reader.store(true, Ordering::Relaxed);
    let _ = reader.join();
    drop(flat_guard);
    let v = Venue::new(&creds);
    assert_eq!(v.position(SYMBOL), 0.0, "the drill leaves the symbol flat");
    assert!(v.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId").is_empty());
    assert!(v.open_order_client_ids("/fapi/v1/openOrders", "clientOrderId").is_empty());
    println!("DRILL done in {:?}: flat, nothing resting", started.elapsed());
}

/// A perp stop-limit on the Algo Service, through our code (module doc). No position is opened and no
/// order is reduce-only; every limit is far from the market, so nothing can fill.
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO stop-limits, none can fill - run by hand (see module doc)"]
fn perp_stop_limit_on_the_demo_algo_service() {
    vike_log::test_init();
    let started = Instant::now();
    let Some(creds) = drill_credentials() else { return };
    let venue = Venue::new(&creds);
    let regular = |v: &Venue| v.open_order_client_ids("/fapi/v1/openOrders", "clientOrderId");
    let algo = |v: &Venue| v.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId");
    if venue.position(SYMBOL) != 0.0 || !regular(&venue).is_empty() || !algo(&venue).is_empty() {
        println!("SKIP: {SYMBOL} is not flat/clean on the demo account");
        return;
    }
    let flat_guard = LeaveFlat { creds: creds.clone() };
    let (rest, properties) = demo_rest(&venue, &creds);
    let stop_reader = Arc::new(AtomicBool::new(false));
    let (frames, reader) = listen(&creds, stop_reader.clone());
    std::thread::sleep(Duration::from_secs(3));

    let tick = properties.tick_size;
    let on_grid = |px: f64| (px / tick).round() * tick;
    let last = venue.last_price(SYMBOL);
    let want = (properties.min_notional.max(5.0) * 1.5 / last).max(properties.min_qty);
    let qty = (want / properties.step_size).ceil() * properties.step_size;
    let tag = now_ms() % 100_000_000;
    let stop_limit = |order_client_id: &str, side: i32, trigger: f64, limit: f64| OrderRequest {
        price: Some(limit),
        reduce_only: false,
        ..order(order_client_id, side, qty, OrderType::StopLimit, Some(trigger))
    };

    // 1. a far SELL stop-limit, through our submit_order (the venue may band the limit at placement)
    let far = format!("vtslF{tag}");
    let mut placed = None;
    for pct in [0.90, 0.96] {
        let trigger = on_grid(last * pct);
        let req = stop_limit(&far, -1, trigger, trigger - 10.0 * tick);
        let event = rest.submit_order(&req);
        finding("SL/place", format!("far stop-limit, trigger {pct}x of {last}: {event:?}"));
        if accepted(&event).is_some() {
            placed = Some(req);
            break;
        }
    }
    let far_request = placed.expect("a far stop-limit must be accepted on the algo endpoint");
    let wire = rest.build_algo_params(&far_request);

    // 2. read back: the venue's row, the stream, our confirm, the report path
    let rows = venue.call("GET", "/fapi/v1/openAlgoOrders", &[venue.symbol()]).expect("open algo");
    let row = rows
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["clientAlgoId"].as_str() == Some(far.as_str()))
        .cloned()
        .expect("openAlgoOrders lists the stop-limit");
    finding("SL/row", format!("the resting stop-limit's openAlgoOrders row: {row}"));
    assert_eq!(row["orderType"], "STOP", "{row}");
    assert_eq!(row["timeInForce"], "GTC", "{row}");
    assert_eq!(number(&row["price"]), sent(&wire, "price"), "the limit the venue holds: {row}");
    assert_eq!(number(&row["triggerPrice"]), sent(&wire, "triggerPrice"), "{row}");
    assert!(!regular(&venue).contains(&far), "openOrders does not list an untriggered stop-limit");
    let saw_new = frames.wait("ALGO_UPDATE NEW", Duration::from_secs(8), |f| {
        f.algo(&far).iter().any(|(x, _)| x == "NEW")
    });
    finding("SL/new", format!("ALGO_UPDATE NEW for the stop-limit: {saw_new}"));
    let confirmed = rest.confirm_order(&far, now_ms());
    assert!(accepted(&confirmed).is_some(), "a resting stop-limit confirms live: {confirmed:?}");
    // The report path is what a running node reads; it carries no price, so the limit and the
    // trigger are judged on the venue's own row above.
    let reports = vike_binance::recon_client(&creds, &format!("{SYMBOL}.P"), demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    let report = reports
        .iter()
        .find(|r| r.order_client_id.as_deref() == Some(far.as_str()))
        .expect("the report path lists the stop-limit");
    assert_eq!(report.order_type, OrderType::StopLimit, "{report:?}");

    // 3. an untriggered algo order is not modifiable: the advisory, and the venue still holds the limit
    let modified = rest.modify_order(&far_request, None, far_request.price.map(|p| p - tick));
    assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");

    // 4. cancel through our code
    assert_eq!(rest.cancel_order(&far).ok(), Some(OrderCancelAck::Canceled), "our cancel");
    let canceled = frames.wait("ALGO_UPDATE CANCELED", Duration::from_secs(8), |f| {
        f.algo(&far).iter().any(|(x, _)| x == "CANCELED")
    });
    finding("SL/cancel-far", format!("ALGO_UPDATE CANCELED for the far stop-limit: {canceled}"));
    assert!(!algo(&venue).contains(&far), "the far stop-limit is gone");

    // 5. a TRIGGERED stop-limit whose limit rests 3% away: one each side, whichever fires first
    let up = format!("vtslU{tag}");
    let down = format!("vtslD{tag}");
    let place = |order_client_id: &str, side: i32| -> bool {
        for ticks in [2.0, 6.0, 20.0] {
            let now = venue.last_price(SYMBOL);
            let base = (now / tick).floor() * tick;
            // a BUY fires above the market and rests under it; a SELL the mirror
            let (trigger, limit) = if side > 0 {
                (base + ticks * tick, on_grid(now * 0.97))
            } else {
                (base - ticks * tick, on_grid(now * 1.03))
            };
            let event = rest.submit_order(&stop_limit(order_client_id, side, trigger, limit));
            finding(
                "SL/near",
                format!(
                    "{order_client_id} trigger {trigger} limit {limit} (last {now}): {event:?}"
                ),
            );
            if accepted(&event).is_some() {
                return true;
            }
        }
        false
    };
    let placed_any = place(&up, 1) | place(&down, -1);
    let fired =
        |f: &Frames, c: &str| f.algo(c).iter().any(|(x, _)| x == "TRIGGERED" || x == "FINISHED");
    let triggered = placed_any
        && frames.wait(
            "a near stop-limit to TRIGGER",
            Duration::from_secs(90).min(BUDGET.saturating_sub(started.elapsed())),
            |f| fired(f, &up) || fired(f, &down),
        );
    if triggered {
        let (t, other) = if fired(&frames, &up) { (&up, &down) } else { (&down, &up) };
        std::thread::sleep(Duration::from_secs(2));
        let statuses: Vec<String> = frames.algo(t).into_iter().map(|(x, _)| x).collect();
        let child = |f: &Frames| -> Vec<String> {
            f.of("ORDER_TRADE_UPDATE")
                .into_iter()
                .filter(|u| u["o"]["c"].as_str() == Some(t.as_str()))
                .map(|u| {
                    let o = &u["o"];
                    format!("x={} X={} o={} i={} p={}", o["x"], o["X"], o["o"], o["i"], o["p"])
                })
                .collect()
        };
        finding("SL/trigger", format!("{t} ALGO_UPDATE statuses: {statuses:?}"));
        finding("SL/child", format!("{t} ORDER_TRADE_UPDATE frames: {:?}", child(&frames)));
        let algo_get = venue.call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", t.clone())]);
        finding(
            "SL/algo-get",
            algo_get.as_ref().map_or_else(
                |e| format!("{} {}", e.code, e.msg),
                |b| format!("algoStatus {} actualOrderId {}", b["algoStatus"], b["actualOrderId"]),
            ),
        );
        let child_rests = |v: &Venue| regular(v).contains(t);
        finding(
            "SL/lists",
            format!(
                "after the trigger: openOrders lists the child {}; openAlgoOrders lists the stop {}",
                child_rests(&venue),
                algo(&venue).contains(t)
            ),
        );
        // our confirm of the TRIGGERED stop-limit (a member: this client placed it) reads its CHILD,
        // which rests: live, nothing more
        let live = rest.confirm_order(t, now_ms());
        finding("SL/confirm", format!("our confirm_order while the child rests: {live:?}"));
        assert!(matches!(live.as_slice(), [Event::OrderAccepted(_)]), "{live:?}");
        // THE question: our cancel of a TRIGGERED stop-limit must take its resting child
        let ours = rest.cancel_order(t);
        std::thread::sleep(Duration::from_secs(2));
        let still = child_rests(&venue);
        finding(
            "SL/cancel",
            format!(
                "our cancel_order: {ours:?}; the child still rests after it: {still}; child frames: {:?}",
                child(&frames)
            ),
        );
        if still {
            let algo_del =
                venue.call("DELETE", "/fapi/v1/algoOrder", &[("clientAlgoId", t.clone())]);
            let order_del = venue.call(
                "DELETE",
                "/fapi/v1/order",
                &[venue.symbol(), ("origClientOrderId", t.clone())],
            );
            finding(
                "SL/raw-cancel",
                format!(
                    "DELETE algoOrder: {:?}; DELETE /fapi/v1/order: {:?}",
                    algo_del.map_err(|e| (e.code, e.msg)),
                    order_del.map_err(|e| (e.code, e.msg))
                ),
            );
        }
        assert!(!still, "our cancel of a triggered stop-limit must cancel its resting child");
        // and the confirm once the child has ENDED: asked as a member again (a client still holding
        // the hint), the stop's state is its child's CANCELED, not the algo endpoint's TRIGGERED
        rest.algo_order_client_ids.insert(t);
        let algo_now = venue
            .call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", t.clone())])
            .map_or_else(|e| format!("{} {}", e.code, e.msg), |b| b["algoStatus"].to_string());
        let ended = rest.confirm_order(t, now_ms());
        finding(
            "SL/confirm-ended",
            format!("algoStatus now {algo_now}; our confirm_order: {ended:?}"),
        );
        assert!(
            ended.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
            "the ended child is the stop's state: {ended:?}"
        );
        assert!(rest.cancel_order(other).is_ok(), "the untriggered one cancels");
    } else {
        for c in [&up, &down] {
            assert!(rest.cancel_order(c).is_ok(), "cancel {c}");
        }
        finding("SL/trigger", "NOT MEASURED: no near stop-limit triggered inside the window");
    }

    // 6. flat and clean, whatever the path
    stop_reader.store(true, Ordering::Relaxed);
    let _ = reader.join();
    drop(flat_guard);
    let v = Venue::new(&creds);
    assert_eq!(v.position(SYMBOL), 0.0, "the drill leaves the symbol flat");
    assert!(algo(&v).is_empty() && regular(&v).is_empty(), "nothing rests");
    println!("STOP-LIMIT DRILL done in {:?}: flat, nothing resting", started.elapsed());
}

/// Cancels every SPOT order on `SYMBOL` (a `Drop` runs on a panicking test too; it never panics).
pub(crate) struct SpotClean(pub(crate) Credentials);

impl Drop for SpotClean {
    fn drop(&mut self) {
        let signer = BinanceHmacSigner::new(&self.0, now_ms);
        let params = [("symbol", SYMBOL.to_string())];
        let r = UreqTransport::new("binance").signed(
            demo_spot(),
            spot::PATH_OPEN_ORDERS,
            "DELETE",
            &params,
            &signer,
        );
        println!(
            "CLEANUP spot {SYMBOL}: {}",
            r.map_or_else(|e| e.to_string(), |_| "cancelled".into())
        );
    }
}

/// The SPOT stop-limit and stop through our `VenueRestBinanceSpot` (module doc). Both BUYs fire 10% above
/// the market, so nothing can fill.
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO spot stop orders, none can fill - run by hand (see module doc)"]
fn spot_stop_limit_on_the_demo_host() {
    vike_log::test_init();
    let Some(creds) = drill_credentials() else { return };
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_spot(), spot::PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    let properties_value = parse_symbol_properties(&info)[SYMBOL];
    let rest = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: demo_spot().to_string(),
        symbol: SYMBOL.to_string(),
        properties: properties_value,
        base_asset: "ETH".to_string(),
    };
    rest.signer.set_offset_ms(rest.server_time_offset(now_ms()).expect("server time"));
    let open = |r: &VenueRestBinanceSpot<BinanceHmacSigner, UreqTransport>| -> Vec<Value> {
        let params = [("symbol", SYMBOL.to_string())];
        let rows =
            r.transport.signed(demo_spot(), spot::PATH_OPEN_ORDERS, "GET", &params, &r.signer);
        rows.expect("spot openOrders").as_array().cloned().unwrap_or_default()
    };
    if !open(&rest).is_empty() {
        println!("SKIP: spot {SYMBOL} has orders resting on the demo account");
        return;
    }
    let _clean = SpotClean(creds.clone());
    let ticker = rest
        .transport
        .public(demo_spot(), spot::PATH_TICKER, &[("symbol", SYMBOL.into())])
        .expect("ticker");
    let last: f64 = ticker["price"].as_str().and_then(|p| p.parse().ok()).expect("a price");
    let on_grid = |px: f64| (px / properties_value.tick_size).round() * properties_value.tick_size;
    let qty = ((properties_value.min_notional.max(5.0) * 2.0 / last) / properties_value.step_size)
        .ceil()
        * properties_value.step_size;
    let tag = now_ms() % 100_000_000;
    let stop_limit = OrderRequest {
        order_client_id: format!("vtsslL{tag}"),
        venue: "binance".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty,
        order_type: OrderType::StopLimit,
        trigger_price: Some(on_grid(last * 1.10)),
        price: Some(on_grid(last * 1.12)),
        timestamp: now_ms(),
        ..Default::default()
    };
    let stop = OrderRequest {
        order_client_id: format!("vtsslS{tag}"),
        order_type: OrderType::Stop,
        price: None,
        ..stop_limit.clone()
    };
    for req in [&stop_limit, &stop] {
        let event = rest.submit_order(req);
        finding("SPOT/place", format!("{} {}: {event:?}", req.order_type, req.order_client_id));
        assert!(
            accepted(&event).is_some(),
            "the spot {} must be accepted: {event:?}",
            req.order_type
        );
    }
    let rows = open(&rest);
    let reports = vike_binance::recon_client(&creds, SYMBOL, demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    for req in [&stop_limit, &stop] {
        let order_client_id = req.order_client_id.as_str();
        let row = rows
            .iter()
            .find(|r| r["clientOrderId"].as_str() == Some(order_client_id))
            .unwrap_or_else(|| panic!("spot openOrders lists {order_client_id}: {rows:?}"));
        finding("SPOT/row", format!("{}: {row}", req.order_type));
        let wire = rest.build_order_params(req);
        let (word, limit) = match req.order_type {
            OrderType::StopLimit => ("STOP_LOSS_LIMIT", sent(&wire, "price")),
            _ => ("STOP_LOSS", None),
        };
        assert_eq!(row["type"], word, "{row}");
        assert_eq!(number(&row["stopPrice"]), sent(&wire, "stopPrice"), "{row}");
        if limit.is_some() {
            assert_eq!(number(&row["price"]), limit, "{row}");
            assert_eq!(row["timeInForce"], "GTC", "{row}");
        }
        let confirmed = rest.confirm_order(order_client_id, now_ms());
        assert!(accepted(&confirmed).is_some(), "{order_client_id} confirms live: {confirmed:?}");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            .unwrap_or_else(|| panic!("the report path lists {order_client_id}: {reports:?}"));
        assert_eq!(report.order_type, req.order_type, "{report:?}");
    }
    let modified = rest.modify_order(&stop_limit, None, stop_limit.price);
    assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
    for req in [&stop_limit, &stop] {
        let ack = rest.cancel_order(&req.order_client_id).ok();
        assert_eq!(ack, Some(OrderCancelAck::Canceled), "our cancel of {}", req.order_client_id);
    }
    assert!(open(&rest).is_empty(), "nothing rests on spot {SYMBOL}");

    // A TRIGGERED spot stop-limit whose limit rests 3% under the market (nothing can fill): is it the
    // SAME order after the trigger (its id and client id), still listed by openOrders and the report?
    // That is what lets the reconcile see it across a restart and name its fill by its own id.
    let near = OrderRequest { order_client_id: format!("vtsslN{tag}"), ..stop_limit.clone() };
    let mut near_request = None;
    for ticks in [1.0, 2.0, 4.0, 8.0] {
        let now: f64 = rest
            .transport
            .public(demo_spot(), spot::PATH_TICKER, &[("symbol", SYMBOL.into())])
            .ok()
            .and_then(|t| t["price"].as_str().and_then(|p| p.parse().ok()))
            .expect("a price");
        let req = OrderRequest {
            trigger_price: Some(on_grid(now) + ticks * properties_value.tick_size),
            price: Some(on_grid(now * 0.97)),
            ..near.clone()
        };
        let event = rest.submit_order(&req);
        finding(
            "SPOT/near",
            format!(
                "trigger {:?} limit {:?} (last {now}): {event:?}",
                req.trigger_price, req.price
            ),
        );
        if accepted(&event).is_some() {
            near_request = Some(req);
            break;
        }
    }
    let near_request = near_request.expect("a near spot stop-limit must be accepted");
    let order_client_id = near_request.order_client_id.as_str();
    let order_of = |r: &VenueRestBinanceSpot<BinanceHmacSigner, UreqTransport>| -> Value {
        let params =
            [("symbol", SYMBOL.to_string()), ("origClientOrderId", order_client_id.to_string())];
        r.transport
            .signed(demo_spot(), spot::PATH_ORDER, "GET", &params, &r.signer)
            .expect("GET order")
    };
    let placed = order_of(&rest);
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut fired = None;
    while Instant::now() < deadline {
        let now = order_of(&rest);
        if now["isWorking"] == true || now["status"] != "NEW" {
            fired = Some(now);
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    finding("SPOT/placed", format!("the near stop-limit before the trigger: {placed}"));
    match fired {
        Some(after) => {
            finding("SPOT/fired", format!("the same order after the trigger: {after}"));
            assert_eq!(
                after["orderId"], placed["orderId"],
                "a triggered spot stop keeps its order id"
            );
            assert_eq!(after["clientOrderId"], placed["clientOrderId"], "and its client id");
            let listed =
                open(&rest).iter().any(|r| r["clientOrderId"].as_str() == Some(order_client_id));
            let reports = vike_binance::recon_client(&creds, SYMBOL, demo_hosts())
                .expect("recon client")
                .fetch_order_status_reports(0)
                .expect("report path");
            let report =
                reports.iter().find(|r| r.order_client_id.as_deref() == Some(order_client_id));
            finding(
                "SPOT/fired-lists",
                format!("openOrders lists it: {listed}; the report: {report:?}"),
            );
            assert!(listed, "openOrders lists the triggered stop-limit");
            assert_eq!(report.map(|r| r.order_type.clone()), Some(OrderType::StopLimit));
        }
        None => finding(
            "SPOT/fired",
            "NOT MEASURED: the market never crossed the trigger in the window",
        ),
    }
    assert_eq!(
        rest.cancel_order(order_client_id).ok(),
        Some(OrderCancelAck::Canceled),
        "our cancel of {order_client_id}"
    );
    assert!(open(&rest).is_empty(), "nothing rests on spot {SYMBOL}");
    println!("SPOT STOP DRILL done: placed, read back and cancelled");
}
