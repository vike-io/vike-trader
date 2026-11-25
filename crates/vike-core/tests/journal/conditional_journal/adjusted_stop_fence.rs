//! An operator's CORE-HELD reduce-only ADJUSTABLE STOP through the determinism fence (decision 0267,
//! the attributes wave): its adjustment is applied by the live book on a price that reaches its level,
//! market data that is never journaled, so the exit `Snapshot` holds the target while the refold still holds
//! the adjustable stop. `vike_core::replay::conditionals_hash` reads both as the target, so the valid
//! session replays green, and a `Snapshot` whose target was tampered with still fails the book fence.

use vike_core::replay::{JournalReplayError, replay_offline};
use vike_core::{CoreConfig, spawn_core};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Command, OrderIntent};
use vike_journal::JournalRecord;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStopAdjust, OrderStopAdjustTo, OrderType};

use super::support::{core_config, unique_dir};
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::journal::{records, rewrite_journal};

/// Polymarket: its row has no stop, so the core holds a reduce-only one, its adjustment included.
const VENUE: &str = "polymarket";

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

fn config(dir: &std::path::Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

/// **A stop that adjusted between `Snapshot`s replays green; a tampered target does not.** Four fills (+4; the
/// 4th trips the cadence `Snapshot`, the replay base), then `sell 4 --type stop --trigger 95 --reduce-only`
/// adjusting to a stop at 101 once the price reaches 105: held by the core. A quote at 107 adjusts it in
/// the live book and one at 103 does not fire it, so the exit `Snapshot` holds the stop at 101 with no
/// adjustment while the `OrderEmulationArmed` record holds the adjustable stop at 95.
#[test]
fn a_stop_that_adjusted_between_snaps_passes_the_fence_and_a_tampered_target_fails_it() {
    let dir = unique_dir("adjusted-stop-fence");
    let handle = spawn_core(engine_on(VENUE, "BTCUSDT", RecordingClient::default()), config(&dir));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(tid)).unwrap();
    }
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        venue: VENUE.into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 4.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        reduce_only: true,
        adjust: Some(OrderStopAdjust { at: 105.0, to: OrderStopAdjustTo::Stop { stop: 101.0 } }),
        ..Default::default()
    }))));
    handle.tick_sender().quote(quote(107.0, 30)).unwrap();
    handle.tick_sender().quote(quote(103.0, 31)).unwrap();
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::OrderEmulationArmed { resolved, .. } => resolved.kind_terms.clone(),
            _ => None,
        })
        .expect("held under the adjustable stop's kind terms");
    assert_eq!(armed.adjust.map(|a| (a.price, a.at)), Some((Some(95.0), 105.0)), "{armed:?}");
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::OrderEmulationFire { .. })),
        "103 is above the adjusted stop: nothing fired"
    );
    let last_snap = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("a clean shutdown writes an exit Snapshot");
    let JournalRecord::Snapshot { order_emulation_arms: conditionals, .. } = &recs[last_snap]
    else {
        unreachable!()
    };
    let [held] = conditionals.as_slice() else { panic!("one arm: {conditionals:?}") };
    assert_eq!(
        (held.terms.price, held.terms.kind_terms.is_none()),
        (Some(101.0), true),
        "the live book adjusted it: a plain stop at 101 now"
    );
    replay_offline(&dir).expect("an adjustment between Snapshots is market data, not a divergence");

    let mut tampered = recs;
    if let JournalRecord::Snapshot { order_emulation_arms: conditionals, .. } =
        &mut tampered[last_snap]
    {
        conditionals[0].terms.price = Some(102.0);
    }
    let doctored = unique_dir("adjusted-stop-fence-doctored");
    rewrite_journal(&doctored, tampered);
    match replay_offline(&doctored) {
        Err(JournalReplayError::RestoreMismatch { field, .. }) => {
            assert_eq!(field, "order_emulation_arms")
        }
        other => panic!("a target the session never had must fail the book fence: {other:?}"),
    }
}
