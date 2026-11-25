//! The ORDINARY accept replays what the order kept (a follow-up of W19g, `lost_prefix_fold.rs`).
//!
//! `OrderRecord::defer_refused` keeps a fill, cancel or expiry that finds the order `Initialized`, and
//! `land_lost_prefix_answer` replays it when the answer lands. The answer used to be recognised in ONE shape:
//! a bare `OrderAccepted` on an order still `Initialized`. A venue that sends the fill FIRST and then
//! `OrderSubmitted` and `OrderAccepted` walks the order `Initialized -> Submitted -> Accepted` the ordinary way,
//! so nothing replayed it and the order rested `Accepted filled 0` while the venue held it filled (the CI box
//! 2026-10-24, a Deribit market sell: `terminal event dropped on a live order ... status="INITIALIZED"`, then a
//! held `OrphanLocalOrder`). `on_event` now opens the same replay when an ordinary accept finds the buffer
//! non-empty.
//!
//! Three groups, one test per row:
//!
//! * the rows the change HEALS (the fill, partial fill, cancel or expiry comes first; `OrderSubmitted` and
//!   `OrderAccepted` follow in the ordinary way);
//! * the rows it must NOT touch, pinned as the engine's end-state bytes (`state_hash`) and verdicts, recorded
//!   on the tree BEFORE the change: every order of events that was legal then, and the existing replay for a
//!   bare answer;
//! * the rows it does NOT heal, pinned so a later widening is a decision and not an accident: a fill that finds
//!   the order `Submitted` is refused and NOT kept (`defer_refused` keeps only from `Initialized`).
//!
//! Verdict words below: `A` = the event was folded, `D` = it was dropped.
//!
//! ## The table (S `OrderSubmitted`, A `OrderAccepted`, F bare `Fill`, W `OrderFilled` wrap, P
//! `OrderPartiallyFilled` wrap, C `OrderCanceled`, X `OrderExpired`; MEASURED on `origin/main` 2056258b86 before
//! the change, in a the CI box lane, by running these same tests against it)
//!
//! | event order | before | after |
//! |---|---|---|
//! | F W S A | `Accepted`, filled 0, 1 wrap KEPT for ever, `closing_events_dropped_order_still_live` 1 | `Filled` 1.0, nothing kept |
//! | F P S A | `Accepted`, filled 0, 1 kept (position 0.5 right) | `PartiallyFilled` 0.5 |
//! | P P W S A | `Accepted`, filled 0, 3 kept | `Filled` 1.0, replayed in arrival order |
//! | C S A | `Accepted`, 1 kept (the venue canceled it) | `Canceled` |
//! | X S A | `Accepted`, 1 kept | `Expired` |
//! | W S A(no venue id) | `Accepted`, 1 kept | `Filled` |
//! | W A(no venue id) S A | `Accepted`, 1 kept | `Filled` |
//! | F W S A W W (r5 `out_of_order`) | `Filled` 1.0, but only because the venue sent the wrap AGAIN | `Filled` 1.0 at the accept; same `state_hash`; both redeliveries dropped |
//! | W A(id) | `Filled` (the existing bare-answer replay) | unchanged, bytes pinned |
//! | A(id) S F W, S A F W, S A P P W, S A C, S R, A W, S A X, S A A, S A0 P W, C A, P A W | the ordinary and the answer-first roads | unchanged, bytes pinned (`legal_rows`) |
//! | S F W A (the fill lands BETWEEN the bridge's two sends) | `Accepted`, filled 0, NOTHING kept | unchanged: not healed |
//! | S F W (no accept ever; what Hyperliquid's bridge sent for an instant fill until it began to say the accept itself) | `Submitted`, filled 0, `closing_events_dropped_order_still_live` 1 | unchanged: not healed |

use vike_execution::testing::{EngineBuilder, RecordingClient, resting_limit};
use vike_execution::{Engine, EventBus, EventBusFold, EventBusOutbox};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderFilled,
    OrderPartiallyFilled, OrderRejected, OrderSubmitted, PositionSide,
};
use vike_model::{OrderEventKind, OrderStatus};

const ORDER_CLIENT_ID: &str = "c1";

/// A buy of 1 @ 100 on binance/BTCUSDT, registered `Initialized` as the core registers a submit.
fn engine() -> Engine<RecordingClient> {
    let mut engine_instance =
        EngineBuilder { venue: "binance".into(), ..Default::default() }.build();
    engine_instance.collect_applied_fills = true;
    engine_instance.submit_order(
        &resting_limit(ORDER_CLIENT_ID),
        1,
        &mut EventBusOutbox::default(),
    );
    engine_instance
}

