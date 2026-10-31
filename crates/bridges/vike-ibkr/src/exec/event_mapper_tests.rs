use super::*;
use crate::exec::id_registry::IdRegistry;
use std::assert_matches;
use vike_model::events::{Event, FillEvent};

fn mapper_with(coid: &str, order_id: i32) -> EventMapper {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(order_id);
    let alloc = ids.next_order_id().unwrap();
    assert_eq!(alloc, order_id);
    ids.bind(order_id, coid);
    EventMapper::new(ids)
}

/// An execDetails with an EMPTY `execId` is refused at ingress: no events, and nothing buffered,
/// so its commissionReport cannot later join and mint a fill either.
///
/// This gates the `TradeId::new` guard in `on_exec_details`, not the type. Reverting it to a
/// permissive id turns both asserted emptinesses into fills — and the hazard is worse than one
/// un-dedupable fill: `emitted`/`pending`/`commissions` are ALL keyed on the exec id, so several
/// id-less executions would share one `""` slot, overwriting each other's halves and making a
/// `reqExecutions` replay (which returns the whole day) re-emit or swallow at random.
#[test]
fn exec_details_without_an_exec_id_is_refused_at_ingress() {
    let mut m = mapper_with("coid-A", 101);
    let evs = lifecycle(m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: String::new(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
        ..Default::default()
    }));
    assert!(evs.is_empty(), "an id-less execution emits nothing, not even the synth accept");

    // ...and nothing was buffered, so the matching commission cannot mint a fill afterwards.
    let joined = lifecycle(m.on_commission_report(IbCommissionReport {
        exec_id: String::new(),
        commission: 1.0,
        currency: "USD".into(),
    }));
    assert!(joined.is_empty(), "no pending half exists to join: {joined:?}");
}

#[test]
fn fill_before_accept_synthesizes_accepted_then_filled() {
    // A fresh mapper whose id is bound but never saw an ACCEPTED status: an execDetails must
    // synthesize OrderAccepted, then (after the commission joins) OrderFilled.
    let mut m = mapper_with("coid-A", 101);
    let evs = lifecycle(m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
        ..Default::default()
    }));
    // Accepted is emitted immediately; the fill waits for the commission join.
    assert_matches!(evs.as_slice(), [Event::OrderAccepted(_)]);
    let evs2 = lifecycle(m.on_commission_report(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.0,
        currency: "USD".into(),
    }));
    match evs2.as_slice() {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.commission, 1.0);
            assert_eq!(f.fill.trade_id, "e1");
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }
}

#[test]
fn exec_details_buffered_until_commission_joins_by_exec_id() {
    let mut m = mapper_with("coid-A", 101);
    // Pre-accept via a Submitted status so no synth is needed.
    let _ = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    let e = lifecycle(m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e9".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: false,
        shares: 4.0,
        price: 191.0,
        ts: 7,
        ..Default::default()
    }));
    assert!(e.is_empty(), "fill must NOT emit before its commission joins");
    let joined = lifecycle(m.on_commission_report(IbCommissionReport {
        exec_id: "e9".into(),
        commission: 0.5,
        currency: "USD".into(),
    }));
    assert_matches!(joined.as_slice(), [Event::OrderFilled(_)]);
}

#[test]
fn advisory_code_emits_nothing() {
    let mut m = mapper_with("coid-A", 101);
    assert!(m.on_error(2104, 101, "Market data farm connection is OK").is_empty());
    assert!(m.on_error(2137, 101, "advisory").is_empty());
}

#[test]
fn order_rejection_code_emits_reject_and_forgets() {
    let mut m = mapper_with("coid-A", 101);
    let evs = m.on_error(201, 101, "Order rejected - reason: insufficient buying power");
    assert_matches!(evs.as_slice(), [Event::OrderRejected(_)]);
}

#[test]
fn idless_async_rejection_routes_to_sole_unacked_order() {
    // ibapi's global order_update_stream drops the order id from order-rejection Notices, so an
    // async hard rejection (delivered AFTER submit_order returned Ok) arrives as
    // on_error(code, 0, msg). With exactly one order still unacked, it must resolve to that
    // order and emit OrderRejected — the no-order-vanishes contract.
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let evs = m.on_error(201, 0, "Order rejected - insufficient buying power");
    match evs.as_slice() {
        [Event::OrderRejected(r)] => assert_eq!(r.client_order_id, "coid-A"),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
    // Idempotent: after the reject the order is forgotten (zero unacked) → a duplicate id-less
    // notice synthesizes no second terminal.
    assert!(m.on_error(201, 0, "duplicate").is_empty());
}

/// The defect of 2026-10-25 (v0.1.88, paper gateway): IB answered a flagged GTC limit order with 10052, id-less on
/// the stream and with no order status behind it, and nothing ended the order. Now the sole unacked order is
/// rejected AT ONCE and IB's own words are the reason.
#[test]
fn invalid_time_in_force_rejects_the_sole_unacked_order_with_ibs_words() {
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let evs = m.on_error(10052, 0, "Invalid time in force:GTC");
    match evs.as_slice() {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.client_order_id, "coid-A");
            assert_eq!(r.reason.as_str(), "Invalid time in force:GTC");
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
    // ...and the order is forgotten: a second 10052 has nobody to reject.
    assert!(m.on_error(10052, 0, "Invalid time in force:GTC").is_empty());
}

/// A notice that an order was ROUTED to the overnight session (10329) rejects nothing, however alone the order is.
#[test]
fn an_overnight_routing_notice_does_not_reject_the_sole_unacked_order() {
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    assert!(m.on_error(10329, 0, "This order will be directly routed to OVERNIGHT").is_empty());
    assert!(m.on_error(10329, 101, "This order will be directly routed to OVERNIGHT").is_empty());
}

#[test]
fn idless_async_rejection_ambiguous_when_multiple_unacked() {
    // Two orders in flight: an id-less rejection cannot be safely attributed, so NO terminal is
    // synthesized here (the core submit-ack watchdog is the backstop). Never corrupt a sibling.
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let a = ids.next_order_id().unwrap();
    ids.bind(a, "coid-A");
    let b = ids.next_order_id().unwrap();
    ids.bind(b, "coid-B");
    let mut m = EventMapper::new(ids);
    m.on_submit(a, 1.0);
    m.on_submit(b, 1.0);
    assert!(m.on_error(201, 0, "ambiguous").is_empty());
}

#[test]
fn idless_async_rejection_ignored_after_order_acked() {
    // Once the sole in-flight order goes active it leaves the unacked set, so a stray id-less
    // rejection no longer misattributes to it (an accepted order rejects via a resolvable
    // OrderStatus, not this path).
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let _ = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    assert!(m.on_error(201, 0, "late").is_empty());
}

#[test]
fn rebind_of_unacked_order_emits_accepted_once() {
    // Mid-flight order: core submitted it (unacked), IB accepted server-side, but the confirming
    // OrderStatus was lost in a socket blip. On reconnect IB replays it as an OpenOrder →
    // rebind_open_order MUST emit exactly one OrderAccepted to unstick the FSM, and a later
    // Submitted status must NOT re-emit a second (idempotent).
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 1.0);
    let evs = m.rebind_open_order(101, "coid-A");
    match evs.as_slice() {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "coid-A");
            assert_eq!(a.venue_order_id.as_deref(), Some("101"));
        }
        other => panic!("expected exactly one OrderAccepted, got {other:?}"),
    }
    // A subsequent active status for the same order does NOT emit a duplicate accept.
    let dup = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    assert!(dup.is_empty(), "second accept must not be emitted (idempotent)");
}

#[test]
fn rebind_of_external_order_emits_nothing_and_stays_resolvable() {
    // An EXTERNAL order (never submitted this process → not in unacked, e.g. opened in a prior
    // session): rebind emits NOTHING, but the id map is seeded so a later Cancelled still
    // resolves to its coid and emits exactly one OrderCanceled.
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(55);
    let mut m = EventMapper::new(ids);
    let evs = m.rebind_open_order(55, "coid-ext");
    assert!(evs.is_empty(), "external rebind must emit nothing");
    let cancel = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 55,
        order_ref: "coid-ext".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    match cancel.as_slice() {
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "coid-ext"),
        other => panic!("expected exactly one OrderCanceled, got {other:?}"),
    }
}

#[test]
fn terminal_status_cleans_the_map() {
    let mut m = mapper_with("coid-A", 101);
    let evs = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    assert_matches!(evs.as_slice(), [Event::OrderCanceled(_)]);
    // After a terminal, the id is forgotten → a late duplicate resolves to nothing.
    let dup = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "".into(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    assert!(dup.is_empty());
}

fn mapper_submitted(coid: &str, order_id: i32, qty: f64) -> EventMapper {
    let mut m = mapper_with(coid, order_id);
    m.on_submit(order_id, qty);
    // ack it so fills don't need exec-before-open synthesis noise in these tests
    let _ = lifecycle(m.on_order_status(IbOrderStatus {
        order_id,
        order_ref: coid.into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    m
}

/// The events a mapper call produced WITHOUT the bare `Fill`s: the lifecycle (wrapper) view the older tests
/// were written against. The bare fill has its own tests (R9, the last section of this file).
fn lifecycle(evs: Vec<Event>) -> Vec<Event> {
    evs.into_iter().filter(|e| !matches!(e, Event::Fill(_))).collect()
}

fn commission_raw(m: &mut EventMapper, exec_id: &str) -> Vec<Event> {
    m.on_commission_report(IbCommissionReport {
        exec_id: exec_id.into(),
        commission: 0.0,
        currency: "USD".into(),
    })
}
fn commission(m: &mut EventMapper, exec_id: &str) -> Vec<Event> {
    lifecycle(commission_raw(m, exec_id))
}
fn exec(m: &mut EventMapper, order_id: i32, coid: &str, exec_id: &str, shares: f64) -> Vec<Event> {
    lifecycle(m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares,
        price: 190.0,
        ts: 1,
        ..Default::default()
    }))
}

#[test]
fn multi_fill_emits_partial_then_final_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    let _ = exec(&mut m, 101, "coid-A", "e2", 6.0);
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]);
    // forgotten: a stray later exec for order 101 resolves to nothing
    assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
}

#[test]
fn single_full_fill_is_terminal_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
    assert!(
        m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "".into(),
            status: "Filled".into(),
            filled: 5.0,
            avg_fill_price: 190.0,
            ..Default::default()
        })
        .is_empty()
    );
}

#[test]
fn unknown_total_fill_falls_back_to_terminal_filled() {
    // No on_submit(total) → external/unknown → keep current OrderFilled-terminal behaviour.
    // (order_id 101, not the plan text's illustrative 55: mapper_with asserts the allocated id
    // round-trips, and IdRegistry::next_order_id floors every allocation at 101.)
    let mut m = mapper_with("coid-X", 101);
    let _ = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-X".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    }));
    let _ = exec(&mut m, 101, "coid-X", "e1", 3.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
}

#[test]
fn partial_then_cancel_is_valid_and_forgets() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    let c = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
        ..Default::default()
    }));
    assert_matches!(c.as_slice(), [Event::OrderCanceled(_)]);
    // forgotten
    assert!(
        m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "".into(),
            status: "Cancelled".into(),
            filled: 4.0,
            avg_fill_price: 0.0,
            ..Default::default()
        })
        .is_empty()
    );
}

fn cancelled(order_id: i32, coid: &str) -> IbOrderStatus {
    IbOrderStatus {
        order_id,
        order_ref: coid.into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
        ..Default::default()
    }
}

fn real_commission(m: &mut EventMapper, exec_id: &str, amount: f64) -> Vec<Event> {
    m.on_commission_report(IbCommissionReport {
        exec_id: exec_id.into(),
        commission: amount,
        currency: "USD".into(),
    })
}

/// F7 (re-review of Part B). A cancel that arrives while a fill still waits for its commission is HELD, and the
/// fill goes out with the REAL commission when it comes, the cancel right behind it (the order the engine's FSM
/// needs). It used to flush the fill at once with `commission 0.0 'USD'` and remember the execution as emitted,
/// which dropped the real fee for good.
#[test]
fn a_cancel_waits_for_the_commission_of_a_fill_in_flight_and_the_fill_carries_the_real_fee() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    assert!(
        m.on_order_status(cancelled(101, "coid-A")).is_empty(),
        "the cancel waits: nothing is invented in its place"
    );
    let evs = real_commission(&mut m, "e1", 1.25);
    match evs.as_slice() {
        [Event::Fill(f), Event::OrderPartiallyFilled(p), Event::OrderCanceled(c)] => {
            assert_eq!(f.commission, 1.25, "the bare fill carries the REAL fee");
            assert_eq!(f.commission_asset.as_str(), "USD");
            assert_eq!(p.fill.commission, 1.25);
            assert_eq!((f.trade_id.as_str(), p.client_order_id.as_str()), ("e1", "coid-A"));
            assert_eq!(c.client_order_id, "coid-A");
        }
        other => panic!("expected [Fill, PartiallyFilled, Canceled], got {other:?}"),
    }
    assert!(m.pending.is_empty() && m.held_cancels.is_empty());
    assert_eq!(m.ids().resolve(101, ""), None, "the order is retired once the cancel went out");
    assert!(m.recently_finished("coid-A"));
}

