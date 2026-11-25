//! Pins the binance spot (`"binance"`: Mapped GTC/IOC/FOK, Unsupported GTD/Day), binance
//! perp (`"binance-perp"` lane sub-key: same trio plus native Mapped GTD, Unsupported Day)
//! and aster (still `Ignored{GTC}`) rows of the ONE cross-venue TIF authority
//! (`vike_venues::venue_time_in_force::venue_time_in_force`) against THIS shared family builder, which
//! CONSUMES the rows on its LIMIT path. Byte-identity pins: a default/GTC binance request
//! and EVERY aster request still emit `timeInForce=GTC` in the same slot; market orders
//! carry no TIF param at all; an Unsupported TIF (gated at submit) emits no TIF param;
//! `goodTillDate` exists ONLY on the perp lane's GTD row and is never invented.
use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
use vike_model::{OrderRequest, SymbolProperties, TimeInForce};
use vike_venues::venue_time_in_force::{TimeInForceOutcome, venue_time_in_force};

use super::{build_perp_order_params, build_spot_order_params};
use crate::execution::rest::perp::TIME_IN_FORCE_LANE;

fn limit_request(tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        order_client_id: "c-tif".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.01,
        order_type: vike_model::OrderType::Limit,
        price: Some(50000.0),
        time_in_force: tif,
        ..Default::default()
    }
}