fn submitted() -> Event {
    Event::OrderSubmitted(OrderSubmitted { order_client_id: ORDER_CLIENT_ID.into(), timestamp: 4 })
}

fn accepted() -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: ORDER_CLIENT_ID.into(),
        venue_order_id: Some("v-1".into()),
        timestamp: 5,
    })
}

/// The resolver's OPTIMISTIC accept (no venue order id).
fn accepted_no_id() -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: ORDER_CLIENT_ID.into(),
        venue_order_id: None,
        timestamp: 5,
    })
}

fn fill(trade_id: &'static str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        order_client_id: ORDER_CLIENT_ID.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    }
}

/// The bare fill the account folds (the pump sends it with its wrap).
fn bare(trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(fill(trade_id, qty))
}

fn part(trade_id: &'static str, qty: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: ORDER_CLIENT_ID.into(),
        fill: fill(trade_id, qty),
        timestamp: 6,
    })
}

fn full(trade_id: &'static str, qty: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: ORDER_CLIENT_ID.into(),
        fill: fill(trade_id, qty),
        timestamp: 6,
    })
}

fn canceled() -> Event {
    Event::OrderCanceled(OrderCanceled {
        order_client_id: ORDER_CLIENT_ID.into(),
        reason: "venue".to_string().into(),
        timestamp: 6,
        post_only_would_cross: false,
        refusal_id: None,
    })
}

fn expired() -> Event {
    Event::OrderExpired(OrderExpired { order_client_id: ORDER_CLIENT_ID.into(), timestamp: 6 })
}

fn rejected() -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: ORDER_CLIENT_ID.into(),
        reason: "venue".to_string().into(),
        timestamp: 6,
        refusal_id: None,
    })
}

/// Fold `stream` in arrival order; the verdicts as one word.
fn walk(engine: &mut Engine<RecordingClient>, stream: &[Event]) -> String {
    let mut bus = EventBus::new();
    let mut verdicts = String::new();
    for event in stream {
        verdicts.push(match bus.publish(event.clone(), engine) {
            EventBusFold::Applied => 'A',
            EventBusFold::Dropped => 'D',
        });
    }
    verdicts
}

/// Everything a row is judged on.
#[derive(Debug, PartialEq)]
struct Seen {
    status: OrderStatus,
    filled_qty: f64,
    average_fill_price: f64,
    venue_order_id: Option<String>,
    /// How many refused events the order still holds.
    kept: usize,
    closing_events_dropped_order_still_live: u64,
    events_dropped_order_already_closed: u64,
    position: f64,
}

fn seen(engine: &Engine<RecordingClient>) -> Seen {
    let order = &engine.registry[ORDER_CLIENT_ID];
    Seen {
        status: order.status,
        filled_qty: order.filled_qty,
        average_fill_price: order.average_fill_price,
        venue_order_id: order.venue_order_id.clone(),
        kept: order.deferred_events.len(),
        closing_events_dropped_order_still_live: engine.closing_events_dropped_order_still_live,
        events_dropped_order_already_closed: engine.events_dropped_order_already_closed,
        position: engine.position_size(PositionSide::Both),
    }
}

/// What a row expects: the venue id `v-1`, nothing kept, nothing stranded; the average price `100` once
/// anything filled.
fn want(
    status: OrderStatus,
    filled_qty: f64,
    closing_events_dropped_order_still_live: u64,
    position: f64,
) -> Seen {
    Seen {
        status,
        filled_qty,
        average_fill_price: if filled_qty > 0.0 { 100.0 } else { 0.0 },
        venue_order_id: Some("v-1".to_string()),
        kept: 0,
        closing_events_dropped_order_still_live,
        events_dropped_order_already_closed: 0,
        position,
    }
}

/// The non-fill outcomes the strategy hears, in order.
fn heard(engine: &Engine<RecordingClient>) -> Vec<OrderEventKind> {
    engine.order_events.iter().map(|o| o.event.kind.clone()).collect()
}

fn state_hash(engine: &Engine<RecordingClient>) -> u64 {
    vike_execution::state_hash(&[engine.snapshot_state()])
}

// ---- the rows the change heals ----

#[test]
fn a_full_fill_before_submitted_lands_the_order_filled() {
    let mut engine_under_test = engine();
    let verdicts =
        walk(&mut engine_under_test, &[bare("t1", 1.0), full("t1", 1.0), submitted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 1, 1.0));
    // the refusal of the wrap before the order could take it is still counted, as it always was
    assert_eq!(verdicts, "ADAA");
    assert_eq!(heard(&engine_under_test), vec![OrderEventKind::Accepted]);
}

