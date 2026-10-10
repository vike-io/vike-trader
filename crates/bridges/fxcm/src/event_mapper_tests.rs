use super::*;
use std::assert_matches;

#[test]
fn instrument_reverse_maps() {
    assert_eq!(from_fxcm_instrument("EUR/USD"), "EURUSD");
    assert_eq!(from_fxcm_instrument("xau/usd"), "XAUUSD");
}

#[test]
fn fill_event_dual_publishes() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"kind":"fill","trade_id":"T77","instrument":"EUR/USD","side":"S",
                "amount":10000,"rate":1.0912,"commission":0.05,"ts":1700}"#,
    )
    .unwrap();
    let evs = map_fxcm_event(&v, "coid-3");
    assert_eq!(evs.len(), 2);
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.trade_id, "T77");
            assert_eq!(f.client_order_id, "coid-3");
            assert_eq!(f.symbol, "EURUSD");
            assert_eq!(f.side, -1);
            assert_eq!(f.last_qty, 10000.0);
            assert_eq!(f.last_px, 1.0912);
            assert_eq!(f.commission, 0.05);
            assert_eq!(f.ts, 1700);
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    assert_matches!(&evs[1], Event::OrderFilled(w) if w.fill.trade_id == "T77");
}

/// A fill envelope with no `trade_id` is DROPPED, not folded with an empty id.
///
/// This module's own doc states the contract the drop protects: the shim RE-SURFACES trades after
/// a ForexConnect reconnect (audit A3), and the core dropping the overlap by `trade_id` is the
/// only thing that stops those re-surfaced fills re-booking. An empty id did not dedup badly — it
/// skipped `ExecutionEngine`'s guard entirely, so the fill applied unconditionally and re-booked
/// its commission and realized PnL on every reconnect. Nothing else in the envelope is a stable
/// per-fill identity (`amount`/`rate`/`ts` all repeat on the re-surface), so there is nothing to
/// synthesize from and refusal is the honest answer.
#[test]
fn a_fill_envelope_without_a_trade_id_is_dropped() {
    for envelope in [
        // absent — what `unwrap_or_default()` used to turn into `""`
        serde_json::json!({"kind":"fill","instrument":"EUR/USD","side":"B",
                               "amount":10000,"rate":1.09,"commission":0.0,"ts":1700}),
        // explicitly empty — the same value, spelled by the shim
        serde_json::json!({"kind":"fill","trade_id":"","instrument":"EUR/USD","side":"B",
                               "amount":10000,"rate":1.09,"commission":0.0,"ts":1700}),
    ] {
        let evs = map_fxcm_event(&envelope, "coid-9");
        assert!(
            evs.is_empty(),
            "an id-less fill envelope must publish NOTHING — neither the bare Fill nor the \
                 OrderFilled wrap: {evs:?}"
        );
    }
}

#[test]
fn a_dropped_fill_takes_its_fsm_wrap_with_it() {
    // The bare `Event::Fill` (Account) and the `OrderFilled` wrap (FSM) are two views of ONE
    // execution. Publishing the wrap alone would advance the order FSM for a fill the Account
    // never booked, so a refusal must drop both halves.
    let v = serde_json::json!({"kind":"fill","trade_id":"","instrument":"EUR/USD","side":"B",
                                   "amount":10000,"rate":1.09,"commission":0.0,"ts":1700});
    let evs = map_fxcm_event(&v, "coid-9");
    assert!(!evs.iter().any(|e| matches!(e, Event::Fill(_))));
    assert!(!evs.iter().any(|e| matches!(e, Event::OrderFilled(_))));
}

#[test]
fn canceled_and_rejected_and_unknown() {
    let c = serde_json::json!({"kind":"canceled","ts":9});
    assert_matches!(
        &map_fxcm_event(&c, "c1")[0], Event::OrderCanceled(e) if e.client_order_id == "c1"
    );

    let r = serde_json::json!({"kind":"rejected","reason":"NO_MARGIN","ts":9});
    assert_matches!(
        &map_fxcm_event(&r, "c1")[0], Event::OrderRejected(e) if e.reason == "NO_MARGIN"
    );

    assert!(map_fxcm_event(&serde_json::json!({"kind":"heartbeat"}), "c1").is_empty());
}

/// EUR/USD's base unit size on the FXCM demo, and the multiplier every sizing test below uses.
/// Named rather than inlined because it is the whole content of the conversion: `qty` is base
/// units, the shim places `qty / BASE` lots, and the fill reports `qty` again.
const BASE: f64 = 1000.0;

