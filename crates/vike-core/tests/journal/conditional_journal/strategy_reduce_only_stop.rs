//! A mounted strategy's REDUCE-ONLY STOP (`LiveBroker::submit_stop_reduce`) on a venue whose row refuses it
//! (polymarket) is held by the core, and the journal carries it with NO new key and NO format step: the
//! strategy's intent is an ordinary `StrategySubmit` of a stop-kind, `reduce_only` `Submit` (fields
//! `OrderRequest` always had), and the arm is the existing `ConditionalArmed` with the existing
//! `reduce_only` flag. The session replays to a bit-identical fence, the exit `Snap` restores the arm WITH
//! its flag, and a journal of the strategy's PLAIN `submit_stop` (what every older build wrote) restores a
//! plain arm exactly as before.

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{LiveBroker, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{BarUpdate, OrderIntent};
use vike_journal::JournalRecord;
use vike_marketdata::Bar;
use vike_model::events::Event;
use vike_model::{OrderType, Strategy};

use super::support::{core_config, unique_dir};
use crate::kit::engines::engine_on;
use crate::kit::events::{ohlc_bar, sim_bare_fill};
use crate::kit::journal::records;
use crate::kit::mounts::mount_of;

const VENUE: &str = "polymarket";

/// Arms ONE stop on its first bar: reduce-only (the verb under test) or plain (the older verb).
struct ArmOnce {
    reduce_only: bool,
    done: bool,
}

impl Strategy<LiveBroker> for ArmOnce {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if self.done {
            return;
        }
        self.done = true;
        if self.reduce_only {
            broker.submit_stop_reduce(-1, 5.0, 95.0);
        } else {
            broker.submit_stop(-1, 5.0, 95.0);
        }
    }
}

/// A bare fill on polymarket: +1 at 100.
fn fill(tid: &str) -> Event {
    let mut ev = sim_bare_fill(tid, 1.0, 100.0);
    if let Event::Fill(f) = &mut ev {
        f.venue = VENUE.into();
    }
    ev
}

/// Four warm-up fills (the 4th trips the cadence `Snap`, the replay BASE) leave +4; the mounted strategy
/// then arms its stop on the first closed bar.
fn session(tag: &str, reduce_only: bool) -> crate::scratch::Scratch {
    let dir = unique_dir(tag);
    let mount = mount_of(VENUE, "BTCUSDT", "1m", Box::new(ArmOnce { reduce_only, done: false }));
    let cfg = vike_core::CoreConfig {
        strategy: Some(mount),
        conditionals_on_ticks: false,
        ..core_config(&dir)
    };
    let handle = spawn_core(engine_on(VENUE, "BTCUSDT", RecordingClient::default()), cfg);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(tid)).unwrap();
    }
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: VENUE.into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: ohlc_bar(60_000, 100.0, 101.0, 99.0, 100.0),
        })
        .unwrap();
    handle.shutdown_and_join();
    dir
}

/// **THE NEW SHAPE.** The journal holds the strategy's stop as a `StrategySubmit` of a reduce-only stop
/// `Submit` and, because the core held it, a `ConditionalArmed` carrying the flag and NO `MintedSubmit`;
/// the session replays (the arm is a function of the journaled submit) and the exit `Snap` restores the arm
/// with its flag.
#[test]
fn a_mounts_core_held_reduce_only_stop_is_journaled_replays_and_restores_with_its_flag() {
    let dir = session("strategy-reduce-only", true);

    let recs = records(&dir);
    let submits: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::StrategySubmit { intent: OrderIntent::Submit(req), .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(submits.len(), 1, "the strategy's one order intent");
    assert_eq!(
        (&submits[0].order_type, submits[0].trigger_price, submits[0].reduce_only, submits[0].side),
        (&OrderType::Stop, Some(95.0), true, -1),
        "a reduce-only stop Submit, in fields OrderRequest always had"
    );
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1, "held by the core, once");
    assert!(armed[0].reduce_only, "…with the existing flag");
    assert_eq!((armed[0].side, armed[0].qty, armed[0].price), (-1, 5.0, Some(95.0)));
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::MintedSubmit { .. })),
        "nothing was minted: it was held, not refused"
    );

    replay_offline(&dir)
        .expect("the session replays: the arm is a function of the journaled submit");

    let state = restore_from_journal(&dir).unwrap().expect("the exit Snap");
    assert_eq!(state.conditionals.len(), 1, "the arm is in the restored books");
    assert!(state.conditionals[0].terms.reduce_only, "…with its flag: the Snap carries it");
}

/// **THE OLDER SHAPE IS UNCHANGED.** A journal of the strategy's PLAIN `submit_stop` (an `ArmConditional`
/// intent and a `ConditionalArmed` without the flag, which is what every build before the verb wrote)
/// replays and restores a PLAIN arm: the flag is absent from the record, never written when false.
#[test]
fn an_older_journal_of_a_plain_strategy_stop_replays_and_restores_a_plain_arm() {
    let dir = session("strategy-plain-stop", false);

    let recs = records(&dir);
    assert!(
        recs.iter().any(|r| matches!(
            r,
            JournalRecord::StrategySubmit { intent: OrderIntent::ArmConditional(_), .. }
        )),
        "the plain verb is an ArmConditional intent"
    );
    // The wire form never carries a false flag: the key an older reader knows nothing of is absent.
    let armed = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .expect("the plain stop was armed");
    assert!(!armed.reduce_only);
    assert!(
        !serde_json::to_string(armed).unwrap().contains("reduce_only"),
        "a false flag is never written"
    );

    replay_offline(&dir).expect("an older-shaped session replays");
    let state = restore_from_journal(&dir).unwrap().expect("the exit Snap");
    assert_eq!(state.conditionals.len(), 1);
    assert!(!state.conditionals[0].terms.reduce_only, "restored plain, as before");
}
