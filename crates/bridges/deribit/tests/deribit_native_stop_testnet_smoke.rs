//! Deribit NATIVE STOP LIVE TESTNET drill: the stop kinds `DeribitRest` builds (`order_params_on`),
//! driven through the PRODUCTION client (`DeribitExecutionClient`: the order socket, the
//! `user.trades` pump, the audit-A3 resync, the submit fence) against the real testnet, where the
//! offline fake (`tests/offline/native_stops.rs`) can only say what we believe.
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box server/deribit-native-stops env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_native_stop_testnet_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, TESTNET ONLY: every socket is `TESTNET_WS` and [`only_testnet`] refuses to run
//! on anything else. Instrument `BTC-PERPETUAL` (INVERSE: the amount is US dollars, one lot is 10).
//! ⚠ NOT leave-flat by construction: step 1 opens a 10 USD LONG with a market order that FILLS, and
//! step 3 closes it with a stop that really triggers; a `Drop` guard cancels our labels (all start
//! with [`PREFIX`], never `cancel_all`) and flattens whatever position this run added, so a failed
//! assertion or a panic still ends flat. A run killed from outside (SIGKILL) skips destructors:
//! cancel `n0stp*` and close the position on testnet by hand.
//!
//! Double-gated like every `*_smoke.rs`: network + the default DEMO account's keys through
//! `vike_secrets::smoke_directory`; absent credentials print `SKIP` and return. The running node
//! holds a Deribit demo session of its own (the venue allows several); a reconcile pass of the
//! node that lands while one of these rests may HOLD it as an `UnknownOrder` row, so read the
//! node's `recon ls` after the run.
//!
//! What it proves, in order:
//!   1. a market BUY through the client opens the position (the stop below needs one to reduce);
//!   2. a FAR reduce-only sell stop is `preflight_order`-clean, ACCEPTED, reads back from the venue
//!      as `untriggered` / `stop_market` / `reduce_only` (by label, by instrument and through the
//!      real `DeribitReconClient`, which lists it as a live `Stop` carrying our coid), and the
//!      client's cancel ends it;
//!   2. (b) a stop-limit, a take-profit (trigger on the MARK series) and a stop on the INDEX series
//!      are accepted as `stop_limit` / `take_market` / `stop_market` with the series asked for, read
//!      back `untriggered` and cancelled;
//!   3. a TIGHT stop (one tick under the last price) TRIGGERS on the venue and closes the position:
//!      the events are `OrderSubmitted`, `OrderAccepted`, the stop's `Fill`, then `OrderFilled`
//!      (printed with their times), the position is back where it started and nothing rests. The
//!      stop's own venue id ceases to exist when it triggers (a NEW market order executes and
//!      carries the stop's `label`), which is why the fill joins by coid;
//!   4. a stop on the wrong side of the market is refused by the venue (code 10034
//!      `trigger_price_too_high`) and surfaces as a terminal `OrderRejected` naming the cause;
//!   4. (b) (A27) a stop CANCELLED FROM ANOTHER SESSION (`private/cancel_by_label` on the drill's second
//!      socket, which the client under test never sees) is told to the node as `OrderCanceled`: the
//!      client's pump subscribes `user.orders.any.any.raw` and `map_deribit_order_row` ends the stop;
//!   5. (A27, judged) a reduce-only stop that fires with NO position to reduce leaves the venue's open
//!      orders, and the node is told it ended (`OrderCanceled`) instead of keeping it `Accepted`.
//!
//! A27's evidence is the RAW frames: a second socket subscribes `user.orders.any.any.raw` for the whole
//! run and the drill prints every frame about its own labels as `CAPTURED user.orders ...`, also when an
//! assertion fails. The row shapes `tests/offline/stop_events.rs` builds are the DOCUMENTED ones; what
//! this prints is what replaces them (and what to read first when step 4b or 5 fails: a mapper that
//! ignores the venue's real shape is the finding, not the test).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tungstenite::Message;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_deribit::rpc::parse_response;
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_deribit::ws_auth::{build_client_credentials_auth, build_private_subscribe};
use vike_deribit::{DeribitExecutionClient, DeribitReconClient, fetch_deribit_properties};
use vike_exec::recon::ReconClient;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderStatus, OrderType, TriggerBy, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// Every label of this drill starts with it, and nothing else on the account does.
const PREFIX: &str = "n0stp";
const INST: &str = "BTC-PERPETUAL";
/// `BTC-PERPETUAL`'s tick, and the lot (and minimum amount) in US dollars.
const TICK: f64 = 0.5;
const LOT: f64 = 10.0;
/// How many times the tight stop is re-placed when the market does not touch it in [`WAIT_TRIGGER`].
const ATTEMPTS: usize = 8;
const WAIT_TRIGGER: Duration = Duration::from_secs(40);

