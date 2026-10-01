//! Trigger-source submit-gate pins (offline, the `tif_gate.rs` twin): a `trigger_by` a binance
//! lane cannot express — spot Mark/Index (spot's only price series is last trades), perp Index
//! (no such fapi `workingType`) — yields the emitter-split pair `[OrderSubmitted, OrderRejected]`
//! and the wire is NEVER touched; this holds on the spot submit, the perp submit, and per-order
//! inside the perp native batch (denied orders are partitioned out of the wire chunk; supported
//! ones still batch — a Mark stop rides the wire with `workingType=MARK_PRICE`). The mapped wire
//! strings themselves are pinned crate-side in `family::order_map::trigger_table_tests`.

use std::sync::Mutex;

use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TriggerBy};

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// A transport that PANICS on any wire touch — proves the deny gate returns before the POST.
struct NoWire;
impl RestTransport for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("denied trigger_by must never reach the wire (signed {path})");
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("denied trigger_by must never reach the wire (public {path})");
    }
}

/// Captures the signed params and acks every order — for the mixed-batch partition pin.
struct Capture {
    calls: Mutex<Vec<Vec<(String, String)>>>,
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
        let n = params
            .iter()
            .find(|(k, _)| *k == "batchOrders")
            .and_then(|(_, v)| serde_json::from_str::<serde_json::Value>(v).ok())
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(1);
        let acks: Vec<serde_json::Value> =
            (0..n).map(|i| serde_json::json!({ "orderId": i + 1 })).collect();
        Ok(serde_json::Value::Array(acks))
    }
    fn public(
        &self,
        _base: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        unreachable!("no public call in submit_batch")
    }
}

fn stop_req(coid: &str, tb: Option<TriggerBy>) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 0.01,
        order_type: "stop".to_string(),
        trigger_price: Some(58000.0),
        reduce_only: true,
        trigger_by: tb,
        ..Default::default()
    }
}

fn assert_denied_pair_with(events: &[Event], coid: &str, reason_needle: &str) {
    assert_eq!(events.len(), 2, "{coid}: exactly the emitter-split pair: {events:?}");
    assert!(matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == coid));
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, coid);
            assert!(
                r.reason.contains(reason_needle),
                "loud deny reason must contain {reason_needle:?}, got: {}",
                r.reason
            );
        }
        other => panic!("{coid}: expected terminal OrderRejected, got {other:?}"),
    }
}

#[test]
fn spot_submit_denies_mark_and_index_without_touching_the_wire() {
    let client = BinanceSpotRest {
        link_id: None,
        signer: NullSigner,
        transport: NoWire,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        base_asset: "BTC".to_string(),
    };
    for (coid, tb) in [("c-mark", TriggerBy::Mark), ("c-index", TriggerBy::Index)] {
        assert_denied_pair_with(
            &client.submit_order(&stop_req(coid, Some(tb))),
            coid,
            "not supported on binance",
        );
    }
}

#[test]
fn perp_submit_denies_index_without_touching_the_wire() {
    let client = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: NoWire,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let events = VenueRest::submit_order(&client, &stop_req("c-index", Some(TriggerBy::Index)));
    // the lane sub-key names the lane in the deny ("not supported on binance-perp")
    assert_denied_pair_with(&events, "c-index", "not supported on binance-perp");
}

/// A Mark stop passes the gate and rides the wire with `workingType=MARK_PRICE`; a None stop
/// rides with NO workingType at all (the byte-identical default).
#[test]
fn perp_submit_wires_mark_stop_and_default_stays_bare() {
    for (tb, want) in [(Some(TriggerBy::Mark), Some("MARK_PRICE")), (None, None)] {
        let client = BinancePerpRest {
            link_id: None,
            signer: NullSigner,
            transport: Capture { calls: Mutex::new(Vec::new()) },
            base_url: "https://unused.invalid".to_string(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: 1.0,
        };
        let events = VenueRest::submit_order(&client, &stop_req("c-ok", tb));
        assert!(
            matches!(events.last(), Some(Event::OrderAccepted(_))),
            "stop must reach the wire and ack: {events:?}"
        );
        let calls = client.transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let got = calls[0].iter().find(|(k, _)| k == "workingType").map(|(_, v)| v.as_str());
        assert_eq!(got, want, "{tb:?}");
    }
}

/// The perp native batch partitions per order: the Index stop is denied and never enters the
/// wire chunk; the Mark stop still batches with its `workingType=MARK_PRICE`.
#[test]
fn perp_batch_partitions_denied_trigger_sources_out_of_the_chunk() {
    let client = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: Capture { calls: Mutex::new(Vec::new()) },
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let reqs = vec![
        stop_req("c-mark", Some(TriggerBy::Mark)),
        stop_req("c-index", Some(TriggerBy::Index)),
    ];
    let events = client.submit_batch(&reqs);
    // both Submitted; c-index Rejected; c-mark Accepted off the one-order chunk
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "c-index"
            && r.reason.contains("not supported on binance-perp")))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderAccepted(a) if a.client_order_id == "c-mark")),
        "{events:?}"
    );
    let calls = client.transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "ONE chunk, holding only the sendable order");
    let batch = calls[0].iter().find(|(k, _)| k == "batchOrders").map(|(_, v)| v.clone()).unwrap();
    assert!(batch.contains("MARK_PRICE"), "{batch}");
    assert!(!batch.contains("c-index"), "the denied order never entered the chunk: {batch}");
}
