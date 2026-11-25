//! A NEW sidecar takes up what the old one left (A34): the Rust half, over real child processes of the scripted fake
//! (`src/bin/fake_jforex_bridge.rs`, mode `adopt-die-once`: a sidecar whose `ready` announces `adopt`, whose first launch
//! dies on a `cancel` of `DIE`, and whose later launches send the `position` lines and the `adopted` census the test
//! wrote into `<log>.positions` / `<log>.adopted`).
//!
//! What these prove: a native protection (stop-loss, take-profit) that rested when an ADOPTING sidecar died is not ended
//! on the node, it is sent again to the next sidecar, which takes the price over (the answer is not a second
//! acceptance), and a protection the venue no longer holds ends `OrderCanceled` in the venue's words; the census
//! lifts the `stranded` refusal for an order it adopted and names a cancel of one it does not know; and a symbol the
//! node holds a position in that the census lists no order on is flat at the venue. They do not prove the JForex half
//! (what `engine.getOrders()` lists at a real `onStart`): that is `StrategyBridge.adoptOpenOrders` (`StopLossTest.java`
//! over SDK stand-ins) and the live proof in `crates/bridges/dukascopy/CLAUDE.md`. A jar that does NOT adopt keeps
//! the old ending at a death (`dukascopy_restart.rs`'s `a_resting_stop_loss_at_a_sidecars_death_ends_canceled_in_words...`).

use std::time::Duration;

use crate::support::{BRIDGE, config, drain, engine_holding, launches, resting_stop, wait_until};
use vike_bridge_core::venue_link::VenueLinkState;
use vike_dukascopy::ExecutionClientDukascopy;
use vike_dukascopy::test_support::order;
use vike_execution::recon::ReconcileClient;
use vike_execution::testing::{RecordingClient, recv_event};
use vike_execution::{
    Engine, EventBusFold, EventBusOutbox, EventHandler, ExecutionClient, Ingest, event_channel,
};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus};
use vike_venues::venue_timings::VenueTimingsReconnect;

/// A restart policy in milliseconds: 40 ms, doubling to 160 ms.
const FAST: VenueTimingsReconnect =
    VenueTimingsReconnect::new(Duration::from_millis(40), Duration::from_millis(160), "a test");

fn resting_limit(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Limit,
        price: Some(1.1),
        ..order(order_client_id)
    }
}

/// A native take-profit on a long EURUSD position: a reduce-only SELL limit.
fn resting_take_profit(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Limit,
        side: -1,
        price: Some(1.2),
        reduce_only: true,
        ..order(order_client_id)
    }
}

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch dir")
}

fn adopting_client(
    log: &std::path::Path,
) -> (ExecutionClientDukascopy, tokio::sync::mpsc::Receiver<Ingest>) {
    let (events, rx) = event_channel(64);
    let client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["adopt-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    (client, rx)
}

fn folds(engine: &mut Engine<RecordingClient>, event: &Event) -> bool {
    matches!(engine.on_event(event, &mut EventBusOutbox::default()), EventBusFold::Applied)
}

fn status(engine: &Engine<RecordingClient>, order_client_id: &str) -> OrderStatus {
    engine.registry[order_client_id].status
}

/// The ladder a healthy sidecar gives a market order: Submitted, Accepted, the bare Fill, the OrderFilled wrap.
fn expect_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, order_client_id: &str) {
    assert!(
        matches!(recv_event(rx), Event::OrderSubmitted(e) if e.order_client_id == order_client_id)
    );
    assert!(
        matches!(recv_event(rx), Event::OrderAccepted(e) if e.order_client_id == order_client_id)
    );
    assert!(matches!(recv_event(rx), Event::Fill(f) if f.order_client_id == order_client_id));
    assert!(
        matches!(recv_event(rx), Event::OrderFilled(e) if e.order_client_id == order_client_id)
    );
}

fn sidecar_up(client: &ExecutionClientDukascopy) -> bool {
    client.sidecar_state() == VenueLinkState::Up
}

