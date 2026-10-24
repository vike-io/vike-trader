//! The ORDINARY accept replays what the order kept (a follow-up of W19g, `lost_prefix_fold.rs`).
//!
//! `ManagedOrder::defer_refused` keeps a fill, cancel or expiry that finds the order `Initialized`, and
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
//! | F W S A | `Accepted`, filled 0, 1 wrap KEPT for ever, `dropped_terminal_on_live` 1 | `Filled` 1.0, nothing kept |
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
//! | S F W (no accept ever; what Hyperliquid's bridge sent for an instant fill until it began to say the accept itself) | `Submitted`, filled 0, `dropped_terminal_on_live` 1 | unchanged: not healed |

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Fold, Outbox};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderFilled,
    OrderPartiallyFilled, OrderRejected, OrderSubmitted, PositionSide,
};
use vike_model::{OrderEventKind, OrderStatus};

use crate::limit_order::req;
use crate::support::EngineBuilder;

const COID: &str = "c1";

/// A buy of 1 @ 100 on binance/BTCUSDT, registered `Initialized` as the core registers a submit.
fn engine() -> ExecutionEngine<RecordingClient> {
    let mut eng = EngineBuilder { venue: "binance".into(), ..Default::default() }.build();
    eng.collect_applied_fills = true;
    eng.submit_order(&req(COID), 1, &mut Outbox::default());
    eng
}

fn submitted() -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 4 })
}

fn accepted() -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: COID.into(),
        venue_order_id: Some("v-1".into()),
        ts: 5,
    })
}

/// The resolver's OPTIMISTIC accept (no venue order id).
fn accepted_no_id() -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: COID.into(),
        venue_order_id: None,
        ts: 5,
    })
}

fn fill(trade_id: &'static str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: COID.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
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
        client_order_id: COID.into(),
        fill: fill(trade_id, qty),
        ts: 6,
    })
}

fn full(trade_id: &'static str, qty: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: COID.into(),
        fill: fill(trade_id, qty),
        ts: 6,
    })
}

fn canceled() -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: COID.into(),
        reason: "venue".to_string().into(),
        ts: 6,
    })
}

fn expired() -> Event {
    Event::OrderExpired(OrderExpired { client_order_id: COID.into(), ts: 6 })
}

fn rejected() -> Event {
    Event::OrderRejected(OrderRejected {
        client_order_id: COID.into(),
        reason: "venue".to_string().into(),
        ts: 6,
    })
}

/// Fold `stream` in arrival order; the verdicts as one word.
fn walk(eng: &mut ExecutionEngine<RecordingClient>, stream: &[Event]) -> String {
    let mut bus = EventBus::new();
    let mut verdicts = String::new();
    for event in stream {
        verdicts.push(match bus.publish(event.clone(), eng) {
            Fold::Applied => 'A',
            Fold::Dropped => 'D',
        });
    }
    verdicts
}

/// Everything a row is judged on.
#[derive(Debug, PartialEq)]
struct Seen {
    status: OrderStatus,
    filled_qty: f64,
    avg_fill_px: f64,
    venue_order_id: Option<String>,
    /// How many refused events the order still holds.
    kept: usize,
    dropped_terminal_on_live: u64,
    stranded_terminal_drops: u64,
    position: f64,
}

fn seen(eng: &ExecutionEngine<RecordingClient>) -> Seen {
    let order = &eng.registry[COID];
    Seen {
        status: order.status,
        filled_qty: order.filled_qty,
        avg_fill_px: order.avg_fill_px,
        venue_order_id: order.venue_order_id.clone(),
        kept: order.deferred_events.len(),
        dropped_terminal_on_live: eng.dropped_terminal_on_live,
        stranded_terminal_drops: eng.stranded_terminal_drops,
        position: eng.position_size(PositionSide::Both),
    }
}

/// What a row expects: the venue id `v-1`, nothing kept, nothing stranded; the average price `100` once
/// anything filled.
fn want(
    status: OrderStatus,
    filled_qty: f64,
    dropped_terminal_on_live: u64,
    position: f64,
) -> Seen {
    Seen {
        status,
        filled_qty,
        avg_fill_px: if filled_qty > 0.0 { 100.0 } else { 0.0 },
        venue_order_id: Some("v-1".to_string()),
        kept: 0,
        dropped_terminal_on_live,
        stranded_terminal_drops: 0,
        position,
    }
}