/// Several fills in flight: the cancel goes out behind the LAST commission, not the first.
#[test]
fn a_held_cancel_goes_out_behind_the_last_of_the_commissions_it_waits_for() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 2.0);
    let _ = exec(&mut m, 101, "coid-A", "e2", 2.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let first = real_commission(&mut m, "e1", 0.5);
    assert_matches!(first.as_slice(), [Event::Fill(_), Event::OrderPartiallyFilled(_)]);
    let second = real_commission(&mut m, "e2", 0.5);
    assert_matches!(
        second.as_slice(),
        [Event::Fill(_), Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)]
    );
}

/// A commission that does not come is not waited for for ever: the cancel is RELEASED after the bound, the
/// fills book with an UNKNOWN fee (0.0, no fee currency: `commission` is a bare `f64`), and the late real
/// commission is a no-op that is not parked.
#[test]
fn a_cancel_whose_commission_never_comes_is_released_after_the_bound_with_an_unknown_fee() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    // Not yet: the first pass only stamps the cancel, and a pass inside the bound releases nothing.
    assert!(m.release_held_cancels(0, CANCEL_COMMISSION_WAIT_MS).is_empty());
    assert!(
        m.release_held_cancels(CANCEL_COMMISSION_WAIT_MS - 1, CANCEL_COMMISSION_WAIT_MS).is_empty()
    );
    let out = m.release_held_cancels(CANCEL_COMMISSION_WAIT_MS, CANCEL_COMMISSION_WAIT_MS);
    match out.as_slice() {
        [Event::Fill(f), Event::OrderPartiallyFilled(p), Event::OrderCanceled(c)] => {
            assert_eq!(f.commission, 0.0, "fee UNKNOWN is booked as 0.0");
            assert_eq!(
                f.commission_asset.as_str(),
                "",
                "...with no fee currency, so nothing is attributed"
            );
            assert_eq!(p.client_order_id, "coid-A");
            assert_eq!(c.client_order_id, "coid-A");
        }
        other => panic!("expected [Fill, PartiallyFilled, Canceled], got {other:?}"),
    }
    assert!(
        m.fee_book.get("e1").is_some_and(|b| b.fill_commission == 0.0 && b.fill_asset.is_empty()),
        "the execution is remembered as booked with an unknown fee"
    );
    // The late real commission: NOT parked, and no longer only logged: it goes out as a fee adjustment
    // carrying the whole real fee, which the engine books once (decision 0238).
    match real_commission(&mut m, "e1", 1.25).as_slice() {
        [Event::FeeAdjustment(a)] => {
            assert_eq!(
                a.extra_commission, 1.25,
                "the fill carried 0.0, so the adjustment is the whole fee"
            );
            assert_eq!(a.commission_asset.as_str(), "USD");
            assert_eq!(
                (
                    a.trade_id.as_str(),
                    a.client_order_id.as_str(),
                    a.symbol.as_str(),
                    a.venue.as_str()
                ),
                ("e1", "coid-A", "AAPL.SMART.USD", "ibkr"),
                "it names the fill's own trade id, order and symbol"
            );
            assert_eq!((a.last_px, a.ts), (190.0, 1), "and the fill's price and time");
        }
        other => panic!("expected one FeeAdjustment, got {other:?}"),
    }
    assert!(m.commissions.is_empty());
    // ...once: the same report again (a replay) emits nothing.
    assert!(real_commission(&mut m, "e1", 1.25).is_empty());
    assert!(
        m.release_held_cancels(10 * CANCEL_COMMISSION_WAIT_MS, CANCEL_COMMISSION_WAIT_MS)
            .is_empty()
    );
}

/// **The venue is ASKED for the commission before the fee is booked unknown.** The stranded-fill ladder's first ask falls
/// at `PENDING_COMMISSION_GRACE_MS` (15 s), after the cancel's own bound (5 s), so the release used to preempt the one
/// mechanism that re-delivers a lost commission report (`reqExecutions`). Now the bound that runs out ASKS (once), the
/// release moves out by `COMMISSION_REREQUEST_WAIT_MS`, and the report the venue sends back joins the fill with its
/// REAL fee, the cancel right behind it: nothing is booked unknown.
#[test]
fn a_cancel_asks_the_venue_for_the_commission_before_it_books_the_fee_unknown() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let w = CANCEL_COMMISSION_WAIT_MS;

    assert!(!m.ask_for_held_commissions(0, w), "the first pass has not stamped the cancel yet");
    assert!(m.release_held_cancels(0, w).is_empty(), "...the release pass stamps it");
    assert!(!m.ask_for_held_commissions(w - 1, w), "inside the bound nothing is asked");
    assert!(
        m.ask_for_held_commissions(w, w),
        "the bound ran out with a fill unjoined: ask the venue"
    );
    assert!(!m.ask_for_held_commissions(w + 1, w), "once per cancel");

    // The release moved out: the old bound is no longer the one.
    assert!(m.release_held_cancels(w, w).is_empty());
    assert!(m.release_held_cancels(w + COMMISSION_REREQUEST_WAIT_MS - 1, w).is_empty());
    // The answer inside the new bound is the ordinary join, with the real fee.
    let evs = real_commission(&mut m, "e1", 1.25);
    match evs.as_slice() {
        [Event::Fill(f), Event::OrderPartiallyFilled(_), Event::OrderCanceled(c)] => {
            assert_eq!(f.commission, 1.25, "the REAL fee, not 0.0");
            assert_eq!(f.commission_asset.as_str(), "USD");
            assert_eq!(c.client_order_id, "coid-A");
        }
        other => panic!("expected [Fill, PartiallyFilled, Canceled], got {other:?}"),
    }
    assert!(m.held_cancels.is_empty());
    assert!(
        m.fee_book.get("e1").is_some_and(|b| b.fill_commission == 1.25),
        "the fee went out REAL, so there is nothing unknown to correct"
    );
}

/// An ask the venue does not answer ends as before, one reply window later: the fee is booked unknown, loudly.
#[test]
fn an_unanswered_ask_releases_the_fee_unknown_after_the_reply_window() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let w = CANCEL_COMMISSION_WAIT_MS;
    assert!(m.release_held_cancels(0, w).is_empty());
    assert!(m.ask_for_held_commissions(w, w));
    let out = m.release_held_cancels(w + COMMISSION_REREQUEST_WAIT_MS, w);
    match out.as_slice() {
        [Event::Fill(f), Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)] => {
            assert_eq!(f.commission, 0.0, "nobody answered: the fee is unknown, booked 0.0");
        }
        other => panic!("expected [Fill, PartiallyFilled, Canceled], got {other:?}"),
    }
    assert!(
        m.fee_book.get("e1").is_some_and(|b| b.fill_commission == 0.0 && b.fill_asset.is_empty())
    );
}

/// No ask while the connection LEARNS the venue (the request would end into the learning's `SnapshotEnd`): the cancel is
/// released at the old bound, and asked as soon as the learning is over.
#[test]
fn nothing_is_asked_while_the_connection_learns_the_venue() {
    use crate::transport::Snapshot;
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let w = CANCEL_COMMISSION_WAIT_MS;
    assert!(m.release_held_cancels(0, w).is_empty());

    m.begin_learning();
    assert!(!m.ask_for_held_commissions(w, w), "learning the open orders");
    let _ = m.snapshot_ended(Snapshot::OpenOrders, true);
    assert!(!m.ask_for_held_commissions(w, w), "learning the executions");
    let _ = m.snapshot_ended(Snapshot::Executions, true);
    assert!(m.ask_for_held_commissions(w, w), "once the learning is over the venue is asked");
}

/// No ask with the stream DEAD (nothing can answer a request into a socket nobody reads): the cancel is released at the
/// old bound with the fee unknown, not held `COMMISSION_REREQUEST_WAIT_MS` longer for an answer that cannot come.
#[test]
fn nothing_is_asked_with_the_stream_dead_and_the_cancel_goes_at_the_old_bound() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let w = CANCEL_COMMISSION_WAIT_MS;
    assert!(m.release_held_cancels(0, w).is_empty());

    m.ids_mut().mark_stream_dead("connection reset by peer");
    assert!(!m.ask_for_held_commissions(w, w), "a dead stream is never asked");
    let out = m.release_held_cancels(w, w);
    assert!(
        matches!(
            out.as_slice(),
            [Event::Fill(_), Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)]
        ),
        "released at the old bound: {out:?}"
    );
    assert!(
        m.fee_book.get("e1").is_some_and(|b| b.fill_commission == 0.0 && b.fill_asset.is_empty())
    );
}

/// A cancel with nothing in flight goes out at once, as before.
#[test]
fn a_cancel_with_no_fill_in_flight_goes_out_at_once() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert_matches!(
        m.on_order_status(cancelled(101, "coid-A")).as_slice(),
        [Event::OrderCanceled(_)]
    );
}

/// The held cancel is moot when the commission completes the order: it ends `Filled`, no cancel follows.
#[test]
fn a_held_cancel_is_dropped_when_the_fill_it_waited_for_completes_the_order() {
    let mut m = mapper_submitted("coid-A", 101, 4.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let evs = real_commission(&mut m, "e1", 1.0);
    assert_matches!(evs.as_slice(), [Event::Fill(_), Event::OrderFilled(_)]);
    assert!(m.held_cancels.is_empty());
}

// -----------------------------------------------------------------------------------------
// Commission-before-exec: the arrival order IBKR explicitly permits (`ibapi` 3.2.1's
// `CommissionReport`, https://docs.rs/ibapi/3.2.1/ibapi/orders/struct.CommissionReport.html).
// Before the reverse index existed, the commission was discarded on arrival AND its execution then
// stranded in `pending` forever, so the fill was never emitted at all.
// -----------------------------------------------------------------------------------------

#[test]
fn commission_before_exec_emits_exactly_one_fill_instead_of_dropping_it() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let parked = lifecycle(m.on_commission_report(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    }));
    assert!(parked.is_empty(), "the commission alone emits nothing — it is parked, not dropped");
    let evs = lifecycle(m.on_exec_details(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 5,
        ..Default::default()
    }));
    match evs.as_slice() {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.last_px, 190.0);
            // the parked half's values are carried onto the fill, not defaulted away
            assert_eq!(f.fill.commission, 1.25);
            assert_eq!(f.fill.commission_asset, "USD");
            assert_eq!(f.fill.ts, 5);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.commissions.is_empty(), "the park is consumed by the join");
    assert!(m.pending.is_empty(), "nothing strands in pending");
}

#[test]
fn both_arrival_orders_produce_an_identical_fill() {
    // The join is by exec_id, never by arrival, so the two orderings must be indistinguishable
    // downstream — the property `ibapi` 3.2.1's docs prescribe.
    let fill_of = |commission_first: bool| -> FillEvent {
        let mut m = mapper_submitted("coid-A", 101, 3.0);
        let c =
            IbCommissionReport { exec_id: "e1".into(), commission: 0.75, currency: "USD".into() };
        let d = IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: "e1".into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: false,
            shares: 3.0,
            price: 188.5,
            ts: 42,
            ..Default::default()
        };
        let evs = lifecycle(if commission_first {
            assert!(m.on_commission_report(c).is_empty());
            m.on_exec_details(d)
        } else {
            assert!(m.on_exec_details(d).is_empty());
            m.on_commission_report(c)
        });
        match evs.as_slice() {
            [Event::OrderFilled(f)] => f.fill.clone(),
            other => panic!("expected exactly one OrderFilled, got {other:?}"),
        }
    };
    assert_eq!(fill_of(true), fill_of(false));
}

