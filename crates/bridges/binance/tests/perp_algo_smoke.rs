//! LIVE DEMO drill for Binance USDⓈ-M CONDITIONAL orders on the Algo Service (network + demo creds —
//! run by hand, never in a loop; NOT in `.github/workflows/live-smokes.yml`):
//!
//!     cargo test -p vike-binance --test perp_algo_smoke -- --ignored --nocapture
//!
//! `crates/bridges/binance/src/algo.rs` moved the bridge's stop from `POST /fapi/v1/order`
//! (`-4120` since 2025-12-09) to `POST /fapi/v1/algoOrder`. Four facts about the DEMO venue are not
//! documented and this drill MEASURES them (it prints a `FINDING` line for each):
//!
//! * **row 14** — is the resting stop listed by `GET /fapi/v1/openOrders`, by `GET /fapi/v1/openAlgoOrders`,
//!   or both? (`BinancePerpRest::fetch_open_orders` and the report path read both either way.)
//! * **row 18** — when a stop triggers, is the CHILD order's `ORDER_TRADE_UPDATE.o.c` the stop's
//!   `clientAlgoId`, and is `o.i` the `ALGO_UPDATE.ai`? (Measured 2026-10-19: yes to both, and the
//!   child fill arrives BEFORE `TRIGGERED`; `crates/bridges/binance/src/algo.rs`'s module doc has the
//!   answers, so no fill needs re-keying. Re-run this when the venue changes anything.)
//! * **row 20 / 26** — does the demo host behave like the live one: the OLD call answers `-4120`, the
//!   algo calls work, and what does the algo endpoint answer for an order it does not know?
//!
//! ## What it does (ETHUSDT, a minimal position, far-from-market levels)
//!
//! 0. refuses to run unless the base URL is the DEMO host and the symbol is flat with nothing resting;
//! 1. listens on the user-data stream the code really uses (`DEMO_FAPI_WS`) and records every frame;
//! 2. opens a minimal long with a market BUY (our `BinancePerpRest`);
//! 3. (a) a raw `POST /fapi/v1/algoOrder` STOP_MARKET far away — the ack shape; our `submit_order` for a second;
//! 4. (b) the `ALGO_UPDATE NEW` frame, if any; (c) `openOrders` vs `openAlgoOrders`, our confirm / reconcile;
//! 5. (d) cancel by `clientAlgoId` through our code (the non-member fallback AND the member path) and
//!    the `ALGO_UPDATE CANCELED` frame; the answers for an unknown order on both endpoints;
//! 6. (e) the OLD call: `POST /fapi/v1/order` `type=STOP_MARKET` must answer `-4120`;
//! 7. (f) a stop one tick under the market that TRIGGERS: `TRIGGERING/TRIGGERED/FINISHED`, the child
//!    `ORDER_TRADE_UPDATE`, then flat. If the market does not move in time the stop is cancelled instead;
//! 8. ALWAYS leaves the account flat: a `Drop` guard cancels every algo and regular order on the symbol
//!    and closes any position with a reduce-only market order, even when an assertion fails.
//!
//! **DEMO ONLY.** Refuses any base URL but `DEMO_FAPI_REST`. Credentials come from the smoke store
//! (`vike_secrets::smoke_directory`) and are never printed, logged or put on argv.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::family::listenkey::open_user_data_ws;
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, DEMO_FAPI_WS, PATH_EXCHANGE_INFO,
    parse_binance_perp_instruments,
};
use vike_binance::perp_user_data::{HeaderAuth, LISTENKEY_PATH};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, UserStream};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The drill's symbol: not BTCUSDT, which the lane twin and the market-maker smokes mount.
const SYMBOL: &str = "ETHUSDT";
/// Everything below must finish well inside this.
const BUDGET: Duration = Duration::from_secs(300);

// ---- the venue, raw ------------------------------------------------------------------------------

struct Venue {
    transport: UreqTransport,
    signer: BinanceHmacSigner,
}

