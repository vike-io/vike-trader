//! The five scenarios, each returning `Ok` / `Err(reason)` so the matrix records a per-cell outcome.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_bridge_core::exec_actor::{ExecActor, ExecCommand};
use vike_bridge_core::resolve_ambiguous_submit;
// The shared scripted user-data stream double (`test-support`). Spelled `as Scripted`, the
// shorthand the sibling user-data suites use: `scripted` ALSO defines a distinct `ScriptedStream`
// (the market/depth double), so aliasing to that name would give one type the other's spelling.
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, run_user_data_forever};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, EventSender, ExecutionClient, ExecutionEngine, Ingest,
    OrderStatus, Outbox, RiskGate, event_channel,
};
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent, OrderCancelRejected};

use super::{AccountLane, ConformanceBridge, ContractOrder, ExecKind, FillShape, submitted};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Scenario {
    Lifecycle,
    TransportDeath,
    CancelAfterClose,
    ReconnectMidOrder,
    PositionFold,
}

impl Scenario {
    pub(super) const ALL: [Scenario; 5] = [
        Scenario::Lifecycle,
        Scenario::TransportDeath,
        Scenario::CancelAfterClose,
        Scenario::ReconnectMidOrder,
        Scenario::PositionFold,
    ];
    pub(super) fn label(self) -> &'static str {
        match self {
            Scenario::Lifecycle => "lifecycle",
            Scenario::TransportDeath => "transport-death",
            Scenario::CancelAfterClose => "cancel-after-close",
            Scenario::ReconnectMidOrder => "reconnect-mid-order",
            Scenario::PositionFold => "position-fold",
        }
    }
}

/// A per-check assertion that records the failure reason rather than unwinding, so the coverage
/// matrix can show which cell failed and why. Uses an `if/else` guard (not `if !cond`) so a float
/// comparison inside `$cond` never trips `clippy::neg_cmp_op_on_partial_ord`.
macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {
        } else {
            return Err(format!($($arg)*));
        }
    };
}

pub(super) fn run_cell(bridge: &dyn ConformanceBridge, scenario: Scenario) -> Result<(), String> {
    match scenario {
        Scenario::Lifecycle => scenario_lifecycle(bridge),
        Scenario::TransportDeath => scenario_transport_death(bridge),
        Scenario::CancelAfterClose => scenario_cancel_after_close(bridge),
        Scenario::ReconnectMidOrder => scenario_reconnect(bridge),
        Scenario::PositionFold => scenario_position_fold(bridge),
    }
}

/// (1) submit → accept → fill reaches exactly one terminal (`Filled`) with `filled_qty == 1.0`.
/// A [`FillShape::Cumulative`] venue fills in two pieces (partial 0.4 → `PartiallyFilled`, then
/// 0.6 → `Filled`); a [`FillShape::Whole`] venue reports the full 1.0 in ONE execution (its mapper
/// has no partial state) → straight to `Filled`. The submit/accept setup and the terminal asserts
/// are shared; only the fill leg branches.
fn scenario_lifecycle(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "life-1";
    let mut order = ContractOrder::new(b.order(coid));

    order.fold(&submitted(coid)); // Rust-side half of the emitter split
    check!(
        order.status() == OrderStatus::Submitted,
        "after submit expected Submitted, got {:?}",
        order.status()
    );

    let accepted = b.decode(&b.frame_accepted(coid, "v-1"));
    check!(
        accepted.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "accept frame did not decode to OrderAccepted: {accepted:?}"
    );
    for e in &accepted {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Accepted,
        "after accept expected Accepted, got {:?}",
        order.status()
    );

    match b.fill_shape() {
        FillShape::Cumulative => {
            // partial fill 0.4 of 1.0 → PartiallyFilled
            for e in &b.decode(&b.frame_fill(coid, "t1", 0.4, 0.4, 1.0, 50_000.0, false)) {
                order.fold(e);
            }
            check!(
                order.status() == OrderStatus::PartiallyFilled,
                "after partial expected PartiallyFilled, got {:?}",
                order.status()
            );
            // remaining 0.6 completes the order
            let full = b.decode(&b.frame_fill(coid, "t2", 0.6, 1.0, 1.0, 50_000.0, true));
            check!(
                full.iter().any(|e| matches!(e, Event::OrderFilled(_))),
                "full-fill frame did not decode to OrderFilled: {full:?}"
            );
            for e in &full {
                order.fold(e);
            }
        }
        FillShape::Whole => {
            // ONE execution reports the whole 1.0 → Filled (no intermediate partial state exists).
            let full = b.decode(&b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true));
            check!(
                full.iter().any(|e| matches!(e, Event::OrderFilled(_))),
                "whole-fill frame did not decode to OrderFilled: {full:?}"
            );
            for e in &full {
                order.fold(e);
            }
        }
    }

    check!(
        order.status() == OrderStatus::Filled,
        "expected terminal Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "expected exactly ONE terminal, got {}",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "a terminal was lost on a live order");
    check!((order.filled_qty() - 1.0).abs() < 1e-9, "filled_qty {} != 1.0", order.filled_qty());
    Ok(())
}