#[test]
fn commission_before_exec_still_synthesizes_the_accept_first() {
    // Exec-before-open AND commission-before-exec at once: the accept must still lead, so the
    // FSM sees a valid [Accepted, Filled] pair from the single call.
    let mut m = mapper_with("coid-A", 101);
    assert!(commission(&mut m, "e1").is_empty());
    let evs = exec(&mut m, 101, "coid-A", "e1", 10.0);
    match evs.as_slice() {
        [Event::OrderAccepted(a), Event::OrderFilled(f)] => {
            assert_eq!(a.client_order_id, "coid-A");
            assert_eq!(f.client_order_id, "coid-A");
        }
        other => panic!("expected [OrderAccepted, OrderFilled], got {other:?}"),
    }
}

#[test]
fn commission_first_partials_account_identically_to_exec_first() {
    // Both legs reversed: `fill_state`'s partial-vs-final accounting must be unchanged, since
    // it advances in the shared `join_fill` rather than in either arrival path.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(commission(&mut m, "e1").is_empty());
    assert_matches!(
        exec(&mut m, 101, "coid-A", "e1", 4.0).as_slice(),
        [Event::OrderPartiallyFilled(_)]
    );
    assert!(commission(&mut m, "e2").is_empty());
    assert_matches!(exec(&mut m, 101, "coid-A", "e2", 6.0).as_slice(), [Event::OrderFilled(_)]);
    // forgotten on the final fill, exactly as in the exec-first ordering
    assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
}

#[test]
fn duplicate_commission_does_not_double_emit_a_fill() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
    // IB re-sends the same commissionReport. The fill for e1 already emitted, so the duplicate
    // is DROPPED at the top of `on_commission_report` — emitting nothing and, unlike before the
    // `emitted` index existed, not PARKING either (a park here would leak one `commissions`
    // entry per re-delivered commission, which a `reqExecutions` replay produces by the dozen).
    assert!(commission(&mut m, "e1").is_empty());
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.commissions.is_empty(), "a commission for an emitted fill must not park");
}

#[test]
fn parked_commission_is_released_once_its_exec_proves_unroutable() {
    // The DEFINITIVE expiry: the execution arrives but resolves to no local order, so the pair
    // can never join and the park must not be held for the process lifetime.
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    assert!(commission(&mut m, "ext-1").is_empty());
    assert_eq!(m.commissions.len(), 1);
    let evs = exec(&mut m, 999, "", "ext-1", 3.0); // order 999 was never bound
    assert!(evs.is_empty(), "an unroutable execution still emits nothing");
    assert!(m.commissions.is_empty(), "the park is released, not held forever");
    assert!(m.pending.is_empty());
}

#[test]
fn parked_commissions_are_capped_and_evict_the_oldest_first() {
    // The BACKSTOP expiry: the key is a venue-supplied exec_id, so the map is bounded.
    let mut m = mapper_submitted("coid-A", 101, 1.0);
    for i in 0..MAX_PARKED_COMMISSIONS {
        assert!(commission(&mut m, &format!("orphan-{i}")).is_empty());
    }
    assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS);
    // One more park evicts the OLDEST, never the newest (which is the one still mid-race).
    assert!(commission(&mut m, "newest").is_empty());
    assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS, "the cap holds");
    assert!(!m.commissions.contains_key("orphan-0"), "the oldest park is the one evicted");
    assert!(m.commissions.contains_key("orphan-1"));
    assert!(m.commissions.contains_key("newest"), "the newest park survives");
    // The surviving park still joins normally — the cap does not break the live pair.
    assert_matches!(exec(&mut m, 101, "coid-A", "newest", 1.0).as_slice(), [Event::OrderFilled(_)]);
}

// -----------------------------------------------------------------------------------------
// The MIRROR leak (#949's documented residual): an execution whose commissionReport never
// arrives, on an order that is never cancelled, was held forever and its fill NEVER emitted —
// position and realized PnL silently short. Cured by RECOVERING the real commission from the
// venue (`sweep_pending` → the exec loop's `reqExecutions`), never by inventing one.
// -----------------------------------------------------------------------------------------

#[test]
fn a_fresh_pending_entry_is_not_swept_before_the_grace_window() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    // The first sweep only STAMPS the entry (the mapper has no clock, so it cannot have been
    // stamped at arrival) — it can never fire on the same pass that discovers it.
    assert!(m.sweep_pending(1_000, 15_000).is_empty());
    assert!(m.sweep_pending(1_000 + 14_999, 15_000).is_empty(), "one ms short of the window");
    assert_eq!(m.pending.len(), 1, "the sweep removes nothing");
}

#[test]
fn an_ordinary_pair_that_joins_promptly_is_never_swept() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
    // Nothing buffered ⇒ no re-request is ever provoked on the happy path, at any age.
    assert!(m.sweep_pending(i64::MAX / 2, 15_000).is_empty());
}

#[test]
fn a_stranded_execution_is_reported_for_recovery_then_escalated_once() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    let mut now = 0;
    m.sweep_pending(now, 15_000); // stamp
    // Each attempt restarts the entry's clock, so the ladder is one request per grace window.
    for attempt in 1..=MAX_PENDING_RECOVERY_ATTEMPTS {
        now += 15_000;
        let s = m.sweep_pending(now, 15_000);
        assert_eq!(s.recover, vec!["e1".to_string()], "attempt {attempt}");
        assert!(s.stranded.is_empty(), "attempt {attempt} must not escalate yet");
    }
    // Budget spent → ONE stranded row carrying everything an operator needs to see what the
    // platform is short.
    now += 15_000;
    let s = m.sweep_pending(now, 15_000);
    assert!(s.recover.is_empty(), "no fourth request");
    assert_eq!(
        s.stranded,
        vec![StrandedFill {
            exec_id: "e1".into(),
            client_order_id: "coid-A".into(),
            order_id: 101,
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            shares: 10.0,
            price: 190.0,
        }]
    );
    // …and never again: the escalation is reported once, not once per second forever.
    now += 15_000;
    assert!(m.sweep_pending(now, 15_000).is_empty());
}

#[test]
fn a_stranded_execution_is_never_evicted_so_a_late_commission_still_emits_its_fill() {
    // The #949 constraint this design is built around: evicting a `pending` entry would lose
    // THE FILL, so the sweep annotates and never removes. Even past escalation the entry is
    // still joinable — and it joins with the REAL commission, never a fabricated 0.0.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    let mut now = 0;
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 3) {
        m.sweep_pending(now, 15_000);
        now += 15_000;
    }
    assert_eq!(m.pending.len(), 1, "escalation holds the entry, it does not drop it");
    match m
        .on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 1.75,
            currency: "USD".into(),
        })
        .as_slice()
    {
        [Event::Fill(bare), Event::OrderFilled(f)] => {
            assert_eq!(bare.trade_id, f.fill.trade_id);
            assert_eq!(bare.commission, f.fill.commission);
            assert_eq!(f.fill.commission, 1.75, "the REAL commission, not a fabricated 0.0");
            assert_eq!(f.fill.last_qty, 10.0);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.pending.is_empty());
}

#[test]
fn the_venue_replay_of_a_stranded_pair_emits_the_fill_with_the_real_commission() {
    // The whole recovery round trip, as the exec loop drives it: the commission is lost, the
    // sweep asks for it, IBKR re-delivers BOTH halves, and the ordinary join emits the fill.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "no fill without a commission");
    m.sweep_pending(0, 15_000);
    assert_eq!(m.sweep_pending(15_000, 15_000).recover, vec!["e1".to_string()]);
    // `reqExecutions` replays the execution AND its commission report (in either order — the
    // join is by exec_id, never by arrival).
    assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "replayed exec re-buffers");
    match m
        .on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 2.5,
            currency: "USD".into(),
        })
        .as_slice()
    {
        [Event::Fill(bare), Event::OrderFilled(f)] => {
            assert_eq!(bare.trade_id, f.fill.trade_id);
            assert_eq!(bare.commission, f.fill.commission);
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.commission, 2.5);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }
    assert!(m.pending.is_empty(), "nothing strands after the recovery");
}

// -----------------------------------------------------------------------------------------
// Replay SAFETY: `reqExecutions` returns the whole day indiscriminately, so every re-delivery
// path has to be inert for a fill that already emitted. These are the tests that keep the
// recovery from being worse than the disease.
// -----------------------------------------------------------------------------------------

#[test]
fn replaying_an_emitted_partial_does_not_advance_fill_state_and_lose_later_executions() {
    // THE regression guard. Order of 10: e1=4 emits a partial, e2=3 strands. The replay
    // re-delivers e1 as well. If the replayed e1 re-entered `pending` and re-joined,
    // `fill_state.filled` would reach 4+4+3 = 11 ≥ 10, so e2 would be classified FINAL — the
    // order forgotten while 3 shares are still working, and every later execution silently
    // dropped. With the `emitted` index, e2 is correctly still a PARTIAL.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    let _ = exec(&mut m, 101, "coid-A", "e2", 3.0); // commission lost → stranded

    // The replay: both halves of the already-emitted e1 come back. Both are no-ops.
    assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty(), "replayed exec must not buffer");
    assert!(commission(&mut m, "e1").is_empty(), "replayed commission must not emit");
    assert!(m.commissions.is_empty(), "…and must not park either");
    assert_eq!(m.pending.len(), 1, "only the genuinely stranded e2 is buffered");

    // e2's replayed commission now joins — and is still a PARTIAL (4+3 = 7 < 10).
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderPartiallyFilled(_)]);
    // Proof the order was NOT forgotten: a third execution still resolves and completes it.
    let _ = exec(&mut m, 101, "coid-A", "e3", 3.0);
    assert_matches!(commission(&mut m, "e3").as_slice(), [Event::OrderFilled(_)]);
}

#[test]
fn replaying_a_fully_emitted_pair_emits_nothing_in_either_arrival_order() {
    for commission_first in [false, true] {
        let mut m = mapper_submitted("coid-A", 101, 5.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
        assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
        if commission_first {
            assert!(commission(&mut m, "e1").is_empty());
            assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
        } else {
            assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
            assert!(commission(&mut m, "e1").is_empty());
        }
        assert!(m.pending.is_empty(), "commission_first={commission_first}");
        assert!(m.commissions.is_empty(), "commission_first={commission_first}");
    }
}

#[test]
fn emitted_index_is_capped_and_evicts_the_oldest_first() {
    // Bounded like `commissions`, for the same venue-supplied-key reason — but eviction here
    // is safe: the worst case is a duplicate fill event, which `vike_exec::ExecutionEngine`
    // dedups by `trade_id`. Cap the map by driving that many complete fills.
    let mut m = mapper_with("coid-A", 101);
    for i in 0..MAX_EMITTED_EXEC_IDS {
        // No `on_submit` ⇒ unknown total ⇒ each fill is terminal and forgets the order, so
        // rebind before the next one.
        m.ids_mut().bind(101, "coid-A");
        let id = format!("x{i}");
        let _ = exec(&mut m, 101, "coid-A", &id, 1.0);
        assert_matches!(commission(&mut m, &id).as_slice(), [Event::OrderFilled(_)]);
    }
    assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS);
    m.ids_mut().bind(101, "coid-A");
    let _ = exec(&mut m, 101, "coid-A", "newest", 1.0);
    assert_matches!(commission(&mut m, "newest").as_slice(), [Event::OrderFilled(_)]);
    assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS, "the cap holds");
    assert!(!m.emitted.contains_key("x0"), "the oldest record is the one evicted");
    assert!(m.emitted.contains_key("x1"));
    assert!(m.emitted.contains_key("newest"));
}

#[test]
fn a_cancel_flushed_partial_is_recorded_as_emitted_and_survives_a_replay() {
    // The `flushed` set #949 introduced folded into `emitted`. It kept its original job (the
    // late REAL commission for a cancel-flushed partial is a no-op) and gained non-consumption,
    // so a `reqExecutions` replay of that same pair cannot resurrect it either.
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
    let out = lifecycle(m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 4.0,
        avg_fill_price: 190.0,
        ..Default::default()
    }));
    assert!(out.is_empty(), "held for the commission");
    let out = lifecycle(m.release_held_cancels(0, 0));
    assert_matches!(out.as_slice(), [Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)]);
    // Late real commission: still a no-op, still not parked (the #949 assertions).
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.commissions.is_empty());
    // NEW: the whole pair replayed — also inert, where a consuming set would have re-armed.
    assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty());
    assert!(commission(&mut m, "e1").is_empty());
    assert!(m.pending.is_empty());
    assert!(m.commissions.is_empty());
}

// -----------------------------------------------------------------------------------------
// LEARNING the venue on a fresh connection (review of W14b, findings 1, 2, 5, 6): the rows of the
// venue's open orders, the fill tracking of a rebound order, the accept it is owed, and the
// commissions of executions nobody can join.
// -----------------------------------------------------------------------------------------

