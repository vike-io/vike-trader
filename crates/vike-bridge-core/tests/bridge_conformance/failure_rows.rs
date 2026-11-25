//! Rows 9 and 10 (`bridge_conformance.rs`'s module doc): a fill a decoder must refuse, and a venue's own refusal of an
//! order. Both run every covered bridge's REAL decoder; each has a control test that shows the check refusing a decoder
//! that breaks the contract.

use serde_json::Value;

use vike_bridge_core::event_map::{fill_is_bookable, fill_report_is_bookable};
use vike_model::events::{Event, FillEvent, LiquiditySide, OrderRejected, PositionSide};
use vike_model::{FillReport, OrderRequest, OrderStatus};

use super::venues::patch_f64;
use super::{
    ContractOrder, FillFields, FillFrame, FillJoin, RESTART_VENUE_ID, VenueRejectRow,
    covered_bridges, submitted,
};

/// The order client id every row-9 and row-10 fixture is built for.
pub(super) const FAILURE_ORDER_CLIENT_ID: &str = "failure-order-client-id-1";

// ===================================================================================================
// Row 9: MalformedFill.
// ===================================================================================================

/// `fixture` with every field `pointers` name removed; `Err` for a pointer that names no field, which would prove
/// nothing.
fn without(fixture: &Value, pointers: &[String]) -> Result<Value, String> {
    let mut out = fixture.clone();
    for pointer in pointers {
        let (parent, leaf) =
            pointer.rsplit_once('/').ok_or_else(|| format!("`{pointer}` is no JSON pointer"))?;
        let removed =
            out.pointer_mut(parent).and_then(Value::as_object_mut).and_then(|o| o.remove(leaf));
        if removed.is_none() {
            return Err(format!("`{pointer}` names no field of the fixture {fixture}"));
        }
    }
    Ok(out)
}

/// `fixture` with every field `pointers` name set to 0, in the field's own wire type.
fn zeroed(fixture: &Value, pointers: &[String]) -> Value {
    let mut out = fixture.clone();
    for pointer in pointers {
        patch_f64(&mut out, pointer, 0.0);
    }
    out
}

/// The malformed twins of `fixture` row 9 runs, each named for what it lacks.
fn malformed(fixture: &Value, fields: &FillFields) -> Result<Vec<(&'static str, Value)>, String> {
    Ok(vec![
        ("no price", without(fixture, &fields.price)?),
        ("no quantity", without(fixture, &fields.quantity)?),
        ("no execution id", without(fixture, &fields.execution_id)?),
        ("a price of 0", zeroed(fixture, &fields.price)),
        ("a quantity of 0", zeroed(fixture, &fields.quantity)),
    ])
}

/// Row 9 over a fill decoder: the intact frame decodes to bookable fills, and every malformed twin to NO event that
/// carries a fill, neither the bare fill (it would be booked at 0) nor its wrap (the order would end filled with
/// nothing booked).
fn check_malformed_fill(
    fill: &FillFrame,
    decode: impl Fn(&Value) -> Vec<Event>,
) -> Result<(), String> {
    let intact = decode(&fill.frame);
    let fills: Vec<&FillEvent> = intact.iter().filter_map(Event::fill).collect();
    if fills.is_empty() || !fills.iter().all(|f| fill_is_bookable(f)) {
        return Err(format!("the intact fixture decodes to no bookable fill: {intact:?}"));
    }
    for (lacks, frame) in malformed(&fill.frame, &fill.fields)? {
        let decoded = decode(&frame);
        if decoded.iter().any(|e| e.fill().is_some()) {
            return Err(format!("a fill with {lacks} is not refused: {decoded:?}"));
        }
    }
    Ok(())
}

/// Row 9 over a REST fill read: the intact body parses to bookable rows, every malformed twin to no row.
fn check_malformed_fill_report(
    body: &Value,
    fields: &FillFields,
    parse: fn(&Value) -> Vec<FillReport>,
) -> Result<(), String> {
    let intact = parse(body);
    if intact.is_empty() || !intact.iter().all(fill_report_is_bookable) {
        return Err(format!("the intact fixture parses to no bookable fill row: {intact:?}"));
    }
    for (lacks, body) in malformed(body, fields)? {
        let parsed = parse(&body);
        if !parsed.is_empty() {
            return Err(format!("a fill row with {lacks} is not skipped: {parsed:?}"));
        }
    }
    Ok(())
}

