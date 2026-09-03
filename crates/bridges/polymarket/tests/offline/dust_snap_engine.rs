//! Offline END-TO-END gate for the Polymarket **dust-snap fill tracker**: decoded user-channel
//! events driven through a REAL `vike_exec::ExecutionEngine`, which is where the premise of the
//! whole feature lives (the `ManagedOrder` FSM rejects a fill pushing cumulative filled past the
//! submitted qty, and the bare `Fill` folds `Account` guarded only by `seen_trade_ids`).
//!
//! The scripted decoder tests in `user_ws.rs` pin the decoder's output; these pin what that output
//! DOES to the engine — including the three failure modes an adversarial review found:
//!
//! - a cent-tick OVERFILL is snapped so the order actually reaches `Filled` (untracked it strands);
//! - the venue's MATCHED/MINED/CONFIRMED repeats of one match fold the position exactly once AND
//!   do not inflate the tracker's cumulative into snapping a later real fill away;
//! - a sub-tolerance remainder still RESTING is not force-completed, so the venue's later real fill
//!   of it is not double-counted.
//!
//! No network, no credentials.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::Event;
use vike_model::OrderRequest;
use vike_polymarket::{decode_user, decode_user_with_tracker, FillTracker, PolymarketRegistry};

const COID: &str = "coid-1";
const CLOB: &str = "0xORD";
const ASSET: &str = "111";
const QTY: f64 = 100.0;

fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(10_000.0, "polymarket", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "polymarket",
        ASSET,
    );
    let mut outbox = Outbox::default();
    e.submit_order(
        &OrderRequest {
            client_order_id: COID.to_string(),
            venue: "polymarket".to_string(),
            symbol: ASSET.to_string(),
            side: 1,
            qty: QTY,
            price: Some(0.5),
            ..Default::default()
        },
        0,
        &mut outbox,
    );
    // The adapter — not the engine — emits the submit/accept pair (the emitter split); replay it
    // so the FSM is in the same live state a real Polymarket order would be in.
    feed(
        &mut e,
        &[
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                client_order_id: COID.to_string(),
                ts: 0,
            }),
            Event::OrderAccepted(vike_model::events::OrderAccepted {
                client_order_id: COID.to_string(),
                venue_order_id: Some(CLOB.to_string().into()),
                ts: 0,
            }),
        ],
    );
    e
}

fn feed(e: &mut ExecutionEngine<RecordingClient>, evs: &[Event]) {
    let mut outbox = Outbox::default();
    for ev in evs {
        e.on_event(ev, &mut outbox);
    }
}

fn status(e: &ExecutionEngine<RecordingClient>) -> String {
    let view = e.local_view();
    view.orders
        .iter()
        .find(|(coid, _)| coid.as_str() == COID)
        .map(|(_, o)| format!("{:?}", o.status))
        .unwrap_or_else(|| "<missing>".into())
}

fn tracked() -> (PolymarketRegistry, FillTracker) {
    let reg = PolymarketRegistry::new();
    let t = FillTracker::new();
    t.register(COID, QTY);
    let _ = reg.on_accept(COID, CLOB, 1);
    (reg, t)
}

fn taker(id: &str, size: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "event_type": "trade", "type": "TRADE", "id": id, "status": status,
        "asset_id": ASSET, "side": "BUY", "size": size, "price": "0.50",
        "taker_order_id": CLOB, "maker_orders": []
    })
}

fn terminal() -> serde_json::Value {
    serde_json::json!({
        "event_type": "order", "type": "UPDATE", "id": CLOB, "asset_id": ASSET,
        "original_size": "100", "size_matched": "100"
    })
}

