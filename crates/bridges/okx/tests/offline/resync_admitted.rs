//! **The reconnect-resync and the gap-sentinel replay the history of the instruments the engine trades, not
//! only the mounted one** (decision 0174's reopener; offline: a recording `OkxTransport`, canned envelopes, no
//! network, nothing `#[ignore]`d).
//!
//! A fill or a stop's end that landed on an ADMITTED instrument while the private (or the business) socket was
//! down used to be found by the next reconcile pass or by nothing: the replay read `orders-history`,
//! `fills-history` and `orders-algo-history` for the mounted instrument alone. `okx_history_events_of` and
//! `okx_algo_history_events_of` read the mounted instrument FIRST with the requests it has always made and then
//! each foreign instrument with the SAME requests on its own `instId`.
//!
//! OKX sizes in CONTRACTS and numbers its trades per instrument, so a foreign fill is sized by ITS OWN `ctVal`
//! and carries the scoped id the user-data stream spells for it (`INSTID:tradeId`): a replayed fill the stream
//! already folded is then a KNOWN id, and an instrument whose size cannot be read is skipped, never sized with
//! a stand-in. Pins: nothing foreign = the mounted instrument's two requests and nothing else; the foreign
//! order of reads, repeats and the mounted instrument skipped; the first rate-limit answer ends the walk (the
//! mounted instrument's events still come back); the reads are `pace` apart.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::events::Event;
use vike_okx::exec::{okx_algo_history_events_of, okx_history_events_of};
use vike_okx::history::scope_fill_ids;
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;

use crate::r6_okx_parity::{fixture, okx_perp};

const BTC: &str = "BTC-USDT-SWAP";
const ETH: &str = "ETH-USDT-SWAP";
const SOL: &str = "SOL-USDT-SWAP";
const XRP: &str = "XRP-USDT-SWAP";

