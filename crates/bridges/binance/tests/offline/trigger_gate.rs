//! Trigger-source submit-gate pins (offline, the `tif_gate.rs` twin): a `trigger_by` a binance
//! lane cannot express — spot Mark/Index (spot's only price series is last trades), perp Index
//! (no such fapi `workingType`) — yields the emitter-split pair `[OrderSubmitted, OrderRejected]`
//! and the wire is NEVER touched; this holds on the spot submit, the perp submit, and per-order
//! inside the perp native batch (denied orders never reach the wire; a supported perp stop is a
//! CONDITIONAL order and rides `POST /fapi/v1/algoOrder` with `workingType=MARK_PRICE`, since the
//! venue refuses it on `/fapi/v1/order` and `batchOrders`). The mapped wire strings themselves are
//! pinned crate-side: aster's stop in `family::order_map::trigger_table_tests`, binance's in
//! `algo::algo_tests`.

use std::assert_matches;
use std::sync::Mutex;

use vike_binance::perp::BinancePerpRest;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, TriggerBy};

use crate::tif_gate::{Capture, NullSigner, assert_denied_pair_with};

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

fn stop_req(coid: &str, tb: Option<TriggerBy>) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 0.01,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(58000.0),
        reduce_only: true,
        trigger_by: tb,
        ..Default::default()
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
        algo_coids: Default::default(),
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
            algo_coids: Default::default(),
            symbol: "BTCUSDT".to_string(),
            properties: SymbolProperties::default(),
            leverage: 1.0,
        };
        let events = VenueRest::submit_order(&client, &stop_req("c-ok", tb));
        assert_matches!(
            events.last(),
            Some(Event::OrderAccepted(_)),
            "stop must reach the wire and ack: {events:?}"
        );
        let calls = client.transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let got = calls[0].iter().find(|(k, _)| k == "workingType").map(|(_, v)| v.as_str());
        assert_eq!(got, want, "{tb:?}");
    }
}

/// The perp native batch partitions per order: the Index stop is denied and never reaches the wire;
/// the Mark stop is a CONDITIONAL order, which `batchOrders` refuses (`-4120`), so it leaves the chunk
/// and goes to `/fapi/v1/algoOrder` on its own with its `workingType=MARK_PRICE`.
#[test]
fn perp_batch_partitions_denied_trigger_sources_out_of_the_chunk() {
    let client = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: Capture { calls: Mutex::new(Vec::new()) },
        base_url: "https://unused.invalid".to_string(),
        algo_coids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    };
    let reqs = vec![
        stop_req("c-mark", Some(TriggerBy::Mark)),
        stop_req("c-index", Some(TriggerBy::Index)),
    ];
    let events = client.submit_batch(&reqs);
    // both Submitted; c-index Rejected; c-mark Accepted off its own algo POST
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
    assert_eq!(calls.len(), 1, "ONE request, the sendable stop's own algo POST");
    let get = |k: &str| calls[0].iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    assert_eq!(get("batchOrders"), None, "a conditional order never rides batchOrders");
    assert_eq!(get("algoType"), Some("CONDITIONAL"));
    assert_eq!(get("workingType"), Some("MARK_PRICE"));
    assert_eq!(get("clientAlgoId"), Some("c-mark"));
    assert!(
        !calls[0].iter().any(|(_, v)| v.contains("c-index")),
        "the denied order never reached the wire"
    );
}