/// (2) A dead venue path still synthesizes a terminal — no order silently vanishes. Exercised via
/// the EXACT shared seam each venue's `ExecutionClient` uses at submit.
fn scenario_transport_death(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "dead-1";
    match b.exec_kind() {
        ExecKind::CommandActor => {
            // The shared ExecActor — bybit's `BybitExecutionClient(ExecActor)` and okx's
            // `OkxExecutionClient(ExecActor)` forward `submit` straight to this. A dead venue thread
            // (login failure / panic) means the command channel is closed; submit MUST synthesize a
            // terminal OrderRejected rather than let the intent vanish.
            let (events, mut rx) = event_channel(16);
            let mut client = dead_exec_actor(events);
            client.submit(&b.order(coid));

            let ev = recv_ingest_event(&mut rx)?;
            match &ev {
                Event::OrderRejected(r) => {
                    check!(
                        r.client_order_id == coid,
                        "reject coid mismatch: {}",
                        r.client_order_id
                    );
                    check!(!r.reason.as_str().is_empty(), "synthesized reject must carry a reason");
                }
                other => {
                    return Err(format!(
                        "dead ExecActor must synthesize OrderRejected, got {other:?}"
                    ));
                }
            }
            // Folds to a terminal in the real FSM — no vanish.
            let mut order = ContractOrder::new(b.order(coid));
            order.fold(&ev);
            check!(
                order.status() == OrderStatus::Rejected,
                "expected Rejected, got {:?}",
                order.status()
            );
            check!(
                order.terminals_applied == 1,
                "expected exactly ONE terminal, got {}",
                order.terminals_applied
            );
        }
        ExecKind::RestPoll => {
            // The shared post-timeout resolver every REST-poll venue (binance/deribit) reaches for
            // after an E_TIMEOUT_AMBIGUOUS submit. The two no-vanish halves:
            //   venue-confirmed-absent → a TRUE terminal OrderRejected (never a silent vanish);
            //   inconclusive re-query   → an OPTIMISTIC OrderAccepted (never a FALSE terminal that
            //                              would strand a position the venue actually opened).
            let absent = resolve_ambiguous_submit(coid, 1, Ok(None));
            match &absent {
                Event::OrderRejected(r) => {
                    check!(r.client_order_id == coid, "reject coid mismatch: {}", r.client_order_id)
                }
                other => {
                    return Err(format!(
                        "venue-absent submit must resolve to OrderRejected, got {other:?}"
                    ));
                }
            }
            let mut order = ContractOrder::new(b.order(coid));
            order.fold(&absent);
            check!(
                order.status() == OrderStatus::Rejected,
                "expected Rejected, got {:?}",
                order.status()
            );
            check!(
                order.terminals_applied == 1,
                "expected exactly ONE terminal, got {}",
                order.terminals_applied
            );

            let inconclusive = resolve_ambiguous_submit(
                coid,
                1,
                Err(VenueApiError { code: 0, msg: "boom".into() }),
            );
            check!(
                matches!(inconclusive, Event::OrderAccepted(_)),
                "inconclusive re-query must NOT fabricate a false terminal, got {inconclusive:?}"
            );
        }
    }
    Ok(())
}

/// (3) A cancel arriving after the order already closed must be handled without corrupting the
/// single terminal — both a late venue `OrderCanceled` replay AND a late `OrderCancelRejected`.
fn scenario_cancel_after_close(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "cac-1";
    let mut order = ContractOrder::new(b.order(coid));

    // Drive to a terminal Filled first.
    order.fold(&submitted(coid));
    for e in &b.decode(&b.frame_accepted(coid, "v-1")) {
        order.fold(e);
    }
    for e in &b.decode(&b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true)) {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Filled,
        "setup: expected Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "setup: expected one terminal, got {}",
        order.terminals_applied
    );

    // (i) A late venue OrderCanceled replay (a cancel/close race). The FSM rejects Canceled-from-
    // Filled; it is dropped as a benign out-of-order artifact — NOT a lost terminal (status is
    // already terminal) — and the order stays Filled with exactly one terminal.
    let canceled = b.decode(&b.frame_canceled(coid));
    check!(
        canceled.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
        "cancel frame did not decode to OrderCanceled: {canceled:?}"
    );
    for e in &canceled {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Filled,
        "after late cancel expected still Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "late cancel added a second terminal: {}",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "late cancel wrongly counted as a lost terminal");

    // (ii) A late OrderCancelRejected advisory (the shared cancel path's failure event) after close
    // is likewise dropped — a non-terminal advisory on a terminal order — leaving state intact.
    order.fold(&Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: coid.into(),
        reason: "venue unavailable".into(),
        ts: 1,
    }));
    check!(
        order.status() == OrderStatus::Filled,
        "after late cancel-reject expected still Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "cancel-reject disturbed the single terminal: {}",
        order.terminals_applied
    );
    Ok(())
}

