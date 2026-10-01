//! **The polymarket half of the restart law** (`vike_bridge_core::user_data`'s module doc).
//!
//! `run_resync_supervisor` now drops a replayed event stamped before the mount, because the venue
//! history closures it replays are bounded by a ROW COUNT and by no time at all, and folding a
//! previous session's fills through the non-idempotent `Account::apply_fill` double-books their
//! fees and realized PnL. Polymarket is the consumer that made a blanket floor look dangerous: its
//! `resync_fetch` reads `get_orders`, i.e. RESTING orders whose creation legitimately predates the
//! mount, and **erring toward "dropped a live resting order" is far worse than erring toward
//! "re-booked a fill"**.
//!
//! This file is the evidence that the blanket floor is nonetheless safe HERE, taken from the real
//! code rather than from the argument:
//!
//!   1. a real `/data/orders` row for a LIVE resting order decodes to NOTHING at all —
//!      `user_ws::decode_order` matches on the WS-only `type` field, whose `PLACEMENT` value is its
//!      ignored `_ => {}` arm and which REST order rows do not carry — so there is no open-order
//!      event for any floor to drop;
//!   2. the events its history path CAN emit are stamped `ts: 0` (`decode_trade` reads only the WS
//!      `timestamp` field; the `/data/trades` REST rows carry `match_time` instead), and
//!      `exec_actor::is_pre_spawn` lets an unstamped event ride through by construction;
//!   3. driven end to end through the REAL `run_resync_supervisor` with a floor set at NOW, a
//!      months-old resting order and its months-old trade row still reach the sink.
//!
//! Point 2 is a property of code that could change, so it is asserted rather than assumed: if
//! polymarket ever starts stamping its replayed fills, this file goes red BEFORE the floor starts
//! eating them. ⚠ And it would eat them wholesale, not selectively — `match_time` is in SECONDS
//! (`"1700000005"`), so a stamp taken from it would be ~1.7e9 against a floor of ~1.8e12 and EVERY
//! replayed fill would look historical. That is the residual this file exists to catch early; it
//! errs toward losing a gap fill, never toward losing live order state.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::user_data::run_resync_supervisor;
use vike_model::events::Event;
use vike_polymarket::{PolymarketRegistry, map_polymarket_history};

/// A CLOB order hash (`0x` + 64 hex), the id `/data/orders` rows are keyed by.
const OUR_ORDER: &str = "0xd375be993c3e1a53dd8be58f66fbdb3eca151058abeb239f4ab7131be318179b";
/// An on-chain match id, the id `/data/trades` rows are keyed by.
const OUR_TRADE: &str = "0x39bd1a6cff56b2f61b7fd6e07089d387802bd34b6b929bd3a01f3970b1f38f5b";
/// A verbatim live-captured outcome token id (the ERC-1155 `asset_id`, i.e. the venue "symbol").
const TOKEN_YES: &str =
    "71321045679252212594626385532706912750332728571942532289631379312455583992563";

/// Epoch SECONDS, deliberately: this is the unit `/data/trades` `match_time` uses, and the whole
/// point of asserting `ts == 0` below is that nothing reads it into a millisecond field.
const LONG_AGO_SECS: i64 = 1_700_000_005;

fn seeded_registry() -> PolymarketRegistry {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("coid-resting", OUR_ORDER, 1);
    reg
}

/// One REAL `GET /data/orders` row: a LIVE, months-old, untouched resting order. Field-for-field
/// the shape `recon_client`'s own parse tests pin, which is where its faithfulness comes from.
fn resting_order_row() -> serde_json::Value {
    serde_json::json!([{
        "id": OUR_ORDER, "status": "LIVE",
        "market": "0x66bbf6d55e0296278858b3147689f3df9259374f158f9f028b608baa322a639c",
        "asset_id": TOKEN_YES, "side": "BUY",
        "original_size": "100", "size_matched": "0", "price": "0.42",
        "order_type": "GTC", "created_at": LONG_AGO_SECS
    }])
}