/// Refuses to go anywhere but the testnet (the second lock on the door: every socket below is
/// spelled `TESTNET_WS` / `TESTNET_REST`, which the adapter has no mainnet twin of).
fn only_testnet() {
    assert!(
        TESTNET_WS == "wss://test.deribit.com/ws/api/v2"
            && TESTNET_REST == "https://test.deribit.com",
        "REFUSING TO RUN: the resolved endpoint is not the Deribit testnet ({TESTNET_WS}, {TESTNET_REST})"
    );
}

/// The double-gate every `*_smoke.rs` in this crate uses.
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "deribit");
    if creds.is_none() {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

fn public(path: &str, params: &[(&str, String)]) -> Value {
    only_testnet();
    let r = UreqTransport::new("deribit").public(TESTNET_REST, path, params);
    r.unwrap_or_else(|e| panic!("testnet {path}: {} ({})", e.msg, e.code))["result"].clone()
}

/// The last traded price of the instrument.
fn last_price() -> f64 {
    let t = public("/api/v2/public/ticker", &[("instrument_name", INST.to_string())]);
    let last = t["last_price"].as_f64().expect("a last price");
    println!("ticker: last={last} mark={} index={}", t["mark_price"], t["index_price"]);
    last
}

fn snap(x: f64) -> f64 {
    (x / TICK).round() * TICK
}

/// One authed testnet socket for the READS and the cleanup (the client under test owns its own).
struct Conn {
    t: DeribitOrderTransport,
}

impl Conn {
    fn open(key: &str, secret: &str) -> Result<Self, String> {
        only_testnet();
        let mut t = DeribitOrderTransport::new(TESTNET_WS, key, secret, None);
        t.connect().map_err(|e| e.0)?;
        Ok(Self { t })
    }

    fn send(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        let env = self.t.call(method, params)?;
        match env.get("error").filter(|e| !e.is_null()) {
            Some(e) => Err(VenueApiError {
                code: e["code"].as_i64().unwrap_or(-1),
                msg: e["message"].as_str().unwrap_or("").to_string(),
            }),
            None => Ok(env["result"].clone()),
        }
    }

    fn array(&mut self, method: &str, params: &Value) -> Vec<Value> {
        let v = self.send(method, params).unwrap_or_else(|e| panic!("{method}: {}", e.msg));
        v.as_array().cloned().unwrap_or_default()
    }

    /// The net `BTC-PERPETUAL` position (US dollars, signed).
    fn position(&mut self) -> f64 {
        self.array("private/get_positions", &json!({"currency": "BTC", "kind": "future"}))
            .iter()
            .find(|p| p["instrument_name"] == INST)
            .and_then(|p| p["size"].as_f64())
            .unwrap_or(0.0)
    }

    /// Every open order of the account whose label starts with [`PREFIX`].
    fn open_ours(&mut self) -> Vec<Value> {
        self.array("private/get_open_orders", &json!({}))
            .into_iter()
            .filter(|o| o["label"].as_str().is_some_and(|l| l.starts_with(PREFIX)))
            .collect()
    }
}

/// Cancels, by label, whatever rests under [`PREFIX`] (OUR labels only), then returns the position to
/// `start` with a plain market order. The `Drop` guard's body, and also what the drill ends on.
fn sweep(key: &str, secret: &str, start: f64) {
    let Ok(mut c) = Conn::open(key, secret) else {
        println!("CLEANUP FAILED: no testnet socket; cancel `{PREFIX}*` and flatten by hand");
        return;
    };
    for o in c.open_ours() {
        let label = o["label"].as_str().unwrap_or_default().to_string();
        let r = c.send("private/cancel_by_label", &json!({ "label": label }));
        println!("cleanup cancel_by_label {label}: {r:?}");
    }
    let delta = c.position() - start;
    if delta.abs() > 1e-9 {
        let method = if delta > 0.0 { "private/sell" } else { "private/buy" };
        let r = c.send(
            method,
            &json!({"instrument_name": INST, "amount": delta.abs(), "type": "market",
                    "label": format!("{PREFIX}99")}),
        );
        println!("cleanup flatten {delta}: {:?}", r.map(|v| v["order"]["order_state"].clone()));
    }
}

struct Guard {
    key: String,
    secret: String,
    start: f64,
}

impl Drop for Guard {
    fn drop(&mut self) {
        sweep(&self.key, &self.secret, self.start);
    }
}

/// A RAW `user.orders.any.any.raw` capture on a socket of its own (A27's evidence: the frames the
/// mapper must read, VERBATIM). It authenticates and subscribes on a worker thread and keeps every
/// notification of that channel; dropping it stops the thread and PRINTS the frames about this
/// drill's own labels, so a failed assertion still leaves them in the output.
struct Capture {
    frames: Arc<Mutex<Vec<(u128, String)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    fn start(key: &str, secret: &str) -> Self {
        only_testnet();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let (stop, ready) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (key, secret) = (key.to_string(), secret.to_string());
        let (log, flag, up) = (Arc::clone(&frames), Arc::clone(&stop), Arc::clone(&ready));
        let t0 = Instant::now();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = tungstenite::connect(TESTNET_WS).expect("capture WS connect");
            configure_ws_stream(&socket, Duration::from_millis(500));
            let say = |socket: &mut tungstenite::WebSocket<_>, frame: Value, id: i64| {
                socket.send(Message::Text(frame.to_string().into())).expect("capture WS send");
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    let Ok(Message::Text(raw)) = socket.read() else { continue };
                    let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else { continue };
                    let (fid, _result, error) = parse_response(&frame);
                    if fid == Some(id) {
                        assert!(error.is_none_or(|e| e.is_null()), "capture WS ack {id}: {frame}");
                        return;
                    }
                }
                panic!("capture WS ack {id} timed out");
            };
            say(&mut socket, build_client_credentials_auth(&key, &secret, None, 1), 1);
            let channels = ["user.orders.any.any.raw".to_string()];
            say(&mut socket, build_private_subscribe(&channels, 2), 2);
            up.store(true, Ordering::SeqCst);
            while !flag.load(Ordering::Relaxed) {
                match socket.read() {
                    Ok(Message::Text(raw)) => log
                        .lock()
                        .unwrap()
                        .push((t0.elapsed().as_millis(), raw.as_str().to_string())),
                    Ok(Message::Ping(p)) => {
                        let _ = socket.send(Message::Pong(p));
                    }
                    Ok(_) => {}
                    Err(e) if is_timeout(&e) => {}
                    Err(e) => {
                        println!("capture WS read ended: {e}");
                        return;
                    }
                }
            }
            let _ = socket.close(None);
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(ready.load(Ordering::SeqCst), "the user.orders capture socket never came up");
        Self { frames, stop, handle: Some(handle) }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        std::thread::sleep(Duration::from_secs(2)); // the last frames of the run
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let frames = self.frames.lock().unwrap();
        let ours: Vec<_> = frames.iter().filter(|(_, f)| f.contains(PREFIX)).collect();
        println!(
            "== CAPTURED user.orders (RAW): {} frame(s) about {PREFIX}* of {} seen",
            ours.len(),
            frames.len()
        );
        for (ms, frame) in ours {
            println!("CAPTURED user.orders {ms:>7}ms {frame}");
        }
    }
}

