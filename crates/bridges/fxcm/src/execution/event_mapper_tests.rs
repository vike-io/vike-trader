use super::*;
use std::assert_matches;

/// **A fill names its instrument by its USER name** (decision 0290), its kept row's (the fixtures' rows as the catalog
/// makes them): `EUR/USD` is `EURUSD`, `XAU/USD` `XAUUSD`, the mixed-case CFD `USOil` `USOIL.CFD`, `NVDA.ext`
/// `NVDA.EXT.CFD`. A name no kept row names (another case of a listed one included) is booked under the venue's own
/// spelling, never dropped and never respelled.
#[test]
fn a_fill_names_its_instrument_by_its_user_name() {
    let symbol = |instrument: &str| {
        let v = serde_json::json!({"kind": "fill", "trade_id": "T1", "instrument": instrument,
            "side": "B", "amount": 1000, "rate": 1.0, "timestamp": 1});
        match map_fxcm_event(&v, "c").first() {
            Some(Event::Fill(f)) => f.symbol.to_string(),
            other => panic!("{instrument}: {other:?}"),
        }
    };
    assert_eq!(symbol("EUR/USD"), "EURUSD");
    assert_eq!(symbol("XAU/USD"), "XAUUSD");
    assert_eq!(symbol("USOil"), "USOIL.CFD");
    assert_eq!(symbol("NVDA.ext"), "NVDA.EXT.CFD");
    assert_eq!(symbol("xau/usd"), "xau/usd", "no row of that spelling: the venue's own name");
    assert_eq!(symbol("X.Y/USD"), "X.Y/USD");
}