/// **A protection that rested when an ADOPTING sidecar died stays accepted on the node, is sent again to the next
/// sidecar, and is taken over.** The wave-1 ending (`OrderCanceled "bridge died ... the venue may STILL hold the
/// stop-loss"`) is for a jar that cannot take it over; this jar can, so the node keeps believing in the protection the
/// venue provably holds. Driven through the real order FSM: the assertion is what the node ends up BELIEVING, and that the
/// answer to the stop sent again was not a second acceptance (the FSM would have logged an invalid transition) and not
/// an ending (a dropped terminal). A cancel of it works afterwards, as it could not on a sidecar that knows nothing.
///
/// ⚠ Mutation proof: make the EOF drain end an acknowledged protection regardless of `adopts`
/// (`keeps_protections`) and the first assertion after the death goes red on the `OrderCanceled` it reads.
#[test]
fn a_protection_resting_at_an_adopting_sidecars_death_is_sent_again_and_taken_over() {
    for (what, protection) in
        [("stop-loss", resting_stop("p1")), ("take-profit", resting_take_profit("p1"))]
    {
        let dir = scratch("vike-duka-adopt-protection");
        let log = dir.path().join("launches.txt");
        let (mut client, mut rx) = adopting_client(&log);
        let mut engine = engine_holding(&protection);

        client.submit(&protection);
        for _ in 0..2 {
            let event = recv_event(&mut rx);
            assert!(folds(&mut engine, &event), "{what}: {event:?}");
        }
        assert_eq!(status(&engine, "p1"), OrderStatus::Accepted, "{what}");

        // The sidecar dies with the protection resting; the venue keeps it (the fake's book file).
        client.cancel("DIE");
        wait_until("the second sidecar to be up", || {
            launches(&log).len() == 2 && sidecar_up(&client)
        });

        // Its cancel is the first thing the new sidecar answers after the protection sent again: if the take-over's
        // acceptance had been passed on, or the death had ended the protection, the first event would be one of those.
        client.cancel("p1");
        let ended = recv_event(&mut rx);
        match &ended {
            Event::OrderCanceled(c) => {
                assert_eq!(c.order_client_id, "p1", "{what}");
                // The sidecar's own cancel answer carries no words (`fake_jforex_bridge`'s `Cancel`
                // arm); the death's carries the `bridge died` sentence.
                assert!(
                    c.reason.is_empty(),
                    "{what}: this is the operator's cancel, not the death's: {}",
                    c.reason
                );
            }
            other => {
                panic!("{what}: the first event after the restart is the cancel's, got {other:?}")
            }
        }
        assert!(
            folds(&mut engine, &ended),
            "{what}: the FSM takes the cancel of an Accepted order"
        );
        assert_eq!(status(&engine, "p1"), OrderStatus::Canceled, "{what}");
        assert_eq!(
            engine.closing_events_dropped_order_still_live, 0,
            "{what}: nothing was dropped on a live order"
        );

        // The venue's book: taken over (not added twice) and then taken off by the cancel; the cancels the launches saw.
        client.detach();
        let cancels =
            std::fs::read_to_string(format!("{}.cancels", log.display())).expect("cancel log");
        assert_eq!(cancels.lines().collect::<Vec<_>>(), ["DIE", "p1"], "{what}");
        assert!(drain(&mut rx).is_empty(), "{what}: nothing else was said about p1");
    }
}

/// **A protection the venue no longer holds ends `OrderCanceled` in the venue's words at the restart.** The stop-loss
/// fired (or was taken off in the platform) while no sidecar listened: the new sidecar refuses the stop sent again with
/// `no open long order to protect`, which the node reads as the end of an order it had accepted (a cancel the core takes;
/// a rejection it would DROP), and says what happened.
#[test]
fn a_protection_the_venue_lost_while_no_sidecar_listened_ends_canceled_in_the_venues_words() {
    let dir = scratch("vike-duka-adopt-lost");
    let log = dir.path().join("launches.txt");
    let (mut client, mut rx) = adopting_client(&log);
    let stop = resting_stop("p1");
    let mut engine = engine_holding(&stop);

    client.submit(&stop);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "p1"), OrderStatus::Accepted);

    // The venue's stop-loss fires while the sidecar is gone: its book no longer lists the stop.
    std::fs::write(format!("{}.book", log.display()), "").expect("book file");
    client.cancel("DIE");

    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderCanceled(c) => {
            assert_eq!(c.order_client_id, "p1");
            assert!(c.reason.contains("no open long order to protect"), "{}", c.reason);
            assert!(
                c.reason.contains("the venue ended this order after accepting it"),
                "{}",
                c.reason
            );
        }
        other => {
            panic!("a protection the venue lost ends with a cancel the core takes, got {other:?}")
        }
    }
    assert!(folds(&mut engine, &ended), "the order FSM must take the end of an Accepted stop");
    assert_eq!(status(&engine, "p1"), OrderStatus::Canceled);
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
    client.detach();
}