/// The events the client pushed into its ingest lane, printed as they arrive with their time.
struct Events {
    /// One non-blocking read of the ingest lane.
    next: Box<dyn FnMut() -> Option<Ingest>>,
    t0: Instant,
    seen: Vec<(String, String)>,
}

fn word(e: &Event) -> (String, String) {
    match e {
        Event::OrderSubmitted(x) => (x.client_order_id.clone(), "OrderSubmitted".to_string()),
        Event::OrderAccepted(x) => (
            x.client_order_id.clone(),
            format!("OrderAccepted venue_order_id={:?}", x.venue_order_id),
        ),
        Event::OrderRejected(x) => {
            (x.client_order_id.clone(), format!("OrderRejected reason={:?}", x.reason))
        }
        Event::OrderCanceled(x) => {
            (x.client_order_id.clone(), format!("OrderCanceled reason={:?}", x.reason))
        }
        Event::OrderTriggered(x) => (x.client_order_id.clone(), "OrderTriggered".to_string()),
        Event::OrderFilled(x) => (x.client_order_id.clone(), "OrderFilled".to_string()),
        Event::OrderPartiallyFilled(x) => {
            (x.client_order_id.clone(), "OrderPartiallyFilled".to_string())
        }
        Event::Fill(f) => (
            f.client_order_id.clone(),
            format!(
                "Fill trade_id={} side={} qty={} px={}",
                f.trade_id, f.side, f.last_qty, f.last_px
            ),
        ),
        other => (String::new(), format!("{other:?}")),
    }
}