/// (4) A mid-order reconnect (through the REAL shared pump) that replays the mid-order state must
/// not duplicate/lose the terminal or double-count a fill.
///
/// [`FillShape::Cumulative`]: session 1 delivers accept + partial(t1) then drops; session 2 REPLAYS
/// accept + partial(t1) (as a resync would) then delivers the full fill(t2) — proving the replayed
/// partial is deduped by `trade_id` (no double-count) and exactly one terminal survives.
/// [`FillShape::Whole`]: a whole-fill venue's terminal fill inherently ENDS the pump (it sets
/// `stop`), so it can never be replayed across a reconnect — the mid-order state is the resting
/// ACCEPTED order. Session 1 delivers accept then drops; session 2 replays accept (the FSM folds the
/// second accept as a benign idempotent drop, not a lost terminal) then delivers the whole fill. It
/// proves the reconnect neither duplicates nor loses the eventual terminal; the fill-dedup guarantee
/// is the Cumulative variant's to make.
fn scenario_reconnect(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "recon-1";
    let mut order = ContractOrder::new(b.order(coid));
    order.fold(&submitted(coid)); // Rust-side submit before the venue stream opens

    let accept = || Ok(StreamMsg::Text(b.frame_accepted(coid, "v-1").to_string()));
    let (s1, s2) = match b.fill_shape() {
        FillShape::Cumulative => {
            let partial = || {
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t1", 0.4, 0.4, 1.0, 50_000.0, false).to_string(),
                ))
            };
            let s1 = Scripted::new(vec![
                accept(),
                partial(),
                Err(StreamError::Closed("mid-order drop".into())),
            ]);
            let s2 = Scripted::new(vec![
                accept(),  // replayed accept
                partial(), // replayed partial — must be deduped, not double-counted
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t2", 0.6, 1.0, 1.0, 50_000.0, true).to_string(),
                )),
            ]);
            (s1, s2)
        }
        FillShape::Whole => {
            // No partial exists to replay: the drop lands after the resting accept, and the whole
            // fill arrives only after the reconnect (it would otherwise stop the pump before it).
            let s1 =
                Scripted::new(vec![accept(), Err(StreamError::Closed("mid-order drop".into()))]);
            let s2 = Scripted::new(vec![
                accept(), // replayed accept — benign idempotent drop in the FSM
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true).to_string(),
                )),
            ]);
            (s1, s2)
        }
    };
    let mut sessions = vec![s2, s1]; // popped from the end → session 1 opens first
    let mut reconnects = 0usize;

    let stop = AtomicBool::new(false);
    let result = run_user_data_forever(
        || match sessions.pop() {
            Some(s) => OpenOutcome::Ready(s),
            None => OpenOutcome::Stopped,
        },
        |frame| b.decode(frame),
        |ev| {
            let terminal = matches!(ev, Event::OrderFilled(_));
            order.fold(&ev);
            if terminal {
                stop.store(true, Ordering::Relaxed); // full fill folded — end the pump cleanly
            }
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2), // tiny backoff so the reconnect sleep is fast
        None,
        || reconnects += 1,
    );

    check!(result.is_ok(), "pump ended with an auth error: {result:?}");
    check!(reconnects == 1, "expected exactly ONE reconnect, got {reconnects}");
    check!(
        order.status() == OrderStatus::Filled,
        "expected terminal Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "reconnect produced {} terminals (want exactly 1)",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "reconnect lost a terminal");
    check!(
        (order.filled_qty() - 1.0).abs() < 1e-9,
        "reconnect miscounted the fill (a replay was double-counted): filled_qty {} != 1.0",
        order.filled_qty()
    );
    Ok(())
}