/// **Row 9, MalformedFill.** Every covered bridge's fill decoder refuses a fill that states no price, no quantity or
/// no execution id (or a price or a quantity of 0), and so does its REST fill read where [`FillJoin`] has a fixture.
#[test]
fn a_fill_with_no_price_quantity_or_execution_id_is_refused_all_covered_venues() {
    for bridge in covered_bridges() {
        check_malformed_fill(&bridge.malformed_fill(), |frame| bridge.decode(frame))
            .unwrap_or_else(|why| panic!("[{} × malformed-fill] {why}", bridge.venue()));
        if let FillJoin::Row { body, parse, fields, .. } = bridge.fill_join() {
            check_malformed_fill_report(
                &body(FAILURE_ORDER_CLIENT_ID, RESTART_VENUE_ID),
                &fields,
                parse,
            )
            .unwrap_or_else(|why| panic!("[{} × malformed-fill, REST] {why}", bridge.venue()));
        }
    }
}

/// A fill of `{"px", "qty", "id"}` as a decoder that DEFAULTS an absent field would build it.
fn defaulted_fill(frame: &Value, synthesize_id: bool) -> Vec<Event> {
    let number = |key: &str| frame.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let id = match frame.get("id").and_then(Value::as_str) {
        Some(id) => id,
        None if synthesize_id => "minted",
        None => return Vec::new(),
    };
    vec![Event::Fill(FillEvent {
        trade_id: vike_model::events::TradeId::new(id).expect("a non-empty id"),
        order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
        venue: "probe".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: number("qty"),
        last_price: number("px"),
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })]
}

/// The row bites: a decoder that books an absent price or quantity at 0, or mints an id for a fill that states none,
/// is refused; one that refuses all three passes; a pointer that names no field of the fixture proves nothing and is
/// refused too.
#[test]
fn a_decoder_that_books_a_defaulted_fill_fails_the_malformed_fill_row() {
    let fill = || FillFrame {
        frame: serde_json::json!({"px": 100.0, "qty": 2.0, "id": "t1"}),
        fields: FillFields::at("", &["px"], &["qty"], &["id"]),
    };
    let refusing = |frame: &Value| {
        defaulted_fill(frame, false)
            .into_iter()
            .filter(|e| e.fill().is_some_and(fill_is_bookable))
            .collect::<Vec<_>>()
    };
    assert_eq!(check_malformed_fill(&fill(), refusing), Ok(()));
    let books_zero = check_malformed_fill(&fill(), |frame| defaulted_fill(frame, false));
    assert!(books_zero.as_ref().is_err_and(|why| why.contains("no price")), "{books_zero:?}");
    let mints = |frame: &Value| {
        defaulted_fill(frame, true)
            .into_iter()
            .filter(|e| e.fill().is_some_and(fill_is_bookable))
            .collect::<Vec<_>>()
    };
    let minted = check_malformed_fill(&fill(), mints);
    assert!(minted.as_ref().is_err_and(|why| why.contains("no execution id")), "{minted:?}");

    let stray = FillFrame { fields: FillFields::at("", &["price"], &["qty"], &["id"]), ..fill() };
    assert!(
        check_malformed_fill(&stray, refusing).is_err_and(|why| why.contains("names no field"))
    );
    let nothing = check_malformed_fill(&fill(), |_| Vec::new());
    assert!(nothing.is_err_and(|why| why.contains("intact")), "a fixture that decodes to nothing");
}

// ===================================================================================================
// Row 10: VenueReject.
// ===================================================================================================