fn venue_row(order_id: i32, order_ref: &str, total_qty: f64, status: &str) -> IbOpenOrder {
    IbOpenOrder {
        order_id,
        order_ref: order_ref.into(),
        client_id: 7,
        account: "DU1234567".into(),
        total_qty,
        filled_qty: 0.0,
        status: status.into(),
        ..Default::default()
    }
}

fn learning_mapper() -> EventMapper {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let mut m = EventMapper::new(ids);
    m.begin_learning();
    m
}

/// **Finding 1.** A manual TWS order reaches a non-master client as `orderId` 0 with NO `orderRef`.
/// Bound as ours it made `resolve(0, "")` answer `Some("")`, so the next id-less async rejection (the
/// bridge delivers every one of them with `order_id` 0) was emitted for the coid `""` and the real
/// unacked order was never rejected. It binds nothing now.
#[test]
fn a_manual_tws_order_row_binds_nothing_and_the_next_idless_rejection_still_finds_its_order() {
    let mut m = learning_mapper();
    let id = m.ids_mut().next_order_id().unwrap();
    m.ids_mut().bind(id, "coid-real");
    m.on_submit(id, 1.0);

    assert!(m.on_venue_order(&venue_row(0, "", 5.0, "Submitted")).is_empty());
    assert_eq!(m.ids().resolve(0, ""), None, "nothing is bound under id 0");

    let evs = m.on_error(201, 0, "order rejected");
    assert_matches!(
        evs.as_slice(),
        [Event::OrderRejected(r)] if r.client_order_id == "coid-real",
        "the id-less rejection is attributed to the one unacked order, got {evs:?}"
    );
}

#[test]
fn a_row_without_a_ref_or_with_no_usable_id_or_quantity_is_not_rebound() {
    let mut m = learning_mapper();
    assert!(m.on_venue_order(&venue_row(55, "", 5.0, "Submitted")).is_empty(), "no orderRef");
    assert!(m.on_venue_order(&venue_row(-3, "coid-x", 5.0, "Submitted")).is_empty(), "no id");
    assert!(m.on_venue_order(&venue_row(56, "coid-y", 0.0, "Submitted")).is_empty(), "no total");
    assert!(m.on_venue_order(&venue_row(57, "coid-z", f64::NAN, "Submitted")).is_empty());
    for (id, coid) in [(55, "coid-x"), (56, "coid-y"), (57, "coid-z")] {
        assert_eq!(m.ids().resolve(id, coid), None, "{coid} is not bound");
    }
}

#[test]
fn a_row_of_a_finished_order_is_not_rebound() {
    let mut m = learning_mapper();
    for (id, status) in [(60, "Filled"), (61, "Cancelled"), (62, "ApiCancelled")] {
        assert!(m.on_venue_order(&venue_row(id, &format!("coid-{id}"), 5.0, status)).is_empty());
        assert_eq!(m.ids().resolve(id, ""), None);
    }
}

/// A row that contradicts a binding this connection holds changes nothing.
#[test]
fn a_row_contradicting_a_live_binding_is_ignored() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    m.begin_learning();
    assert!(m.on_venue_order(&venue_row(101, "coid-B", 5.0, "Submitted")).is_empty());
    assert_eq!(m.ids().resolve(101, ""), Some("coid-A".to_string()));
    assert_eq!(m.ids().order_id_of("coid-B"), None);
}

/// **Finding 5.** A rebuilt mapper has no `unacked` set, so a rebound order was never announced and an
/// engine order whose accept the outage swallowed stayed `Submitted`. A working row announces it (the
/// FSM drops the duplicate on an order that is already accepted).
#[test]
fn a_working_row_of_our_own_is_rebound_and_announced_as_accepted_once() {
    let mut m = learning_mapper();
    let evs = m.on_venue_order(&venue_row(55, "coid-r", 10.0, "Submitted"));
    assert_matches!(
        evs.as_slice(),
        [Event::OrderAccepted(a)] if a.client_order_id == "coid-r",
        "got {evs:?}"
    );
    assert!(m.on_venue_order(&venue_row(55, "coid-r", 10.0, "Submitted")).is_empty(), "once");
    assert_eq!(m.ids().resolve(55, ""), Some("coid-r".to_string()));
    // Its terminal status, which carries no orderRef, resolves to the coid.
    let evs = m.on_order_status(IbOrderStatus {
        order_id: 55,
        order_ref: String::new(),
        status: "Cancelled".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    });
    assert_matches!(evs.as_slice(), [Event::OrderCanceled(c)] if c.client_order_id == "coid-r");
}

#[test]
fn a_row_that_rests_without_working_is_bound_but_not_announced() {
    let mut m = learning_mapper();
    assert!(m.on_venue_order(&venue_row(58, "coid-pc", 10.0, "PendingCancel")).is_empty());
    assert_eq!(m.ids().resolve(58, ""), Some("coid-pc".to_string()));
}

/// **Finding 2.** A rebound order had no `fill_state`, so its first execution (replayed OR live) was
/// classified FINAL: the mapper forgot the order and reported a partial as complete, and every later
/// execution resolved to nothing. Its tracking is seeded from the row's total.
#[test]
fn a_rebound_orders_first_execution_is_a_partial_and_the_one_that_completes_it_is_final() {
    let mut m = learning_mapper();
    m.register_symbol("coid-r", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let _ = m.on_venue_order(&venue_row(55, "coid-r", 10.0, "Submitted"));

    assert!(exec(&mut m, 55, "coid-r", "e1", 4.0).is_empty());
    let evs = commission(&mut m, "e1");
    assert_matches!(
        evs.as_slice(),
        [Event::OrderPartiallyFilled(f)] if f.client_order_id == "coid-r",
        "a partial of a rebound order is a partial, got {evs:?}"
    );
    assert_eq!(m.ids().resolve(55, ""), Some("coid-r".to_string()), "still followed");

    assert!(exec(&mut m, 55, "coid-r", "e2", 6.0).is_empty());
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]);
    assert_eq!(m.ids().resolve(55, ""), None, "forgotten only when it is complete");
}

#[test]
fn a_rebound_order_filled_in_three_pieces_is_partial_twice_then_final() {
    let mut m = learning_mapper();
    m.register_symbol("coid-r", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let _ = m.on_venue_order(&venue_row(55, "coid-r", 9.0, "PreSubmitted"));
    let mut kinds = Vec::new();
    for (n, shares) in [3.0, 3.0, 3.0].into_iter().enumerate() {
        let id = format!("e{n}");
        let _ = exec(&mut m, 55, "coid-r", &id, shares);
        kinds.push(match commission(&mut m, &id).as_slice() {
            [Event::OrderPartiallyFilled(_)] => "partial",
            [Event::OrderFilled(_)] => "final",
            other => panic!("unexpected {other:?}"),
        });
    }
    assert_eq!(kinds, ["partial", "partial", "final"]);
}

/// **Finding 6.** The replay of the day's executions carries one commission per execution; for the
/// orders that finished before this connection nothing can join them, and each PARKED for good (a
/// `warn!` storm at the 1024 cap on a busy account). They are dropped.
#[test]
fn the_replayed_executions_of_finished_orders_park_no_commission() {
    let mut m = learning_mapper();
    for n in 0..(MAX_PARKED_COMMISSIONS + 50) {
        let id = format!("old-{n}");
        assert!(exec(&mut m, 900 + (n as i32), "coid-finished", &id, 1.0).is_empty());
        assert!(commission(&mut m, &id).is_empty());
    }
    assert!(m.commissions.is_empty(), "no commission is parked: {}", m.commissions.len());
    assert!(m.pending.is_empty());
}

/// The mark is not a verdict: an execution that did not resolve the first time and does the second
/// (the order was learned in between) joins its commission as usual.
#[test]
fn an_execution_that_resolves_on_its_replay_joins_its_commission() {
    let mut m = learning_mapper();
    m.register_symbol("coid-r", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    assert!(exec(&mut m, 55, "coid-r", "e1", 4.0).is_empty(), "unroutable: nothing learned yet");
    assert!(commission(&mut m, "e1").is_empty());
    let _ = m.on_venue_order(&venue_row(55, "coid-r", 10.0, "Submitted"));
    // The replay: the same execution again, then its commission.
    assert!(exec(&mut m, 55, "coid-r", "e1", 4.0).is_empty());
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
}

#[test]
fn the_snapshot_requests_are_walked_in_order_and_only_the_awaited_one_counts() {
    use crate::transport::Snapshot;
    let mut m = EventMapper::new(IdRegistry::default());
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Ignore, "not learning");
    m.begin_learning();
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Ignore, "not awaited yet");
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert_eq!(m.learning(), Learning::Executions);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::Ignore);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    assert_eq!(m.learning(), Learning::Done);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Ignore, "the sweep's own");
}

#[test]
fn an_incomplete_snapshot_fails_the_learning_for_good() {
    use crate::transport::Snapshot;
    let mut m = EventMapper::new(IdRegistry::default());
    m.begin_learning();
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, false), LearnStep::Failed);
    assert_eq!(m.learning(), Learning::Failed);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Ignore);

    let mut m = EventMapper::new(IdRegistry::default());
    m.begin_learning();
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, false), LearnStep::Failed);
    assert_eq!(m.learning(), Learning::Failed);
}

// -----------------------------------------------------------------------------------------
// Review 18: R1 (stats, the suspicious snapshot, the recovered fill), R2 (nothing bound outside the learning
// window without its fill tracking), R7 (a GTC filled on an earlier day).
// -----------------------------------------------------------------------------------------

fn exec_of(
    m: &mut EventMapper,
    order_id: i32,
    coid: &str,
    exec_id: &str,
    shares: f64,
    recoverable: bool,
) -> Vec<Event> {
    m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares,
        price: 190.0,
        ts: 1,
        client_id: 7,
        recoverable,
    })
}

/// **R2.** After a failed (or abandoned) learning TWS still pushes unsolicited `openOrder` updates. Such a
/// row of our own for an order this connection does not hold used to be bound with NO fill tracking, so the
/// order's first execution was classified FINAL (the id forgotten, every later fill dropped).
#[test]
fn an_own_row_outside_the_learning_window_for_an_unknown_order_binds_nothing() {
    for failed in [false, true] {
        let mut ids = IdRegistry::default();
        ids.on_next_valid_id(101);
        let mut m = EventMapper::new(ids);
        if failed {
            m.begin_learning();
            assert_eq!(
                m.snapshot_ended(crate::transport::Snapshot::OpenOrders, false),
                LearnStep::Failed
            );
        }
        assert!(m.on_venue_order(&venue_row(55, "coid-r", 10.0, "Submitted")).is_empty());
        assert_eq!(m.ids().resolve(55, ""), None, "not bound (failed learning: {failed})");
        // So its fill is not classified at all, rather than as a final one that forgets the order.
        assert!(exec_of(&mut m, 55, "coid-r", "e1", 4.0, false).is_empty());
        assert!(commission(&mut m, "e1").is_empty());
    }
}

/// An update for an order this connection HOLDS keeps the long-standing silent rebind and does not touch
/// its fill tracking.
#[test]
fn an_open_order_update_for_an_order_this_connection_holds_keeps_its_fill_tracking() {
    let mut m = mapper_submitted("coid-A", 101, 5.0);
    assert!(m.on_venue_order(&venue_row(101, "coid-A", 99.0, "Submitted")).is_empty());
    assert!(exec(&mut m, 101, "coid-A", "e1", 3.0).is_empty());
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert!(exec(&mut m, 101, "coid-A", "e2", 2.0).is_empty());
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)], "total stayed 5");
}

/// **R7.** A GTC order partly filled on an EARLIER day: the day's executions replay cannot hold those
/// fills, so the order would never complete. Its total is reduced by the part of the venue's filled
/// quantity the replay did not account for.
#[test]
fn a_gtc_filled_in_part_on_an_earlier_day_completes_on_the_fill_that_finishes_it() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-gtc", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let mut row = venue_row(55, "coid-gtc", 10.0, "Submitted");
    row.filled_qty = 6.0; // yesterday
    let _ = m.on_venue_order(&row);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done, "nothing replayed");
    assert!(exec(&mut m, 55, "coid-gtc", "e1", 4.0).is_empty());
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]);
}