/// (5) The MONEY side of the Lifecycle frames. The same submit → accept → fill(s) sequence, folded
/// through a REAL `ExecutionEngine` mounted on the symbol the venue's own order names (its `symbol`
/// IS the production mount's spelling — see [`ConformanceBridge::order`]). For a
/// [`AccountLane::FillFrames`] venue every bare `Event::Fill` the frames decode to must fold, so the
/// engine's net position on that symbol equals their signed sum; for a [`AccountLane::Separate`]
/// venue the frames must decode to NO bare fill and the position must stay flat.
///
/// This is the row that sees a LABEL mismatch: the engine folds a fill only when its `symbol` is
/// the mounted string (`vike_exec::ExecutionEngine::accepts_symbol`), while rows 1-4 fold the FSM by
/// client order id and so stay green when every fill is dropped.
fn scenario_position_fold(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "pos-1";
    let order = b.order(coid);
    let mounted = order.symbol.clone();
    let mut eng = ExecutionEngine::new(
        Account::new(1.0, b.venue(), None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        b.venue(),
        &mounted,
    );
    let mut outbox = Outbox::default();
    eng.submit_order(&order, 1, &mut outbox);
    check!(eng.registry.contains_key(coid), "the RiskGate refused the scenario order");

    let mut stream = vec![submitted(coid)];
    stream.extend(b.decode(&b.frame_accepted(coid, "v-1")));
    match b.fill_shape() {
        FillShape::Cumulative => {
            stream.extend(b.decode(&b.frame_fill(coid, "t1", 0.4, 0.4, 1.0, 50_000.0, false)));
            stream.extend(b.decode(&b.frame_fill(coid, "t2", 0.6, 1.0, 1.0, 50_000.0, true)));
        }
        FillShape::Whole => {
            stream.extend(b.decode(&b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true)));
        }
    }
    for ev in &stream {
        eng.on_event(ev, &mut outbox);
    }

    let bare: Vec<&FillEvent> = stream
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    let held: f64 = eng
        .local_view()
        .positions
        .iter()
        .filter(|((sym, _), _)| *sym == mounted)
        .map(|(_, qty)| *qty)
        .sum();
    match b.account_lane() {
        AccountLane::FillFrames => {
            check!(
                !bare.is_empty(),
                "the fill frames decoded to no bare Fill — if this venue books positions on another \
                 lane, declare AccountLane::Separate with the reason"
            );
            let want: f64 = bare.iter().map(|f| f64::from(f.side) * f.last_qty).sum();
            let fill_labels: Vec<&str> = bare.iter().map(|f| f.symbol.as_str()).collect();
            check!(
                want.abs() > 1e-9,
                "the bare fills net to zero, so the check cannot fail: {fill_labels:?}"
            );
            check!(
                (held - want).abs() < 1e-9,
                "the engine mounted on {mounted:?} holds {held} after bare fills labelled \
                 {fill_labels:?} netting {want} (order {:?}) — a fill was DROPPED from the position \
                 bookkeeping",
                eng.registry.get(coid).map(|mo| mo.status)
            );
        }
        AccountLane::Separate(lane) => {
            check!(
                bare.is_empty(),
                "declared AccountLane::Separate({lane:?}) but the lifecycle frames decoded {} bare \
                 fill(s) — reclassify the venue",
                bare.len()
            );
            check!(held.abs() < 1e-9, "no bare fill was decoded, yet the position moved to {held}");
        }
    }
    Ok(())
}

// ===================================================================================================
// Shared test scaffolding.
// ===================================================================================================

/// Spawn an `ExecActor` whose venue thread dies immediately (drops the command receiver), and block
/// until that receiver is provably gone — so the subsequent `submit` deterministically lands on a
/// closed channel. Mirrors `exec_actor_dead_thread.rs`.
fn dead_exec_actor(events: EventSender) -> ExecActor {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let actor = ExecActor::spawn(
        "dead-venue",
        events,
        move |cmd_rx: std::sync::mpsc::Receiver<ExecCommand>| {
            drop(cmd_rx); // venue login failed → the command receiver is gone
            let _ = done_tx.send(());
        },
    );
    done_rx.recv().expect("dead-venue thread signalled");
    actor
}

/// Block (bounded) for the next `Event` on the ingest lane. Uses a private current-thread runtime so
/// the harness needs no `#[tokio::test]`; mirrors the model tests' `recv_event`.
fn recv_ingest_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Result<Event, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("runtime build: {e}"))?;
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .map_err(|_| "timed out waiting for an ingest event".to_string())?
        .ok_or_else(|| "ingest channel closed with no event".to_string())?;
    match ingest {
        Ingest::Event(ev) => Ok(ev),
        other => Err(format!("expected Ingest::Event, got {other:?}")),
    }
}