type Asked = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(endpoint, instId[/state])` of every read. Every instrument holds one filled order and one fill
/// (`tradeId` "7" on each: per-instrument numbering) of 3 contracts, and one `canceled` stop.
struct Venue {
    asked: Asked,
    /// `(instId, code)`: every read of that instrument answers an OKX error envelope with this code.
    fail_on: Option<(&'static str, &'static str)>,
}

fn short(inst: &str) -> String {
    inst.split('-').next().unwrap_or(inst).to_lowercase()
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
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        let inst = get("instId");
        let state = get("state");
        let tag = if state.is_empty() { inst.clone() } else { format!("{inst}/{state}") };
        self.asked.lock().unwrap().push((tail.clone(), tag));
        if let Some((bad, code)) = self.fail_on
            && bad == inst
        {
            return Ok(json!({"code": code, "msg": "Requests too frequent", "data": []}));
        }
        let rows = match tail.as_str() {
            "orders-history" => vec![json!({
                "instId": inst, "ordId": format!("o-{}", short(&inst)),
                "clOrdId": format!("c-{}", short(&inst)), "state": "filled", "uTime": "5",
            })],
            "fills-history" => vec![json!({
                "instId": inst, "ordId": format!("o-{}", short(&inst)), "tradeId": "7",
                "fillPx": "10", "fillSz": "3", "side": "buy", "fee": "-0.1", "feeCcy": "USDT",
                "execType": "T", "ts": "5",
            })],
            "orders-algo-history" => vec![json!({
                "instId": inst, "algoClOrdId": format!("s-{}-{state}", short(&inst)),
                "state": state, "uTime": "6",
            })],
            other => panic!("not a history read: {other}"),
        };
        Ok(json!({"code": "0", "msg": "", "data": rows}))
    }

    fn public(
        &self,
        _base: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("a history replay reads nothing public: the contract value is the caller's")
    }
}

fn rest(fail_on: Option<(&'static str, &'static str)>) -> (OkxPerpRest<Venue>, Asked) {
    let asked: Asked = Arc::default();
    (okx_perp(&fixture(), Venue { asked: Arc::clone(&asked), fail_on }), asked)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn pair(endpoint: &str, tag: &str) -> (String, String) {
    (endpoint.to_string(), tag.to_string())
}

/// `ctVal` of the scripted instruments: BTC 0.01 (the mounted one's), ETH 0.1; SOL's cannot be read.
fn ct_of(inst: &str) -> Option<f64> {
    match inst {
        ETH => Some(0.1),
        XRP => Some(1.0),
        _ => None,
    }
}

fn fills(events: &[Event]) -> Vec<(String, String, f64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.symbol.to_string(), f.trade_id.to_string(), f.last_qty)),
            _ => None,
        })
        .collect()
}

#[test]
fn nothing_foreign_is_the_mounted_instruments_two_requests_and_nothing_else() {
    let (client, seen) = rest(None);
    let events = okx_history_events_of(&client, BTC, 0.01, &[], &ct_of, Duration::ZERO);
    assert_eq!(
        seen.lock().unwrap().clone(),
        [pair("orders-history", BTC), pair("fills-history", BTC)]
    );
    let got = fills(&events);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].0.as_str(), got[0].1.as_str()), (BTC, "7"), "the mounted id stays bare");
    assert!((got[0].2 - 0.03).abs() < 1e-12, "3 contracts of 0.01: {}", got[0].2);
}

#[test]
fn a_foreign_instrument_is_read_after_the_mounted_one_sized_and_scoped_as_its_own() {
    let (client, seen) = rest(None);
    let foreign = strings(&[ETH, "eth-usdt-swap", BTC, "", SOL, XRP]);
    let events = okx_history_events_of(&client, BTC, 0.01, &foreign, &ct_of, Duration::ZERO);
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            pair("orders-history", BTC),
            pair("fills-history", BTC),
            pair("orders-history", ETH),
            pair("fills-history", ETH),
            pair("orders-history", XRP),
            pair("fills-history", XRP),
        ],
        "mounted first; a repeat, the mounted instrument and an empty label are never read twice; SOL's \
         contract value cannot be read, so it is not read at all"
    );
    let got = fills(&events);
    assert_eq!(
        got.iter().map(|(s, id, _)| (s.as_str(), id.as_str())).collect::<Vec<_>>(),
        [(BTC, "7"), (ETH, "ETH-USDT-SWAP:7"), (XRP, "XRP-USDT-SWAP:7")],
        "the same tradeId on three instruments is three ids"
    );
    assert!((got[1].2 - 0.3).abs() < 1e-12, "3 contracts of ETH's own 0.1: {}", got[1].2);
    assert!((got[2].2 - 3.0).abs() < 1e-12, "3 contracts of XRP's own 1.0: {}", got[2].2);
}

#[test]
fn the_first_rate_limit_ends_the_walk_and_the_mounted_events_still_come_back() {
    let (client, seen) = rest(Some((ETH, "50011")));
    let foreign = strings(&[ETH, XRP]);
    let events = okx_history_events_of(&client, BTC, 0.01, &foreign, &ct_of, Duration::ZERO);
    assert!(
        seen.lock().unwrap().iter().all(|(_, tag)| tag != XRP),
        "nothing is asked after a rate limit: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(fills(&events).len(), 1, "the mounted instrument's replay is not lost");
}

#[test]
fn the_reads_of_two_foreign_instruments_are_a_pace_apart_and_the_first_follows_at_once() {
    let (client, _seen) = rest(None);
    // A pace of thirty seconds: a walk that slept before the FIRST foreign instrument would take that long.
    let started = Instant::now();
    okx_history_events_of(&client, BTC, 0.01, &strings(&[ETH]), &ct_of, Duration::from_secs(30));
    assert!(started.elapsed() < Duration::from_secs(10), "one foreign instrument waits for nobody");
    let pace = Duration::from_millis(60);
    let started = Instant::now();
    okx_history_events_of(&client, BTC, 0.01, &strings(&[ETH, XRP]), &ct_of, pace);
    assert!(started.elapsed() >= pace, "two are a pace apart: {:?}", started.elapsed());
}

#[test]
fn the_algo_socket_replays_the_stops_that_ended_on_each_instrument() {
    let (client, seen) = rest(None);
    let events =
        okx_algo_history_events_of(&client, &strings(&[ETH, "eth-usdt-swap", BTC]), Duration::ZERO);
    assert_eq!(
        seen.lock().unwrap().clone(),
        [
            pair("orders-algo-history", &format!("{BTC}/canceled")),
            pair("orders-algo-history", &format!("{BTC}/order_failed")),
            pair("orders-algo-history", &format!("{ETH}/canceled")),
            pair("orders-algo-history", &format!("{ETH}/order_failed")),
        ],
        "the mounted instrument's two states, then each foreign instrument's"
    );
    let ended: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(c) => Some(c.client_order_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ended,
        ["s-btc-canceled", "s-btc-order_failed", "s-eth-canceled", "s-eth-order_failed"]
    );

    let (client, seen) = rest(Some((ETH, "50011")));
    okx_algo_history_events_of(&client, &strings(&[ETH, XRP]), Duration::ZERO);
    assert!(
        seen.lock().unwrap().iter().all(|(_, tag)| !tag.starts_with(XRP)),
        "a rate limit ends the algo walk too"
    );
}

/// The scoped id is a pure rewrite of `tradeId`: a row with none, and a body that is not a list, come back as
/// they came.
#[test]
fn scoping_rewrites_only_a_trade_id_that_is_there() {
    let rows = json!([{"tradeId": "7", "x": 1}, {"tradeId": "", "x": 2}, {"x": 3}]);
    assert_eq!(
        scope_fill_ids(&rows, ETH),
        json!([{"tradeId": "ETH-USDT-SWAP:7", "x": 1}, {"tradeId": "", "x": 2}, {"x": 3}])
    );
    assert_eq!(scope_fill_ids(&json!({"not": "a list"}), ETH), json!({"not": "a list"}));
}