/// The lines of the fake venue's file `<log>.<what>` (none when it was never written).
fn venue_file(log: &std::path::Path, what: &str) -> Vec<String> {
    std::fs::read_to_string(format!("{}.{what}", log.display()))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A protection of each kind named `DIE-p1` (the fake's first launch dies on its cancel, unanswered), and the census line
/// of the long entry it protects: `carries` = whether that entry still carries the protection's price at the venue.
fn protections_dying_on_their_cancel(carries: bool) -> [(&'static str, OrderRequest, String); 2] {
    let (stop_loss, take_profit) = if carries { ("1.09", "1.2") } else { ("0", "0") };
    [
        ("stop-loss", resting_stop("DIE-p1"), format!("e1 EURUSD 1 FILLED 1000 {stop_loss} 0\n")),
        (
            "take-profit",
            resting_take_profit("DIE-p1"),
            format!("e1 EURUSD 1 FILLED 1000 0 {take_profit}\n"),
        ),
    ]
}

/// Start an adopting client, rest `protection` on the node's FSM, write the venue the next sidecar will read (`census`
/// line; `book_holds` = whether the venue still holds the protection), then cancel it: the first sidecar dies with that
/// cancel in flight.
fn die_during_the_cancel_of(
    log: &std::path::Path,
    protection: &OrderRequest,
    census: &str,
    book_holds: bool,
) -> (ExecutionClientDukascopy, tokio::sync::mpsc::Receiver<Ingest>, Engine<RecordingClient>) {
    let (mut client, mut rx) = adopting_client(log);
    let mut engine = engine_holding(protection);
    client.submit(protection);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "DIE-p1"), OrderStatus::Accepted);
    std::fs::write(format!("{}.adopted", log.display()), census).expect("adopted file");
    if !book_holds {
        std::fs::write(format!("{}.book", log.display()), "").expect("book file");
    }
    client.cancel("DIE-p1");
    (client, rx, engine)
}

/// **A protection whose cancel LANDED at the venue just before the sidecar died ends `OrderCanceled` and is never set
/// again.** The entry it protected is still open at the venue without the price, so the protection sent blind to the next
/// sidecar would be SET again (the fake models `StopPlan`'s re-arm in `<log>.armed`); the next sidecar reads the venue
/// first, finds no open order carrying the price, and the node ends it with nothing sent.
///
/// ⚠ Mutation proof: send every kept protection again at the start regardless of `Shared::cancelling` (the code before)
/// and the fake re-arms it (`<log>.armed` names it) and no ending ever comes (`recv_event` times out).
#[test]
fn a_protection_whose_cancel_landed_before_the_death_ends_canceled_and_is_never_set_again() {
    for (what, protection, census) in protections_dying_on_their_cancel(false) {
        let dir = scratch("vike-duka-adopt-cancel-landed");
        let log = dir.path().join("launches.txt");
        let (mut client, mut rx, mut engine) =
            die_during_the_cancel_of(&log, &protection, &census, false);

        let ended = recv_event(&mut rx);
        match &ended {
            Event::OrderCanceled(c) => {
                assert_eq!(c.order_client_id, "DIE-p1", "{what}");
                assert!(
                    !c.reason.is_empty(),
                    "{what}: the node's own words, not a venue's cancel answer"
                );
            }
            other => panic!("{what}: the protection ends canceled, got {other:?}"),
        }
        assert!(
            folds(&mut engine, &ended),
            "{what}: the FSM takes the cancel of an Accepted order"
        );
        assert_eq!(status(&engine, "DIE-p1"), OrderStatus::Canceled, "{what}");
        assert_eq!(launches(&log).len(), 2, "{what}: the second sidecar read the venue");
        client.detach();
        assert!(
            venue_file(&log, "armed").is_empty(),
            "{what}: the venue was never asked to set it again"
        );
        assert_eq!(
            venue_file(&log, "cancels"),
            ["DIE-p1"],
            "{what}: no command for it reached the second sidecar"
        );
        assert!(drain(&mut rx).is_empty(), "{what}: nothing else was said about it");
    }
}

/// **A protection the venue STILL holds after a death during its cancel is cancelled again, never re-armed.** The cancel
/// died with the sidecar before it reached the venue: the next sidecar reads the venue first, finds the entry still
/// carrying the price, takes it over (nothing changes at the venue, the acceptance is swallowed) and is sent the cancel
/// again, whose answer ends it. The venue saw the price set again never, and the cancel twice.
///
/// ⚠ Mutation proof: send the kept protection again with no cancel after it (the code before) and no ending ever comes.
#[test]
fn a_protection_the_venue_still_holds_after_a_death_during_its_cancel_is_cancelled_again_not_rearmed()
 {
    for (what, protection, census) in protections_dying_on_their_cancel(true) {
        let dir = scratch("vike-duka-adopt-cancel-held");
        let log = dir.path().join("launches.txt");
        let (mut client, mut rx, mut engine) =
            die_during_the_cancel_of(&log, &protection, &census, true);

        let ended = recv_event(&mut rx);
        match &ended {
            Event::OrderCanceled(c) => {
                assert_eq!(c.order_client_id, "DIE-p1", "{what}");
                assert!(
                    c.reason.is_empty(),
                    "{what}: the second sidecar's answer to the cancel sent again: {}",
                    c.reason
                );
            }
            other => panic!("{what}: the cancel sent again ends it, got {other:?}"),
        }
        assert!(
            folds(&mut engine, &ended),
            "{what}: the FSM takes the cancel of an Accepted order"
        );
        assert_eq!(status(&engine, "DIE-p1"), OrderStatus::Canceled, "{what}");
        assert_eq!(engine.closing_events_dropped_order_still_live, 0, "{what}");
        client.detach();
        assert!(
            venue_file(&log, "armed").is_empty(),
            "{what}: the venue was never asked to set it again"
        );
        assert_eq!(
            venue_file(&log, "cancels"),
            ["DIE-p1", "DIE-p1"],
            "{what}: the cancel, then the cancel sent again"
        );
        assert!(
            !venue_file(&log, "book").contains(&"DIE-p1".to_string()),
            "{what}: the venue no longer holds it"
        );
        assert!(drain(&mut rx).is_empty(), "{what}: the take-over's acceptance was not passed on");
    }
}

/// **The census lifts the `stranded` refusal for what the new sidecar adopted, and a cancel of an id it does not know is
/// refused by name.** A resting limit in flight at the death is rejected `bridge died` (dropped by the core on an
/// accepted order) and remembered as stranded, so a cancel of it was refused by name; the adopting sidecar lists it
/// (`<log>.adopted`), and the cancel then reaches it and is served. An id the census does not list (and nobody
/// stranded) reaches the sidecar and gets the sidecar's own named refusal instead of silence.
#[test]
fn the_census_lifts_the_stranded_refusal_for_an_adopted_order_and_a_stranger_is_refused_by_name() {
    let dir = scratch("vike-duka-adopt-census");
    let log = dir.path().join("launches.txt");
    std::fs::write(format!("{}.adopted", log.display()), "c1 EURUSD 1 OPENED 0 0 0\n")
        .expect("adopted file");
    let (mut client, mut rx) = adopting_client(&log);

    client.submit(&resting_limit("c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderSubmitted(e) if e.order_client_id == "c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderAccepted(e) if e.order_client_id == "c1"));
    client.cancel("DIE");
    assert!(
        matches!(recv_event(&mut rx), Event::OrderRejected(e) if e.order_client_id == "c1" && e.reason.starts_with("bridge died")),
        "an order that is not a protection keeps the rejection at a death"
    );
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));

    // The census arrives right after `ready`: until the reader has handled it the cancel is still refused as stranded.
    let mut served = false;
    for _ in 0..2000 {
        client.cancel("c1");
        match recv_event(&mut rx) {
            Event::OrderCancelRejected(r) => {
                assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
                std::thread::sleep(Duration::from_millis(5));
            }
            Event::OrderCanceled(c) => {
                assert_eq!(c.order_client_id, "c1");
                served = true;
                break;
            }
            other => panic!("expected the refusal or the cancel, got {other:?}"),
        }
    }
    assert!(served, "the census never lifted the stranded refusal of the adopted order");

    client.cancel("ghost");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.order_client_id, "ghost");
            assert!(r.reason.contains("knows no order"), "{}", r.reason);
        }
        other => panic!("a cancel of an id nobody holds is refused by name, got {other:?}"),
    }
    client.detach();
}