#[test]
fn fill_event_dual_publishes() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"kind":"fill","trade_id":"T77","instrument":"EUR/USD","side":"S",
                "amount":10000,"rate":1.0912,"commission":0.05,"timestamp":1700}"#,
    )
    .unwrap();
    let events = map_fxcm_event(&v, "order-client-id-3");
    assert_eq!(events.len(), 2);
    match &events[0] {
        Event::Fill(f) => {
            assert_eq!(f.trade_id, "T77");
            assert_eq!(f.order_client_id, "order-client-id-3");
            assert_eq!(f.symbol, "EURUSD");
            assert_eq!(f.side, -1);
            assert_eq!(f.last_qty, 10000.0);
            assert_eq!(f.last_price, 1.0912);
            assert_eq!(f.commission, 0.05);
            assert_eq!(f.timestamp, 1700);
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    assert_matches!(&events[1], Event::OrderFilled(w) if w.fill.trade_id == "T77");
}

/// A fill envelope with no `trade_id` is DROPPED, not folded with an empty id.
///
/// This module's own doc states the contract the drop protects: the shim RE-SURFACES trades after
/// a ForexConnect reconnect (audit A3), and the core dropping the overlap by `trade_id` is the
/// only thing that stops those re-surfaced fills re-booking. An empty id did not dedup badly — it
/// skipped `Engine`'s guard entirely, so the fill applied unconditionally and re-booked
/// its commission and realized PnL on every reconnect. Nothing else in the envelope is a stable
/// per-fill identity (`amount`/`rate`/`ts` all repeat on the re-surface), so there is nothing to
/// synthesize from and refusal is the honest answer.
#[test]
fn a_fill_envelope_without_a_trade_id_is_dropped() {
    for envelope in [
        // absent — what `unwrap_or_default()` used to turn into `""`
        serde_json::json!({"kind":"fill","instrument":"EUR/USD","side":"B",
                               "amount":10000,"rate":1.09,"commission":0.0,"timestamp":1700}),
        // explicitly empty — the same value, spelled by the shim
        serde_json::json!({"kind":"fill","trade_id":"","instrument":"EUR/USD","side":"B",
                               "amount":10000,"rate":1.09,"commission":0.0,"timestamp":1700}),
    ] {
        let events = map_fxcm_event(&envelope, "order-client-id-9");
        assert!(
            events.is_empty(),
            "an id-less fill envelope must publish NOTHING — neither the bare Fill nor the \
                 OrderFilled wrap: {events:?}"
        );
    }
}

#[test]
fn a_dropped_fill_takes_its_state_machine_wrap_with_it() {
    // The bare `Event::Fill` (Account) and the `OrderFilled` wrap (FSM) are two views of ONE
    // execution. Publishing the wrap alone would advance the order FSM for a fill the Account
    // never booked, so a refusal must drop both halves.
    let v = serde_json::json!({"kind":"fill","trade_id":"","instrument":"EUR/USD","side":"B",
                                   "amount":10000,"rate":1.09,"commission":0.0,"timestamp":1700});
    let events = map_fxcm_event(&v, "order-client-id-9");
    assert!(!events.iter().any(|e| matches!(e, Event::Fill(_))));
    assert!(!events.iter().any(|e| matches!(e, Event::OrderFilled(_))));
}

#[test]
fn canceled_and_rejected_and_unknown() {
    let c = serde_json::json!({"kind":"canceled","timestamp":9});
    assert_matches!(
        &map_fxcm_event(&c, "c1")[0], Event::OrderCanceled(e) if e.order_client_id == "c1"
    );

    assert!(map_fxcm_event(&serde_json::json!({"kind":"heartbeat"}), "c1").is_empty());
}

/// **FX-4: a venue rejection of an ACCEPTED order ends it ONCE, cancelled, in the venue's words.** Every decoded envelope
/// names an accepted order, and the FSM refuses `Accepted -> Rejected`: the `OrderRejected` this used to emit was dropped
/// and the order stayed live in the book. Folded through the REAL `OrderRecord` FSM.
#[test]
fn a_rejection_after_the_accept_ends_the_order_cancelled_in_the_venues_words() {
    let r = serde_json::json!({"kind":"rejected","order_id":"812345679","reason":"rejected","timestamp":9});
    let events = map_fxcm_event(&r, "c1");
    let [Event::OrderCanceled(c)] = &events[..] else { panic!("one OrderCanceled: {events:?}") };
    assert_eq!(c.order_client_id, "c1");
    assert_eq!(c.timestamp, 9);
    assert_eq!(
        c.reason,
        "fxcm: the venue rejected the order after accepting it (Orders table status R: rejected)"
    );
    assert_eq!(c.refusal_id.as_deref(), Some("order.venue_refused"));
    assert!(closes_routing(&events) && ends_cancelability(&events));

    let mut order = vike_model::OrderRecord::new(vike_model::OrderRequest {
        order_client_id: "c1".into(),
        venue: "fxcm".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    });
    for e in [
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: "c1".into(),
            timestamp: 1,
        }),
        map_placement("c1", 2, Ok("812345679")).remove(0),
    ]
    .iter()
    .chain(&events)
    {
        assert!(order.apply(e).is_ok(), "{e:?} is a legal edge");
    }
    assert_eq!(order.status, vike_model::OrderStatus::Canceled);
}

