//! tif step-2 submit-gate pins (offline): a TIF a binance lane cannot express — spot GTD/Day,
//! perp Day — yields the emitter-split pair `[OrderSubmitted, OrderRejected]` and the wire is
//! NEVER touched, as does a perp GTD whose date fails the checkable-here fapi bounds (missing /
//! below now+600s / at-or-past the year-9999 cap: `perp::deny_invalid_gtd`); this holds on the
//! spot submit, the perp submit, and per-order inside the perp native batch (denied orders are
//! partitioned out of the wire chunk; supported ones still batch — including a VALID perp GTD,
//! which rides the wire as `timeInForce=GTD` + its `goodTillDate` companion). The mapped wire
//! strings for GTC/IOC/FOK (and the GTD param slots) are pinned crate-side in
//! `family::order_map::time_in_force_table_tests`.

use std::assert_matches;
use std::sync::Mutex;

use vike_binance::execution::rest::perp::VenueRestBinancePerp;
use vike_binance::execution::rest::spot::VenueRestBinanceSpot;
use vike_bridge_core::execution_drive;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TimeInForce};

/// A signer that prepares an empty request; the transport doubles ignore it. The ONE copy in the
/// `offline` binary: every member that needs a signer (`trigger_gate.rs`, this file's twin, among
/// them) imports this one rather than defining its own.
pub(crate) struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// A transport that PANICS on any wire touch — proves the deny gate returns before the POST. The
/// ONE copy in the `offline` binary: `trigger_gate.rs` denies through it too.
pub(crate) struct NoWire;
impl RestTransport for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("a denied order must never reach the wire (signed {path})");
    }
    vike_bridge_core::no_public_call!("a denied order must never reach the wire");
}

/// Captures the signed params and acks every order — for the mixed-batch partition pin.
/// `trigger_gate.rs`, this file's twin, uses it too.
pub(crate) struct Capture {
    pub(crate) calls: Mutex<Vec<Vec<(String, String)>>>,
}
impl RestTransport for Capture {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
        // a single order (a stop included) is acked by an object naming it, as the venue does
        let Some(n) = params
            .iter()
            .find(|(k, _)| *k == "batchOrders")
            .and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(v).ok())
            .and_then(|v| v.as_array().map(Vec::len))
        else {
            return Ok(serde_json::json!({ "orderId": 1, "algoId": 1 }));
        };
        // ack every order in the batch (the response array is index-matched to the chunk)
        let acks: Vec<serde_json::Value> =
            (0..n).map(|i| serde_json::json!({ "orderId": i + 1 })).collect();
        Ok(serde_json::Value::Array(acks))
    }
    vike_bridge_core::no_public_call!(unreachable: "no public call in submit_batch");
}

fn limit_request(order_client_id: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        time_in_force: tif,
        ..execution_drive::limit("binance", order_client_id, "BTCUSDT", 0.01, 50000.0)
    }
}

#[test]
fn spot_submit_denies_gtd_and_day_without_touching_the_wire() {
    let client = VenueRestBinanceSpot {
        link_id: None,
        signer: NullSigner,
        transport: NoWire,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".to_string(),
    };
    for (order_client_id, tif) in [("c-gtd", TimeInForce::Gtd), ("c-day", TimeInForce::Day)] {
        let events = client.submit_order(&limit_request(order_client_id, tif));
        execution_drive::assert_denied_pair(&events, order_client_id, "not supported on binance");
    }
}