/// One REAL `GET /data/trades` row for that same order, matched months ago — `match_time`, NOT
/// `timestamp`, which is the field the decoder actually reads.
fn old_trade_row() -> serde_json::Value {
    serde_json::json!([{
        "id": OUR_TRADE, "status": "CONFIRMED", "asset_id": TOKEN_YES,
        "side": "BUY", "size": "100", "price": "0.42",
        "match_time": LONG_AGO_SECS.to_string(),
        "taker_order_id": OUR_ORDER, "maker_orders": []
    }])
}

/// **(1) A live resting order is not an event.** The concern a blanket floor raises here — that it
/// could drop live order state — cannot arise, because no such event is produced in the first
/// place: `/data/orders` rows carry no `type`, and `decode_order`'s `PLACEMENT`/absent arm is
/// `_ => {}`. Only a CANCELLATION or a terminal UPDATE decodes to anything.
#[test]
fn a_live_resting_order_row_decodes_to_no_event_at_all() {
    let reg = seeded_registry();
    let evs = map_polymarket_history(&serde_json::json!([]), &resting_order_row(), &reg);
    assert!(
        evs.is_empty(),
        "a LIVE `/data/orders` row must decode to nothing — if this ever produces an event, the \
         blanket history floor in `run_resync_supervisor` can drop live order state and must gain \
         a polymarket opt-out (`spawn_ms = 0`) before that lands: {evs:?}"
    );
}

/// **(2) Everything the history path DOES emit is unstamped.** This is the property that makes the
/// floor a provable no-op for this venue, and it is asserted over the real mapper so a future
/// change to the decoder reddens here first.
#[test]
fn every_replayed_polymarket_event_is_unstamped() {
    let reg = seeded_registry();
    let evs = map_polymarket_history(
        &old_trade_row(),
        &serde_json::json!([{ "id": OUR_ORDER, "type": "CANCELLATION" }]),
        &reg,
    );
    assert!(!evs.is_empty(), "the fixtures must produce something to assert about");
    for ev in &evs {
        assert_eq!(
            vike_bridge_core::exec_actor::event_ts(ev),
            0,
            "polymarket's replay is unstamped BY CONSTRUCTION (`decode_trade` reads the WS \
             `timestamp`, absent from `/data/trades` rows, which carry `match_time` — in SECONDS). \
             A stamp appearing here means the history floor has started dropping replayed fills: \
             {ev:?}"
        );
    }
}

/// **(3) End to end through the REAL supervisor, with the floor armed at NOW.** A months-old
/// resting order, its months-old trade, and its cancellation all still reach the sink.
#[test]
fn a_months_old_resting_order_survives_the_reconnect_history_floor() {
    let reg = seeded_registry();
    let replayed = map_polymarket_history(
        &old_trade_row(),
        &serde_json::json!([
            // the live resting order (decodes to nothing — assertion 1) ...
            resting_order_row()[0],
            // ... and its eventual cancellation, which does decode
            { "id": OUR_ORDER, "type": "CANCELLATION" }
        ]),
        &reg,
    );
    let expected = replayed.len();
    assert!(expected > 0, "the fixtures must produce something for the floor to pass through");

    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::<Event>::new()));
    let fetched = Arc::new(AtomicBool::new(false));
    // The floor a real mount installs: sampled by `spawn_pump_with_resync` at wiring time.
    let spawn_ms = vike_model::clock::now_ms();

    let (stop_t, seen_t, fetched_t) = (stop.clone(), seen.clone(), fetched.clone());
    let handle = std::thread::spawn(move || {
        let mut rows = Some(replayed);
        run_resync_supervisor(
            weak,
            0,
            spawn_ms,
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(0),
            move || {
                let out = rows.take().unwrap_or_default();
                fetched_t.store(true, Ordering::Relaxed);
                out
            },
            |ev| {
                seen_t.lock().unwrap().push(ev);
                true
            },
            None,
        );
    });

    generation.fetch_add(1, Ordering::Relaxed); // the WS reconnect this lane fires on
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fetched.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline, "the reconnect resync never fired");
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(20)); // let the emit loop drain
    drop(generation);
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        expected,
        "the history floor dropped {} of polymarket's {expected} replayed events — this venue's \
         replay is unstamped and must pass through WHOLE, however old the order is: {got:?}",
        expected - got.len()
    );
    assert!(
        got.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "coid-resting")),
        "the months-old order's cancellation must survive: {got:?}"
    );
}