/// **An order the dying sidecar never answered is not rejected: the next sidecar reads the venue for it** (decision
/// 0303: its submit may have reached the venue). One the venue holds (the read lists it resting under its order client
/// id) is accepted with the venue's order id, and its cancel reaches the new sidecar, which took it up; one the venue
/// does not hold gets no word and stays `Submitted` on the node, for the stuck-order ladder. Neither is sent again.
///
/// ⚠ Mutation proof: the drain's rejection of an order the sidecar never answered (the code before) ends both
/// `Rejected` "bridge died", and the held one then never reads `Accepted`.
#[test]
fn an_order_the_dying_sidecar_never_answered_is_accepted_when_the_venue_holds_it_and_left_submitted_when_not()
 {
    let dir = scratch("vike-duka-adopt-unanswered");
    let log = dir.path().join("launches.txt");
    std::fs::write(format!("{}.adopted", log.display()), "unanswered-held EURUSD 1 OPENED 0 0 0\n")
        .expect("adopted file");
    let (mut client, mut rx) = adopting_client(&log);
    let (held, gone) = (resting_limit("unanswered-held"), resting_limit("unanswered-gone"));
    let (mut held_engine, mut gone_engine) = (engine_holding(&held), engine_holding(&gone));
    for (order, engine) in [(&held, &mut held_engine), (&gone, &mut gone_engine)] {
        client.submit(order);
        let submitted = recv_event(&mut rx);
        assert!(
            matches!(&submitted, Event::OrderSubmitted(e) if e.order_client_id == order.order_client_id),
            "{submitted:?}"
        );
        assert!(folds(engine, &submitted));
    }
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));

    let accepted = recv_event(&mut rx);
    match &accepted {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, "unanswered-held");
            assert_eq!(a.venue_order_id.as_deref(), Some("FAKE-ORD"), "the venue's own id");
        }
        other => panic!("the order the venue holds is accepted, got {other:?}"),
    }
    assert!(folds(&mut held_engine, &accepted));
    assert_eq!(status(&held_engine, "unanswered-held"), OrderStatus::Accepted);
    client.cancel("unanswered-held");
    assert!(
        matches!(recv_event(&mut rx), Event::OrderCanceled(c) if c.order_client_id == "unanswered-held"),
        "the new sidecar took it up and serves its cancel"
    );
    client.detach();

    let rest = drain(&mut rx);
    assert!(
        rest.iter().all(|e| e.order_client_id() != Some("unanswered-gone")),
        "no word for the order the venue does not hold: {rest:?}"
    );
    assert_eq!(
        status(&gone_engine, "unanswered-gone"),
        OrderStatus::Submitted,
        "left for the ladder"
    );
    let cancels = venue_file(&log, "cancels");
    assert_eq!(cancels, ["DIE", "unanswered-held"], "nothing was sent again for either order");
}