/// Perp lane: Day stays Unsupported (`"binance-perp"` row) — denied without a wire touch. GTD is
/// no longer an Unsupported-row deny on this lane; its VALIDITY denials are pinned below.
#[test]
fn perp_submit_denies_day_without_touching_the_wire() {
    let client = VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: NoWire,
        base_url: "https://unused.invalid".to_string(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let events = VenueRest::submit_order(&client, &limit_request("c-day", TimeInForce::Day));
    // the lane sub-key names the lane in the deny ("not supported on binance-perp")
    execution_drive::assert_denied_pair(&events, "c-day", "not supported on binance-perp");
}

/// Perp lane GTD validity gate (`perp::deny_invalid_gtd`): a GTD with no `gtd_expiry`, one below
/// the venue's now+600s floor, and one at/past the year-9999 cap are each a terminal
/// `OrderRejected` with the wire NEVER touched — a date is never invented, never silently fixed.
#[test]
fn perp_submit_denies_invalid_gtd_without_touching_the_wire() {
    let client = VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: NoWire,
        base_url: "https://unused.invalid".to_string(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let now = vike_model::now_ms();
    let cases: [(&str, Option<i64>, &str); 3] = [
        ("c-gtd-dateless", None, "requires gtd_expiry"),
        ("c-gtd-near", Some(now + 60_000), "600s"),
        ("c-gtd-max", Some(vike_binance::execution::rest::perp::GTD_MAX_MS), "venue maximum"),
    ];
    for (order_client_id, expiry, needle) in cases {
        let mut req = limit_request(order_client_id, TimeInForce::Gtd);
        req.gtd_expiry = expiry;
        let events = VenueRest::submit_order(&client, &req);
        execution_drive::assert_denied_pair(&events, order_client_id, needle);
        let not_sent = Some(vike_model::OrderRefusalId::NotSent.word());
        assert!(
            matches!(&events[..], [_, Event::OrderRejected(r)] if r.refusal_id.as_deref() == not_sent),
            "{events:?}"
        );
    }
}

/// A VALID perp GTD passes both gates and rides the wire with `timeInForce=GTD` + the
/// `goodTillDate` companion carrying the request's exact `gtd_expiry`.
#[test]
fn perp_submit_wires_valid_gtd_with_good_till_date() {
    let client = VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: Capture { calls: Mutex::new(Vec::new()) },
        base_url: "https://unused.invalid".to_string(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let exp = vike_model::now_ms() + 3_600_000;
    let mut req = limit_request("c-gtd-ok", TimeInForce::Gtd);
    req.gtd_expiry = Some(exp);
    let events = VenueRest::submit_order(&client, &req);
    assert_matches!(
        events.last(),
        Some(Event::OrderAccepted(_)),
        "valid GTD must reach the wire and ack: {events:?}"
    );
    let calls = client.transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one wire call");
    let time_in_force_index =
        calls[0].iter().position(|(k, _)| k == "timeInForce").expect("timeInForce");
    assert_eq!(calls[0][time_in_force_index].1, "GTD");
    assert_eq!(
        calls[0][time_in_force_index + 1],
        ("goodTillDate".to_string(), exp.to_string()),
        "goodTillDate directly follows timeInForce with the request's exact expiry"
    );
}

/// A mixed batch partitions: the denied orders (Day = Unsupported row; a dateless GTD = the
/// validity gate) are terminally rejected and never enter the wire chunk; the supported orders
/// still batch (ONE wire call whose batchOrders array carries the three supported order client ids —
/// including a VALID GTD, whose element carries its goodTillDate companion).
#[test]
fn perp_batch_partitions_denied_tifs_out_of_the_wire_chunk() {
    let client = VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport: Capture { calls: Mutex::new(Vec::new()) },
        base_url: "https://unused.invalid".to_string(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let exp = vike_model::now_ms() + 3_600_000;
    let mut gtd_ok = limit_request("c-gtd-ok", TimeInForce::Gtd);
    gtd_ok.gtd_expiry = Some(exp);
    let requests = vec![
        limit_request("c-ok-1", TimeInForce::Gtc),
        limit_request("c-bad-day", TimeInForce::Day),
        limit_request("c-bad-gtd", TimeInForce::Gtd), // dateless — the validity gate denies it
        gtd_ok,
        limit_request("c-ok-2", TimeInForce::Ioc),
    ];
    let events = client.submit_batch(&requests);
    // 5 Submitted + 2 Rejected (the denied ones) + per-order acks for the 3 sent ones
    let submitted: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderSubmitted(s) => Some(s.order_client_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        submitted,
        ["c-ok-1", "c-bad-day", "c-bad-gtd", "c-gtd-ok", "c-ok-2"],
        "every order gets its Submitted"
    );
    let rejected: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.order_client_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, ["c-bad-day", "c-bad-gtd"], "only the denied orders are rejected");
    let accepted: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderAccepted(a) => Some(a.order_client_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(accepted, ["c-ok-1", "c-gtd-ok", "c-ok-2"], "the supported orders still batch");

    let calls = client.transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one wire chunk for the three supported orders");
    let batch = calls[0].iter().find(|(k, _)| k == "batchOrders").map(|(_, v)| v.clone()).unwrap();
    assert!(
        batch.contains("c-ok-1") && batch.contains("c-ok-2") && batch.contains("c-gtd-ok"),
        "batch carries the supported: {batch}"
    );
    assert!(!batch.contains("c-bad"), "no denied order ever reaches the wire: {batch}");
    // and the honored TIFs ride the wire: GTC for the default row, IOC for the requested one,
    // GTD with its goodTillDate companion for the valid good-till-date order
    assert!(batch.contains("\"timeInForce\":\"GTC\"") || batch.contains("GTC"));
    assert!(batch.contains("IOC"), "requested IOC must be on the wire: {batch}");
    assert!(batch.contains("\"timeInForce\":\"GTD\""), "valid GTD must be on the wire: {batch}");
    assert!(
        batch.contains(&format!("\"goodTillDate\":\"{exp}\"")),
        "the GTD element must carry its goodTillDate: {batch}"
    );
}