/// **FX-1: FXCM's code is read out of a failed request's words, and only an internal error or a code FXCM's list does not
/// name leaves a write open.** The words are verbatim `onRequestFailed` text (`ORA-20102: Access Violation` is the measured
/// one, `tests/fixtures/order_permission_probe.json`; the `ORA-20114` words below were measured on the demo too);
/// `20001` and `20131` are FXCM's documented answers, never captured; `20103` is on no list.
#[test]
fn a_failed_write_is_unknown_on_an_internal_error_or_an_unlisted_code() {
    use vike_utility::errors::FailureOutcome;
    assert_eq!(ora_code("ORA-20102: Access Violation"), Some(20102));
    assert_eq!(ora_code("FXCM native error -6: ORA-20001: Internal system error"), Some(20001));
    assert_eq!(ora_code("ORA-20131"), Some(20131));
    for none in ["request failed", "ORA-", "ORA-x", ""] {
        assert_eq!(ora_code(none), None, "{none:?}");
    }
    for unknown in ["ORA-20001: Internal system error", "ORA-20103: order not found"] {
        assert_eq!(venue_failure_outcome(unknown), FailureOutcome::OutcomeUnknown, "{unknown}");
    }
    assert_eq!(venue_failure_outcome("ORA-20131: account is locked"), FailureOutcome::NotNow);
    for refused in [
        "ORA-20102: Access Violation",
        "ORA-20114: Cannot place more than one order of this type within order group",
        "ORA-20186: order does not exist",
        "request failed",
    ] {
        assert_eq!(venue_failure_outcome(refused), FailureOutcome::Refused, "{refused}");
    }
    assert!(write_left_open(REQUEST_FAILED, "ORA-20001: Internal system error"));
    assert!(write_left_open(SENT_NO_ANSWER, "") && write_left_open(SENT_NO_ORDER_ID, ""));
    assert!(
        !write_left_open(REQUEST_FAILED, "ORA-20131: account is locked"),
        "not now: refused now"
    );
    assert!(!write_left_open(REQUEST_FAILED, "ORA-20102: Access Violation"));
    assert!(!write_left_open(-8, "ORA-20001"), "only the venue's failure is read for its code");
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
fn quantity_is_base_units_and_converts_to_lots() {
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
/// arithmetic. `forex_connect_base_unit_size` refuses these too — this is the Rust half, which is the half
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

/// A PRICED limit is no longer refused: its price is the RATE the shim rests the entry at
/// (`forex_connect_place_entry_at`). The pre-ABI-2 shim could only rest at a pip distance from the quote, which
/// is why #1470 refused it; the boundary now is a price that cannot BE a rate, and a kind that is
/// neither market nor limit. A market order ignores `price` entirely, since it never rests.
#[test]
fn a_priced_limit_passes_and_only_a_non_rate_or_a_wrong_kind_is_refused() {
    assert_eq!(
        preflight_request(&OrderType::Limit, BASE, Some(1.0912), BASE),
        Ok(1),
        "a limit carrying a price is placed at that rate"
    );
    assert_eq!(
        preflight_request(&OrderType::Limit, BASE, None, BASE),
        Ok(1),
        "an unpriced limit is what the caps row declares — it must still place"
    );
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let reason = preflight_request(&OrderType::Limit, BASE, Some(bad), BASE)
            .expect_err("a price that is not a positive finite rate must be refused");
        assert!(reason.contains("rate"), "the refusal must say what is wrong: {reason}");
    }
    // Refused BY NAME, never placed as another kind (a kind added later is refused the same way).
    for kind in [
        OrderType::StopLimit,
        OrderType::TakeProfit,
        OrderType::Unrecognized("market_if_touched".to_string()),
    ] {
        for price in [None, Some(1.0912)] {
            let reason = preflight_request(&kind, BASE, price, BASE).expect_err(
                "only market, limit and a trade's stop are placeable — these would rest as a plain \
                 limit entry",
            );
            assert!(reason.contains(&format!("`{kind}`")), "{kind:?} @ {price:?}: {reason}");
        }
    }
    // A `stop` is placeable since 2026-10-26, as the protective order of a trade (`crate::execution::exit`): the
    // preflight only sizes it (its trigger, its reduce-only flag and its account are the router's
    // and the binder's to judge), and the sizing refusal still applies to it.
    assert_eq!(preflight_request(&OrderType::Stop, BASE, None, BASE), Ok(1));
    assert!(preflight_request(&OrderType::Stop, BASE / 2.0, None, BASE).is_err());
    assert_eq!(
        preflight_request(&OrderType::Market, BASE, Some(1.0912), BASE),
        Ok(1),
        "a market order never rests, so a stray price on it changes nothing"
    );
    // …and the sizing refusal still applies on both paths (the checks are independent).
    assert!(preflight_request(&OrderType::Market, BASE / 2.0, None, BASE).is_err());
    assert!(preflight_request(&OrderType::Limit, BASE / 2.0, Some(1.0912), BASE).is_err());
}

/// The exec thread asks `kind_refusal` before the subscription and base-unit reads: nothing for the three
/// placeable kinds, and for every other kind the very sentence `preflight_request` refuses it with, naming it.
#[test]
fn a_kind_is_refused_before_any_session_read_in_the_preflights_own_words() {
    for kind in [OrderType::Market, OrderType::Limit, OrderType::Stop] {
        assert_eq!(kind_refusal(&kind), None, "{kind}");
    }
    for kind in [
        OrderType::StopLimit,
        OrderType::TakeProfit,
        OrderType::Unrecognized("market_if_touched".to_string()),
    ] {
        let why = kind_refusal(&kind).expect("refused");
        assert!(why.contains(&format!("`{kind}`")), "{why}");
        assert_eq!(preflight_request(&kind, BASE, None, BASE), Err(why), "{kind}");
    }
}

/// An amend asks the shim for ONE thing, a new rate. A different size is refused (the order would
/// keep its old size while the caller believed it had the new one), and so is an amend that names
/// no price or a price that cannot be a rate.
#[test]
fn an_amend_plans_a_rate_and_refuses_what_it_cannot_do() {
    assert_eq!(plan_amend(BASE, None, Some(1.0925)), Ok(1.0925));
    assert_eq!(
        plan_amend(BASE, Some(BASE), Some(1.0925)),
        Ok(1.0925),
        "the order's own size is no change"
    );
    let (why, refusal) = plan_amend(BASE, Some(2.0 * BASE), Some(1.0925)).unwrap_err();
    assert!(why.contains("price"));
    assert_eq!(refusal, OrderRefusalId::VenueCapability, "a size amend is what fxcm cannot do");
    assert_eq!(plan_amend(BASE, Some(BASE), None).map_err(|e| e.1), Err(OrderRefusalId::NotSent));
    assert!(plan_amend(BASE, None, None).is_err());
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let refused = plan_amend(BASE, None, Some(bad)).map_err(|e| e.1);
        assert_eq!(refused, Err(OrderRefusalId::NotSent), "{bad}");
    }
}

