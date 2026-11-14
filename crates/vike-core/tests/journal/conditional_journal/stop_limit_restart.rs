//! An operator's CORE-HELD reduce-only STOP-LIMIT through the journal (wave 3 of the order kinds):
//! armed under `kind_terms` (the legacy `price`/`trail`/`extreme` empty, so an older build skips it and
//! never arms a plain stop), replayed to the fence, restored AS a stop-limit, HELD after the restart until
//! its account's first reconcile pass, and then fired as a reduce-only LIMIT at its limit, capped to the
//! restored position: never a market.

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, spawn_core};
use vike_execution::recon::{ReconcileBalanceTolerance, ReconcilePolicy};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Command, Engine, OrderIntent, ReconcileReports};
use vike_journal::JournalRecord;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

use super::support::{core_config, unique_dir};
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::journal::records;

const VENUE: &str = "polymarket";

/// A bare polymarket fill: +1 at 100.
fn fill(tid: &str) -> Event {
    let mut event = sim_bare_fill(tid, 1.0, 100.0);
    if let Event::Fill(f) = &mut event {
        f.venue = VENUE.into();
    }
    event
}

fn quote(px: f64, ts: i64) -> vike_execution::QuoteUpdate {
    let mut q = sim_quote(px, ts);
    q.venue = VENUE.into();
    q
}

/// The profile's tick lane is OFF: the core-held arm turns it on for itself, through the restart too.
fn config(dir: &std::path::Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

/// The first (empty) reconcile pass of polymarket's one account: all the hold asks is that it LANDED.
fn first_pass() -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconcilePolicy::default(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

/// **ARMED, JOURNALED, REPLAYED, RESTORED, RE-HELD, FIRED AS A LIMIT.** Run 1: four fills (+4; the 4th
/// trips the cadence `Snapshot`), then `sell 5 --type stop_limit --trigger 95 --price 94 --reduce-only` on
/// polymarket, whose row has no stop-limit: it is HELD (a `OrderEmulationArmed` under `kind_terms`, nothing
/// minted) and the session replays. Run 2 restores the arm as a stop-limit and is crossed with NO pass:
/// nothing fires and the exit `Snapshot` still carries it. Run 3 hears the first pass and the same crossing:
/// it fires ONCE, under its own id, a reduce-only LIMIT at 94 for the 4 the book holds.
#[test]
fn a_restored_core_held_stop_limit_is_held_then_fires_as_a_capped_limit() {
    let dir = unique_dir("core-held-stop-limit");
    let handle = spawn_core(engine_on(VENUE, "BTCUSDT", RecordingClient::default()), config(&dir));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(tid)).unwrap();
    }
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        venue: VENUE.into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 5.0,
        order_type: OrderType::StopLimit,
        price: Some(94.0),
        trigger_price: Some(95.0),
        reduce_only: true,
        ..Default::default()
    }))));
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::OrderEmulationArmed { arm_id, resolved, .. } => Some((arm_id, resolved)),
            _ => None,
        })
        .collect();
    let [(arm_id, resolved)] = armed.as_slice() else { panic!("armed once: {armed:?}") };
    assert!(resolved.reduce_only);
    assert_eq!(
        (resolved.price, resolved.trail, resolved.extreme),
        (None, None, None),
        "no legacy key"
    );
    let terms = resolved.kind_terms.as_ref().expect("the stop-limit's terms");
    assert_eq!(
        (&terms.kind, terms.price, terms.limit),
        (&OrderType::StopLimit, Some(95.0), Some(94.0))
    );
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::MintedSubmit { .. })),
        "held, not sent"
    );
    replay_offline(&dir)
        .expect("the session replays: the arm is a function of the journaled submit");

    let restart = |drive: &dyn Fn(&vike_core::CoreHandle)| {
        let state = restore_from_journal(&dir).unwrap().expect("the exit Snapshot");
        let config_value = CoreConfig {
            seed_cash: state.engines[0].equity_seed,
            order_client_id_session: Some((
                state.order_client_id_session.clone(),
                state.order_client_id_sequence,
            )),
            arm_sequence: state.arm_sequence,
            order_emulation_arms: state.order_emulation_arms.clone(),
            ..config(&dir)
        };
        let engine = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
        let handle = spawn_core(engine, config_value);
        drive(&handle);
        handle.shutdown_and_join();
    };
    let fires = || -> Vec<(String, OrderRequest)> {
        records(&dir)
            .into_iter()
            .filter_map(|r| match r {
                JournalRecord::OrderEmulationFire { arm_id, request: req, .. } => {
                    Some((arm_id, req))
                }
                _ => None,
            })
            .collect()
    };

    restart(&|h| h.tick_sender().quote(quote(94.5, 31)).unwrap());
    assert!(fires().is_empty(), "crossed before the account's first pass: HELD, nothing fired");
    let state = restore_from_journal(&dir).unwrap().expect("the exit Snapshot");
    let [snap] = state.order_emulation_arms.as_slice() else {
        panic!("{:?}", state.order_emulation_arms)
    };
    assert!(snap.terms.reduce_only, "still armed, with its flag");
    assert_eq!(
        snap.terms.kind_terms.as_ref().map(|t| (&t.kind, t.limit)),
        Some((&OrderType::StopLimit, Some(94.0))),
        "…as a stop-limit, not a plain stop"
    );

    restart(&|h| {
        h.send_command(first_pass());
        h.tick_sender().quote(quote(96.0, 32)).unwrap(); // above: nothing
        h.tick_sender().quote(quote(94.5, 33)).unwrap(); // crosses 95
    });
    let fired = fires();
    let [(fired_id, req)] = fired.as_slice() else { panic!("fired once: {fired:?}") };
    assert_eq!(fired_id, *arm_id, "under its original id");
    assert_eq!(
        (req.side, req.qty, req.reduce_only, &req.order_type, req.price),
        (-1, 4.0, true, &OrderType::Limit, Some(94.0)),
        "a reduce-only LIMIT at the arm's limit, capped to the restored +4"
    );
}