/// The feature's premise, proven against the real FSM: UNTRACKED, a cent-tick overfill strands the
/// order short of `Filled`; TRACKED, the snap lets it complete with the exact submitted position.
#[test]
fn dust_overfill_strands_untracked_and_completes_tracked() {
    // --- untracked (pre-feature behavior) ---
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept(COID, CLOB, 1);
    let mut e = engine();
    feed(&mut e, &decode_user(&taker("t1", "60", "MATCHED"), &reg));
    feed(&mut e, &decode_user(&taker("t2", "40.02", "MATCHED"), &reg));
    assert!(
        (e.position_size("BOTH") - 100.02).abs() < 1e-9,
        "untracked position {}",
        e.position_size("BOTH")
    );
    let untracked_status = status(&e);

    // --- tracked: the tail is snapped to the exact remaining 40.0 ---
    let (reg, t) = tracked();
    let mut e = engine();
    feed(&mut e, &decode_user_with_tracker(&taker("t1", "60", "MATCHED"), &reg, Some(&t)));
    feed(&mut e, &decode_user_with_tracker(&taker("t2", "40.02", "MATCHED"), &reg, Some(&t)));
    assert!(
        (e.position_size("BOTH") - 100.0).abs() < 1e-9,
        "tracked position {}",
        e.position_size("BOTH")
    );
    feed(&mut e, &decode_user_with_tracker(&terminal(), &reg, Some(&t)));
    assert_eq!(
        status(&e),
        "Filled",
        "tracked order must reach Filled (untracked: {untracked_status})"
    );
}

/// FINDING 1, end to end: the venue delivers the SAME match as MATCHED, MINED and CONFIRMED. The
/// core dedups the repeats (position folds once) and the tracker must not fold them either — if it
/// did, the tail fill would be snapped down and real qty destroyed.
#[test]
fn status_repeats_neither_double_fold_nor_snap_away_the_tail() {
    let (reg, t) = tracked();
    let mut e = engine();
    for st in ["MATCHED", "MINED", "CONFIRMED"] {
        feed(&mut e, &decode_user_with_tracker(&taker("t1", "0.02", st), &reg, Some(&t)));
    }
    assert!(
        (e.position_size("BOTH") - 0.02).abs() < 1e-9,
        "repeats folded more than once: {}",
        e.position_size("BOTH")
    );
    // the real tail must arrive INTACT (a re-folding tracker would emit 99.94 here)
    feed(&mut e, &decode_user_with_tracker(&taker("t2", "99.98", "MATCHED"), &reg, Some(&t)));
    assert!(
        (e.position_size("BOTH") - 100.0).abs() < 1e-9,
        "tail qty was destroyed: {}",
        e.position_size("BOTH")
    );
    feed(&mut e, &decode_user_with_tracker(&terminal(), &reg, Some(&t)));
    assert_eq!(status(&e), "Filled");
}

/// FINDING 2, end to end: a sub-tolerance remainder still resting must not be minted early —
/// otherwise the venue's later real fill of it folds `Account` a SECOND time (the bare `Fill` is
/// guarded only by `seen_trade_ids`, and a real trade id never matches the ":dust" one).
#[test]
fn a_resting_remainder_is_filled_once_not_twice() {
    let (reg, t) = tracked();
    let mut e = engine();
    feed(&mut e, &decode_user_with_tracker(&taker("t1", "99.97", "MATCHED"), &reg, Some(&t)));
    // the venue really fills the 0.03 later
    feed(&mut e, &decode_user_with_tracker(&taker("t2", "0.03", "MATCHED"), &reg, Some(&t)));
    assert!(
        (e.position_size("BOTH") - 100.0).abs() < 1e-9,
        "remainder double-counted: {}",
        e.position_size("BOTH")
    );
    feed(&mut e, &decode_user_with_tracker(&terminal(), &reg, Some(&t)));
    assert_eq!(status(&e), "Filled");
}

/// The dust-RESIDUAL half, end to end: the venue's matches stop 0.02 short, the terminal UPDATE
/// mints exactly one synthetic completing fill, and the order reaches `Filled` at the full qty.
#[test]
fn dust_residual_completes_the_order_at_the_terminal() {
    let (reg, t) = tracked();
    let mut e = engine();
    feed(&mut e, &decode_user_with_tracker(&taker("t1", "99.98", "MATCHED"), &reg, Some(&t)));
    assert!((e.position_size("BOTH") - 99.98).abs() < 1e-9);
    feed(&mut e, &decode_user_with_tracker(&terminal(), &reg, Some(&t)));
    assert!(
        (e.position_size("BOTH") - 100.0).abs() < 1e-9,
        "residual not minted: {}",
        e.position_size("BOTH")
    );
    assert_eq!(status(&e), "Filled");
}