/// **The conversion, and the defect it closes.** `qty` is BASE UNITS — the unit every other
/// venue in this workspace uses, the unit an `OrderRequest` carries, and the unit a fill's
/// `last_qty` comes back in. It used to be read as a LOT COUNT, so a caller sending the number
/// it holds everywhere else placed that many lots.
///
/// The row that mattered is the first one: `10000` base units is TEN lots of 1000 — it used to
/// be ten thousand lots, ten million base units, a thousand times the intended size, placed
/// with no event saying so.
#[test]
fn qty_is_base_units_and_converts_to_lots() {
    for (qty, lots) in [(1000.0, 1), (10_000.0, 10), (100_000.0, 100), (2000.0, 2)] {
        assert_eq!(
            lots_for(qty, BASE),
            Ok(lots),
            "qty {qty} base units is {lots} lot(s) of {BASE}; reading it AS lots is what placed \
                 {qty} lots = {} base units",
            qty * BASE
        );
    }
    // A base unit size of 1 (some CFD instruments) makes the two units coincide — the identity
    // case, kept so the conversion is not silently special-cased on the common multiplier.
    assert_eq!(lots_for(7.0, 1.0), Ok(7));
}

/// A size that is not an EXACT multiple of the base unit is refused, because FXCM cannot place
/// a fraction of a lot and every available substitute is a number the caller did not choose.
///
/// The first four rows are the shapes the pre-conversion expression
/// `(qty.round() as i32).max(1)` silently substituted, restated in base units; the rest are the
/// degenerate ones. Each row names what would otherwise go on the wire, because that value IS
/// the defect.
#[test]
fn a_size_that_is_not_a_whole_number_of_lots_is_refused() {
    for (qty, why) in [
        (500.0, "half a lot — the old floor placed a WHOLE one, 2x the requested size"),
        (1500.0, "one and a half lots — rounding either way is somebody else's size"),
        (999.0, "one base unit short of a lot"),
        (1000.5, "a fractional base unit"),
        (0.0, "degenerate — the old `.max(1)` made it one lot"),
        (-3000.0, "negative — the old `.max(1)` placed one lot on the side `req.side` chose"),
        (f64::NAN, "NaN as i32 == 0, then `.max(1)`"),
        (f64::INFINITY, "saturates to i32::MAX lots"),
        (1e30, "saturates to i32::MAX lots"),
    ] {
        let out = lots_for(qty, BASE);
        assert!(out.is_err(), "qty {qty} must be REFUSED: {why}");
        let reason = out.unwrap_err();
        assert!(!reason.trim().is_empty(), "every refusal must name itself: {qty}");
    }
}

/// A refusal naming the two placeable neighbours, so the caller can re-send a size THEY chose
/// rather than guess what this venue would have rounded to.
#[test]
fn an_inexact_size_names_the_placeable_sizes_on_either_side() {
    let reason = lots_for(1500.0, BASE).unwrap_err();
    assert!(reason.contains("1000"), "must name the size below: {reason}");
    assert!(reason.contains("2000"), "must name the size above: {reason}");
}

/// The multiplier itself is checked, and hard. It arrives over the FFI boundary: a zero makes
/// every division degenerate and a negative one flips the side, so neither may fall through to
/// arithmetic. `fc_base_unit_size` refuses these too — this is the Rust half, which is the half
/// a box with no SDK can run.
#[test]
fn a_degenerate_base_unit_size_refuses_every_order() {
    for base in [0.0, -1000.0, f64::NAN, f64::INFINITY] {
        assert!(
            lots_for(10_000.0, base).is_err(),
            "a base unit size of {base} must refuse the order, not size it"
        );
    }
}

/// The priced-limit refusal, and its exact boundary. A limit WITH a price is refused (the shim
/// would rest it at a pip distance from the live quote — a different price); a limit with NO
/// price still places, because resting at that distance is precisely what the caps row
/// declares; a MARKET order ignores `price` entirely, since it never rests.
#[test]
fn a_priced_limit_is_refused_and_an_unpriced_one_is_not() {
    assert!(
        preflight_request("limit", BASE, Some(1.0912), BASE).is_err(),
        "a limit carrying a price must be refused — the shim cannot honor it"
    );
    assert!(
        preflight_request("stop", BASE, Some(1.0912), BASE).is_err(),
        "every non-market kind rests the same way, so every one of them refuses a price"
    );
    assert_eq!(
        preflight_request("limit", BASE, None, BASE),
        Ok(1),
        "an unpriced limit is what the caps row declares — it must still place"
    );
    assert_eq!(
        preflight_request("market", BASE, Some(1.0912), BASE),
        Ok(1),
        "a market order never rests, so a stray price on it changes nothing"
    );
    // …and the sizing refusal still applies on the market path (the two are independent).
    assert!(preflight_request("market", BASE / 2.0, None, BASE).is_err());
}