#[test]
fn a_partial_fill_before_submitted_lands_the_order_partially_filled_and_it_then_completes() {
    let mut engine_under_test = engine();
    let verdicts =
        walk(&mut engine_under_test, &[bare("t1", 0.5), part("t1", 0.5), submitted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::PartiallyFilled, 0.5, 0, 0.5));
    assert_eq!(verdicts, "ADAA");

    assert_eq!(walk(&mut engine_under_test, &[bare("t2", 0.5), full("t2", 0.5)]), "AA");
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 0, 1.0));
}

#[test]
fn several_fills_before_submitted_replay_in_arrival_order() {
    let mut engine_under_test = engine();
    let verdicts = walk(
        &mut engine_under_test,
        &[part("t1", 0.25), part("t2", 0.25), full("t3", 0.5), submitted(), accepted()],
    );
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DDDAA");
}

#[test]
fn a_venue_cancel_before_submitted_lands_the_order_canceled() {
    let mut engine_under_test = engine();
    let verdicts = walk(&mut engine_under_test, &[canceled(), submitted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Canceled, 0.0, 1, 0.0));
    assert_eq!(verdicts, "DAA");
    // the strategy hears the answer and then the outcome, in that order
    assert_eq!(
        heard(&engine_under_test),
        vec![OrderEventKind::Accepted, OrderEventKind::Canceled { reason: "venue".into() }]
    );
}

#[test]
fn a_venue_expiry_before_submitted_lands_the_order_expired() {
    let mut engine_under_test = engine();
    let verdicts = walk(&mut engine_under_test, &[expired(), submitted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Expired, 0.0, 1, 0.0));
    assert_eq!(verdicts, "DAA");
    assert_eq!(heard(&engine_under_test), vec![OrderEventKind::Accepted, OrderEventKind::Expired]);
}

#[test]
fn an_accept_that_names_no_venue_order_replays_the_kept_fill_too() {
    let mut engine_under_test = engine();
    let verdicts = walk(&mut engine_under_test, &[full("t1", 1.0), submitted(), accepted_no_id()]);
    assert_eq!(
        seen(&engine_under_test),
        Seen { venue_order_id: None, ..want(OrderStatus::Filled, 1.0, 1, 0.0) }
    );
    assert_eq!(verdicts, "DAA");
}

#[test]
fn an_optimistic_accept_that_was_refused_first_does_not_stop_the_ordinary_accept_replaying() {
    // The bare accept with no venue id is refused from `Initialized` (and not kept); the ordinary pair that
    // follows is the one that lands the order and replays the fill.
    let mut engine_under_test = engine();
    let verdicts =
        walk(&mut engine_under_test, &[full("t1", 1.0), accepted_no_id(), submitted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DDAA");
}

#[test]
fn a_fill_redelivered_after_the_replay_is_dropped_by_the_dedup_and_the_end_state_is_the_old_one() {
    // The shape of the frozen `out_of_order` scenario of `fixtures/r5/hub.json` (fill, wrap, submitted,
    // accepted, then the venue redelivers the wrap twice). Before the change the order rested `Accepted` until
    // the first redelivery healed it, and ended `Filled 1.0`; now the replay lands it at the accept and BOTH
    // redeliveries are dedup'd. The end state is the same bytes (the kept buffer is outside the hash).
    let mut engine_under_test = engine();
    let verdicts = walk(
        &mut engine_under_test,
        &[
            bare("t1", 1.0),
            full("t1", 1.0),
            submitted(),
            accepted(),
            full("t1", 1.0),
            full("t1", 1.0),
        ],
    );
    assert_eq!(state_hash(&engine_under_test), OUT_OF_ORDER_HASH);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 1, 1.0));
    assert_eq!(verdicts, "ADAADD");
}

#[test]
fn a_snap_taken_while_the_order_holds_the_fill_across_submitted_refolds_to_the_live_state() {
    // The buffer rides the `Snapshot`: a refold that starts from one taken after `OrderSubmitted` and before the
    // accept replays as the live run does, and both land the same hash.
    let mut live = engine();
    walk(&mut live, &[bare("t1", 1.0), full("t1", 1.0), submitted()]);
    assert_eq!(live.registry[ORDER_CLIENT_ID].status, OrderStatus::Submitted);
    assert_eq!(
        live.registry[ORDER_CLIENT_ID].deferred_events.len(),
        1,
        "the wrap is held across Submitted"
    );
    let snap = live.snapshot_state();

    let mut refold = Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(walk(&mut live, &[accepted()]), "A");
    assert_eq!(walk(&mut refold, &[accepted()]), "A");

    assert_eq!(refold.registry[ORDER_CLIENT_ID].status, OrderStatus::Filled);
    assert_eq!(refold.registry[ORDER_CLIENT_ID].filled_qty, 1.0);
    assert_eq!(state_hash(&refold), state_hash(&live), "the refold lands the live run's bytes");
}

