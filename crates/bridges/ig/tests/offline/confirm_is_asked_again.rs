//! What a submit whose `/confirms` did not settle it comes to, and the catalog id a refusal of the loop carries,
//! through the real exec client over `fake_ig` (a hand-written loopback gateway, never IG): the shapes are the ones
//! `tests/fixtures/` captured or IG documents, and no order reaches a venue.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ig::spawn_execution_client;
use vike_model::events::Event;
use vike_model::{OrderRefusalId, OrderRequest, OrderType};

use crate::fake_ig::{self, FakeIg, Route};

const EPIC: &str = "CS.D.EURUSD.MINI.IP";

fn table() -> Route {
    Route::ok(
        "GET /markets/",
        json!({ "instrument": { "epic": EPIC, "type": "CURRENCIES", "currencies": [{ "code": "USD" }] } }),
    )
}

fn gateway(routes: Vec<Route>) -> FakeIg {
    let mut all = vec![fake_ig::login_route(), fake_ig::accounts_route("USD"), table()];
    all.extend(routes);
    fake_ig::serve(all)
}

fn order(order_client_id: &str, order_type: OrderType, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ig".into(),
        // the USER name of the epic's kept row (decision 0290)
        symbol: crate::kept_rows::user_name(EPIC),
        side: 1,
        qty: 0.5,
        order_type,
        trigger_price: Some(1.2),
        reduce_only,
        timestamp: 42,
        ..Default::default()
    }
}

/// The events of one submit through the real client, read until `done` says enough or `within` passes.
fn submit(
    ig: &FakeIg,
    req: &OrderRequest,
    within: Duration,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    crate::kept_rows::hold();
    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(ig.config(), events, None);
    client.submit(req);
    let deadline = Instant::now() + within;
    let mut got = Vec::new();
    while !done(&got) && Instant::now() < deadline {
        while let Ok(ingest) = rx.try_recv() {
            if let Ingest::Event(event) = ingest {
                got.push(event);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    client.detach();
    got
}

fn is_rejected(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::OrderRejected(_)))
}

fn is_filled(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::OrderFilled(_)))
}

fn deal(status: &str) -> Value {
    json!({ "dealStatus": status, "dealId": "DIAAA1", "epic": EPIC, "direction": "BUY",
            "size": 0.5, "level": 1.2, "status": "OPEN" })
}

const LADDER: usize = 7;

/// A PUT that reached IG and whose reply was lost read as `status 0`, "provably never sent", so the amend's refusal said the
/// order keeps its terms over a level IG may have moved.
#[test]
fn a_put_whose_reply_is_lost_is_an_unknown_outcome() {
    let ig = fake_ig::serve(vec![
        fake_ig::login_route(),
        fake_ig::accounts_route("USD"),
        Route::hang_up("PUT /workingorders/otc"),
    ]);
    let session = vike_ig::IgSession::login(&ig.config()).expect("the login");
    let e = session.put("/workingorders/otc/D1", "2", &json!({})).expect_err("no answer");
    assert!(e.outcome_unknown(), "{e}");
}

/// **A dead stop must not read as protection**: the POST is taken, every `/confirms` of the ladder fails, and only `/confirms`
/// says IG refused it. The old client stopped asking at the ladder and the order read `Accepted` with no id for ever.
#[test]
fn a_stop_igs_confirm_refuses_late_ends_as_a_cancel_in_igs_words() {
    let ig = gateway(vec![
        Route::ok("POST /workingorders/otc", json!({ "dealReference": "REF-STOP" })),
        Route::ok("GET /confirms/", json!({ "dealStatus": "REJECTED", "reason": "MARKET_CLOSED" }))
            .after(LADDER),
    ]);
    let got = submit(&ig, &order("stop-1", OrderType::Stop, false), Duration::from_secs(15), |e| {
        e.iter().any(|e| matches!(e, Event::OrderCanceled(_)))
    });
    assert!(
        got.iter().any(|e| matches!(e, Event::OrderAccepted(a) if a.venue_order_id.is_none())),
        "first the id-less accept: {got:?}"
    );
    assert!(
        got.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.reason.contains("MARKET_CLOSED"))),
        "{got:?}"
    );
    assert!(!is_rejected(&got), "the FSM refuses a reject from Accepted: {got:?}");
}

/// A create whose reply was lost: the ladder fails, the order stays `Submitted`, and the old client left it to the stuck-order
/// ladder's reject over a deal IG took. Asked again, it books the deal.
#[test]
fn an_unknown_create_is_asked_about_until_igs_confirm_answers() {
    let ig = gateway(vec![
        Route::hang_up("POST /positions/otc"),
        Route::ok(
            "GET /confirms/vk",
            json!({ "dealStatus": "ACCEPTED", "dealId": "DIAAA1", "epic": EPIC, "direction": "BUY",
                    "size": 0.5, "level": 1.2, "status": "OPEN", "dealReference": own_reference() }),
        )
        .after(LADDER),
    ]);
    let got =
        submit(&ig, &order("lost-1", OrderType::Market, false), Duration::from_secs(15), is_filled);
    assert!(is_filled(&got), "{got:?}");
    assert!(!is_rejected(&got), "{got:?}");
}