impl Venue {
    fn new(creds: &Credentials) -> Self {
        // DEMO ONLY: the one base URL this file ever signs against.
        assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com", "demo host constant moved");
        Self {
            transport: UreqTransport::new("binance"),
            signer: BinanceHmacSigner::new(creds, now_ms),
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.transport.signed(DEMO_FAPI_REST, path, method, params, &self.signer)
    }

    fn symbol(&self) -> (&'static str, String) {
        ("symbol", SYMBOL.to_string())
    }

    /// The signed position amount on `SYMBOL` (0.0 when flat), or the venue's error.
    fn try_position(&self) -> Result<f64, VenueApiError> {
        let rows = self.call("GET", "/fapi/v2/positionRisk", &[self.symbol()])?;
        Ok(rows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("positionAmt")?.as_str()?.parse::<f64>().ok())
            .sum())
    }

    /// [`Self::try_position`] for the drill's body, where a failed read is a failed drill.
    fn position(&self) -> f64 {
        self.try_position().expect("positionRisk")
    }

    fn last_price(&self) -> f64 {
        let t = self
            .transport
            .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[self.symbol()])
            .expect("ticker/price");
        t["price"].as_str().and_then(|p| p.parse().ok()).expect("a ticker price")
    }

    fn open_coids(&self, path: &str, key: &str) -> Vec<String> {
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

/// Leaves the symbol flat and clean whatever happened (a `Drop` runs on a panicking test too).
struct LeaveFlat {
    creds: Credentials,
}

impl Drop for LeaveFlat {
    fn drop(&mut self) {
        let v = Venue::new(&self.creds);
        let _ = v.call("DELETE", "/fapi/v1/algoOpenOrders", &[v.symbol()]);
        let _ = v.call("DELETE", "/fapi/v1/allOpenOrders", &[v.symbol()]);
        // ⚠ NEVER panic in here: this runs while a failed assertion unwinds, and a second panic aborts the
        // process before the position is flattened. Flatten first (retrying the read once), report after.
        let amt = v.try_position().or_else(|_| v.try_position());
        match amt {
            Ok(amt) if amt != 0.0 => {
                let params = vec![
                    v.symbol(),
                    ("side", if amt > 0.0 { "SELL" } else { "BUY" }.to_string()),
                    ("type", "MARKET".to_string()),
                    ("quantity", format!("{}", amt.abs())),
                    ("reduceOnly", "true".to_string()),
                ];
                let r = v.call("POST", "/fapi/v1/order", &params);
                println!(
                    "CLEANUP flatten {amt}: {}",
                    r.map_or_else(|e| e.to_string(), |_| "sent".into())
                );
            }
            Ok(_) => {}
            Err(e) => {
                println!("CLEANUP could not read the {SYMBOL} position ({e}): CHECK IT BY HAND")
            }
        }
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
    fn algo(&self, coid: &str) -> Vec<(String, Value)> {
        self.of("ALGO_UPDATE")
            .into_iter()
            .filter_map(|f| {
                let o = f.get("o").or_else(|| f.get("ao"))?.clone();
                (o["caid"].as_str() == Some(coid))
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
    let auth = HeaderAuth::new(creds.api_key.expose().clone());
    let never = AtomicBool::new(false);
    let mut ws =
        match open_user_data_ws(DEMO_FAPI_REST, LISTENKEY_PATH, DEMO_FAPI_WS, "", &auth, &never) {
            OpenOutcome::Ready(ws) => ws,
            OpenOutcome::Transport(e) => panic!("user-data connect failed: {e}"),
            OpenOutcome::Auth(e) => panic!("user-data auth failed: {e}"),
            OpenOutcome::Stopped => unreachable!(),
        };
    let frames = Frames::default();
    let sink = frames.clone();
    let reader = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match ws.recv() {
                Ok(StreamMsg::Text(t)) => {
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
                Ok(StreamMsg::Ping(p)) => {
                    let _ = ws.pong(p);
                }
                Ok(StreamMsg::Other) | Err(StreamError::Timeout) => {}
                Err(StreamError::Closed(why)) => {
                    println!("FRAME-STREAM closed: {why}");
                    break;
                }
            }
        }
    });
    (frames, reader)
}

// ---- the drill -----------------------------------------------------------------------------------

fn order(coid: &str, side: i32, qty: f64, kind: OrderType, trigger: Option<f64>) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: format!("{SYMBOL}.P"),
        side,
        qty,
        order_type: kind,
        trigger_price: trigger,
        reduce_only: trigger.is_some(),
        ts: now_ms(),
        ..Default::default()
    }
}

fn accepted(events: &[Event]) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderAccepted(a) => Some(a.venue_order_id.as_deref().unwrap_or("").to_string()),
        _ => None,
    })
}