/// An amend ends in exactly ONE event: `OrderModified` under the SAME venue order id, or the
/// non-terminal `OrderModifyRejected` — which never ships an empty reason.
#[test]
fn an_amend_maps_to_one_modified_or_one_non_terminal_rejection() {
    match map_amend("c-1", Some("v-9"), 5, 1.0925, Ok(())) {
        Event::OrderModified(m) => {
            assert_eq!(m.order_client_id, "c-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("v-9"), "EditOrder edits in place");
            assert_eq!(m.new_price, Some(1.0925));
            assert_eq!(m.new_qty, None, "a price-only amend leaves the size alone");
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    for reason in ["the venue said no", "", "   "] {
        let refused = Err((reason, OrderRefusalId::VenueRefused));
        match map_amend("c-1", Some("v-9"), 5, 1.0925, refused) {
            Event::OrderModifyRejected(r) => {
                assert_eq!(r.order_client_id, "c-1");
                assert!(!r.reason.trim().is_empty(), "a rejection always carries a reason");
                assert_eq!(r.refusal_id.as_deref(), Some(OrderRefusalId::VenueRefused.word()));
            }
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        }
    }
}

/// The submit split must admit EXACTLY the kinds the `fxcm` caps row declares as market, and
/// nothing else — a kind this misclassifies would silently rest instead of executing (the
/// original defect) or execute instead of resting (its mirror image).
#[test]
fn market_classification_matches_the_declared_capabilities_row() {
    assert!(is_market(&OrderType::Market));
    // a STORED upper-case word is not ours: `Unrecognized`, never market (and refused outright)
    assert!(!is_market(&OrderType::from_stored("MARKET")));
    assert!(preflight_request(&OrderType::from_stored("MARKET"), BASE, None, BASE).is_err());
    assert!(!is_market(&OrderType::Limit));
    assert!(!is_market(&OrderType::Stop));
    assert!(!is_market(&OrderType::TakeProfit));
    assert!(!is_market(&OrderType::from_stored("")));

    // Every kind the row declares supported is routed by this split, and `"market"` is
    // classified as market while the other declared kind is not.
    let kinds = vike_venues::capabilities_for("fxcm").supported_order_kinds;
    assert!(kinds.contains(&"market"), "row must declare market now that the shim places it");
    assert!(kinds.contains(&"limit"));
    for k in kinds {
        assert_eq!(
            is_market(&OrderType::from_stored(k)),
            *k == "market",
            "{k}: split must agree with the row"
        );
    }
}

/// The emitter split's venue half: an accepted placement carries the venue order id, a failed
/// one is a TERMINAL reject (never a vanish), and a reject NEVER ships an empty reason.
#[test]
fn a_placement_maps_to_exactly_one_terminal_or_accept() {
    let ok = map_placement("c-1", 7, Ok("v-99"));
    assert_eq!(ok.len(), 1);
    assert_matches!(&ok[0], Event::OrderAccepted(a)
            if a.order_client_id == "c-1" && a.venue_order_id.as_deref() == Some("v-99") && a.timestamp == 7);

    // The venue's refusal carries its catalog id (refusal step 2's `order.venue_refused`).
    let bad = map_placement("c-2", 7, Err(("NO_MARGIN", OrderRefusalId::VenueRefused)));
    assert_matches!(&bad[0], Event::OrderRejected(r)
            if r.order_client_id == "c-2" && r.reason == "NO_MARGIN"
                && r.refusal_id.as_deref() == Some(OrderRefusalId::VenueRefused.word()));

    // An empty reason is FILLED IN: the contract is that a synthesized reject carries one.
    let blank = map_placement("c-3", 7, Err(("   ", OrderRefusalId::NotSent)));
    match &blank[0] {
        Event::OrderRejected(r) => assert!(
            !r.reason.as_str().trim().is_empty(),
            "a synthesized reject must carry a reason, got {:?}",
            r.reason
        ),
        other => panic!("expected OrderRejected, got {other:?}"),
    }

    // A placement that may rest is no refusal (decision 0303): nothing ends the order.
    assert!(map_placement("c-4", 7, Err(("no answer", OrderRefusalId::OutcomeUnknown))).is_empty());
}

/// What a shim's failed placement was, by its code and FXCM's words: the venue's own refusal (`-6`), one left open (a
/// post-send code, an internal error), and the shim's refusal before the send (`-3`, no offer).
#[test]
fn a_failed_placement_is_named_by_what_it_was() {
    assert_eq!(
        placement_refusal(REQUEST_FAILED, "ORA-20102: Access Violation"),
        OrderRefusalId::VenueRefused
    );
    assert_eq!(
        placement_refusal(REQUEST_FAILED, "ORA-20001: Internal"),
        OrderRefusalId::OutcomeUnknown
    );
    assert_eq!(placement_refusal(SENT_NO_ANSWER, "no answer"), OrderRefusalId::OutcomeUnknown);
    assert_eq!(placement_refusal(-3, "offer not found"), OrderRefusalId::NotSent);
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
                               "rate":1.09,"commission":0.0,"timestamp":0})
    };

    let known = map_drained_event(&fill("v-1"), &routes);
    assert_eq!(known.len(), 2, "a routed fill dual-publishes: {known:?}");
    assert_matches!(&known[0], Event::Fill(f) if f.order_client_id == "c-1");

    assert!(
        map_drained_event(&fill("v-unknown"), &routes).is_empty(),
        "an envelope naming an order this process never placed must publish nothing"
    );
    assert!(
        map_drained_event(&serde_json::json!({"kind":"canceled","timestamp":0}), &routes)
            .is_empty(),
        "an envelope with NO order_id routes to nothing either"
    );
}