#[test]
fn a_second_accept_after_the_replay_is_the_ordinary_refusal() {
    let mut engine_under_test = engine();
    let verdicts =
        walk(&mut engine_under_test, &[full("t1", 1.0), submitted(), accepted(), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DAAD");
}

/// OKX's `orders` channel pushes a `live` row for a MARKET order too (a `live` row, then its `filled` row
/// within the same millisecond, measured on the demo), and the pump sends that row as a bare accept, never
/// held by the submit fence. The loop's own `OrderSubmitted` and `OrderAccepted` follow it and the fence then
/// lets the held fill go: the stream's accept is the answer, the loop's two are refused as the repeats they
/// are, and the order ends `Filled` once with nothing kept, stranded or dropped on a live order.
#[test]
fn a_streams_bare_accept_then_the_loops_own_answer_then_the_held_fill_ends_filled_once() {
    let mut engine_under_test = engine();
    let verdicts = walk(
        &mut engine_under_test,
        &[accepted(), submitted(), accepted(), bare("t1", 1.0), full("t1", 1.0)],
    );
    assert_eq!(verdicts, "ADDAA");
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Filled, 1.0, 0, 1.0));
    assert_eq!(
        heard(&engine_under_test),
        vec![OrderEventKind::Accepted],
        "the strategy hears ONE accept"
    );
}

// ---- the rows it must not touch ----