fn finding(row: &str, text: impl std::fmt::Display) {
    println!("FINDING row {row}: {text}");
}

#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO orders, flattens - run by hand (see module doc)"]
fn perp_conditional_orders_on_the_demo_algo_service() {
    vike_log::test_init();
    let started = Instant::now();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    // The owner's fence: these account ids are never driven by a drill, whatever their tier says.
    const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("DRILL account id {id:?} (default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return;
    }
    let venue = Venue::new(&creds);

    // 0. the symbol must be flat with nothing resting, or this drill touches nothing
    let (pos, regular, algo) = (
        venue.position(),
        venue.open_coids("/fapi/v1/openOrders", "clientOrderId"),
        venue.open_coids("/fapi/v1/openAlgoOrders", "clientAlgoId"),
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

    let info = venue
        .transport
        .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[venue.symbol()])
        .expect("exchangeInfo");
    let props = parse_binance_perp_instruments(&info)[SYMBOL].properties;
    let rest = BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport: UreqTransport::new("binance")
            .with_rate_gate(vike_binance::ratelimit::perp_rest_gate()),
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: props,
        leverage: 2.0,
        algo_coids: Default::default(),
    };

    // 1. the stream the code really uses
    let stop_reader = Arc::new(AtomicBool::new(false));
    let (frames, reader) = listen(&creds, stop_reader.clone());
    std::thread::sleep(Duration::from_secs(3));

    // 2. a minimal long
    let last = venue.last_price();
    let want = (props.min_notional.max(5.0) * 1.5 / last).max(props.min_qty);
    let qty = (want / props.step_size).ceil() * props.step_size;
    let tag = now_ms() % 100_000_000;
    let open = rest.submit_order(&order(&format!("vtalgoO{tag}"), 1, qty, OrderType::Market, None));
    println!(
        "OPEN market BUY {} @ ~{last}: accepted={:?}",
        format_to_step_f(qty, props.step_size),
        accepted(&open)
    );
    assert!(accepted(&open).is_some(), "the entry must be accepted: {open:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while venue.position() < qty * 0.99 {
        assert!(Instant::now() < deadline, "the entry never showed in positionRisk");
        std::thread::sleep(Duration::from_millis(500));
    }

    // 3. (a) a raw New Algo Order far below the market: the ack shape
    let tick = props.tick_size;
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
    finding(
        "15/host",
        format!(
            "ALGO_UPDATE NEW pushed on {DEMO_FAPI_WS}: {saw_new}; frame types seen: {:?}",
            frames
                .all()
                .iter()
                .map(|f| f["e"].as_str().unwrap_or("?").to_string())
                .collect::<std::collections::BTreeSet<_>>()
        ),
    );

    // (c) openOrders vs openAlgoOrders
    let in_regular = venue.open_coids("/fapi/v1/openOrders", "clientOrderId");
    let in_algo = venue.open_coids("/fapi/v1/openAlgoOrders", "clientAlgoId");
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
    // our confirm (a1 is not in the client's set: regular lookup absent -> algo fallback) and reconcile
    let c1 = rest.confirm_order(&a1, now_ms());
    println!("OURS confirm_order a1 (non-member): {c1:?}");
    assert!(accepted(&c1).is_some(), "a resting stop must confirm live, not absent: {c1:?}");
    let snap = rest.reconcile_positions().expect("reconcile_positions");
    let snap_coids: Vec<&str> = snap.open_orders.iter().map(|o| o.client_order_id()).collect();
    println!("OURS reconcile open_orders: {snap_coids:?}");
    assert!(
        snap_coids.contains(&a1.as_str()) && snap_coids.contains(&a2.as_str()),
        "the snapshot lists both stops"
    );
    let reports = vike_binance::recon_client(&creds, &format!("{SYMBOL}.P"), false)
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    println!(
        "OURS report path: {} rows, coids {:?}",
        reports.len(),
        reports.iter().map(|r| r.client_order_id.clone()).collect::<Vec<_>>()
    );
    assert!(reports.iter().any(|r| r.client_order_id.as_deref() == Some(a1.as_str())));

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
        ghost_del.as_ref().err().is_some_and(|e| vike_binance::algo::is_unknown_order(e.code)),
        "the algo DELETE of an unknown id must answer -2011/-2013 or `is_unknown_order` must learn its word: {ghost_del:?}"
    );

    assert!(rest.cancel_order(&a1).is_ok(), "non-member cancel falls back to the algo endpoint");
    assert!(rest.cancel_order(&a2).is_ok(), "member cancel goes straight to the algo endpoint");
    let canceled = frames.wait("ALGO_UPDATE CANCELED for both", Duration::from_secs(8), |f| {
        f.algo(&a1).iter().any(|(x, _)| x == "CANCELED")
            && f.algo(&a2).iter().any(|(x, _)| x == "CANCELED")
    });
    finding("15/cancel", format!("ALGO_UPDATE CANCELED seen for both: {canceled}"));
    let after = venue.open_coids("/fapi/v1/openAlgoOrders", "clientAlgoId");
    assert!(!after.contains(&a1) && !after.contains(&a2), "both stops are gone: {after:?}");
    // The stream's second ack: the captured NEW frame, through the bridge's mapper, is an OrderAccepted
    // carrying the SAME algo id the REST ack returned (so a lost ack is rescued by it).
    let new_frame = frames.algo(&a1).into_iter().find(|(x, _)| x == "NEW").map(|(_, f)| f);
    let rescued = new_frame.map(|f| {
        vike_binance::perp_mapper::map_binance_perp_opts(&f, "binance", "ETHUSDT.P", false)
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
    let replayed = |ev: &[Event], coid: &str| {
        ev.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid))
    };
    let until = Instant::now() + Duration::from_secs(8);
    let history = loop {
        let h = vike_binance::exec::perp_history_events(&rest, &format!("{SYMBOL}.P"));
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
            ("quantity", format_to_step_f(qty, props.step_size)),
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
        let now = venue.last_price();
        let trigger = ((now / tick).floor() - ticks) * tick;
        let ev = rest.submit_order(&order(&near, -1, qty, OrderType::Stop, Some(trigger)));
        println!("NEAR stop trigger {trigger} (last {now}, {ticks} ticks under): {ev:?}");
        if accepted(&ev).is_some() {
            near_events = Some(ev);
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
            while venue.position() != 0.0 && Instant::now() < flat_deadline {
                std::thread::sleep(Duration::from_millis(500));
            }
            assert_eq!(venue.position(), 0.0, "a triggered reduce-only stop flattens the position");
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
    assert_eq!(v.position(), 0.0, "the drill leaves the symbol flat");
    assert!(v.open_coids("/fapi/v1/openAlgoOrders", "clientAlgoId").is_empty());
    assert!(v.open_coids("/fapi/v1/openOrders", "clientOrderId").is_empty());
    println!("DRILL done in {:?}: flat, nothing resting", started.elapsed());
}