/// Row 10 over a decoder: the venue's refusal decodes to exactly ONE terminal, an `OrderRejected` of the order carrying
/// `words` and the refusal id `order.venue_refused`, which folds once from `Submitted` to `Rejected` (and a replay of it adds no second terminal); `unknown`
/// decodes to no terminal and no accept.
fn check_venue_reject(
    order: OrderRequest,
    decode: impl Fn(&Value) -> Vec<Event>,
    frame: &Value,
    words: &str,
    unknown: Option<&Value>,
) -> Result<(), String> {
    let order_client_id = order.order_client_id.clone();
    let decoded = decode(frame);
    let terminals: Vec<&Event> = decoded.iter().filter(|e| e.ended_order().is_some()).collect();
    let [
        Event::OrderRejected(OrderRejected {
            order_client_id: rejected, reason, refusal_id, ..
        }),
    ] = terminals.as_slice()
    else {
        return Err(format!("the refusal decodes to {decoded:?}, not exactly one OrderRejected"));
    };
    if *rejected != order_client_id {
        return Err(format!("the refusal ends {rejected:?}, not the order {order_client_id:?}"));
    }
    if words.trim().is_empty() || !reason.as_str().contains(words) {
        return Err(format!("the reject reads {reason:?}, not the venue's words {words:?}"));
    }
    // The refusal catalog's id for a refusal in the venue's own words (refusal step 2).
    let venue_refused = vike_model::OrderRefusalId::VenueRefused.word();
    if refusal_id.as_deref() != Some(venue_refused) {
        return Err(format!("the reject's refusal id is {refusal_id:?}, not {venue_refused:?}"));
    }
    let mut folded = ContractOrder::new(order);
    folded.fold(&submitted(&order_client_id));
    for _replay in 0..2 {
        for event in &decoded {
            folded.fold(event);
        }
    }
    if folded.status() != OrderStatus::Rejected
        || folded.terminals_applied != 1
        || folded.dropped_terminal_on_live != 0
    {
        return Err(format!(
            "the refusal folds to {:?} with {} terminal(s) and {} dropped on a live order",
            folded.status(),
            folded.terminals_applied,
            folded.dropped_terminal_on_live
        ));
    }
    if let Some(unknown) = unknown {
        let decoded = decode(unknown);
        if decoded.iter().any(|e| e.ended_order().is_some() || matches!(e, Event::OrderAccepted(_)))
        {
            return Err(format!(
                "an OUTCOME UNKNOWN answer must end nothing and accept nothing: {decoded:?}"
            ));
        }
    }
    Ok(())
}

/// Row 10 over a venue whose seam refuses an order only AFTER taking it ([`VenueRejectRow::RefusedAfterTaking`]): the
/// answer that took the order (`taken_by`, when it is not `frame` itself) and the refusal decode to an accept of the
/// order and then exactly ONE end, an `OrderCanceled` of the order carrying `words` and the refusal id
/// `order.venue_refused`, and they end the order once, with no terminal dropped, whether the engine holds it
/// `Submitted` or `Accepted` (a replay adds no second end).
fn check_refused_after_taking(
    order: OrderRequest,
    decode: impl Fn(&Value) -> Vec<Event>,
    taken_by: Option<&Value>,
    frame: &Value,
    words: &str,
) -> Result<(), String> {
    let order_client_id = order.order_client_id.clone();
    let mut decoded = taken_by.map(&decode).unwrap_or_default();
    decoded.extend(decode(frame));
    let terminals: Vec<&Event> = decoded.iter().filter(|e| e.ended_order().is_some()).collect();
    let [Event::OrderCanceled(canceled)] = terminals.as_slice() else {
        return Err(format!(
            "the refusal after taking decodes to {decoded:?}, not exactly one OrderCanceled"
        ));
    };
    if canceled.order_client_id != order_client_id {
        return Err(format!(
            "the refusal ends {:?}, not the order {order_client_id:?}",
            canceled.order_client_id
        ));
    }
    if words.trim().is_empty() || !canceled.reason.as_str().contains(words) {
        return Err(format!(
            "the cancel reads {:?}, not the venue's words {words:?}",
            canceled.reason
        ));
    }
    let venue_refused = vike_model::OrderRefusalId::VenueRefused.word();
    if canceled.refusal_id.as_deref() != Some(venue_refused) {
        return Err(format!(
            "the cancel's refusal id is {:?}, not {venue_refused:?}",
            canceled.refusal_id
        ));
    }
    let accept = decoded
        .iter()
        .position(|e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == order_client_id));
    let cancel = decoded.iter().position(|e| matches!(e, Event::OrderCanceled(_)));
    if !matches!((accept, cancel), (Some(accept), Some(cancel)) if accept < cancel) {
        return Err(format!("the cancel is not preceded by the accept it implies: {decoded:?}"));
    }
    let acknowledged = Event::OrderAccepted(vike_model::events::OrderAccepted {
        order_client_id: order_client_id.clone(),
        venue_order_id: None,
        timestamp: 1,
    });
    for (held, before) in [("Submitted", vec![]), ("Accepted", vec![acknowledged])] {
        let mut folded = ContractOrder::new(order.clone());
        folded.fold(&submitted(&order_client_id));
        for event in before.iter().chain(&decoded).chain(&decoded) {
            folded.fold(event);
        }
        if folded.status() != OrderStatus::Canceled
            || folded.terminals_applied != 1
            || folded.dropped_terminal_on_live != 0
        {
            return Err(format!(
                "held {held}, the refusal folds to {:?} with {} terminal(s) and {} dropped on a live order",
                folded.status(),
                folded.terminals_applied,
                folded.dropped_terminal_on_live
            ));
        }
    }
    Ok(())
}