/// The non-fill outcomes the strategy hears, in order.
fn heard(eng: &ExecutionEngine<RecordingClient>) -> Vec<OrderEventKind> {
    eng.order_events.iter().map(|o| o.event.kind.clone()).collect()
}

fn state_hash(eng: &ExecutionEngine<RecordingClient>) -> u64 {
    vike_exec::state_hash(&[eng.snapshot_state()])
}

// ---- the rows the change heals ----

#[test]
fn a_full_fill_before_submitted_lands_the_order_filled() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[bare("t1", 1.0), full("t1", 1.0), submitted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 1, 1.0));
    // the refusal of the wrap before the order could take it is still counted, as it always was
    assert_eq!(verdicts, "ADAA");
    assert_eq!(heard(&eng), vec![OrderEventKind::Accepted]);
}

#[test]
fn a_partial_fill_before_submitted_lands_the_order_partially_filled_and_it_then_completes() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[bare("t1", 0.5), part("t1", 0.5), submitted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::PartiallyFilled, 0.5, 0, 0.5));
    assert_eq!(verdicts, "ADAA");

    assert_eq!(walk(&mut eng, &[bare("t2", 0.5), full("t2", 0.5)]), "AA");
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 0, 1.0));
}

#[test]
fn several_fills_before_submitted_replay_in_arrival_order() {
    let mut eng = engine();
    let verdicts = walk(
        &mut eng,
        &[part("t1", 0.25), part("t2", 0.25), full("t3", 0.5), submitted(), accepted()],
    );
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DDDAA");
}

#[test]
fn a_venue_cancel_before_submitted_lands_the_order_canceled() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[canceled(), submitted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Canceled, 0.0, 1, 0.0));
    assert_eq!(verdicts, "DAA");
    // the strategy hears the answer and then the outcome, in that order
    assert_eq!(
        heard(&eng),
        vec![OrderEventKind::Accepted, OrderEventKind::Canceled { reason: "venue".into() }]
    );
}

#[test]
fn a_venue_expiry_before_submitted_lands_the_order_expired() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[expired(), submitted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Expired, 0.0, 1, 0.0));
    assert_eq!(verdicts, "DAA");
    assert_eq!(heard(&eng), vec![OrderEventKind::Accepted, OrderEventKind::Expired]);
}

#[test]
fn an_accept_that_names_no_venue_order_replays_the_kept_fill_too() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[full("t1", 1.0), submitted(), accepted_no_id()]);
    assert_eq!(seen(&eng), Seen { venue_order_id: None, ..want(OrderStatus::Filled, 1.0, 1, 0.0) });
    assert_eq!(verdicts, "DAA");
}

#[test]
fn an_optimistic_accept_that_was_refused_first_does_not_stop_the_ordinary_accept_replaying() {
    // The bare accept with no venue id is refused from `Initialized` (and not kept); the ordinary pair that
    // follows is the one that lands the order and replays the fill.
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[full("t1", 1.0), accepted_no_id(), submitted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DDAA");
}

#[test]
fn a_fill_redelivered_after_the_replay_is_dropped_by_the_dedup_and_the_end_state_is_the_old_one() {
    // The shape of the frozen `out_of_order` scenario of `fixtures/r5/hub.json` (fill, wrap, submitted,
    // accepted, then the venue redelivers the wrap twice). Before the change the order rested `Accepted` until
    // the first redelivery healed it, and ended `Filled 1.0`; now the replay lands it at the accept and BOTH
    // redeliveries are dedup'd. The end state is the same bytes (the kept buffer is outside the hash).
    let mut eng = engine();
    let verdicts = walk(
        &mut eng,
        &[
            bare("t1", 1.0),
            full("t1", 1.0),
            submitted(),
            accepted(),
            full("t1", 1.0),
            full("t1", 1.0),
        ],
    );
    assert_eq!(state_hash(&eng), OUT_OF_ORDER_HASH);
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 1, 1.0));
    assert_eq!(verdicts, "ADAADD");
}