/// The two pruning predicates, and the asymmetry between them — which is the whole point.
#[test]
fn a_fill_ends_cancelability_but_does_not_close_routing() {
    let fill = map_fxcm_event(
        &serde_json::json!({"kind":"fill","trade_id":"T1","instrument":"EUR/USD","side":"B",
                                "amount":10000,"rate":1.09,"commission":0.0,"timestamp":0}),
        "c-1",
    );
    assert!(ends_cancelability(&fill), "a filled order has no resting order left to cancel");
    assert!(
        !closes_routing(&fill),
        "an order can trade in pieces, and a reconnect replays them — dropping the route on \
             the first fill would make the second one unroutable"
    );

    for kind in ["canceled", "rejected"] {
        let events = map_fxcm_event(&serde_json::json!({"kind": kind, "timestamp": 0}), "c-1");
        assert!(closes_routing(&events), "{kind}: nothing can follow it, so the route is dead");
        assert!(ends_cancelability(&events), "{kind}: and so are the cancel ids");
    }

    // A heartbeat prunes nothing at all.
    assert!(!closes_routing(&[]));
    assert!(!ends_cancelability(&[]));
}

/// The venue-rate echo warns past HALF A TICK: the requested 1.10300 against the venue's own 1.10300
/// (or float noise) is quiet, a whole tick away on a 5-digit pair is not, and an unknown grid
/// (`tick = 0.0`) still catches a real difference without crying on noise.
#[test]
fn venue_rate_differs_past_half_a_tick() {
    let tick = 1e-5;
    assert!(!venue_rate_differs(1.10300, 1.10300, tick));
    assert!(!venue_rate_differs(1.10300, 1.10300 + 1e-12, tick));
    assert!(venue_rate_differs(1.10300, 1.10250, tick), "the amended rate is 50 ticks away");
    assert!(venue_rate_differs(1.10300, 1.10301, tick), "one whole tick is a difference");
    assert!(venue_rate_differs(1.10300, 1.10250, 0.0));
    assert!(!venue_rate_differs(1.10300, 1.10300, 0.0));
    assert!(venue_rate_differs(1.10300, f64::NAN, tick), "a non-finite venue rate is a difference");
}