/// Where the day's replay already holds the filled quantity, nothing is reduced (adding the venue's number
/// to the replayed sum would count the same fills twice and finalise early).
#[test]
fn a_fill_the_replay_already_holds_is_not_counted_twice() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-r", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let mut row = venue_row(55, "coid-r", 10.0, "Submitted");
    row.filled_qty = 4.0; // today
    let _ = m.on_venue_order(&row);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    // The replay: the 4 that the venue counted.
    assert!(exec(&mut m, 55, "coid-r", "e1", 4.0).is_empty());
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    // 6 more complete it; a premature final would have come at the first fill.
    assert!(exec(&mut m, 55, "coid-r", "e2", 3.0).is_empty());
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert!(exec(&mut m, 55, "coid-r", "e3", 3.0).is_empty());
    assert_matches!(commission(&mut m, "e3").as_slice(), [Event::OrderFilled(_)]);
}

/// **R1, the suspicious snapshot.** Rows exist, none is attributed to this client, and some carry an
/// `orderRef`: a decoder that left `client_id` / `account` unset looks exactly like that. The connection
/// does not claim to have learned.
#[test]
fn a_snapshot_of_tagged_rows_none_attributed_to_us_is_not_a_learned_venue() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.note_row(false, true, true);
    m.note_row(false, true, true);
    assert!(m.learn_stats().suspicious());
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::Suspicious);
    assert_eq!(m.learning(), Learning::Failed, "and the executions are not asked for");
}

#[test]
fn hand_made_rows_or_no_rows_or_one_of_ours_are_not_suspicious() {
    use crate::transport::Snapshot;
    // Rows made by hand in TWS: no orderRef.
    let mut m = learning_mapper();
    m.note_row(false, false, false);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    // Nothing rests at all.
    let mut m = learning_mapper();
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    // One of ours among strangers.
    let mut m = learning_mapper();
    m.note_row(false, true, true);
    m.note_row(true, true, false);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    let stats = m.learn_stats().clone();
    assert_eq!((stats.rows, stats.ours, stats.foreign_with_ref), (2, 1, 1));
}

/// Finding 2: tagged rows whose client id or account DID decode are somebody else's orders (a hand-typed note in
/// TWS, another API client's tag): counted for the log, never suspicious.
#[test]
fn tagged_rows_whose_owner_decoded_are_not_suspicious() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.note_row(false, true, false);
    m.note_row(false, true, false);
    assert!(!m.learn_stats().suspicious());
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    let stats = m.learn_stats().clone();
    assert_eq!(
        (stats.rows, stats.ours, stats.foreign_with_ref, stats.undecoded_with_ref),
        (2, 0, 2, 0)
    );
    // One undecoded tagged row among them is the blind decoder's shape.
    let mut m = learning_mapper();
    m.note_row(false, true, false);
    m.note_row(false, true, true);
    assert!(m.learn_stats().suspicious());
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::Suspicious);
}

/// The executions replay of the learning is due when the open orders end; held back it is OWED, and handed out
/// once, only while the learning still waits for it, when the hold lifts.
#[test]
fn a_held_back_executions_replay_is_owed_and_taken_once_when_the_hold_lifts() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.hold_executions(true);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(!m.executions_due(), "held back");
    assert!(m.executions_owed());
    assert!(!m.take_owed_executions(), "not while the hold stands");
    m.hold_executions(false);
    assert!(m.take_owed_executions());
    assert!(!m.take_owed_executions(), "once");
    assert!(!m.executions_owed());

    // Not held: due at once, nothing owed.
    let mut m = learning_mapper();
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(m.executions_due());
    assert!(!m.executions_owed());

    // A learning that gave up owes nothing.
    let mut m = learning_mapper();
    m.hold_executions(true);
    let _ = m.snapshot_ended(Snapshot::OpenOrders, true);
    assert!(!m.executions_due());
    m.abandon_learning();
    m.hold_executions(false);
    assert!(!m.take_owed_executions(), "the learning is over");
}

#[test]
fn the_rows_are_counted_only_while_the_open_orders_are_being_learned() {
    let mut m = EventMapper::new(IdRegistry::default());
    m.note_row(true, true, false);
    assert_eq!(
        m.learn_stats().rows,
        0,
        "an openOrder update in normal operation is not a snapshot row"
    );
}

#[test]
fn the_coids_a_learning_rebound_are_named_for_the_log() {
    let mut m = learning_mapper();
    m.note_row(true, true, false);
    let _ = m.on_venue_order(&venue_row(55, "coid-a", 10.0, "Submitted"));
    m.note_row(true, true, false);
    let _ = m.on_venue_order(&venue_row(56, "coid-b", 10.0, "PreSubmitted"));
    assert_eq!(m.learn_stats().bound, ["coid-a", "coid-b"]);
}

/// **R1, the recovery.** An execution of THIS client that no order of the connection explains, replayed
/// while the connection learns, is a fill of an order that FINISHED during the outage. It is recovered by
/// its `orderRef` as a PARTIAL (nothing here says how the order ended) and changes no bookkeeping; the
/// engine's trade-id dedup drops what it already folded. Only for an order the host TAUGHT a symbol: see
/// `an_untaught_replayed_execution_emits_nothing_and_is_left_to_the_reconcile`.
#[test]
fn a_replayed_execution_of_our_client_that_nothing_explains_is_recovered_as_a_partial() {
    let mut m = learning_mapper();
    m.register_symbol("coid-old", "AAPL.SMART.USD");
    m.note_row(false, false, false);
    use crate::transport::Snapshot;
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    let evs = exec_of(&mut m, 900, "coid-old", "old1", 25.0, true);
    assert!(
        evs.is_empty(),
        "no accept is invented for an order the venue no longer holds (F4): {evs:?}"
    );
    let evs = commission(&mut m, "old1");
    assert_matches!(
        evs.as_slice(),
        [Event::OrderPartiallyFilled(w)] if w.client_order_id == "coid-old" && w.fill.last_qty == 25.0,
        "a partial even though nothing says it is not the whole order: {evs:?}"
    );
    assert_eq!(m.ids().resolve(900, ""), None, "nothing was bound");
    assert!(m.fill_state.is_empty(), "and no fill tracking was created");
}

/// Not recoverable (not this client's, or not while learning): dropped as unroutable, as before.
#[test]
fn an_execution_that_is_not_recoverable_is_dropped_as_before() {
    let mut m = learning_mapper();
    assert!(exec_of(&mut m, 900, "coid-old", "old1", 25.0, false).is_empty());
    assert!(commission(&mut m, "old1").is_empty());
    assert!(m.commissions.is_empty() && m.pending.is_empty());
}

/// The replayed execution of a finished order as IB sends it overnight: the instrument is the overnight
/// venue's (`AAPL.IBEOS.USD`, measured), not the `AAPL.SMART.USD` the engine mounts.
fn overnight_replay_of(m: &mut EventMapper, coid: &str, exec_id: &str) -> Vec<Event> {
    m.on_exec_details(IbExecDetails {
        order_id: 900,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.IBEOS.USD".into(),
        side_buy: true,
        shares: 25.0,
        price: 190.0,
        ts: 1,
        client_id: 7,
        recoverable: true,
    })
}

/// **An order nobody taught this connection is NOT recovered.** After a rebuild IB replays the whole day's
/// executions of our client id, finished orders included. With no symbol taught for the coid the fill could
/// carry only the instrument IB echoed, and whether the engine books it (`AAPL.SMART.USD`) or drops it
/// (`AAPL.IBEOS.USD`) would be decided by that spelling alone. So nothing is emitted: the execution is
/// remembered as unroutable (its commission is dropped, not parked), ONE `info!` names it, and the reconcile
/// pass is what reports a fill the engine never booked.
#[test]
fn an_untaught_replayed_execution_emits_nothing_and_is_left_to_the_reconcile() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.note_row(false, false, false);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);

    let (evs, logs) = vike_log::capture::captured(|| {
        let first = overnight_replay_of(&mut m, "coid-old", "old1");
        // The same execution delivered again says nothing more: ONE line per execution.
        let again = overnight_replay_of(&mut m, "coid-old", "old1");
        (first, again)
    });
    assert!(
        evs.0.is_empty() && evs.1.is_empty(),
        "no recovered partial for an untaught order: {evs:?}"
    );
    assert!(
        commission(&mut m, "old1").is_empty(),
        "its commission is dropped, so no fill ever joins"
    );
    assert!(m.pending.is_empty() && m.commissions.is_empty(), "nothing is buffered");
    assert!(m.unroutable.contains_key("old1"), "counted as an unroutable execution");
    assert!(m.fill_state.is_empty() && m.emitted.is_empty(), "no bookkeeping was touched");

    let said: Vec<_> = logs.iter().filter(|e| e.message.contains("nobody taught")).collect();
    assert_eq!(said.len(), 1, "one line per execution: {logs:?}");
    let line = said[0];
    assert_eq!(line.level, tracing::Level::INFO);
    assert_eq!(line.field("coid"), Some("coid-old"));
    assert_eq!(line.field("exec_id"), Some("old1"));
    assert_eq!(line.field("side"), Some("buy"));
    assert_eq!(line.field("qty"), Some("25.0"));
    assert_eq!(line.field("price"), Some("190.0"));
    assert_eq!(line.field("echoed"), Some("AAPL.IBEOS.USD"));
    assert!(line.message.contains("MissingFill"), "{}", line.message);
}

/// The commission of the untaught execution may land FIRST: it is parked, and the execution that follows
/// releases it (nothing can join it) instead of pairing with it into a fill.
#[test]
fn a_commission_parked_before_an_untaught_replayed_execution_is_released() {
    let mut m = learning_mapper();
    assert!(commission(&mut m, "old2").is_empty());
    assert!(m.commissions.contains_key("old2"), "parked, its execution has not arrived");
    assert!(overnight_replay_of(&mut m, "coid-old", "old2").is_empty());
    assert!(m.commissions.is_empty() && m.pending.is_empty());
    assert!(m.unroutable.contains_key("old2"));
}

/// The other half: an order the host TAUGHT keeps today's behaviour exactly - `[Fill, PartiallyFilled]` carrying
/// the TAUGHT symbol (the engine mounts it) and not the overnight echo, never final, no bookkeeping, no
/// "nobody taught" line.
#[test]
fn a_taught_replayed_execution_still_yields_its_fill_under_the_taught_symbol() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-old", "AAPL.SMART.USD");
    m.note_row(false, false, false);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);

    let ((first, joined), logs) = vike_log::capture::captured(|| {
        (overnight_replay_of(&mut m, "coid-old", "old1"), commission_raw(&mut m, "old1"))
    });
    assert!(first.is_empty(), "the fill waits for its commission: {first:?}");
    assert_matches!(
        joined.as_slice(),
        [Event::Fill(f), Event::OrderPartiallyFilled(w)]
            if f.symbol == "AAPL.SMART.USD"
                && f.client_order_id == "coid-old"
                && f.trade_id.as_str() == "old1"
                && f.last_qty == 25.0
                && w.fill.symbol == "AAPL.SMART.USD",
        "{joined:?}"
    );
    assert!(m.fill_state.is_empty(), "and no fill tracking was created");
    assert!(m.emitted.contains_key("old1"), "emitted once: a second delivery is inert");
    assert!(overnight_replay_of(&mut m, "coid-old", "old1").is_empty());
    assert!(!logs.iter().any(|e| e.message.contains("nobody taught")), "{logs:?}");
}

/// An execution of a REBOUND order as IB replays it overnight: the instrument is the overnight venue's.
fn overnight_exec_of(
    m: &mut EventMapper,
    order_id: i32,
    coid: &str,
    exec_id: &str,
    shares: f64,
) -> Vec<Event> {
    m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.IBEOS.USD".into(),
        side_buy: true,
        shares,
        price: 190.0,
        ts: 1,
        client_id: 7,
        recoverable: true,
    })
}

/// A learning mapper that rebound one OPEN order of ours (id 55, quantity 10) of which the venue says 4 are filled,
/// with its open orders ended and the executions replay asked for.
fn mapper_with_a_partly_filled_open_order(coid: &str) -> EventMapper {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    let mut row = venue_row(55, coid, 10.0, "Submitted");
    row.filled_qty = 4.0;
    assert_matches!(m.on_venue_order(&row).as_slice(), [Event::OrderAccepted(_)]);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    m
}