/// **Row 10, VenueReject.** Every covered bridge's own refusal of an order ends it once, in the venue's words, or is
/// deferred with the reason it does not cross the `decode` seam. A venue whose stream refuses only after taking the
/// order also shows its refusal before taking it, through the same seam, `order.venue_refused` included.
#[test]
fn a_venues_refusal_ends_its_order_once_in_the_venues_words_all_covered_venues() {
    for bridge in covered_bridges() {
        match bridge.venue_reject(FAILURE_ORDER_CLIENT_ID) {
            VenueRejectRow::Frame { frame, words, unknown } => check_venue_reject(
                bridge.order(FAILURE_ORDER_CLIENT_ID),
                |frame| bridge.decode(frame),
                &frame,
                &words,
                unknown.as_ref(),
            )
            .unwrap_or_else(|why| panic!("[{} × venue-reject] {why}", bridge.venue())),
            VenueRejectRow::RefusedAfterTaking { taken_by, frame, words, why, before } => {
                assert!(!why.trim().is_empty(), "{}: the declaration says why", bridge.venue());
                check_refused_after_taking(
                    bridge.order(FAILURE_ORDER_CLIENT_ID),
                    |frame| bridge.decode(frame),
                    taken_by.as_ref(),
                    &frame,
                    &words,
                )
                .unwrap_or_else(|why| panic!("[{} × venue-reject] {why}", bridge.venue()));
                // The refusal BEFORE taking the order is held as a refusal frame is, its catalog id included.
                check_venue_reject(
                    bridge.order(FAILURE_ORDER_CLIENT_ID),
                    |frame| bridge.decode(frame),
                    &before.frame,
                    &before.words,
                    before.unknown.as_ref(),
                )
                .unwrap_or_else(|why| {
                    panic!("[{} × venue-reject before taking] {why}", bridge.venue())
                });
            }
            VenueRejectRow::Deferred(why) => {
                assert!(!why.trim().is_empty(), "{}: a deferred refusal says why", bridge.venue());
            }
        }
    }
}

/// The row bites: a decoder that drops the venue's words or the refusal id, ends the order some other way, says
/// nothing, or answers an OUTCOME UNKNOWN frame with a reject is refused.
#[test]
fn a_refusal_that_loses_the_order_or_the_venues_words_fails_the_venue_reject_row() {
    let order = || super::limit_order(FAILURE_ORDER_CLIENT_ID, "probe", "BTCUSDT");
    let refusal = serde_json::json!({"refused": "INSUFFICIENT_BALANCE"});
    let unknown = serde_json::json!({"status": "timeout"});
    let reject = |reason: &str| {
        vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
            reason: reason.to_string().into(),
            timestamp: 1,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })]
    };
    let honest = |frame: &Value| match frame.get("refused").and_then(Value::as_str) {
        Some(words) => reject(words),
        None => Vec::new(),
    };
    let check = |decode: &dyn Fn(&Value) -> Vec<Event>| {
        check_venue_reject(order(), decode, &refusal, "INSUFFICIENT_BALANCE", Some(&unknown))
    };
    assert_eq!(check(&honest), Ok(()));
    assert!(check(&|_: &Value| reject("rejected")).is_err_and(|why| why.contains("venue's words")));
    assert!(check(&|_: &Value| Vec::new()).is_err_and(|why| why.contains("exactly one")));
    let unidentified = |frame: &Value| {
        let mut events = honest(frame);
        if let Some(Event::OrderRejected(r)) = events.first_mut() {
            r.refusal_id = None;
        }
        events
    };
    assert!(check(&unidentified).is_err_and(|why| why.contains("refusal id")));
    let canceled = |_: &Value| {
        vec![Event::OrderCanceled(vike_model::events::OrderCanceled {
            order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
            reason: "INSUFFICIENT_BALANCE".to_string().into(),
            timestamp: 1,
            post_only_would_cross: false,
            refusal_id: None,
        })]
    };
    assert!(check(&canceled).is_err_and(|why| why.contains("exactly one")));
    let twice = |frame: &Value| honest(frame).into_iter().chain(reject("x")).collect::<Vec<_>>();
    assert!(check(&twice).is_err_and(|why| why.contains("exactly one")));
    let unknown_rejects = |frame: &Value| match frame.get("refused") {
        Some(_) => honest(frame),
        None => reject("timeout"),
    };
    assert!(check(&unknown_rejects).is_err_and(|why| why.contains("OUTCOME UNKNOWN")));
}

