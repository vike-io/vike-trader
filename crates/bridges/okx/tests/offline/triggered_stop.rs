//! **A STOP that FIRES on OKX ends on the stop's own order**, replayed from frames CAPTURED on the demo
//! account on 2026-10-24 (`tests/fixtures/algo/`, the account id stripped).
//!
//! A stop is an algo order. When it fires OKX places a CHILD order; the child's rows (`orders` channel,
//! `orders-history`, `order`) carry the venue's own `clOrdId` (`O4038740198576748544`) and the stop's
//! client id, OURS, in `algoClOrdId`. The bridge read `clOrdId` alone, so on a v0.1.86 node the child's
//! fill booked under an id the core never minted: the stop stayed `ACCEPTED` for good (a held
//! `OrphanLocalOrder` row named it) and, after a restart, the fill came back from reconcile as a held
//! `MissingFill` under the venue's id. `trade/fills` rows carry no algo field at all (the child's
//! `ordId` is all they share with the stop), so reconcile files them through `orders-algo-history`.
//!
//! These cases use only what the crate exposed BEFORE the algo socket existed, so they fail on the old
//! tree for the stated reason: `algo_channel.rs` holds the rest.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::recon::ReconClient;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate};
use vike_model::events::Event;
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_okx::OkxReconClient;
use vike_okx::event_mapper::map_okx_perp;
use vike_okx::history::map_okx_history;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

/// The stop's client id (ours) and the venue's own id of the order it spawned.
pub const STOP: &str = "b7okxtsb";
pub const CHILD_CLORDID: &str = "O4038740198576748544";
pub const CHILD_ORDID: &str = "4038740198699216897";
pub const SYMBOL: &str = "BTC-USDT-SWAP";
/// BTC-USDT-SWAP's `ctVal`: the frames' `sz` of 0.01 contracts is the 0.0001 BTC the drill traded.
pub const CT_VAL: f64 = 0.01;

/// A captured frame / REST envelope, from `tests/fixtures/algo/`.
macro_rules! fixture {
    ($name:literal) => {
        serde_json::from_str::<serde_json::Value>(include_str!(concat!("../fixtures/algo/", $name)))
            .expect(concat!($name, " is JSON"))
    };
}
pub(crate) use fixture;

/// `"Fill:b7okxtsb"`-shaped, in order.
pub fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => format!("Fill:{}", f.client_order_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
            Event::OrderPartiallyFilled(w) => format!("OrderPartiallyFilled:{}", w.client_order_id),
            Event::OrderAccepted(w) => format!("OrderAccepted:{}", w.client_order_id),
            Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
            Event::OrderTriggered(w) => format!("OrderTriggered:{}", w.client_order_id),
            other => format!("other:{other:?}"),
        })
        .collect()
}

/// An engine holding the drill's stop as it stands after the REST ack: `ACCEPTED`, resting.
pub fn engine_with_resting_stop() -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        SYMBOL,
    );
    let mut stop = ManagedOrder::new(OrderRequest {
        client_order_id: STOP.to_string(),
        venue: "okx".to_string(),
        symbol: SYMBOL.to_string(),
        side: -1,
        qty: 0.0001,
        order_type: OrderType::Stop,
        trigger_price: Some(80211.0),
        reduce_only: true,
        ..Default::default()
    });
    stop.status = OrderStatus::Accepted;
    engine.registry.insert(STOP.to_string(), stop);
    engine
}

/// Fold `frames` (private-socket `orders` frames) through the real mapper and the real engine.
pub fn fold(engine: &mut ExecutionEngine<RecordingClient>, events: &[Event]) {
    let mut outbox = Outbox::default();
    for ev in events {
        engine.on_event(ev, &mut outbox);
    }
}

#[test]
fn the_fired_stops_child_fill_books_on_the_stop_not_on_the_venues_child_id() {
    let events = map_okx_perp(&fixture!("ws_orders_child_filled.json"), "okx", SYMBOL, CT_VAL);
    assert_eq!(
        kinds(&events),
        [format!("Fill:{STOP}"), format!("OrderFilled:{STOP}")],
        "the fill and its wrap carry the STOP's id (`algoClOrdId`), never the child's {CHILD_CLORDID}"
    );
    let Event::Fill(fill) = &events[0] else { panic!("the first event is the bare fill") };
    assert_eq!(fill.trade_id.to_string(), "4540556207");
    assert_eq!(fill.last_px, 80200.0);
    assert!((fill.last_qty - 0.0001).abs() < 1e-12, "0.01 contracts x ctVal: {}", fill.last_qty);
    assert_eq!(fill.side, -1);
}

/// The child's own `live` row is not the STOP being accepted: the REST ack did that, the child's `ordId`
/// is not the stop's venue id, and the FSM allows `OrderAccepted` from `Submitted` only.
#[test]
fn the_child_going_live_is_not_a_second_acceptance_of_the_stop() {
    let events = map_okx_perp(&fixture!("ws_orders_child_live.json"), "okx", SYMBOL, CT_VAL);
    assert_eq!(kinds(&events), Vec::<String>::new());
}

/// A child the venue CANCELS without a fill (the second of two reduce-only stops that fired together:
/// the position was already gone, `cancelSource` 22) ends the stop it came from.
#[test]
fn a_child_the_venue_cancels_ends_its_stop() {
    let events = map_okx_perp(&fixture!("ws_orders_child_canceled.json"), "okx", SYMBOL, CT_VAL);
    assert_eq!(kinds(&events), ["OrderCanceled:b7okxtsi"]);
}