fn time_in_force_param(params: &[(&'static str, String)]) -> Option<String> {
    params.iter().find(|(k, _)| *k == "timeInForce").map(|(_, v)| v.clone())
}

fn gtd_param(params: &[(&'static str, String)]) -> Option<String> {
    params.iter().find(|(k, _)| *k == "goodTillDate").map(|(_, v)| v.clone())
}

#[test]
fn binance_limit_orders_honor_request_time_in_force() {
    let properties = SymbolProperties::default();
    for (tif, wire) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK")] {
        assert_eq!(
            venue_time_in_force("binance", tif),
            TimeInForceOutcome::Mapped(wire),
            "{tif:?}"
        );
        assert_eq!(
            venue_time_in_force(TIME_IN_FORCE_LANE, tif),
            TimeInForceOutcome::Mapped(wire),
            "{tif:?} (perp lane)"
        );
        let req = limit_request(tif);
        for params in [
            build_spot_order_params(&req, "BTCUSDT", &properties, "binance", None),
            build_perp_order_params(&req, "BTCUSDT", &properties, TIME_IN_FORCE_LANE, None),
        ] {
            assert_eq!(time_in_force_param(&params).as_deref(), Some(wire), "{tif:?}");
            assert_eq!(gtd_param(&params), None, "{tif:?}: goodTillDate is GTD-only");
        }
    }
}

/// Byte-identity: the default request (TIF unset = Gtc) emits the exact params the
/// pre-flip hardcode emitted — `timeInForce=GTC` in the same position, same bytes.
#[test]
fn binance_default_time_in_force_bytes_are_unchanged() {
    let req = limit_request(TimeInForce::default());
    let properties = SymbolProperties::default();
    let spot = build_spot_order_params(&req, "BTCUSDT", &properties, "binance", None);
    assert_eq!(spot[6], ("timeInForce", "GTC".to_string()), "same slot, same bytes");
    let perp = build_perp_order_params(&req, "BTCUSDT", &properties, TIME_IN_FORCE_LANE, None);
    assert_eq!(perp[8], ("timeInForce", "GTC".to_string()), "same slot, same bytes");
    assert_eq!(perp[9].0, "price", "no param slots in between");
}

/// Unsupported rows emit no param — the submit gate rejects before the builder runs
/// (pinned in `execution/rest/spot.rs`/`execution/rest/perp.rs`); if one reaches the builder anyway, NO timeInForce param
/// is emitted (Binance rejects a TIF-less LIMIT loudly server-side, never a silent GTC).
/// Spot lane: GTD and Day; perp lane: Day (its GTD is Mapped — see the wire test below).
#[test]
fn binance_unsupported_time_in_force_emits_no_param() {
    let properties = SymbolProperties::default();
    for tif in [Gtd, Day] {
        assert_eq!(venue_time_in_force("binance", tif), TimeInForceOutcome::Unsupported, "{tif:?}");
        let req = limit_request(tif);
        let params = build_spot_order_params(&req, "BTCUSDT", &properties, "binance", None);
        assert_eq!(time_in_force_param(&params), None, "{tif:?} (spot)");
        assert_eq!(gtd_param(&params), None, "{tif:?} (spot): never a goodTillDate");
    }
    assert_eq!(venue_time_in_force(TIME_IN_FORCE_LANE, Day), TimeInForceOutcome::Unsupported);
    let params = build_perp_order_params(
        &limit_request(Day),
        "BTCUSDT",
        &properties,
        TIME_IN_FORCE_LANE,
        None,
    );
    assert_eq!(time_in_force_param(&params), None, "Day (perp)");
    assert_eq!(gtd_param(&params), None, "Day (perp)");
}

/// The perp lane's native GTD wire shape: `timeInForce=GTD` with the venue-mandatory
/// `goodTillDate` companion (verbatim `gtd_expiry` epoch ms) in the NEXT slot, then price —
/// the exact positions the golden order established for the limit tail.
#[test]
fn binance_perp_gtd_wires_time_in_force_and_good_till_date() {
    assert_eq!(venue_time_in_force(TIME_IN_FORCE_LANE, Gtd), TimeInForceOutcome::Mapped("GTD"));
    let properties = SymbolProperties::default();
    let mut req = limit_request(Gtd);
    req.gtd_expiry = Some(1_770_736_694_000);
    let params = build_perp_order_params(&req, "BTCUSDT", &properties, TIME_IN_FORCE_LANE, None);
    assert_eq!(params[8], ("timeInForce", "GTD".to_string()), "exact slot");
    assert_eq!(params[9], ("goodTillDate", "1770736694000".to_string()), "exact slot");
    assert_eq!(params[10].0, "price", "price stays the limit tail");
    // the SPOT builder never emits GTD even with an expiry set — its row is Unsupported
    let spot = build_spot_order_params(&req, "BTCUSDT", &properties, "binance", None);
    assert_eq!(time_in_force_param(&spot), None, "spot has no GTD");
    assert_eq!(gtd_param(&spot), None, "spot has no goodTillDate");
}

/// A date is NEVER invented: a dateless GTD that slipped past the submit gate
/// (`crate::execution::rest::perp::deny_invalid_gtd`) emits `timeInForce=GTD` with NO `goodTillDate` — the
/// venue then rejects the mandatory-param violation loudly, never a made-up expiry.
#[test]
fn binance_perp_dateless_gtd_emits_no_good_till_date() {
    let properties = SymbolProperties::default();
    let req = limit_request(Gtd); // gtd_expiry stays None
    let params = build_perp_order_params(&req, "BTCUSDT", &properties, TIME_IN_FORCE_LANE, None);
    assert_eq!(time_in_force_param(&params).as_deref(), Some("GTD"));
    assert_eq!(gtd_param(&params), None, "no invented goodTillDate");
    assert_eq!(params[9].0, "price", "price directly follows timeInForce");
}

/// Aster is UNFLIPPED: every request TIF still rests GTC (the `Ignored{GTC}` row), so its
/// wire bytes are byte-identical to before the family builder grew the venue parameter —
/// and the perp-lane GTD companion NEVER leaks onto aster's wire, even with an expiry set.
#[test]
fn aster_limit_orders_still_ignore_request_time_in_force() {
    let properties = SymbolProperties::default();
    for tif in [Gtc, Ioc, Fok, Gtd, Day] {
        assert_eq!(
            venue_time_in_force("aster", tif),
            TimeInForceOutcome::Ignored { wire: "GTC" },
            "{tif:?}"
        );
        let mut req = limit_request(tif);
        req.gtd_expiry = Some(1_770_736_694_000); // must never reach aster's wire
        for params in [
            build_spot_order_params(&req, "BTCUSDT", &properties, "aster", None),
            build_perp_order_params(&req, "BTCUSDT", &properties, "aster", None),
        ] {
            assert_eq!(
                time_in_force_param(&params).as_deref(),
                Some("GTC"),
                "aster LIMIT rests GTC regardless of requested {tif:?}"
            );
            assert_eq!(gtd_param(&params), None, "no goodTillDate on aster ({tif:?})");
        }
    }
}

#[test]
fn family_market_orders_carry_no_time_in_force_param() {
    let mut req = limit_request(Ioc);
    req.order_type = vike_model::OrderType::Market;
    req.price = None;
    req.gtd_expiry = Some(1_770_736_694_000); // never emitted off the limit path either
    let properties = SymbolProperties::default();
    for venue in ["binance", TIME_IN_FORCE_LANE, "aster"] {
        for params in [
            build_spot_order_params(&req, "BTCUSDT", &properties, venue, None),
            build_perp_order_params(&req, "BTCUSDT", &properties, venue, None),
        ] {
            assert!(params.iter().all(|(k, _)| *k != "timeInForce"), "{venue}");
            assert!(params.iter().all(|(k, _)| *k != "goodTillDate"), "{venue}");
        }
    }
}