/// Only the two post-send codes are "unknown". `-7`/`-8` also come back BEFORE anything is sent
/// (`findOffer`'s refresh), `-6` is the venue's own refusal and `-10` is the shim's grid refusal:
/// all four stay definite.
#[test]
fn only_the_post_send_codes_are_an_unknown_outcome() {
    assert!(outcome_is_unknown(SENT_NO_ANSWER));
    assert!(outcome_is_unknown(SENT_NO_ORDER_ID));
    for definite in [-1, -2, -3, -4, -5, -6, -7, -8, -10, -14, -15, -16, -101] {
        assert!(!outcome_is_unknown(definite), "{definite} is a definite refusal");
    }
}

/// An amend whose reply was lost is settled by the order's own rate: the requested one means it
/// applied (and the event carries the venue's rate), any other is a refusal that names the real
/// rate, and an order that cannot be read back says the rate is unknown rather than guessing.
#[test]
fn a_lost_amend_reply_is_settled_by_the_rate_the_venue_holds() {
    let tick = 1e-5;
    assert_eq!(
        resolve_unknown_amend(1.10250, "timeout", Ok(Some((1.10250, tick)))),
        Ok(1.10250),
        "the venue holds the requested rate: the amend applied"
    );
    let stale = resolve_unknown_amend(1.10250, "timeout", Ok(Some((1.10300, tick))));
    let text = stale.expect_err("the order still rests at the old rate");
    assert!(text.contains("1.103") && text.contains("1.1025"), "names both rates: {text}");
    // A stale read-back is NOT a verdict: the broker may apply the edit after the read.
    assert!(
        text.contains("NOT confirmed")
            && text.contains("may still apply")
            && text.contains("reconcile decides"),
        "a stale rate must not read as a refusal that settled the matter: {text}"
    );
    assert!(!text.contains("now rests"), "no claim about where the order rests: {text}");
    for unreadable in [Ok(None), Err("timeout".to_string())] {
        let text = resolve_unknown_amend(1.10250, "timeout", unreadable)
            .expect_err("an unreadable order is never read as applied");
        assert!(text.contains("UNKNOWN"), "says the rate is unknown: {text}");
    }
}

/// Only a read-back that shows the REQUESTED rate settles a lost amend reply at once; every other
/// answer (the old rate, an unreadable order, a shim with no read-back) earns a second look.
#[test]
fn only_a_read_back_at_the_requested_rate_skips_the_second_look() {
    let tick = 1e-5;
    assert!(!amend_needs_reread(1.10250, &Ok(Some((1.10250, tick)))));
    assert!(!amend_needs_reread(1.10250, &Ok(Some((1.102504, tick)))), "within half a tick");
    assert!(amend_needs_reread(1.10250, &Ok(Some((1.10300, tick)))), "the old rate");
    assert!(amend_needs_reread(1.10250, &Ok(None)));
    assert!(amend_needs_reread(1.10250, &Err("timeout".to_string())));
}