/// The control: an ORDINARY order has no `algoClOrdId`, so its rows are filed under `clOrdId` as ever.
#[test]
fn an_ordinary_orders_row_is_still_filed_under_its_clordid() {
    let events = map_okx_perp(&fixture!("ws_orders_ordinary_filled.json"), "okx", SYMBOL, CT_VAL);
    assert_eq!(kinds(&events), ["Fill:b7okxtsa", "OrderFilled:b7okxtsa"]);
}

/// **THE DEFECT, end to end.** The measured pair of private-socket frames (the child `live`, then
/// `filled`) folded into an engine that holds the stop `ACCEPTED`: on v0.1.86 the registry kept the
/// stop `ACCEPTED` for good (the fill booked on an unknown id).
#[test]
fn a_fired_stop_ends_filled_not_accepted_for_good() {
    let mut engine = engine_with_resting_stop();
    let mut events = map_okx_perp(&fixture!("ws_orders_child_live.json"), "okx", SYMBOL, CT_VAL);
    events.extend(map_okx_perp(&fixture!("ws_orders_child_filled.json"), "okx", SYMBOL, CT_VAL));
    fold(&mut engine, &events);
    assert_eq!(engine.registry[STOP].status, OrderStatus::Filled);
    let position: f64 = engine.local_view().positions.iter().map(|(_, q)| *q).sum();
    assert!((position + 0.0001).abs() < 1e-9, "the sell moved the position: {position}");
}

// ---- reconcile: `trade/fills` carries no algo field -------------------------------------------------

/// `(path tail, params)` of every request the scripted venue was asked, in order.
type Asked = Arc<Mutex<Vec<(String, Vec<(String, Value)>)>>>;

/// Answers `fills` and `orders-algo-history` from scripted values and records every path it is asked.
struct Venue {
    fills: Value,
    algo_history: Result<Value, VenueApiError>,
    asked: Asked,
}

impl OkxTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path).to_string();
        let params = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.asked.lock().unwrap().push((tail.clone(), params));
        match tail.as_str() {
            "fills" => Ok(self.fills.clone()),
            "orders-algo-history" => self.algo_history.clone(),
            other => panic!("a fill read asks nothing but fills and the stop history: {other}"),
        }
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("the recon client must use the SIGNED lane, not public ({path})");
    }
}

fn recon(
    fills: Value,
    algo_history: Result<Value, VenueApiError>,
) -> (OkxReconClient<Venue>, Asked) {
    let asked: Asked = Arc::default();
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        Venue { fills, algo_history, asked: Arc::clone(&asked) },
        "https://unused.invalid",
        SYMBOL,
        CT_VAL,
    );
    (client, asked)
}

/// The fills the drill left: the stop's CHILD (`ordId` ...897, `clOrdId` `O4038...`) and the entry.
fn fills_envelope() -> Value {
    fixture!("rest_fills.json")
}

#[test]
fn reconcile_files_a_fired_stops_fill_under_the_stops_id() {
    let (client, asked) =
        recon(fills_envelope(), Ok(fixture!("rest_orders_algo_history_effective.json")));
    let fills = client.fetch_fill_reports(0).expect("fills");
    let by_order: Vec<(&str, Option<&str>)> =
        fills.iter().map(|f| (f.venue_order_id.as_str(), f.client_order_id.as_deref())).collect();
    assert_eq!(
        by_order,
        [(CHILD_ORDID, Some(STOP)), ("4038738332804362241", Some("b7okxtsa"))],
        "the child's fill is the stop's (not {CHILD_CLORDID}); the entry's own id is untouched"
    );
    let asked = asked.lock().unwrap();
    let history = asked.iter().find(|(tail, _)| tail == "orders-algo-history").expect("asked");
    let param = |k: &str| history.1.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    assert_eq!(param("ordType"), Some(json!("conditional")));
    assert_eq!(param("state"), Some(json!("effective")), "the stops that FIRED");
    assert_eq!(param("instId"), Some(json!(SYMBOL)));
}

/// A stop history that cannot be read FAILS the fill read (the pass is not delivered), exactly as the
/// open-stop read beside it does: with okx's claim made, a fired stop's fill handed over under the
/// venue's child id is no proof, and the stop would be read as gone after the quiet passes. How many
/// passes in a row, and the way back to a good read, are `claim_restart.rs`'s.
#[test]
fn a_failed_stop_history_read_fails_the_fill_read() {
    let down = Err(VenueApiError { code: 50011, msg: "rate limit".into() });
    let (client, _) = recon(fills_envelope(), down);
    let why = client.fetch_fill_reports(0).expect_err("fills that lack the stop's names");
    assert!(why.contains("orders-algo-history"), "the error names the stop history: {why}");
}

/// No fills in the window, no extra read: the common pass costs what it always did.
#[test]
fn a_pass_with_no_fills_reads_no_stop_history() {
    let (client, asked) = recon(json!({"code": "0", "msg": "", "data": []}), Ok(json!({})));
    assert!(client.fetch_fill_reports(0).expect("fills").is_empty());
    let tails: Vec<String> = asked.lock().unwrap().iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(tails, ["fills"]);
}

// ---- the audit-A3 history replay (a reconnect gap) ------------------------------------------------

#[test]
fn the_history_replay_files_the_child_fill_on_the_stop() {
    let orders = fixture!("rest_orders_history.json")["data"].clone();
    let fills = fixture!("rest_fills.json")["data"].clone();
    let events = map_okx_history(&orders, &fills, "okx", SYMBOL, CT_VAL);
    assert_eq!(
        kinds(&events),
        [
            format!("Fill:{STOP}"),
            format!("OrderFilled:{STOP}"),
            "Fill:b7okxtsa".to_string(),
            "OrderFilled:b7okxtsa".to_string(),
        ],
        "the order row's `algoClOrdId` names the child's fills (the fill rows carry none)"
    );
}