/// The submit split must admit EXACTLY the kinds the `fxcm` caps row declares as market, and
/// nothing else — a kind this misclassifies would silently rest instead of executing (the
/// original defect) or execute instead of resting (its mirror image).
#[test]
fn market_classification_matches_the_declared_caps_row() {
    assert!(is_market("market"));
    assert!(is_market("MARKET"), "core preflight classifies case-insensitively");
    assert!(!is_market("limit"));
    assert!(!is_market("stop"));
    assert!(!is_market("take_profit"));
    assert!(!is_market(""));

    // Every kind the row declares supported is routed by this split, and `"market"` is
    // classified as market while the other declared kind is not.
    let kinds = vike_model::caps_for("fxcm").supported_order_kinds;
    assert!(kinds.contains(&"market"), "row must declare market now that the shim places it");
    assert!(kinds.contains(&"limit"));
    for k in kinds {
        assert_eq!(is_market(k), *k == "market", "{k}: split must agree with the row");
    }
}

/// The emitter split's venue half: an accepted placement carries the venue order id, a failed
/// one is a TERMINAL reject (never a vanish), and a reject NEVER ships an empty reason.
#[test]
fn a_placement_maps_to_exactly_one_terminal_or_accept() {
    let ok = map_placement("c-1", 7, Ok("v-99"));
    assert_eq!(ok.len(), 1);
    assert_matches!(&ok[0], Event::OrderAccepted(a)
            if a.client_order_id == "c-1" && a.venue_order_id.as_deref() == Some("v-99") && a.ts == 7);

    let bad = map_placement("c-2", 7, Err("NO_MARGIN"));
    assert_matches!(&bad[0], Event::OrderRejected(r)
            if r.client_order_id == "c-2" && r.reason == "NO_MARGIN");

    // An empty reason is FILLED IN: the contract is that a synthesized reject carries one.
    let blank = map_placement("c-3", 7, Err("   "));
    match &blank[0] {
        Event::OrderRejected(r) => assert!(
            !r.reason.as_str().trim().is_empty(),
            "a synthesized reject must carry a reason, got {:?}",
            r.reason
        ),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

/// Routing: a known venue order id decodes, an unknown one publishes NOTHING (the declared
/// restart hole). The unknown case is what a restart makes of every re-surfaced trade.
#[test]
fn an_envelope_for_an_unplaced_order_publishes_nothing() {
    let mut routes = HashMap::new();
    routes.insert("v-1".to_string(), "c-1".to_string());
    let fill = |oid: &str| {
        serde_json::json!({"kind":"fill","order_id":oid,"trade_id":"T1",
                               "instrument":"EUR/USD","side":"B","amount":10000,
                               "rate":1.09,"commission":0.0,"ts":0})
    };

    let known = map_drained_event(&fill("v-1"), &routes);
    assert_eq!(known.len(), 2, "a routed fill dual-publishes: {known:?}");
    assert_matches!(&known[0], Event::Fill(f) if f.client_order_id == "c-1");

    assert!(
        map_drained_event(&fill("v-unknown"), &routes).is_empty(),
        "an envelope naming an order this process never placed must publish nothing"
    );
    assert!(
        map_drained_event(&serde_json::json!({"kind":"canceled","ts":0}), &routes).is_empty(),
        "an envelope with NO order_id routes to nothing either"
    );
}

/// The two pruning predicates, and the asymmetry between them — which is the whole point.
#[test]
fn a_fill_ends_cancelability_but_does_not_close_routing() {
    let fill = map_fxcm_event(
        &serde_json::json!({"kind":"fill","trade_id":"T1","instrument":"EUR/USD","side":"B",
                                "amount":10000,"rate":1.09,"commission":0.0,"ts":0}),
        "c-1",
    );
    assert!(ends_cancelability(&fill), "a filled order has no resting order left to cancel");
    assert!(
        !closes_routing(&fill),
        "an order can trade in pieces, and a reconnect replays them — dropping the route on \
             the first fill would make the second one unroutable"
    );

    for kind in ["canceled", "rejected"] {
        let evs = map_fxcm_event(&serde_json::json!({"kind": kind, "ts": 0}), "c-1");
        assert!(closes_routing(&evs), "{kind}: nothing can follow it, so the route is dead");
        assert!(ends_cancelability(&evs), "{kind}: and so are the cancel ids");
    }

    // A heartbeat prunes nothing at all.
    assert!(!closes_routing(&[]));
    assert!(!ends_cancelability(&[]));
}