/// **A restart whose login is refused keeps an order the dying sidecar never answered for the next sidecar's read**
/// (decision 0303): the refused start's reader ends too, before any `ready`, and its drain must not reject what the
/// death held. The sidecar after it reads the venue and accepts the order it holds.
///
/// ⚠ Mutation proof: the drain keeping held orders only at a death (`keeps_protections` of `died`, the code before)
/// rejects the order "bridge died" at the refused start, and the read after it finds nothing to settle.
#[test]
fn an_order_the_dying_sidecar_never_answered_outlives_a_refused_restart_and_is_read_by_the_next() {
    let dir = scratch("vike-duka-adopt-unanswered-refused");
    let log = dir.path().join("launches.txt");
    std::fs::write(format!("{}.adopted", log.display()), "unanswered-held EURUSD 1 OPENED 0 0 0\n")
        .expect("adopted file");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["adopt-die-once".into(), log.display().to_string(), "1".into()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let held = resting_limit("unanswered-held");
    let mut engine = engine_holding(&held);
    client.submit(&held);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    client.cancel("DIE");
    wait_until("the sidecar after the refused login to be up", || {
        launches(&log).len() == 3 && sidecar_up(&client)
    });

    let accepted = recv_event(&mut rx);
    assert!(
        matches!(&accepted, Event::OrderAccepted(a) if a.order_client_id == "unanswered-held"),
        "the order the venue holds is accepted, got {accepted:?}"
    );
    assert!(folds(&mut engine, &accepted));
    assert_eq!(status(&engine, "unanswered-held"), OrderStatus::Accepted);
    client.detach();
}

/// **A protection the dying sidecar never answered, whose price the venue holds, is taken over and accepted, never set
/// again blind** (decision 0303). The venue read finds the entry it protects carrying its price, so the new sidecar is sent
/// it to take that price over (nothing changes at the venue), and its acceptance reaches the node, which held it
/// `Submitted`.
///
/// ⚠ Mutation proof: the drain's rejection (the code before) ends it `Rejected` "bridge died" while the venue holds it.
#[test]
fn a_protection_the_dying_sidecar_never_answered_is_taken_over_and_accepted_when_the_venue_holds_its_price()
 {
    let dir = scratch("vike-duka-adopt-unanswered-stop");
    let log = dir.path().join("launches.txt");
    std::fs::write(format!("{}.adopted", log.display()), "e1 EURUSD 1 FILLED 1000 1.09 0\n")
        .expect("adopted file");
    // The venue took the stop (its book holds it) before the sidecar said a word about it.
    std::fs::write(format!("{}.book", log.display()), "unanswered\n").expect("book file");
    let (mut client, mut rx) = adopting_client(&log);
    let stop = resting_stop("unanswered");
    let mut engine = engine_holding(&stop);
    client.submit(&stop);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    assert_eq!(status(&engine, "unanswered"), OrderStatus::Submitted);
    client.cancel("DIE");

    let accepted = recv_event(&mut rx);
    match &accepted {
        Event::OrderAccepted(a) => assert_eq!(a.order_client_id, "unanswered"),
        other => panic!("the protection the venue holds is accepted, got {other:?}"),
    }
    assert!(folds(&mut engine, &accepted));
    assert_eq!(status(&engine, "unanswered"), OrderStatus::Accepted);
    assert_eq!(launches(&log).len(), 2, "the second sidecar read the venue");
    // The new sidecar owns it now: its cancel is served.
    client.cancel("unanswered");
    let ended = recv_event(&mut rx);
    assert!(
        matches!(&ended, Event::OrderCanceled(c) if c.order_client_id == "unanswered"),
        "{ended:?}"
    );
    assert!(folds(&mut engine, &ended));
    client.detach();
    assert!(venue_file(&log, "armed").is_empty(), "the venue was never asked to set it again");
    assert!(drain(&mut rx).is_empty(), "nothing else was said about it");
}

/// The bare fill and its `OrderFilled` wrap of `order_client_id`, the next two events on the lane, each folded into `engine`;
/// the fill is returned.
fn expect_booked(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    engine: &mut Engine<RecordingClient>,
    order_client_id: &str,
) -> vike_model::events::FillEvent {
    let fill = recv_event(rx);
    let Event::Fill(booked) = &fill else { panic!("the venue's fill is booked, got {fill:?}") };
    assert_eq!(booked.order_client_id, order_client_id);
    assert!(folds(engine, &fill), "{fill:?}");
    let wrap = recv_event(rx);
    assert!(
        matches!(&wrap, Event::OrderFilled(w) if w.order_client_id == order_client_id),
        "the order ends filled, got {wrap:?}"
    );
    assert!(folds(engine, &wrap), "{wrap:?}");
    booked.clone()
}

/// What the venue read booked: the venue's units, price and commission (a charge, the venue's negative figure turned
/// positive), under the trade id the reconcile leg reports the same fill by, so the leg finds it folded and books it no
/// second time; the order filled once on the node and the position is the venue's.
fn assert_booked_once(
    client: &mut ExecutionClientDukascopy,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    engine: &Engine<RecordingClient>,
    booked: &vike_model::events::FillEvent,
) {
    assert_eq!(booked.trade_id.as_str(), "FAKE-ORD:1");
    assert_eq!((booked.last_qty, booked.last_price, booked.commission), (1000.0, 1.1, 0.035));
    let leg = client.recon_client().fetch_fill_reports(0).expect("the venue's fills");
    let leg_ids: Vec<&str> = leg.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(
        leg_ids,
        [booked.trade_id.as_str()],
        "the leg reports the fill under the id the node folded"
    );
    assert_eq!(status(engine, &booked.order_client_id), OrderStatus::Filled);
    assert_eq!(engine.registry[&booked.order_client_id].filled_qty, 1000.0);
    assert_eq!(engine.position_size_of("EURUSD", vike_model::events::PositionSide::Both), 1000.0);
    client.detach();
    let rest = drain(rx);
    assert!(
        rest.iter().all(|e| !matches!(e, Event::Fill(_) | Event::OrderFilled(_))),
        "the fill is booked once: {rest:?}"
    );
}

/// **A market entry the venue had accepted when an ADOPTING sidecar died is not ended in words: the next sidecar reads
/// the venue by its label and the order ends by what the venue says** (DK-2). The venue FILLED it (the read lists a
/// FILLED order under its label): its fill is booked once, with the venue's price and commission, and the order ends
/// `Filled`; the venue lists nothing under its label: it ends `Canceled`, nothing booked. A jar that does not adopt keeps
/// the cancel in words at the death (`dukascopy_restart.rs`'s
/// `an_accepted_market_entry_at_a_sidecars_death_ends_canceled_in_words_and_books_nothing`).
///
/// ⚠ Mutation proof: the drain's cancel at the death (the code before, `ENTRY_BRIDGE_DIED` for every acknowledged
/// entry) ends both `Canceled` "bridge died" at once, and the filled one never reads a fill.
#[test]
fn a_market_entry_accepted_before_an_adopting_sidecars_death_ends_by_what_the_venue_read_lists() {
    for (what, census) in [("filled", "DIE-e1 EURUSD 1 FILLED 1000 0 0 1.1 -0.035\n"), ("gone", "")]
    {
        let dir = scratch("vike-duka-adopt-entry");
        let log = dir.path().join("launches.txt");
        std::fs::write(format!("{}.adopted", log.display()), census).expect("adopted file");
        // What the real sidecar states at its start for an instrument the venue lists an order on.
        if what == "filled" {
            std::fs::write(format!("{}.positions", log.display()), "EURUSD 1000.0 1.1\n")
                .expect("positions file");
        }
        let (mut client, mut rx) = adopting_client(&log);
        let entry = order("DIE-e1");
        let mut engine = engine_holding(&entry);
        // The fake's first launch accepts it and dies before its fill.
        client.submit(&entry);
        for _ in 0..2 {
            let event = recv_event(&mut rx);
            assert!(folds(&mut engine, &event), "{what}: {event:?}");
        }
        assert_eq!(status(&engine, "DIE-e1"), OrderStatus::Accepted, "{what}");
        wait_until("the second sidecar to be up", || {
            launches(&log).len() == 2 && sidecar_up(&client)
        });

        if what == "filled" {
            let booked = expect_booked(&mut rx, &mut engine, "DIE-e1");
            assert_booked_once(&mut client, &mut rx, &engine, &booked);
            continue;
        }
        let ended = recv_event(&mut rx);
        match &ended {
            Event::OrderCanceled(c) => {
                assert_eq!(c.order_client_id, "DIE-e1");
                assert!(
                    c.reason.contains("lists no order under this order's label"),
                    "{}",
                    c.reason
                );
            }
            other => panic!("an entry the venue does not list ends canceled, got {other:?}"),
        }
        assert!(folds(&mut engine, &ended));
        assert_eq!(status(&engine, "DIE-e1"), OrderStatus::Canceled);
        assert_eq!(
            engine.position_size_of("EURUSD", vike_model::events::PositionSide::Both),
            0.0,
            "nothing was booked"
        );
        client.detach();
        assert!(drain(&mut rx).iter().all(|e| !matches!(e, Event::Fill(_))), "nothing was booked");
    }
}

/// **A market entry the dying sidecar never answered, which the venue FILLED, is accepted and its fill booked** (D3b read
/// it as "not held", the read lists no FILLED order as resting, and left it `Submitted` for the ladder, a position the
/// node would never hold). The read lists a FILLED order under its label: the order is accepted with the venue's order
/// id, its fill booked once, and it ends `Filled`.
///
/// ⚠ Mutation proof: drop the `fill_reports` arm of `settle_unanswered` and the order gets no word at all (the read's
/// "not held"), so the first `recv_event` times out.
#[test]
fn an_unanswered_market_entry_the_venue_filled_is_accepted_and_its_fill_booked_once() {
    let dir = scratch("vike-duka-adopt-unanswered-filled");
    let log = dir.path().join("launches.txt");
    std::fs::write(
        format!("{}.adopted", log.display()),
        "unanswered-e1 EURUSD 1 FILLED 1000 0 0 1.1 -0.035\n",
    )
    .expect("adopted file");
    let (mut client, mut rx) = adopting_client(&log);
    let entry = order("unanswered-e1");
    let mut engine = engine_holding(&entry);
    client.submit(&entry);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));

    let accepted = recv_event(&mut rx);
    match &accepted {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, "unanswered-e1");
            assert_eq!(a.venue_order_id.as_deref(), Some("FAKE-ORD"), "the venue's own id");
        }
        other => panic!("the filled order is accepted first, got {other:?}"),
    }
    assert!(folds(&mut engine, &accepted));
    let booked = expect_booked(&mut rx, &mut engine, "unanswered-e1");
    assert_booked_once(&mut client, &mut rx, &engine, &booked);
}

