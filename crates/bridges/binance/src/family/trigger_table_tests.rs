//! Pins the binance-perp AND aster (same `Mapped` CONTRACT_PRICE/MARK_PRICE, Index denied at
//! submit — aster's fapi fork documents the same `workingType` param, see the trigger module
//! doc) rows of the ONE cross-venue trigger-source authority
//! (`vike_bridge_core::trigger::venue_trigger_by`) against THIS shared family builder, which
//! CONSUMES the rows on its perp STOP arm. Byte-identity pin: a `None` request emits no
//! `workingType` at all — the venue default CONTRACT_PRICE rules, the exact pre-`trigger_by`
//! bytes.
use vike_bridge_core::trigger::{TriggerByOutcome, venue_trigger_by};
use vike_model::TriggerBy::{Index, Last, Mark};
use vike_model::{OrderRequest, SymbolProperties, TriggerBy};

use super::build_perp_order_params;
use crate::perp::TIF_LANE;

fn stop_req(tb: Option<TriggerBy>) -> OrderRequest {
    OrderRequest {
        client_order_id: "c-trig".to_string(),
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

fn working_type(params: &[(&'static str, String)]) -> Option<String> {
    params.iter().find(|(k, _)| *k == "workingType").map(|(_, v)| v.clone())
}

/// Byte-identity: a stop with NO requested source emits exactly the pre-field params —
/// `stopPrice` is the tail, no `workingType` anywhere.
#[test]
fn perp_stop_without_trigger_by_is_byte_identical() {
    let props = SymbolProperties::default();
    let params = build_perp_order_params(&stop_req(None), "BTCUSDT", &props, TIF_LANE, None);
    assert_eq!(working_type(&params), None, "None = venue default CONTRACT_PRICE, no param");
    assert_eq!(params.last().unwrap().0, "stopPrice", "stopPrice stays the stop tail");
}

#[test]
fn perp_stop_maps_last_and_mark_onto_working_type() {
    let props = SymbolProperties::default();
    for (tb, wire) in [(Last, "CONTRACT_PRICE"), (Mark, "MARK_PRICE")] {
        assert_eq!(venue_trigger_by(TIF_LANE, tb), TriggerByOutcome::Mapped(wire), "{tb:?}");
        let params =
            build_perp_order_params(&stop_req(Some(tb)), "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(working_type(&params).as_deref(), Some(wire), "{tb:?}");
        // workingType directly follows stopPrice (additive tail — nothing reordered)
        assert_eq!(params[params.len() - 2].0, "stopPrice");
    }
}

/// Index is denied at submit (`deny_unsupported_trigger_by`, pinned in
/// `tests/offline/trigger_gate.rs`); if it reaches the builder anyway, NO workingType is emitted —
/// never a silently substituted series.
#[test]
fn perp_stop_index_emits_no_working_type() {
    assert_eq!(venue_trigger_by(TIF_LANE, Index), TriggerByOutcome::Unsupported);
    let props = SymbolProperties::default();
    let params = build_perp_order_params(&stop_req(Some(Index)), "BTCUSDT", &props, TIF_LANE, None);
    assert_eq!(working_type(&params), None);
}

/// Aster shares binance-perp's law exactly (its fapi fork documents the same `workingType`
/// param — see the trigger module doc, not assumed from binance alone): Last/Mark map onto
/// `workingType`, directly following `stopPrice` (additive tail — nothing reordered).
#[test]
fn aster_stop_maps_last_and_mark_onto_working_type() {
    let props = SymbolProperties::default();
    for (tb, wire) in [(Last, "CONTRACT_PRICE"), (Mark, "MARK_PRICE")] {
        assert_eq!(venue_trigger_by("aster", tb), TriggerByOutcome::Mapped(wire), "{tb:?}");
        let params = build_perp_order_params(&stop_req(Some(tb)), "BTCUSDT", &props, "aster", None);
        assert_eq!(working_type(&params).as_deref(), Some(wire), "{tb:?}");
        assert_eq!(params[params.len() - 2].0, "stopPrice");
    }
}

/// Index has no fapi index `workingType` on aster either (same fork, same gap): denied at
/// submit (`deny_unsupported_trigger_by`, pinned in `aster`'s `tests/trigger_gate.rs`); if it
/// reaches the builder anyway, NO workingType is emitted — never a silently substituted series.
#[test]
fn aster_stop_index_emits_no_working_type() {
    assert_eq!(venue_trigger_by("aster", Index), TriggerByOutcome::Unsupported);
    let props = SymbolProperties::default();
    let params = build_perp_order_params(&stop_req(Some(Index)), "BTCUSDT", &props, "aster", None);
    assert_eq!(working_type(&params), None);
    assert_eq!(params.last().unwrap().0, "stopPrice", "unchanged tail");
}

/// The field is stop-arm-only: limit/market params never grow a workingType, whatever the
/// request carries.
#[test]
fn non_stop_orders_carry_no_working_type() {
    let props = SymbolProperties::default();
    for ot in ["limit", "market"] {
        let mut req = stop_req(Some(Mark));
        req.order_type = ot.to_string();
        req.price = Some(50000.0);
        req.trigger_price = None;
        let params = build_perp_order_params(&req, "BTCUSDT", &props, TIF_LANE, None);
        assert_eq!(working_type(&params), None, "{ot}");
    }
}