/// Every order of events that was legal BEFORE the change, and the existing bare-answer replay: the verdicts
/// and the engine's `state_hash` after the stream, recorded on the tree before the change. The new branch is
/// reached only by an ordinary accept that finds the buffer non-empty, and none of these has one.
#[test]
fn every_order_of_events_that_was_legal_before_keeps_its_verdicts_and_its_bytes() {
    let rows = legal_rows();
    let mut wrong = Vec::new();
    for (name, stream, verdicts, hash) in &rows {
        let mut engine_under_test = engine();
        let got = walk(&mut engine_under_test, stream);
        let got_hash = state_hash(&engine_under_test);
        if got != *verdicts
            || got_hash != *hash
            || !engine_under_test.registry[ORDER_CLIENT_ID].deferred_events.is_empty()
        {
            wrong.push(format!(
                "{name}: verdicts {got:?} (want {verdicts:?}), hash {got_hash:#018x} (want {hash:#018x}), kept {}",
                engine_under_test.registry[ORDER_CLIENT_ID].deferred_events.len()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} rows moved:\n{}",
        wrong.len(),
        rows.len(),
        wrong.join("\n")
    );
}

type LegalRow = (&'static str, Vec<Event>, &'static str, u64);

fn legal_rows() -> Vec<LegalRow> {
    vec![
        (
            "submitted, accepted, fill, wrap (the ordinary road)",
            vec![submitted(), accepted(), bare("t1", 1.0), full("t1", 1.0)],
            "AAAA",
            LEGAL_HASHES[0],
        ),
        (
            "accepted, submitted, fill, wrap (the answer first)",
            vec![accepted(), submitted(), bare("t1", 1.0), full("t1", 1.0)],
            "ADAA",
            LEGAL_HASHES[1],
        ),
        (
            "partial fills, then the full one",
            vec![submitted(), accepted(), part("t1", 0.25), part("t2", 0.25), full("t3", 0.5)],
            "AAAAA",
            LEGAL_HASHES[2],
        ),
        ("cancel after accept", vec![submitted(), accepted(), canceled()], "AAA", LEGAL_HASHES[3]),
        ("reject after submit", vec![submitted(), rejected()], "AA", LEGAL_HASHES[4]),
        (
            "the resolved answer, then the fill",
            vec![accepted(), full("t1", 1.0)],
            "AA",
            LEGAL_HASHES[5],
        ),
        ("expiry after accept", vec![submitted(), accepted(), expired()], "AAA", LEGAL_HASHES[6]),
        ("a duplicate accept", vec![submitted(), accepted(), accepted()], "AAD", LEGAL_HASHES[7]),
        (
            "an optimistic accept, then fills",
            vec![submitted(), accepted_no_id(), part("t1", 0.5), full("t2", 0.5)],
            "AAAA",
            LEGAL_HASHES[8],
        ),
        (
            "a wrap before the bare answer (the existing replay)",
            vec![full("t1", 1.0), accepted()],
            "DA",
            LEGAL_HASHES[9],
        ),
        (
            "a cancel before the bare answer (the existing replay)",
            vec![canceled(), accepted()],
            "DA",
            LEGAL_HASHES[10],
        ),
        (
            "a partial wrap before the bare answer, then the rest",
            vec![part("t1", 0.5), accepted(), full("t2", 0.5)],
            "DAA",
            LEGAL_HASHES[11],
        ),
    ]
}

// ---- the rows it does not heal ----

#[test]
fn known_gap_a_fill_that_finds_the_order_submitted_is_refused_and_not_kept() {
    // The shape of a venue whose fill lands between the bridge's `OrderSubmitted` and its `OrderAccepted`
    // (two separate sends). `defer_refused` keeps only from `Initialized`, so nothing is held and the accept
    // that follows is ordinary: the order rests `Accepted filled 0`. Pinned, NOT endorsed.
    let mut engine_under_test = engine();
    let verdicts =
        walk(&mut engine_under_test, &[submitted(), bare("t1", 1.0), full("t1", 1.0), accepted()]);
    assert_eq!(seen(&engine_under_test), want(OrderStatus::Accepted, 0.0, 1, 1.0));
    assert_eq!(verdicts, "AADA");
}

#[test]
fn known_gap_a_fill_with_no_accept_at_all_leaves_the_order_submitted() {
    // The shape of an instantly-filled order whose bridge maps the venue's reply to `OrderFilled` and sends no
    // `OrderAccepted` (Hyperliquid's `filled` status, until its bridge began to send the accept first). `Submitted +
    // OrderFilled` is not an edge of the FSM table (r5 `fsm.json`), and there is no accept to replay at. The engine's
    // table is unchanged, so a bridge must say the accept itself. Pinned, NOT endorsed.
    let mut engine_under_test = engine();
    let verdicts = walk(&mut engine_under_test, &[submitted(), bare("t1", 1.0), full("t1", 1.0)]);
    assert_eq!(
        seen(&engine_under_test),
        Seen { venue_order_id: None, ..want(OrderStatus::Submitted, 0.0, 1, 1.0) }
    );
    assert_eq!(verdicts, "AAD");
}

// ---- recorded on the tree before the change ----

/// `state_hash` after `legal_rows()[i]`, recorded on the tree before the change (`origin/main` 2056258b86,
/// the CI box lane, 2026-10-24). Rows 0 and 1 agree on purpose: the answer-first road lands the ordinary road's bytes.
/// Re-recorded once when the order client id key was spelled out in full (P4.5b): the JSON's key moved, the
/// verdicts did not. Re-recorded again (2026-11-14) for the phase-4 stored words (P4.5b's order client id keys and P4.5c's field words) on the package
/// tree: every row's verdicts are the ones above, only the JSON's keys moved. Re-recorded (2026-11-19) for format
/// 34's FSM trade ids written as their difference from the account's: the verdicts are the ones above, only the
/// trade-id keys of the canonical JSON moved.
const LEGAL_HASHES: [u64; 12] = [
    0x9a05_b9ba_d9bf_84df,
    0x9a05_b9ba_d9bf_84df,
    0x0a32_7fb1_2c36_4e77,
    0x1802_7830_edc2_1607,
    0xc6c2_4078_e623_df19,
    0x71b2_1fe3_db65_c8e2,
    0x4808_5a70_f145_f823,
    0xf792_c684_5bee_fd29,
    0xc765_b917_f38c_32ad,
    0x4c96_357e_c42b_23a9,
    0x3e05_fb4d_cfdb_0390,
    0xc692_4ef2_450f_1bbc,
];

/// `state_hash` after the `out_of_order`-shaped stream of
/// `a_fill_redelivered_after_the_replay_is_dropped_by_the_dedup_and_the_end_state_is_the_old_one`, recorded on
/// the tree before the change, where the order ended `Filled 1.0` only because the venue redelivered the wrap
/// (the verdicts differed: the first redelivery was folded then, both are dropped now).
/// Re-recorded 2026-11-14 for the phase-4 stored words (P4.5b's order client id keys and P4.5c's field words) (the verdicts `ADAADD` unchanged),
/// and 2026-11-19 for format 34's FSM trade ids written as a difference (the verdicts unchanged).
const OUT_OF_ORDER_HASH: u64 = 0xa2a7_62f7_5526_3fe8;