/// **An OPEN order nobody taught emits no fill, and stays TRACKED.** After a rebuild the venue lists the order (it is
/// rebound, announced as accepted) and replays its executions, which the engine already folded. With no symbol
/// taught the fill could carry only IB's echo (`AAPL.IBEOS.USD` overnight): the engine would book or drop it by
/// that spelling, and the repeat would land in the ring, `recent` and the journal. So no `Fill` and no wrapper leave
/// the bridge, ONE `info!` names the execution, and everything else works: the fill tracking advanced (a fill that
/// completes the order is final), the order cancels under IB's own id, and a LATER teach makes its fills flow.
#[test]
fn an_untaught_open_order_that_is_partly_filled_emits_no_fill_and_stays_tracked() {
    use crate::transport::Snapshot;
    let mut m = mapper_with_a_partly_filled_open_order("coid-open");

    let (evs, logs) = vike_log::capture::captured(|| {
        let first = overnight_exec_of(&mut m, 55, "coid-open", "e1", 4.0);
        let joined = commission_raw(&mut m, "e1");
        // The replay delivers the same execution again: inert, and no second line.
        let again = overnight_exec_of(&mut m, 55, "coid-open", "e1", 4.0);
        (first, joined, again)
    });
    assert!(
        evs.0.is_empty() && evs.1.is_empty() && evs.2.is_empty(),
        "no fill, no wrapper: {evs:?}"
    );
    let said: Vec<_> = logs.iter().filter(|e| e.message.contains("nobody taught")).collect();
    assert_eq!(said.len(), 1, "one line per execution: {logs:?}");
    assert_eq!(said[0].level, tracing::Level::INFO);
    assert_eq!(said[0].field("coid"), Some("coid-open"));
    assert_eq!(said[0].field("exec_id"), Some("e1"));
    assert_eq!(said[0].field("echoed"), Some("AAPL.IBEOS.USD"));
    assert!(said[0].message.contains("MissingFill"), "{}", said[0].message);
    assert!(!logs.iter().any(|e| e.message.contains("no submitted symbol is known")), "{logs:?}");

    // Tracked: the fill is counted (the venue's 4 are accounted for: the total stays 10), and the order resolves.
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    assert_eq!(m.fill_state[&55].filled, 4.0);
    assert_eq!(m.fill_state[&55].total, 10.0);
    assert_eq!(m.ids().resolve(55, ""), Some("coid-open".to_string()));
    assert!(m.emitted.contains_key("e1"));

    // A LATER teach (the tradehub's own `adopt_restored`): the next fill flows under the taught symbol, and the
    // fill that completes the order is FINAL.
    m.register_symbol("coid-open", "AAPL.SMART.USD");
    assert!(overnight_exec_of(&mut m, 55, "coid-open", "e2", 6.0).is_empty());
    assert_matches!(
        commission_raw(&mut m, "e2").as_slice(),
        [Event::Fill(f), Event::OrderFilled(w)]
            if f.symbol == "AAPL.SMART.USD" && f.last_qty == 6.0 && w.client_order_id == "coid-open"
    );
}

/// The taught twin of the test above: the same open, partly filled order, its symbol taught by the host, emits
/// its replayed fill under the TAUGHT symbol, exactly as before, and no line says anything was withheld.
#[test]
fn the_taught_twin_of_an_open_partly_filled_order_still_emits_its_replayed_fill() {
    let mut m = mapper_with_a_partly_filled_open_order("coid-open");
    m.register_symbol("coid-open", "AAPL.SMART.USD");

    let ((first, joined), logs) = vike_log::capture::captured(|| {
        (overnight_exec_of(&mut m, 55, "coid-open", "e1", 4.0), commission_raw(&mut m, "e1"))
    });
    assert!(first.is_empty());
    assert_matches!(
        joined.as_slice(),
        [Event::Fill(f), Event::OrderPartiallyFilled(w)]
            if f.symbol == "AAPL.SMART.USD" && f.last_qty == 4.0 && w.client_order_id == "coid-open"
    );
    assert!(!logs.iter().any(|e| e.message.contains("nobody taught")), "{logs:?}");
}

/// The cancel flush is the other place a fill leaves the mapper: an untaught order's unjoined fill is NOT flushed
/// when the order is cancelled (the commission never came); the cancel itself still goes out.
#[test]
fn an_untaught_orders_unjoined_fill_is_not_flushed_by_its_cancel() {
    let mut m = mapper_with_a_partly_filled_open_order("coid-open");
    assert!(overnight_exec_of(&mut m, 55, "coid-open", "e1", 4.0).is_empty());
    assert!(m.on_order_status(cancelled(55, "coid-open")).is_empty(), "held for the commission");
    let evs = m.release_held_cancels(0, 0);
    assert_matches!(
        evs.as_slice(),
        [Event::OrderCanceled(c)] if c.client_order_id == "coid-open",
        "the cancel, and no fill: {evs:?}"
    );
    assert!(m.pending.is_empty() && m.emitted.contains_key("e1"));
}

/// A recovered fill whose `orderId` collides with a bound order of ours must not touch that order's tracking.
#[test]
fn a_recovered_fill_never_touches_another_orders_tracking() {
    // Submitted and NOT yet acked: the one state a stray removal from `unacked` would change.
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 5.0);
    m.register_symbol("coid-other", "AAPL.SMART.USD"); // taught, so the execution IS recovered
    // A recovered execution of an unrelated coid that happens to carry order id 101.
    let evs = exec_of(&mut m, 101, "coid-other", "e9", 5.0, true);
    // It resolved by the numeric id? No: its orderRef decides, and the ref is not ours.
    assert!(evs.is_empty(), "{evs:?}");
    assert!(m.unacked.contains(&101), "...nor does it take coid-A out of the unacked set (F4)");
    let _ = commission(&mut m, "e9");
    assert_eq!(m.fill_state[&101].filled, 0.0, "coid-A's tracking is untouched");
    assert_eq!(m.ids().resolve(101, ""), Some("coid-A".to_string()));
}

// -----------------------------------------------------------------------------------------
// R9 (review 18): the IBKR mapper emitted only the `OrderFilled`/`OrderPartiallyFilled` WRAPPERS and
// never the bare `Event::Fill`. `Account::apply_fill` (position, realized PnL, balance, commission) is
// reached only from a bare `Event::Fill`; the wrapper only advances the order's FSM. Every other bridge
// emits `[Fill, wrap]` (`vike_bridge_core::terminal_events`).
// -----------------------------------------------------------------------------------------

fn bare_fills(evs: &[Event]) -> Vec<&FillEvent> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect()
}

fn book() -> vike_exec::account::Account {
    vike_exec::account::Account::new(1.0, "ibkr", None, vike_exec::account::BalanceMode::Delta)
}

fn net_size(a: &vike_exec::account::Account) -> f64 {
    a.positions.values().map(|p| p.size).sum()
}

/// A joined partial is `[Fill, OrderPartiallyFilled]`, a completing fill `[Fill, OrderFilled]`, and the
/// bare fill and its wrapper carry the SAME trade id (the IB execution id: the dedup key of both folds).
#[test]
fn a_joined_fill_is_the_bare_fill_then_its_wrapper_with_one_trade_id() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty());
    let evs = commission_raw(&mut m, "e1");
    assert_matches!(
        evs.as_slice(),
        [Event::Fill(f), Event::OrderPartiallyFilled(w)] if f.trade_id == w.fill.trade_id && f.trade_id.as_str() == "e1"
    );
    assert!(exec(&mut m, 101, "coid-A", "e2", 6.0).is_empty());
    assert_matches!(
        commission_raw(&mut m, "e2").as_slice(),
        [Event::Fill(f), Event::OrderFilled(w)] if f.trade_id == w.fill.trade_id
    );
}

/// The point of it: the account's position MOVES on the bare fill, and a replayed fill is refused by
/// the account's own trade-id ledger (the same id), so a replay never moves it twice.
#[test]
fn an_ibkr_fill_moves_the_account_position_and_its_replay_does_not_move_it_twice() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    let evs = commission_raw(&mut m, "e1");
    let fills = bare_fills(&evs);
    assert_eq!(fills.len(), 1, "one bare fill per execution: {evs:?}");

    let mut account = book();
    assert!(account.apply_fill(fills[0]).applied());
    assert_eq!(net_size(&account), 4.0, "the position moved by the fill");
    assert_eq!(
        account.apply_fill(fills[0]),
        vike_exec::account::FillFold::Duplicate,
        "a replay of the same execution id is refused"
    );
    assert_eq!(net_size(&account), 4.0);
}

/// A partial flushed by a cancel whose commission did not come within the bound is a fill too: it carries the
/// bare `Fill` before the wrapper and the `OrderCanceled`.
#[test]
fn a_partial_flushed_by_a_cancel_carries_its_bare_fill() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(exec(&mut m, 101, "coid-A", "e1", 3.0).is_empty());
    let held = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 3.0,
        avg_fill_price: 190.0,
        ..Default::default()
    });
    assert!(held.is_empty(), "the cancel waits for the commission: {held:?}");
    let evs = m.release_held_cancels(0, 0);
    assert_matches!(
        evs.as_slice(),
        [Event::Fill(f), Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)] if f.trade_id.as_str() == "e1"
    );
}

// -----------------------------------------------------------------------------------------
// W14e (re-review of Part B): the symbol a fill carries (F1), recovered fills (F4), the settle of a learned
// total (F5), the knowledge a submit needs (F6).
// -----------------------------------------------------------------------------------------

fn fill_symbols(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.symbol.to_string()),
            Event::OrderFilled(w) => Some(w.fill.symbol.to_string()),
            Event::OrderPartiallyFilled(w) => Some(w.fill.symbol.to_string()),
            _ => None,
        })
        .collect()
}

fn exec_echoing(m: &mut EventMapper, order_id: i32, coid: &str, exec_id: &str, echoed: &str) {
    let _ = m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: echoed.into(),
        side_buy: true,
        shares: 10.0,
        price: 1.1,
        ts: 1,
        ..Default::default()
    });
}

/// **F1.** The engine drops a bare `Fill` whose symbol it neither mounts nor registered the order under, in
/// silence. IB echoes the exec contract (`EUR.IDEALPRO.USD` for a pair the tree writes `EUR.USD.IDEALPRO`), so
/// the fill - and its wrapper, which embeds the same `FillEvent` - carries the symbol the order was SUBMITTED
/// under.
#[test]
fn a_fill_carries_the_symbol_the_order_was_submitted_under_not_the_one_ib_echoes() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.register_symbol("coid-A", "EUR.USD.IDEALPRO");
    exec_echoing(&mut m, 101, "coid-A", "e1", "EUR.IDEALPRO.USD");
    let evs = commission_raw(&mut m, "e1");
    assert_eq!(fill_symbols(&evs), ["EUR.USD.IDEALPRO", "EUR.USD.IDEALPRO"], "{evs:?}");
}

/// The cancel-held fill is stamped the same way (the symbol is resolved when the execution ARRIVES).
#[test]
fn a_fill_released_by_a_cancel_carries_the_submitted_symbol_too() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.register_symbol("coid-A", "EUR.USD.IDEALPRO");
    exec_echoing(&mut m, 101, "coid-A", "e1", "EUR.IDEALPRO.USD");
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let evs = m.release_held_cancels(0, 0);
    assert_eq!(fill_symbols(&evs), ["EUR.USD.IDEALPRO", "EUR.USD.IDEALPRO"], "{evs:?}");
}

/// An order nobody told the symbol of (rebound after a rebuild and not taught by the host) still carries the
/// instrument IB echoed: that is what the fill used to carry, and the `warn!` says the engine may drop it.
#[test]
fn a_fill_of_an_order_with_no_registered_symbol_carries_the_echo() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    exec_echoing(&mut m, 101, "coid-A", "e1", "EUR.IDEALPRO.USD");
    let evs = commission_raw(&mut m, "e1");
    assert_eq!(fill_symbols(&evs), ["EUR.IDEALPRO.USD", "EUR.IDEALPRO.USD"], "{evs:?}");
}

/// What the host teaches about a restored order (`register_symbol` through the `Taught` hand-off) reaches the
/// fills of the order a rebuilt connection rebound from the venue's open orders.
#[test]
fn a_taught_symbol_reaches_the_fills_of_a_rebound_order() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-rested", "EUR.USD.IDEALPRO");
    let _ = m.on_venue_order(&venue_row(55, "coid-rested", 10.0, "Submitted"));
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    exec_echoing(&mut m, 55, "coid-rested", "e1", "EUR.IDEALPRO.USD");
    let evs = commission_raw(&mut m, "e1");
    assert_eq!(fill_symbols(&evs), ["EUR.USD.IDEALPRO", "EUR.USD.IDEALPRO"], "{evs:?}");
}

