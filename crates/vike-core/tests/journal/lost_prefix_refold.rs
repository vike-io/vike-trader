//! W19-D1 and the journal: the venue's ANSWER to an order whose `OrderSubmitted`/`OrderAccepted` never
//! reached the fold (the resolution of an UNKNOWN submit across the gateway hop) is applied by the fold, so
//! the journal that recorded it must REFOLD to the same bytes: `replay_offline` re-pumps the raw records
//! through the same `ExecutionEngine::on_event` and fences the recomputed state hash against the live
//! session's exit `Snap`. Both ends run the new fold, so the fence holds; and a journal whose order took the
//! ordinary `Submitted -> Accepted -> Filled` road refolds as it always did (the door opens only behind an
//! `Err` on an `Initialized` order).

use crate::scratch::Scratch;
use std::path::Path;

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::{Command, GatewayCursor, GatewayEvent, Ingest, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::OrderStatus;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted,
};

use crate::kit::engines::sim_engine;
use crate::kit::events::sim_bare_fill;
use crate::kit::requests::sim_limit;

fn fill(tid: &str) -> FillEvent {
    let Event::Fill(mut f) = sim_bare_fill(tid, 1.0, 100.0) else { unreachable!() };
    f.client_order_id = "ord1".into();
    f
}

fn gateway(event: Event, seq: u64) -> Ingest {
    Ingest::GatewayEvent(Box::new(GatewayEvent {
        event,
        route_key: "sim".into(),
        cursor: GatewayCursor { gw_epoch: 9, seq, ts_ms: seq as i64 * 10 },
    }))
}

/// Run a journaled core: four warmup fills (the 4th trips the cadence `Snap`, the replay BASE), then a
/// `Submit` of `ord1` (it registers `Initialized`), then `answers` over the exec lane, then a clean stop.
fn run(dir: &Path, answers: Vec<Ingest>) {
    let handle = spawn_core(
        sim_engine(),
        CoreConfig {
            seed_cash: 10_000.0,
            coid_session: Some(("cafef00d".into(), 0)),
            journal: Some(JournalConfig {
                dir: dir.to_path_buf(),
                file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
                snapshot_every: 4,
            }),
            ..CoreConfig::default()
        },
    );
    let sender = handle.event_sender();
    for t in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(t, 1.0, 100.0)).unwrap();
    }
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "ord1", 1, 1.0, 100.0,
    )))));
    for a in answers {
        match a {
            Ingest::GatewayEvent(g) => sender.blocking_send_gateway(*g).unwrap(),
            Ingest::Event(e) => sender.blocking_send(e).unwrap(),
            other => panic!("not a venue event: {other:?}"),
        }
    }
    handle.shutdown_and_join();
}

/// `ord1`'s status in the exit `Snap` of the journal at `dir`.
fn exit_status(dir: &Path) -> OrderStatus {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => engines[0]
                .registry
                .iter()
                .find(|(coid, _)| coid == "ord1")
                .map(|(_, order)| order.status),
            _ => None,
        })
        .expect("a clean stop writes an exit Snap holding the order")
}

/// `ord1`'s fill quantity in the exit `Snap` of the journal at `dir`.
fn exit_filled_qty(dir: &Path) -> f64 {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, .. } => engines[0]
                .registry
                .iter()
                .find(|(coid, _)| coid == "ord1")
                .map(|(_, order)| order.filled_qty),
            _ => None,
        })
        .expect("a clean stop writes an exit Snap holding the order")
}

/// How many `Snap`s of the journal at `dir` were written while `ord1` HELD a deferred event.
fn snaps_holding_a_deferred_event(dir: &Path) -> usize {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .filter(|r| match r {
            JournalRecord::Snap { engines, .. } => engines[0]
                .registry
                .iter()
                .any(|(coid, order)| coid == "ord1" && !order.deferred_events.is_empty()),
            _ => false,
        })
        .count()
}

fn answer() -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: "ord1".into(),
        venue_order_id: Some("v-1".into()),
        ts: 5,
    })
}

fn filled() -> Event {
    Event::OrderFilled(OrderFilled { client_order_id: "ord1".into(), fill: fill("f1"), ts: 6 })
}

#[test]
fn a_journal_holding_a_bare_answer_for_an_initialized_order_refolds_to_the_same_bytes() {
    let dir = Scratch::reserved("lost-prefix-bare-answer");
    run(
        &dir,
        vec![gateway(answer(), 1), gateway(Event::Fill(fill("f1")), 2), gateway(filled(), 3)],
    );
    assert_eq!(
        exit_status(&dir),
        OrderStatus::Filled,
        "the live fold applied the answer and the fill"
    );

    let first =
        replay_offline(&dir).expect("the fence holds: the refold applies what the live run did");
    let second = replay_offline(&dir).expect("and again");
    assert_eq!(first.final_hash, second.final_hash, "a refold is deterministic");
    assert!(first.snaps_compared >= 1);
}

#[test]
fn an_ordinary_journal_refolds_as_it_always_did() {
    let dir = Scratch::reserved("lost-prefix-ordinary");
    run(
        &dir,
        vec![
            Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: "ord1".into(),
                ts: 4,
            })),
            Ingest::Event(answer()),
            Ingest::Event(Event::Fill(fill("f1"))),
            Ingest::Event(filled()),
        ],
    );
    assert_eq!(exit_status(&dir), OrderStatus::Filled);
    replay_offline(&dir).expect("the ordinary road never touches the door");
}