impl Events {
    /// Drain for up to `secs` until an event of `coid` starts with `want`; every event is printed.
    fn wait_for(&mut self, coid: &str, want: &str, secs: Duration) -> bool {
        let deadline = Instant::now() + secs;
        while Instant::now() < deadline {
            match (self.next)() {
                Some(Ingest::Event(e)) => {
                    let (c, w) = word(&e);
                    if !c.is_empty() {
                        println!("  <<{:>6}ms {c}: {w}", self.t0.elapsed().as_millis());
                        self.seen.push((c.clone(), w.clone()));
                        if c == coid && w.starts_with(want) {
                            return true;
                        }
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        false
    }

    /// The words seen for `coid`, in order.
    fn words_of(&self, coid: &str) -> Vec<&str> {
        self.seen.iter().filter(|(c, _)| c == coid).map(|(_, w)| w.as_str()).collect()
    }
}

fn order(
    coid: &str,
    side: i32,
    kind: OrderType,
    trigger: Option<f64>,
    reduce: bool,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: INST.to_string(),
        side,
        qty: LOT,
        order_type: kind,
        trigger_price: trigger,
        reduce_only: reduce,
        ts: now_ms(),
        ..Default::default()
    }
}

#[test]
#[ignore = "places TESTNET orders that FILL (opens and closes a 10 USD position); run by hand"]
fn deribit_native_stops_on_the_testnet() {
    vike_log::test_init();
    only_testnet();
    let Some(creds) = load_demo_creds() else { return };
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    let mut raw = Conn::open(&key, &secret).expect("testnet order-WS auth");
    let start = raw.position();
    if start != 0.0 || !raw.open_ours().is_empty() {
        println!(
            "SKIP: the testnet account holds a {INST} position ({start}) or `{PREFIX}*` orders"
        );
        return;
    }
    let _guard = Guard { key: key.clone(), secret: secret.clone(), start };
    // Declared AFTER the guard, so it drops first: the raw `user.orders` frames are printed, then the
    // account is swept.
    let _capture = Capture::start(&key, &secret);

    let grid = fetch_deribit_properties(&creds, INST).expect("the instrument grid");
    assert_eq!((grid.tick_size, grid.step_size), (TICK, LOT), "BTC-PERPETUAL's grid moved");
    let (tx, mut rx) = event_channel(512);
    let mut client = DeribitExecutionClient::spawn(creds, INST.to_string(), grid, tx);
    let recon = DeribitReconClient::connect(&load_demo_creds().expect("creds"), INST)
        .expect("the recon client's own authed socket");
    let mut ev =
        Events { next: Box::new(move || rx.try_recv().ok()), t0: Instant::now(), seen: Vec::new() };
    std::thread::sleep(Duration::from_secs(4)); // the pump's auth + subscribe settle

    // 1. open the smallest long
    println!("== 1. open +{LOT}");
    // The venue keeps a cancelled or filled order under its label for ~31 minutes, so a label is
    // unique to the RUN (a later run, or the probe before it, must not read this run's rows).
    let run = format!("{PREFIX}{:05}", now_ms() % 100_000);
    let lab = |n: &str| format!("{run}{n}");
    let buy = lab("01");
    client.submit(&order(&buy, 1, OrderType::Market, None, false));
    assert!(ev.wait_for(&buy, "OrderAccepted", Duration::from_secs(15)), "{buy} never accepted");
    assert!(ev.wait_for(&buy, "OrderFilled", Duration::from_secs(30)), "{buy} never filled");
    assert_eq!(raw.position(), LOT, "the market buy opened one lot");

    // 2. a far reduce-only stop: accepted, untriggered at the venue, listed by the recon client,
    //    cancelled by the client
    println!("== 2. a far reduce-only stop");
    let far = lab("02");
    let far_stop = order(&far, -1, OrderType::Stop, Some(snap(last_price() * 0.8)), true);
    vike_venues::preflight_order(&far_stop)
        .expect("the capability row lets a deribit stop through");
    client.submit(&far_stop);
    assert!(ev.wait_for(&far, "OrderAccepted", Duration::from_secs(15)), "{far} never accepted");
    let rows =
        raw.array("private/get_order_state_by_label", &json!({"currency": "BTC", "label": far}));
    assert_eq!(rows.len(), 1, "the label finds the stop: {rows:?}");
    let row = &rows[0];
    println!("  venue row: {row}");
    assert_eq!(row["order_state"], "untriggered");
    assert_eq!(row["order_type"], "stop_market");
    assert_eq!(row["reduce_only"], true);
    assert_eq!(row["trigger"], "last_price", "no trigger_by asked: the default series");
    assert_eq!(row["direction"], "sell");
    let by_instrument = raw.array(
        "private/get_open_orders_by_instrument",
        &json!({"instrument_name": INST, "type": "all"}),
    );
    assert!(
        by_instrument.iter().any(|o| o["label"] == far.as_str()),
        "the venue's own open-order read (type=all, what the recon client asks) lists the stop"
    );
    let reports = recon.fetch_order_status_reports(0).expect("the recon read");
    let report = reports
        .iter()
        .find(|r| r.client_order_id.as_deref() == Some(far.as_str()))
        .unwrap_or_else(|| panic!("the restart reconcile must see the resting stop: {reports:?}"));
    assert_eq!(report.order_type, OrderType::Stop);
    assert_eq!(report.status, OrderStatus::Accepted);
    assert_eq!(report.side, -1);
    client.cancel(&far);
    assert!(
        ev.wait_for(&far, "OrderCanceled", Duration::from_secs(15)),
        "{far} cancel not evented"
    );
    assert!(
        raw.open_ours().iter().all(|o| o["label"] != far.as_str()),
        "the cancelled stop no longer rests"
    );

    // 2b. the other kinds and the other trigger series rest too, each read back and cancelled
    println!("== 2b. stop-limit, take-profit, mark and index triggers");
    let reference = last_price();
    struct Kind {
        coid: String,
        kind: OrderType,
        trigger: f64,
        limit: Option<f64>,
        by: Option<TriggerBy>,
        wire: &'static str,
        series: &'static str,
    }
    let kinds = [
        Kind {
            coid: lab("03"),
            kind: OrderType::StopLimit,
            trigger: reference * 0.8,
            limit: Some(reference * 0.79),
            by: None,
            wire: "stop_limit",
            series: "last_price",
        },
        Kind {
            coid: lab("04"),
            kind: OrderType::TakeProfit,
            trigger: reference * 1.2,
            limit: None,
            by: Some(TriggerBy::Mark),
            wire: "take_market",
            series: "mark_price",
        },
        Kind {
            coid: lab("05"),
            kind: OrderType::Stop,
            trigger: reference * 0.8,
            limit: None,
            by: Some(TriggerBy::Index),
            wire: "stop_market",
            series: "index_price",
        },
    ];
    for Kind { coid, kind, trigger, limit, by, wire, series } in kinds {
        let coid = coid.as_str();
        let mut req = order(coid, -1, kind, Some(snap(trigger)), true);
        req.price = limit.map(snap);
        req.trigger_by = by;
        vike_venues::preflight_order(&req).expect("the capability row lets it through");
        client.submit(&req);
        assert!(ev.wait_for(coid, "OrderAccepted", Duration::from_secs(15)), "{coid} not accepted");
        let rows = raw
            .array("private/get_order_state_by_label", &json!({"currency": "BTC", "label": coid}));
        assert_eq!(rows.len(), 1, "{coid}: {rows:?}");
        assert_eq!(rows[0]["order_type"], wire, "{coid}");
        assert_eq!(rows[0]["trigger"], series, "{coid}");
        assert_eq!(rows[0]["order_state"], "untriggered", "{coid}");
        client.cancel(coid);
        assert!(
            ev.wait_for(coid, "OrderCanceled", Duration::from_secs(15)),
            "{coid} not cancelled"
        );
    }
    assert!(raw.open_ours().is_empty(), "everything above was cancelled");

    // 3. a tight stop that really triggers and closes the position
    println!("== 3. a tight stop that triggers");
    let mut closed = None;
    for attempt in 1..=ATTEMPTS {
        let coid = lab(&format!("1{attempt}"));
        let trigger = snap(last_price()) - TICK;
        client.submit(&order(&coid, -1, OrderType::Stop, Some(trigger), true));
        assert!(
            ev.wait_for(&coid, "OrderAccepted", Duration::from_secs(15)),
            "{coid} never accepted"
        );
        if ev.wait_for(&coid, "OrderFilled", WAIT_TRIGGER) {
            closed = Some(coid);
            break;
        }
        println!("  {coid}: the market did not touch {trigger} in {WAIT_TRIGGER:?}; re-placing");
        client.cancel(&coid);
        ev.wait_for(&coid, "OrderCanceled", Duration::from_secs(15));
    }
    let closed = closed.expect("no tight stop was triggered by the testnet market");
    println!("  events of {closed}: {:?}", ev.words_of(&closed));
    let words = ev.words_of(&closed);
    assert!(words[0].starts_with("OrderSubmitted"), "{words:?}");
    assert!(words[1].starts_with("OrderAccepted"), "{words:?}");
    assert!(
        words.iter().any(|w| w.starts_with("Fill ")),
        "the stop's fill carries its coid: {words:?}"
    );
    assert!(words.last().is_some_and(|w| w.starts_with("OrderFilled")), "{words:?}");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(raw.position(), 0.0, "the stop closed the position");
    assert!(raw.open_ours().is_empty(), "nothing rests after the trigger");

    // 4. the wrong side of the market is refused by the venue and surfaces as a terminal reject
    println!("== 4. a stop on the wrong side");
    let wrong = lab("06");
    client.submit(&order(&wrong, -1, OrderType::Stop, Some(snap(last_price() * 1.2)), false));
    assert!(ev.wait_for(&wrong, "OrderRejected", Duration::from_secs(15)), "{wrong} not refused");
    let words = ev.words_of(&wrong);
    assert!(
        words.iter().any(|w| w.contains("trigger_price_too_high")),
        "the venue's reason is carried: {words:?}"
    );
    assert!(raw.open_ours().is_empty(), "a refused stop rests nowhere");

    // 4b. (A27) a stop cancelled from ANOTHER session: the drill's `raw` socket is a session the client
    //     under test never sees, so the only way the node can learn of the end is the pump's
    //     `user.orders` subscription. A reduce-only stop is accepted with no position (measured).
    println!("== 4b. a stop cancelled from another session");
    let elsewhere = lab("07");
    client.submit(&order(&elsewhere, -1, OrderType::Stop, Some(snap(last_price() * 0.8)), true));
    assert!(
        ev.wait_for(&elsewhere, "OrderAccepted", Duration::from_secs(15)),
        "{elsewhere} not accepted"
    );
    let cancelled = raw.send("private/cancel_by_label", &json!({ "label": elsewhere.as_str() }));
    println!("  cancel_by_label from the drill's own session: {cancelled:?}");
    let told = ev.wait_for(&elsewhere, "OrderCanceled", Duration::from_secs(15));
    println!("  events the node got for {elsewhere}: {:?}", ev.words_of(&elsewhere));
    assert!(
        told,
        "{elsewhere} was cancelled by another session and the node was never told (read the CAPTURED \
         user.orders frames: a row the mapper does not read is the finding)"
    );
    assert!(raw.open_ours().is_empty(), "the stop cancelled elsewhere no longer rests");

    // 5. a reduce-only stop whose position is GONE: the venue accepts it, holds it, and when it
    //    triggers it executes nothing. JUDGED (A27): the node must be told it ended, instead of keeping
    //    it ACCEPTED; the venue's own rows (history, label lookup) and the captured frames are printed.
    println!("== 5. a reduce-only stop with no position to reduce");
    let mut fired = false;
    for attempt in 1..=ATTEMPTS {
        let coid = lab(&format!("2{attempt}"));
        client.submit(&order(&coid, -1, OrderType::Stop, Some(snap(last_price()) - TICK), true));
        assert!(
            ev.wait_for(&coid, "OrderAccepted", Duration::from_secs(15)),
            "{coid} not accepted"
        );
        let t = Instant::now();
        let mut vanished = false;
        while t.elapsed() < WAIT_TRIGGER {
            std::thread::sleep(Duration::from_millis(500));
            if raw.open_ours().is_empty() {
                vanished = true;
                break;
            }
        }
        if !vanished {
            client.cancel(&coid);
            ev.wait_for(&coid, "OrderCanceled", Duration::from_secs(15));
            continue;
        }
        println!("  {coid} left the venue's open orders after {:?}", t.elapsed());
        std::thread::sleep(Duration::from_secs(2));
        let by_label = raw
            .send("private/get_order_state_by_label", &json!({"currency": "BTC", "label": coid}));
        println!("  by label: {by_label:?}");
        let hist = raw.array(
            "private/get_order_history_by_instrument",
            &json!({"instrument_name": INST, "count": 4, "include_old": true, "include_unfilled": true}),
        );
        for o in hist.iter().filter(|o| o["label"] == coid.as_str()) {
            println!(
                "  history: id={} state={} type={} reject={} cancel={} filled={}",
                o["order_id"],
                o["order_state"],
                o["order_type"],
                o["reject_reason"],
                o["cancel_reason"],
                o["filled_amount"]
            );
        }
        let told = ev.wait_for(&coid, "OrderCanceled", Duration::from_secs(20));
        let words = ev.words_of(&coid);
        println!("  events the node got for {coid}: {words:?}");
        assert!(
            told,
            "{coid} left the venue's open orders without a fill and the node was never told it \
             ended (read the CAPTURED user.orders frames and the history rows above): {words:?}"
        );
        assert!(
            !words.iter().any(|w| w.starts_with("Fill ") || w.starts_with("OrderFilled")),
            "a reduce-only stop on a flat position filled nothing: {words:?}"
        );
        fired = true;
        break;
    }
    assert!(fired, "no reduce-only stop fired in {ATTEMPTS} attempts: step 5 was not exercised");
    assert_eq!(raw.position(), 0.0, "a reduce-only stop never opens a position");

    // 6. the restart reconcile sees nothing of ours
    let reports = recon.fetch_order_status_reports(0).expect("the recon read");
    assert!(
        reports
            .iter()
            .all(|r| !r.client_order_id.as_deref().is_some_and(|c| c.starts_with(PREFIX))),
        "no resting order of ours: {reports:?}"
    );
    assert_eq!(raw.position(), 0.0, "flat");
    client.detach();
    println!("END: flat, nothing resting");
}