/// The table does not grow for ever: an order that reached a terminal leaves it.
#[test]
fn an_order_that_retires_forgets_its_registered_symbol() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.register_symbol("coid-A", "AAPL.SMART.USD");
    exec_echoing(&mut m, 101, "coid-A", "e1", "AAPL.SMART.USD");
    let _ = commission_raw(&mut m, "e1"); // the 10 complete it
    assert!(m.symbols.is_empty());
    let mut m = mapper_submitted("coid-B", 102, 10.0);
    m.register_symbol("coid-B", "AAPL.SMART.USD");
    let _ = m.on_order_status(cancelled(102, "coid-B"));
    assert!(m.symbols.is_empty());
    let mut m = mapper_submitted("coid-C", 103, 10.0);
    m.register_symbol("coid-C", "AAPL.SMART.USD");
    let _ = m.on_error(201, 103, "rejected");
    assert!(m.symbols.is_empty());
}

/// **F4.** A recovered fill awaiting its commission belongs to a coid of the PREVIOUS connection whose numeric
/// order id may be a live order's here: the cancel of that live order must not flush (or otherwise touch) it.
#[test]
fn a_recovered_fill_awaiting_its_commission_is_not_flushed_by_the_cancel_of_a_colliding_order_id() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.register_symbol("coid-old", "AAPL.SMART.USD"); // taught, so the execution IS recovered
    assert!(exec_of(&mut m, 101, "coid-old", "e9", 5.0, true).is_empty());
    assert!(m.pending.contains_key("e9"));
    let out = m.on_order_status(cancelled(101, "coid-A"));
    assert_matches!(out.as_slice(), [Event::OrderCanceled(c)] if c.client_order_id == "coid-A");
    assert!(m.pending.contains_key("e9"), "the recovered execution still waits for ITS commission");
    let evs = commission_raw(&mut m, "e9");
    assert_matches!(
        evs.as_slice(),
        [Event::Fill(f), Event::OrderPartiallyFilled(_)] if f.client_order_id == "coid-old"
    );
}

/// **F5.** The learning ended while the replayed execution's commission was still in flight. The settle used
/// to take the venue's filled quantity (4) off the total at once - the fill then counted again at the join -
/// so the total fell to 6 and a LATER fill of 2 finalised the order with 4 still to go.
#[test]
fn a_commission_in_flight_when_the_learning_ends_does_not_shrink_the_total_twice() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-r", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let mut row = venue_row(55, "coid-r", 10.0, "Submitted");
    row.filled_qty = 4.0;
    let _ = m.on_venue_order(&row);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(
        exec(&mut m, 55, "coid-r", "e1", 4.0).is_empty(),
        "the replayed fill waits for its commission"
    );
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    // 4 filled of 10: two more is still a partial, four more completes it.
    assert!(exec(&mut m, 55, "coid-r", "e2", 2.0).is_empty());
    assert_matches!(
        commission(&mut m, "e2").as_slice(),
        [Event::OrderPartiallyFilled(_)],
        "6 of 10"
    );
    assert!(exec(&mut m, 55, "coid-r", "e3", 4.0).is_empty());
    assert_matches!(commission(&mut m, "e3").as_slice(), [Event::OrderFilled(_)]);
}

/// The deferred settle still does its job once the commission joins: the part the replay did NOT hold (an earlier
/// day) comes off the total, and the total never goes below what was filled so far.
#[test]
fn a_deferred_settle_still_takes_the_earlier_days_fills_off_the_total_and_never_below_the_filled() {
    use crate::transport::Snapshot;
    let mut m = learning_mapper();
    m.register_symbol("coid-gtc", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let mut row = venue_row(55, "coid-gtc", 10.0, "Submitted");
    row.filled_qty = 6.0; // 2 yesterday + the 4 of today replayed below
    let _ = m.on_venue_order(&row);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(exec(&mut m, 55, "coid-gtc", "e1", 4.0).is_empty());
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    assert_eq!(m.fill_state[&55].total, 10.0, "not settled while a commission is in flight");
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert_eq!(m.fill_state[&55].total, 8.0, "yesterday's 2 are off the total");
    assert_eq!(m.fill_state[&55].venue_filled, 0.0, "settled once");

    // A venue that claims more filled than the order's total cannot push the total under the fills.
    let mut m = learning_mapper();
    m.register_symbol("coid-odd", "AAPL.SMART.USD"); // taught: an untaught rebound order emits no fill
    let mut row = venue_row(56, "coid-odd", 5.0, "Submitted");
    row.filled_qty = 9.0;
    let _ = m.on_venue_order(&row);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(exec(&mut m, 56, "coid-odd", "e1", 2.0).is_empty());
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert_eq!(m.snapshot_ended(Snapshot::Executions, true), LearnStep::Done);
    let st = &m.fill_state[&56];
    assert!(st.total >= st.filled, "total {} below the {} filled", st.total, st.filled);
}

/// **F6.** A submit needs the venue's open orders to be KNOWN (the id counter has moved past the ones that
/// rest): they are not while the first request runs, nor after it failed or could not be trusted.
#[test]
fn the_open_orders_are_unknown_until_their_request_has_ended_complete() {
    use crate::transport::Snapshot;
    let mut m = EventMapper::new(IdRegistry::default());
    assert!(
        !m.orders_unknown(),
        "a connection that never learns (cpapi, scripted tests) is not refused"
    );
    m.begin_learning();
    assert!(m.orders_unknown());
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::RequestExecutions);
    assert!(!m.orders_unknown(), "the rows are in and the counter is past them");

    let mut m = EventMapper::new(IdRegistry::default());
    m.begin_learning();
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, false), LearnStep::Failed);
    assert!(m.orders_unknown(), "a failed request leaves the orders unknown");

    let mut m = EventMapper::new(IdRegistry::default());
    m.begin_learning();
    m.note_row(false, true, true);
    assert_eq!(m.snapshot_ended(Snapshot::OpenOrders, true), LearnStep::Suspicious);
    assert!(m.orders_unknown(), "rows that cannot be attributed are not a known venue");

    let mut m = EventMapper::new(IdRegistry::default());
    m.begin_learning();
    m.abandon_learning();
    assert!(m.orders_unknown());
}

/// The coids that reached a terminal on this connection are remembered (bounded), so the cancel of one is not
/// reported as the alarm an unknown order is.
#[test]
fn a_finished_order_is_remembered_and_the_memory_is_bounded() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(!m.recently_finished("coid-A"));
    let _ = m.on_order_status(cancelled(101, "coid-A"));
    assert!(m.recently_finished("coid-A"));
    for i in 0..(MAX_RECENT_FINISHED + 5) {
        m.retire(&format!("c{i}"));
    }
    assert!(!m.recently_finished("coid-A"), "the oldest memory is the one forgotten");
    assert!(m.recently_finished(&format!("c{}", MAX_RECENT_FINISHED + 4)));
    assert_eq!(m.finished.len(), MAX_RECENT_FINISHED);
}

/// **F4 (the foreign id).** An execution resolved by its `orderRef` can name another number than the one this
/// connection bound for the coid. The order is tracked under the BOUND id: the final fill forgets the right
/// binding (it used to look the unknown number up, find no tracking and call the fill final at once), and the
/// accept does not take a different order's id out of the unacked set.
#[test]
fn an_execution_naming_a_stale_number_is_tracked_under_the_bound_id() {
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 10.0);
    m.on_submit(102, 5.0); // another order, unacked, whose number the stale one is
    m.ids_mut().bind(102, "coid-B");
    let evs = m.on_exec_details(IbExecDetails {
        order_id: 102,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 4.0,
        price: 190.0,
        ts: 1,
        ..Default::default()
    });
    assert_matches!(lifecycle(evs).as_slice(), [Event::OrderAccepted(a)] if a.client_order_id == "coid-A");
    assert!(
        m.unacked.contains(&102),
        "coid-B is still unacked: coid-A's accept took out ITS id (101)"
    );
    assert!(!m.unacked.contains(&101));
    let out = commission_raw(&mut m, "e1");
    assert_matches!(
        out.as_slice(),
        [Event::Fill(_), Event::OrderPartiallyFilled(_)],
        "4 of 10 is a partial: tracked under the bound id 101, not the stale 102"
    );
    assert_eq!(m.fill_state[&101].filled, 4.0);
    assert_eq!(m.ids().resolve(101, ""), Some("coid-A".to_string()));
}

// ---- amends in flight (`exec::amend`) ----------------------------------------------------------------------------

/// A mapper holding the accepted working order `coid-A` (venue id 101) with an amend to 191 in flight.
fn mapper_with_an_amend_in_flight() -> EventMapper {
    let mut m = mapper_with("coid-A", 101);
    m.on_submit(101, 10.0);
    let _ = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        ..Default::default()
    });
    assert!(
        m.note_amend(
            "coid-A",
            crate::exec::amend::PendingAmend::new(
                101,
                (10.0, Some(190.0)),
                (10.0, Some(191.0)),
                false
            )
        )
        .is_none()
    );
    m
}

/// The venue's `openOrder` echo of a working order of ours is the answer to the amend: `OrderModified`, once.
#[test]
fn the_open_order_echo_of_an_amended_order_becomes_order_modified() {
    let mut m = mapper_with_an_amend_in_flight();
    let mut row = venue_row(101, "coid-A", 10.0, "Submitted");
    row.limit_price = Some(190.0);
    assert!(m.on_venue_order(&row).is_empty(), "the old terms confirm nothing");
    row.limit_price = Some(191.0);
    let evs = m.on_venue_order(&row);
    assert_matches!(
        evs.as_slice(),
        [Event::OrderModified(e)] if e.client_order_id == "coid-A" && e.new_price == Some(191.0),
        "got {evs:?}"
    );
    assert!(m.on_venue_order(&row).is_empty(), "answered once");
}

/// An order-rejection-class error that names an order with an amend in flight refuses the amend, NOT the
/// order: the order keeps resting at its old terms, so a terminal `OrderRejected` would be a lie.
#[test]
fn a_rejection_naming_an_order_with_an_amend_in_flight_refuses_only_the_amend() {
    let mut m = mapper_with_an_amend_in_flight();
    let evs = m.on_error(201, 101, "Order rejected - Reason: price outside the limits");
    assert_matches!(
        evs.as_slice(),
        [Event::OrderModifyRejected(r)] if r.client_order_id == "coid-A"
            && r.reason.contains("price outside the limits"),
        "got {evs:?}"
    );
    // ...and once the amend is closed the same error is the order's own rejection again.
    let again = m.on_error(201, 101, "Order rejected");
    assert_matches!(again.as_slice(), [Event::OrderRejected(_)], "got {again:?}");
}

/// An order that ends while its amend is in flight takes the request with it (nothing is owed for a finished order).
#[test]
fn a_finished_order_drops_its_pending_amend() {
    let mut m = mapper_with_an_amend_in_flight();
    let _ = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        ..Default::default()
    });
    assert!(m.sweep_amends(0, 1).is_empty());
    assert!(m.sweep_amends(1_000_000, 1).is_empty(), "no request left to time out");
}

// ---- the fill tracking follows an amended quantity ---------------------------------------------------------------

/// An accepted order of `qty` (venue id 101) with a quantity amend to `new_qty` confirmed by the venue's echo.
fn amended_to(qty: f64, new_qty: f64) -> EventMapper {
    let mut m = mapper_submitted("coid-A", 101, qty);
    assert!(
        m.note_amend(
            "coid-A",
            crate::exec::amend::PendingAmend::new(
                101,
                (qty, Some(190.0)),
                (new_qty, Some(190.0)),
                false
            )
        )
        .is_none()
    );
    let mut row = venue_row(101, "coid-A", new_qty, "Submitted");
    row.limit_price = Some(190.0);
    assert_matches!(m.on_venue_order(&row).as_slice(), [Event::OrderModified(_)]);
    m
}

/// 10 -> 12: the fill of 10 is a PARTIAL (the order is not complete), the next 2 complete it.
#[test]
fn a_quantity_amend_up_makes_the_fill_of_the_old_total_a_partial() {
    let mut m = amended_to(10.0, 12.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    assert_eq!(m.ids().resolve(101, ""), Some("coid-A".to_string()), "still routable");
    let _ = exec(&mut m, 101, "coid-A", "e2", 2.0);
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]);
}

/// 10 -> 6: the fill that reaches the NEW total is final (it used to hang PartiallyFilled for ever).
#[test]
fn a_quantity_amend_down_lets_the_fill_that_reaches_the_new_total_complete_the_order() {
    let mut m = amended_to(10.0, 6.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    let _ = exec(&mut m, 101, "coid-A", "e2", 2.0);
    assert_matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]);
}