#[test]
fn a_snap_taken_while_the_order_holds_the_fill_across_submitted_refolds_to_the_live_state() {
    // The buffer rides the `Snap`: a refold that starts from one taken after `OrderSubmitted` and before the
    // accept replays as the live run does, and both land the same hash.
    let mut live = engine();
    walk(&mut live, &[bare("t1", 1.0), full("t1", 1.0), submitted()]);
    assert_eq!(live.registry[COID].status, OrderStatus::Submitted);
    assert_eq!(live.registry[COID].deferred_events.len(), 1, "the wrap is held across Submitted");
    let snap = live.snapshot_state();

    let mut refold = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(walk(&mut live, &[accepted()]), "A");
    assert_eq!(walk(&mut refold, &[accepted()]), "A");

    assert_eq!(refold.registry[COID].status, OrderStatus::Filled);
    assert_eq!(refold.registry[COID].filled_qty, 1.0);
    assert_eq!(state_hash(&refold), state_hash(&live), "the refold lands the live run's bytes");
}

#[test]
fn a_second_accept_after_the_replay_is_the_ordinary_refusal() {
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[full("t1", 1.0), submitted(), accepted(), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Filled, 1.0, 1, 0.0));
    assert_eq!(verdicts, "DAAD");
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
        let mut eng = engine();
        let got = walk(&mut eng, stream);
        let got_hash = state_hash(&eng);
        if got != *verdicts || got_hash != *hash || !eng.registry[COID].deferred_events.is_empty() {
            wrong.push(format!(
                "{name}: verdicts {got:?} (want {verdicts:?}), hash {got_hash:#018x} (want {hash:#018x}), kept {}",
                eng.registry[COID].deferred_events.len()
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
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[submitted(), bare("t1", 1.0), full("t1", 1.0), accepted()]);
    assert_eq!(seen(&eng), want(OrderStatus::Accepted, 0.0, 1, 1.0));
    assert_eq!(verdicts, "AADA");
}

#[test]
fn known_gap_a_fill_with_no_accept_at_all_leaves_the_order_submitted() {
    // The shape of an instantly-filled order whose bridge maps the venue's reply to `OrderFilled` and sends no
    // `OrderAccepted` (Hyperliquid's `filled` status, until its bridge began to send the accept first). `Submitted +
    // OrderFilled` is not an edge of the FSM table (r5 `fsm.json`), and there is no accept to replay at. The engine's
    // table is unchanged, so a bridge must say the accept itself. Pinned, NOT endorsed.
    let mut eng = engine();
    let verdicts = walk(&mut eng, &[submitted(), bare("t1", 1.0), full("t1", 1.0)]);
    assert_eq!(
        seen(&eng),
        Seen { venue_order_id: None, ..want(OrderStatus::Submitted, 0.0, 1, 1.0) }
    );
    assert_eq!(verdicts, "AAD");
}

// ---- recorded on the tree before the change ----

/// `state_hash` after `legal_rows()[i]`, recorded on the tree before the change (`origin/main` 2056258b86,
/// the CI box lane, 2026-10-24). Rows 0 and 1 agree on purpose: the answer-first road lands the ordinary road's bytes.
const LEGAL_HASHES: [u64; 12] = [
    0x02fd_d548_4adf_d0da,
    0x02fd_d548_4adf_d0da,
    0x23ca_d0fc_381d_2804,
    0x8fd0_a67b_972e_0ca9,
    0xd647_6e75_7521_66f9,
    0x319b_ba7e_0da9_5345,
    0x39f2_02cb_4000_2e1f,
    0x03f4_adbd_a1aa_d84f,
    0xe114_9653_4107_3252,
    0xd546_3d09_76c9_c10e,
    0xb4ec_90e0_ae68_b1e2,
    0x85ca_367c_da68_0247,
];

/// `state_hash` after the `out_of_order`-shaped stream of
/// `a_fill_redelivered_after_the_replay_is_dropped_by_the_dedup_and_the_end_state_is_the_old_one`, recorded on
/// the tree before the change, where the order ended `Filled 1.0` only because the venue redelivered the wrap
/// (the verdicts differed: the first redelivery was folded then, both are dropped now).
const OUT_OF_ORDER_HASH: u64 = 0x36f1_b36e_9367_df21;