/// The declared form bites as hard: a refusal after taking the order that ends it with a reject (the order table has
/// no `Accepted -> Rejected` edge, so a held order would never end), ends nothing, ends it twice, drops the venue's
/// words or the refusal id, or skips the accept is refused.
#[test]
fn a_refusal_after_taking_that_does_not_end_the_order_once_fails_the_venue_reject_row() {
    let order = || super::limit_order(FAILURE_ORDER_CLIENT_ID, "probe", "BTCUSDT");
    let frame = serde_json::json!({"event": "rejected"});
    let accepted = || {
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
            venue_order_id: Some("v-1".to_string().into()),
            timestamp: 1,
        })
    };
    let canceled = |reason: &str| {
        Event::OrderCanceled(vike_model::events::OrderCanceled {
            order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
            reason: reason.to_string().into(),
            timestamp: 1,
            post_only_would_cross: false,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })
    };
    let rejected = || {
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: FAILURE_ORDER_CLIENT_ID.to_string(),
            reason: "rejected".to_string().into(),
            timestamp: 1,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })
    };
    let check = |decode: &dyn Fn(&Value) -> Vec<Event>| {
        check_refused_after_taking(order(), decode, None, &frame, "rejected")
    };
    assert_eq!(
        check(&|_: &Value| vec![accepted(), canceled("rejected: taken, then refused")]),
        Ok(())
    );
    assert!(check(&|_: &Value| vec![rejected()]).is_err_and(|why| why.contains("exactly one")));
    assert!(
        check(&|_: &Value| vec![accepted(), rejected()])
            .is_err_and(|why| why.contains("exactly one"))
    );
    assert!(check(&|_: &Value| vec![accepted()]).is_err_and(|why| why.contains("exactly one")));
    let twice = |_: &Value| vec![accepted(), canceled("rejected"), canceled("rejected")];
    assert!(check(&twice).is_err_and(|why| why.contains("exactly one")));
    let wordless = |_: &Value| vec![accepted(), canceled("CANCELED")];
    assert!(check(&wordless).is_err_and(|why| why.contains("venue's words")));
    let unidentified = |_: &Value| {
        let mut events = vec![accepted(), canceled("rejected")];
        if let Some(Event::OrderCanceled(c)) = events.last_mut() {
            c.refusal_id = None;
        }
        events
    };
    assert!(check(&unidentified).is_err_and(|why| why.contains("refusal id")));
    let unaccepted = |_: &Value| vec![canceled("rejected")];
    assert!(check(&unaccepted).is_err_and(|why| why.contains("accept")));

    // The accept in an earlier answer (`taken_by`) counts, and that answer may end nothing itself.
    let placement = serde_json::json!({"placed": "v-1"});
    let split = |answer: &Value| match answer.get("placed") {
        Some(_) => vec![accepted()],
        None => vec![canceled("rejected")],
    };
    let taken = |decode: &dyn Fn(&Value) -> Vec<Event>| {
        check_refused_after_taking(order(), decode, Some(&placement), &frame, "rejected")
    };
    assert_eq!(taken(&split), Ok(()));
    let taken_and_ended = |answer: &Value| match answer.get("placed") {
        Some(_) => vec![accepted(), canceled("rejected")],
        None => vec![canceled("rejected")],
    };
    assert!(taken(&taken_and_ended).is_err_and(|why| why.contains("exactly one")));
}
