//! Pins the FLIPPED deribit row of the ONE cross-venue TIF authority
//! (`vike_venues::venue_time_in_force::venue_time_in_force`) against the real builder + submit gate: Gtc still
//! emits NO `time_in_force` param (the venue default `good_til_cancelled` rules —
//! byte-identical to pre-flip); Ioc/Fok/Day map to Deribit's own TIF vocabulary on the
//! limit path; market orders keep today's no-TIF shape; Gtd is a LOUD terminal deny at
//! submit, the wire never touched.
use serde_json::Value;
use std::assert_matches;
use vike_model::TimeInForce::{self, Day, Fok, Gtc, Gtd, Ioc};
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties};
use vike_venues::venue_time_in_force::{TimeInForceOutcome, venue_time_in_force};

use super::VenueRestDeribit;
use crate::transport::client::DeribitTransport;

fn rest() -> VenueRestDeribit {
    let transport = DeribitTransport::new("wss://test.invalid", "id", "secret", None);
    VenueRestDeribit::new(transport, "BTC-PERPETUAL", SymbolProperties::default(), "BTC")
}

fn req(order_type: &str, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        order_client_id: "c-tif".to_string(),
        venue: "deribit".to_string(),
        symbol: "BTC-PERPETUAL".to_string(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::from_stored(order_type),
        price: Some(50000.0),
        time_in_force: tif,
        ..Default::default()
    }
}

/// Byte-identity: a default/GTC request (any order type) emits NO `time_in_force` — the
/// venue default rules, exactly as before the flip.
#[test]
fn gtc_still_emits_no_time_in_force_param() {
    let rest = rest();
    assert_eq!(venue_time_in_force("deribit", Gtc), TimeInForceOutcome::NotEmitted);
    for order_type in ["limit", "market"] {
        let params: Value = rest.build_order_params(&req(order_type, Gtc));
        assert!(params.get("time_in_force").is_none(), "{order_type}");
    }
}

#[test]
fn limit_orders_map_ioc_fok_day_to_deribit_vocabulary() {
    let rest = rest();
    for (tif, wire) in [(Ioc, "immediate_or_cancel"), (Fok, "fill_or_kill"), (Day, "good_til_day")]
    {
        assert_eq!(
            venue_time_in_force("deribit", tif),
            TimeInForceOutcome::Mapped(wire),
            "{tif:?}"
        );
        let params: Value = rest.build_order_params(&req("limit", tif));
        assert_eq!(params.get("time_in_force").and_then(|v| v.as_str()), Some(wire), "{tif:?}");
        // market orders keep today's no-TIF shape (the table's resting-path scope)
        let params: Value = rest.build_order_params(&req("market", tif));
        assert!(params.get("time_in_force").is_none(), "market/{tif:?}");
    }
}

/// Gtd is Unsupported (Deribit has no good-till-DATE): the submit gate yields the
/// emitter-split pair and never touches the (unconnected — a call would error, not
/// reject with this reason) transport.
#[test]
fn gtd_is_loud_denied_at_submit() {
    use vike_bridge_core::rest::VenueRest;
    let rest = rest();
    assert_eq!(venue_time_in_force("deribit", Gtd), TimeInForceOutcome::Unsupported);
    let events = rest.submit_order(&req("limit", Gtd));
    assert_eq!(events.len(), 2, "exactly the emitter-split pair: {events:?}");
    assert_matches!(&events[0], Event::OrderSubmitted(_));
    match &events[1] {
        Event::OrderRejected(r) => {
            assert!(
                r.reason.contains("not supported on deribit"),
                "loud deny reason, got: {}",
                r.reason
            );
        }
        other => panic!("expected terminal OrderRejected, got {other:?}"),
    }
}