/// The reference `lost-1` goes out under (`deal_reference`: FNV-1a 64 of the order client id): pinned here because a confirm that names
/// another one is not the order's.
fn own_reference() -> String {
    let h = "lost-1"
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("vk{h:016x}")
}

/// A close's reply was lost, and a close carries no reference to ask `/confirms` about: the activity history says what became
/// of it, under the reference the engine keys every close on. The old client left it to the ladder's reject over a closed position.
#[test]
fn an_unknown_close_is_read_from_the_activity_history() {
    let now = vike_ig::reconcile::ms_to_ig_datetime(vike_model::now_ms());
    let row = json!({
        "date": now, "dealId": "DINEW", "epic": EPIC, "status": "ACCEPTED", "type": "POSITION",
        "details": { "actions": [{ "actionType": "POSITION_CLOSED", "affectedDealId": "DIAAA1" }],
                     "dealReference": "REF-CLOSE", "direction": "BUY", "level": 1.3, "size": 0.5 },
    });
    let ig = gateway(vec![
        Route::hang_up("POST /positions/otc"),
        Route::ok("GET /history/activity", json!({ "activities": [row] })),
    ]);
    let close = order("lost-2", OrderType::Market, true);
    let got = submit(&ig, &close, Duration::from_secs(15), is_filled);
    let fill = got.iter().find_map(|e| match e {
        Event::Fill(f) => Some(f),
        _ => None,
    });
    let fill = fill.unwrap_or_else(|| panic!("the close's fill: {got:?}"));
    assert_eq!(fill.trade_id, "REF-CLOSE", "the reference, as every close is keyed");
    assert_eq!((fill.last_qty, fill.last_price), (0.5, 1.3));
    assert!(!is_rejected(&got), "{got:?}");
}

/// The history names no close of that size: it says nothing, and the order stays the ladder's.
#[test]
fn an_unknown_close_the_history_does_not_show_gets_no_event() {
    let ig = gateway(vec![
        Route::hang_up("POST /positions/otc"),
        Route::ok("GET /history/activity", json!({ "activities": [] })),
    ]);
    let got =
        submit(&ig, &order("lost-3", OrderType::Market, true), Duration::from_secs(4), |_| false);
    assert_eq!(got.len(), 1, "only the OrderSubmitted: {got:?}");
    assert!(ig.count("GET /history/activity") >= 1, "it was asked");
}

/// A 200 naming no `dealReference` is IG taking the order, and an open carries a reference of its own: it is asked about,
/// and on the old code it was a terminal `no dealReference` over a deal that may be live.
#[test]
fn an_answer_with_no_reference_is_asked_about_under_our_own() {
    let ig = gateway(vec![
        Route::ok("POST /positions/otc", json!({})),
        Route::ok("GET /confirms/", deal("ACCEPTED")),
    ]);
    let got =
        submit(&ig, &order("no-ref-1", OrderType::Market, false), Duration::from_secs(5), |e| {
            is_filled(e) || is_rejected(e)
        });
    assert!(!is_rejected(&got), "{got:?}");
    assert!(is_filled(&got), "{got:?}");
    assert_eq!(ig.count("GET /confirms/vk"), 1, "asked under the reference it sent");
}

/// The catalog id of the order's `OrderRejected`, when it was refused.
pub(crate) fn rejection_id(events: &[Event]) -> Option<&str> {
    events.iter().find_map(|e| match e {
        Event::OrderRejected(r) => r.refusal_id.as_deref(),
        _ => None,
    })
}

/// IG's own answer to a create is `order.venue_refused` (decision 0303); a kind this bridge builds no order for, and a
/// resting close IG's close endpoint cannot hold, are `order.venue_capability`, refused unsent.
#[test]
fn a_refused_create_and_an_order_ig_cannot_hold_carry_their_catalog_ids() {
    let refused = json!({ "errorCode": "error.service.create.position.generic" });
    let ig = gateway(vec![Route::status("POST /positions/otc", 400, refused)]);
    let within = Duration::from_secs(5);
    let open = order("refused-1", OrderType::Market, false);
    let got = submit(&ig, &open, within, is_rejected);
    assert_eq!(rejection_id(&got), Some(OrderRefusalId::VenueRefused.word()), "{got:?}");
    for unsent in [
        order("stop-limit-1", OrderType::StopLimit, false),
        order("close-limit-1", OrderType::Limit, true),
    ] {
        let got = submit(&ig, &unsent, within, is_rejected);
        assert_eq!(rejection_id(&got), Some(OrderRefusalId::VenueCapability.word()), "{got:?}");
    }
    assert_eq!(ig.count("POST /positions/otc"), 1, "only the open was sent");
}

/// A cancel of an order this process holds no `dealId` for asks IG nothing: `order.not_sent`.
#[test]
fn a_cancel_of_an_order_with_no_deal_id_is_refused_unsent() {
    let ig = gateway(Vec::new());
    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(ig.config(), events, None);
    client.cancel("unknown-1");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut refused = None;
    while refused.is_none() && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderCancelRejected(r))) => refused = Some(r),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    client.detach();
    let refused = refused.expect("the cancel is answered");
    assert_eq!(refused.refusal_id.as_deref(), Some(OrderRefusalId::NotSent.word()));
    assert_eq!(ig.count("DELETE /workingorders/otc/"), 0, "nothing was asked of IG");
}