/// A quantity below what is already filled never lowers the total under the fills (the venue does not do it; if
/// it ever does the order is complete there): no panic, no negative remainder, and the total stays at `filled`.
#[test]
fn an_amend_below_the_filled_quantity_keeps_the_total_at_the_filled_quantity() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 8.0);
    assert_matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]);
    m.amend_acknowledged("coid-A", 101, 6.0, Some(190.0));
    assert_eq!(m.fill_state[&101].filled, 8.0);
    assert_eq!(m.fill_state[&101].total, 8.0);
    assert_eq!(m.fill_state[&101].qty, 6.0);
}

/// A backend that vouches for an amend synchronously moves the tracking the same way.
#[test]
fn an_acknowledged_amend_moves_the_total_too() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.amend_acknowledged("coid-A", 101, 12.0, Some(190.0));
    assert_eq!(m.fill_state[&101].total, 12.0);
    // Idempotent for the same quantity.
    m.amend_acknowledged("coid-A", 101, 12.0, Some(190.0));
    assert_eq!(m.fill_state[&101].total, 12.0);
}

/// What a learned reduction took off the total (`settle_order`) stays off after an amend: the total moves by the
/// CHANGE in quantity, not to the new quantity.
#[test]
fn an_amend_keeps_the_learned_reduction_of_the_total() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    m.fill_state.get_mut(&101).unwrap().total = 7.0; // 3 were filled before today (settled at the learning)
    m.amend_acknowledged("coid-A", 101, 12.0, Some(190.0));
    assert_eq!(m.fill_state[&101].total, 9.0);
}

// ---- an id-less rejection while an amend is open -----------------------------------------------------------------

/// With an amend open AND a fresh order unacked, an id-less rejection could be either's: the fresh order is NOT
/// terminally rejected (the old "sole unacked" heuristic would have), and the amend stays for its own bound.
#[test]
fn an_idless_rejection_with_an_amend_open_does_not_reject_the_fresh_unacked_order() {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let a = ids.next_order_id().unwrap();
    ids.bind(a, "coid-A");
    let b = ids.next_order_id().unwrap();
    ids.bind(b, "coid-B");
    let mut m = EventMapper::new(ids);
    m.on_submit(a, 10.0);
    let _ = m.on_order_status(IbOrderStatus {
        order_id: a,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        ..Default::default()
    });
    assert!(
        m.note_amend(
            "coid-A",
            crate::exec::amend::PendingAmend::new(a, (10.0, Some(1.0)), (10.0, Some(2.0)), false)
        )
        .is_none()
    );
    m.on_submit(b, 1.0); // the fresh order, unacked
    assert!(m.on_error(201, 0, "Order rejected - price").is_empty(), "no terminal for either");
    assert_eq!(m.sweep_amends(0, 10_000).len(), 0);
    // The fresh order is still the unacked one: with the amend gone its rejection routes to it again.
    let _ = m.take_amend("coid-A");
    assert_matches!(m.on_error(201, 0, "x").as_slice(), [Event::OrderRejected(r)] if r.client_order_id == "coid-B");
}

/// With an amend open and nothing else it could belong to, the id-less rejection is the amend's refusal.
#[test]
fn an_idless_rejection_with_only_an_amend_open_refuses_the_amend() {
    let mut m = mapper_with_an_amend_in_flight();
    let evs = m.on_error(201, 0, "Order rejected - price");
    assert_matches!(
        evs.as_slice(),
        [Event::OrderModifyRejected(r)] if r.client_order_id == "coid-A" && r.reason.contains("price"),
        "got {evs:?}"
    );
    assert!(m.sweep_amends(1_000_000, 1).is_empty(), "the amend is closed");
}

// ---- the fee adjustment (decision 0238): a commission for a fill that already went out ---------------------------

/// The `FeeAdjustment`s a mapper call produced, as `(trade id, extra commission, currency)`; any other event fails the test.
fn adjustments(evs: Vec<Event>) -> Vec<(String, f64, String)> {
    evs.into_iter()
        .map(|e| match e {
            Event::FeeAdjustment(a) => {
                (a.trade_id.to_string(), a.extra_commission, a.commission_asset.to_string())
            }
            other => panic!("expected only fee adjustments, got {other:?}"),
        })
        .collect()
}

/// A fill that joined its commission the ordinary way (the execution, then the report): 4 of 10, fee 1.00 USD.
fn mapper_with_a_joined_fill() -> EventMapper {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    let joined = real_commission(&mut m, "e1", 1.00);
    assert_matches!(joined.as_slice(), [Event::Fill(f), Event::OrderPartiallyFilled(_)] if f.commission == 1.00);
    m
}

/// **Commission AFTER the fill, revised once, by the difference to the fill's own fee.** The fill went out with 1.00; IB
/// corrects it to 1.25: the adjustment carries 0.25 MORE than the fill, in total, in the fee's currency, naming the fill's
/// own trade id, order, symbol, price and time, so the engine books it once.
#[test]
fn a_revised_commission_for_an_emitted_fill_is_a_fee_adjustment_by_the_difference() {
    let mut m = mapper_with_a_joined_fill();
    let out = real_commission(&mut m, "e1", 1.25);
    match out.as_slice() {
        [Event::FeeAdjustment(a)] => {
            assert!((a.extra_commission - 0.25).abs() < 1e-12, "{}", a.extra_commission);
            assert_eq!(a.commission_asset.as_str(), "USD");
            assert_eq!(
                (
                    a.trade_id.as_str(),
                    a.client_order_id.as_str(),
                    a.symbol.as_str(),
                    a.venue.as_str()
                ),
                ("e1", "coid-A", "AAPL.SMART.USD", "ibkr")
            );
            assert_eq!((a.last_px, a.ts), (190.0, 1));
        }
        other => panic!("expected one FeeAdjustment, got {other:?}"),
    }
}

/// **Adjusted twice**: each report states the TOTAL, relative to the fill's own fee, so the second adjustment carries
/// 0.40 (not 0.15 on top of 0.25) and the engine books the change; a third back down carries 0.10.
#[test]
fn a_commission_adjusted_twice_states_the_total_each_time() {
    let mut m = mapper_with_a_joined_fill();
    let first = adjustments(real_commission(&mut m, "e1", 1.25));
    let second = adjustments(real_commission(&mut m, "e1", 1.40));
    let third = adjustments(real_commission(&mut m, "e1", 1.10));
    assert!((first[0].1 - 0.25).abs() < 1e-12);
    assert!((second[0].1 - 0.40).abs() < 1e-12, "the cumulative extra, not the step: {second:?}");
    assert!((third[0].1 - 0.10).abs() < 1e-12, "{third:?}");
    assert_eq!((first.len(), second.len(), third.len()), (1, 1, 1));
}

/// **A duplicate emits nothing**: IB re-sending the figure it sent, a `reqExecutions` replay of the pair, the same
/// revision twice. The fill's own fee (1.00) is also "already told".
#[test]
fn a_repeated_commission_emits_nothing() {
    let mut m = mapper_with_a_joined_fill();
    assert!(real_commission(&mut m, "e1", 1.00).is_empty(), "the fee the fill carried");
    assert_eq!(adjustments(real_commission(&mut m, "e1", 1.25)).len(), 1);
    assert!(real_commission(&mut m, "e1", 1.25).is_empty(), "the revision again");
    // A replayed execDetails is inert (the `emitted` index), so the replayed pair changes nothing either.
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(real_commission(&mut m, "e1", 1.25).is_empty());
    assert!(m.commissions.is_empty(), "nothing parked");
}

/// **Commission BEFORE the fill** (IB delivers the halves in either order): the parked commission joins the execution,
/// the fill carries it, and a LATER revision is a fee adjustment against that figure.
#[test]
fn a_commission_that_came_before_its_fill_is_the_fills_fee_and_a_later_revision_adjusts_it() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    assert!(
        real_commission(&mut m, "e1", 1.00).is_empty(),
        "parked: its execution has not arrived"
    );
    let joined = exec_raw(&mut m, 101, "coid-A", "e1", 4.0);
    assert_matches!(joined.as_slice(), [Event::Fill(f), Event::OrderPartiallyFilled(_)] if f.commission == 1.00);
    let out = adjustments(real_commission(&mut m, "e1", 1.50));
    assert_eq!(out.len(), 1);
    assert!((out[0].1 - 0.50).abs() < 1e-12, "{out:?}");
}

/// **A fill booked with its fee UNKNOWN is corrected by the real commission**: the cancel-held release books 0.0 with no
/// currency, and the report that comes later goes out as an adjustment carrying all of it, once, and then again as a
/// revision when IB changes it.
#[test]
fn a_fill_booked_with_an_unknown_fee_is_corrected_by_the_late_commission_and_its_revision() {
    let mut m = mapper_submitted("coid-A", 101, 10.0);
    let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
    assert!(m.on_order_status(cancelled(101, "coid-A")).is_empty());
    let w = CANCEL_COMMISSION_WAIT_MS;
    assert!(m.release_held_cancels(0, w).is_empty());
    let released = m.release_held_cancels(w, w);
    assert_matches!(released.as_slice(), [Event::Fill(f), _, Event::OrderCanceled(_)] if f.commission == 0.0);

    let first = adjustments(real_commission(&mut m, "e1", 1.00));
    assert_eq!(first, vec![("e1".to_string(), 1.00, "USD".to_string())]);
    assert!(real_commission(&mut m, "e1", 1.00).is_empty(), "the replay of the same report");
    let second = adjustments(real_commission(&mut m, "e1", 1.04));
    assert_eq!(second, vec![("e1".to_string(), 1.04, "USD".to_string())]);
}

/// What a correction is NOT: a currency other than the one the fee was stated in, a non-finite figure, and an exactly-0.0
/// report over a fee that is already non-zero (an unset protobuf field cannot be told from a real zero and must never undo
/// a fee). Each emits nothing, and the next honest report still lands.
#[test]
fn a_report_that_cannot_be_a_correction_emits_nothing() {
    let mut m = mapper_with_a_joined_fill();
    let other_ccy = m.on_commission_report(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "EUR".into(),
    });
    assert!(other_ccy.is_empty(), "another currency than the fee was stated in");
    assert!(real_commission(&mut m, "e1", f64::NAN).is_empty());
    assert!(real_commission(&mut m, "e1", f64::INFINITY).is_empty());
    assert!(real_commission(&mut m, "e1", 0.0).is_empty(), "an unset field is not a refund");
    // none of that moved what the engine was told: the honest revision is still 0.25 over the fill's fee.
    let out = adjustments(real_commission(&mut m, "e1", 1.25));
    assert!((out[0].1 - 0.25).abs() < 1e-12, "{out:?}");
    // ...and a rebate (negative) is a legitimate figure.
    let rebate = adjustments(real_commission(&mut m, "e1", -0.10));
    assert!((rebate[0].1 + 1.10).abs() < 1e-12, "{rebate:?}");
}

/// An order nobody taught emits no fill, so there is no fee to correct: its late commission is the no-op it always was.
#[test]
fn a_commission_for_an_untaught_fill_is_not_a_fee_adjustment() {
    let mut m = mapper_with_a_partly_filled_open_order("coid-open");
    assert!(overnight_exec_of(&mut m, 55, "coid-open", "e1", 4.0).is_empty());
    assert!(commission_raw(&mut m, "e1").is_empty(), "joined, nothing emitted");
    assert!(m.fee_book.is_empty(), "no fill went out, so no fee was told");
    assert!(real_commission(&mut m, "e1", 2.0).is_empty());
}

/// The fee book is bounded like `emitted`: past the cap the OLDEST execution's late report is a no-op again, the newest
/// stay correctable.
#[test]
fn the_fee_book_is_bounded_oldest_first() {
    let mut m = mapper_submitted("coid-A", 101, (MAX_EMITTED_EXEC_IDS + 10) as f64);
    for i in 0..=MAX_EMITTED_EXEC_IDS {
        let id = format!("x{i}");
        let _ = exec_raw(&mut m, 101, "coid-A", &id, 1.0);
        let _ = real_commission(&mut m, &id, 1.0);
    }
    assert_eq!(m.fee_book.len(), MAX_EMITTED_EXEC_IDS);
    assert!(real_commission(&mut m, "x0", 2.0).is_empty(), "the oldest was forgotten");
    let newest = format!("x{MAX_EMITTED_EXEC_IDS}");
    assert_eq!(adjustments(real_commission(&mut m, &newest, 2.0)).len(), 1);
}

/// `exec`, with the bare fills kept (the helper above drops them).
fn exec_raw(
    m: &mut EventMapper,
    order_id: i32,
    coid: &str,
    exec_id: &str,
    shares: f64,
) -> Vec<Event> {
    m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: coid.into(),
        exec_id: exec_id.into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares,
        price: 190.0,
        ts: 1,
        ..Default::default()
    })
}