/// **A symbol the node holds a position in and the venue lists no order on is FLAT at the venue.** The reconcile client
/// (`recon_client`) PULLS what the venue lists (the fake's census files stand for `engine.getOrders()`), and states a flat
/// row for a symbol it heard of from a fill, so the flat symbol reads 0 where the push-line snapshot this replaced read
/// nothing (and, for a close the sidecar never heard, the node's own last belief); a symbol the venue lists keeps the
/// venue's figure, and a line for a symbol no fill of this client explains is adopted as a baseline and BOOKS NOTHING
/// (no synthesized fill reaches the node).
#[test]
fn a_symbol_the_node_holds_and_the_venue_lists_no_order_on_is_flat_at_the_venue() {
    fn venue_size(client: &ExecutionClientDukascopy, symbol: &str) -> Option<f64> {
        client
            .recon_client()
            .fetch_position_status_reports()
            .expect("the venue's answer")
            .into_iter()
            .find(|p| p.symbol == symbol)
            .map(|p| p.qty)
    }

    // (a) the venue lists nothing: EURUSD, long 1000 on the node, is flat at the venue.
    let dir = scratch("vike-duka-adopt-flat");
    let log = dir.path().join("launches.txt");
    let (mut client, mut rx) = adopting_client(&log);
    client.submit(&order("m1")); // a market buy: the fake fills it and the shadow folds the bare fill
    expect_filled(&mut rx, "m1");
    // The venue (the fake lists no position) is flat on a symbol the node holds: the pull says so at once, with no
    // restart and no push line to wait for.
    assert_eq!(venue_size(&client, "EURUSD"), Some(0.0), "the venue lists no order on it");
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));
    wait_until("the census to name EURUSD flat", || venue_size(&client, "EURUSD") == Some(0.0));
    client.detach();

    // (b) the venue lists a position on EURUSD (a line before the census) and one on USDJPY no fill explains.
    let dir = scratch("vike-duka-adopt-listed");
    let log = dir.path().join("launches.txt");
    std::fs::write(
        format!("{}.positions", log.display()),
        "EURUSD 1000.0 1.1\nUSDJPY 2000.0 150.0\n",
    )
    .expect("positions file");
    let (mut client, mut rx) = adopting_client(&log);
    client.submit(&order("m1"));
    expect_filled(&mut rx, "m1");
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));
    wait_until("the lines to arrive", || {
        venue_size(&client, "EURUSD") == Some(1000.0)
            && venue_size(&client, "USDJPY") == Some(2000.0)
    });
    // Everything the sidecar said before this answer was handled before it (one ordered stream): the census too.
    client.submit(&order("m2"));
    expect_filled(&mut rx, "m2");
    assert_eq!(venue_size(&client, "EURUSD"), Some(1000.0), "a symbol with a line is not flat");
    assert_eq!(venue_size(&client, "USDJPY"), Some(2000.0));
    let extra = drain(&mut rx);
    assert!(
        extra.iter().all(|e| !matches!(e, Event::Fill(_))),
        "the baseline of a position no fill explains books nothing: {extra:?}"
    );
    client.detach();
}