// ---- W19g: a fill (or cancel) that reached the core BEFORE the answer ----

#[test]
fn a_fill_that_came_before_the_bare_answer_is_in_the_order_and_the_journal_refolds_to_the_same_bytes()
 {
    // The fill is refused from `Initialized` (the r5 table), the bare answer lands the order, and the fold
    // replays what it held: the exit `Snap` has the order `Filled`, not a ghost `Accepted`. Eight more bare fills
    // sit between the refused wrap and the answer so a cadence `Snap` (every 4 records) is written WHILE the
    // order holds the deferred fill (ASSERTED below, not assumed): the refold must restore it and land where the
    // live run did.
    let dir = Scratch::reserved("lost-prefix-fill-before-answer");
    let mut stream = vec![gateway(Event::Fill(fill("f1")), 1), gateway(filled(), 2)];
    for (i, t) in ["g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8"].into_iter().enumerate() {
        stream.push(gateway(sim_bare_fill(t, 1.0, 100.0), 3 + i as u64));
    }
    stream.push(gateway(answer(), 11));
    run(&dir, stream);
    assert_eq!(
        exit_status(&dir),
        OrderStatus::Filled,
        "the deferred fill was replayed at the answer"
    );

    assert!(
        snaps_holding_a_deferred_event(&dir) >= 1,
        "a cadence Snap was written while the order held the buffer: the refold below starts mid-buffer"
    );

    let first = replay_offline(&dir)
        .expect("the fence holds: the refold defers and replays as the live run did");
    let second = replay_offline(&dir).expect("and again");
    assert_eq!(first.final_hash, second.final_hash, "a refold is deterministic");
    assert!(first.snaps_compared >= 1);
}

#[test]
fn a_fill_that_came_before_submitted_is_in_the_order_and_the_journal_refolds_to_the_same_bytes() {
    // The ORDINARY road: the wrap is refused and kept, then `OrderSubmitted`, then the accept walks
    // `Submitted -> Accepted` the ordinary way and finds the buffer non-empty, so it replays the kept wrap
    // (the same replay as a bare answer, `tests/engine/ordinary_accept_replay.rs`'s rows). As above, eight
    // bare fills sit between the wrap and `OrderSubmitted` so a cadence `Snap` is written while the order
    // holds the buffer: the refold must restore it, carry it across `Submitted` and land where the live run did.
    let dir = Scratch::reserved("lost-prefix-fill-before-submitted");
    let mut stream = vec![gateway(Event::Fill(fill("f1")), 1), gateway(filled(), 2)];
    for (i, t) in ["g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8"].into_iter().enumerate() {
        stream.push(gateway(sim_bare_fill(t, 1.0, 100.0), 3 + i as u64));
    }
    stream.push(gateway(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "ord1".into(), ts: 4 }),
        11,
    ));
    stream.push(gateway(answer(), 12));
    run(&dir, stream);
    assert_eq!(
        exit_status(&dir),
        OrderStatus::Filled,
        "the kept wrap was replayed at the ordinary accept"
    );
    assert_eq!(exit_filled_qty(&dir), 1.0);

    assert!(
        snaps_holding_a_deferred_event(&dir) >= 1,
        "a cadence Snap was written while the order held the buffer: the refold below starts mid-buffer"
    );

    let first = replay_offline(&dir)
        .expect("the fence holds: the refold defers, carries it across Submitted and replays");
    let second = replay_offline(&dir).expect("and again");
    assert_eq!(first.final_hash, second.final_hash, "a refold is deterministic");
    assert!(first.snaps_compared >= 1);
}

#[test]
fn a_fill_redelivered_under_its_trade_id_before_the_answer_counts_once_and_the_journal_refolds_to_the_same_bytes()
 {
    // W13c finding 4: the same trade id twice (a reconnect-resync, another `ts`) BEFORE the answer: both were
    // held and both applied. One fill of 0.25 is in the order, through a refold that starts mid-buffer.
    let dir = Scratch::reserved("lost-prefix-redelivered-fill");
    let mut f = fill("f1");
    f.last_qty = 0.25;
    let wrap = |ts: i64| {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "ord1".into(),
            fill: FillEvent { ts, ..f.clone() },
            ts,
        })
    };
    let mut stream = vec![gateway(wrap(10), 1), gateway(wrap(20), 2)];
    for (i, t) in ["g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8"].into_iter().enumerate() {
        stream.push(gateway(sim_bare_fill(t, 1.0, 100.0), 3 + i as u64));
    }
    stream.push(gateway(answer(), 11));
    run(&dir, stream);
    assert_eq!(exit_status(&dir), OrderStatus::PartiallyFilled);
    assert_eq!(exit_filled_qty(&dir), 0.25, "one fill, not two");
    assert!(snaps_holding_a_deferred_event(&dir) >= 1, "the refold starts mid-buffer");

    let first = replay_offline(&dir).expect("the fence holds");
    let second = replay_offline(&dir).expect("and again");
    assert_eq!(first.final_hash, second.final_hash, "a refold is deterministic");
}
