//! A cancel-then-new amend through a REAL core with a journal (decision record on the unified amend): it
//! needs NO record kind of its own. The Modify command is journaled as it always was, the replacement is
//! journaled by the standard minted-submit record (and its lifecycle is the venue's own events), the
//! segment is not raised above the base format, and the replay fence holds: the journal alone re-derives the
//! state the live run held, the replacement included, wherever the replay's own batches end.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Command, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderSubmitted, PositionSide, TradeId,
};
use vike_model::{Clock, OrderRequest, OrderStatus};

use crate::kit::events::ohlc_bar;
use crate::kit::handle::{CoreJoinGuard, close_binance_bar, wait_for_snapshot};
use crate::kit::journal::{records, rewrite_journal};
use crate::kit::requests::{binance_perp_reduce_only_stop, limit_on};
use vike_core::LiveBroker;
use vike_core::Mount;
use vike_core::replay::restore_from_journal;
use vike_core::testkit::mount_of;
use vike_execution::testing::engine_on;
use vike_execution::{BarUpdate, EngineSnapshot};
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_model::orders::client_id::{
    OrderClientIdReading, OrderClientIdSource, OrderClientIdStrategyCode, parse_order_client_id,
};

fn config(dir: &Path) -> CoreConfig {
    // Message `k` dispatches at `now_ms == k`: no wall-clock wait, a distinct stamp per record.
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            // the 4th journaled message trips a cadence `Snapshot`: the replay's BASE
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}

/// A bare external buy on the binance perpetual: opens the long the reduce-only stop below protects.
fn bare_buy(tid: &str) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("non-empty"),
        order_client_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

fn answered(order_client_id: &str) -> [Event; 2] {
    [
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.into(),
            timestamp: 1,
        }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.into(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 1,
        }),
    ]
}

#[test]
fn a_new_first_amend_is_journaled_by_the_existing_records_and_the_replay_fence_holds() {
    let dir = Scratch::reserved("amend-journal-new-first");
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        config(&dir),
    ));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    for tid in ["t0", "t1", "t2"] {
        events.blocking_send(bare_buy(tid)).unwrap();
    }
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(
        binance_perp_reduce_only_stop("s1", 90.0),
    ))));
    for event in answered("s1") {
        events.blocking_send(event).unwrap();
    }
    wait_for_snapshot(&cell, 5, "s1 resting", |s| {
        s.orders.iter().any(|o| o.order_client_id == "s1" && o.status == OrderStatus::Accepted)
    });

    handle.send_command(Command::Order(OrderIntent::Modify {
        order_client_id: "s1".into(),
        new_qty: None,
        new_price: Some(91.0),
        new_trigger_price: None,
    }));
    wait_for_snapshot(&cell, 5, "the replacement is in the book", |s| {
        s.orders.iter().any(|o| o.order_client_id != "s1")
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.order_client_id != "s1")
        .map(|o| o.order_client_id.clone())
        .expect("a replacement");
    for event in answered(&new) {
        events.blocking_send(event).unwrap();
    }
    events
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            order_client_id: "s1".into(),
            reason: "venue".to_string().into(),
            timestamp: 2,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == "s1" && o.status == OrderStatus::Canceled)
            && s.orders
                .iter()
                .any(|o| o.order_client_id == new && o.status == OrderStatus::Accepted)
    });
    handle.shutdown_and_join();

    // the replacement is journaled by the standard minted-submit record, with its new trigger
    let recs = Journal::read_all(&dir).unwrap();
    let minted: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MintedSubmit { request: req, .. } => Some(req.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(minted.len(), 1, "one minted submit: the replacement ({minted:?})");
    assert_eq!(minted[0].order_client_id, new);
    assert_eq!(minted[0].trigger_price, Some(91.0));
    // ...the Modify command is a journaled command, as it always was...
    assert!(
        recs.iter().any(|r| matches!(
            r,
            JournalRecord::Command {
                message: vike_execution::Ingest::Command(Command::Order(
                    OrderIntent::Modify { .. }
                )),
                ..
            }
        )),
        "the Modify is journaled"
    );
    // ...nothing raised the segment above the base format...
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(30));
    // ...and the journal ALONE re-derives the state the live run held.
    let out = replay_offline(&dir).expect("the replay fence holds");
    assert!(out.records > 0 && out.snaps_compared >= 1);
    let status = |order_client_id: &str| {
        out.engines[0].registry.iter().find(|(c, _)| c == order_client_id).map(|(_, mo)| mo.status)
    };
    assert_eq!(status("s1"), Some(OrderStatus::Canceled));
    assert_eq!(status(&new), Some(OrderStatus::Accepted));
}

/// Rests a reduce-only stop under the tag `stop` on its first bar and moves it to 91 on its second.
struct StopThenMove {
    bars: u32,
}

impl Strategy<LiveBroker> for StopThenMove {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        self.bars += 1;
        match self.bars {
            1 => {
                broker.submit_stop_reduce(-1, 1.0, 90.0);
                // the verb files no tag and a strategy modifies by tag: name the order it just buffered
                broker.submissions.last_mut().expect("the stop").tag = Some("stop".into());
            }
            2 => broker.modify("stop", None, Some(91.0)),
            _ => {}
        }
    }
}

/// `(order client id, status)` of the first engine's registry, sorted.
fn orders(engines: &[EngineSnapshot]) -> Vec<(String, OrderStatus)> {
    let mut v: Vec<_> = engines[0].registry.iter().map(|(c, mo)| (c.clone(), mo.status)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// **A MOUNT'S amend replays to ONE replacement.** A mounted strategy moves its reduce-only stop on binance's
/// perpetual (cancel-then-new, new first). The replay re-derives the replacement from the journaled Modify, as
/// it does an operator's; a re-applied record of the replacement beside it would place it a second time.
#[test]
fn a_mounts_new_first_amend_replays_and_restores_one_replacement() {
    let dir = Scratch::reserved("amend-journal-mount");
    let mount = mount_of("binance", "BTCUSDT.P", "1m", Box::new(StopThenMove { bars: 0 }));
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("binance", "BTCUSDT.P", RecordingClient::default()),
        CoreConfig { strategy: Some(mount), ..config(&dir) },
    ));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    // the 4th buy trips the cadence `Snapshot`, the replay's BASE, before the strategy acts
    for tid in ["t0", "t1", "t2", "t3"] {
        events.blocking_send(bare_buy(tid)).unwrap();
    }
    wait_for_snapshot(&cell, 5, "the long of 4", |s| {
        s.portfolio.venues.iter().flat_map(|v| v.positions.iter()).any(|p| p.size == 4.0)
    });
    close_binance_bar(&handle, "BTCUSDT.P", ohlc_bar(60_000, 100.0, 101.0, 99.0, 100.0));
    wait_for_snapshot(&cell, 5, "the strategy's stop", |s| !s.orders.is_empty());
    let old = cell.load_full().orders[0].order_client_id.clone();
    for event in answered(&old) {
        events.blocking_send(event).unwrap();
    }
    wait_for_snapshot(&cell, 5, "the stop rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == old && o.status == OrderStatus::Accepted)
    });

    close_binance_bar(&handle, "BTCUSDT.P", ohlc_bar(120_000, 100.0, 101.0, 99.0, 100.0));
    wait_for_snapshot(&cell, 5, "the replacement is in the book", |s| {
        s.orders.iter().any(|o| o.order_client_id != old)
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.order_client_id != old)
        .map(|o| o.order_client_id.clone())
        .expect("a replacement");
    for event in answered(&new) {
        events.blocking_send(event).unwrap();
    }
    events
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            order_client_id: old.clone(),
            reason: "venue".to_string().into(),
            timestamp: 2,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == old && o.status == OrderStatus::Canceled)
            && s.orders
                .iter()
                .any(|o| o.order_client_id == new && o.status == OrderStatus::Accepted)
    });
    handle.shutdown_and_join();
    assert_one_replacement(&dir, &old, &new);
}

/// The live run at `dir` ended on `old` Canceled and its one replacement `new` Accepted. A crash refold of
/// the same records (the first `Snapshot` the only one, so the restore refolds the amend) and the replay fence
/// reach the same registry, `order_client_id_sequence` and state hash, and the restore gives `new` the owner of `old`.
fn assert_one_replacement(dir: &Path, old: &str, new: &str) {
    let recs = records(dir);
    let (live, live_sequence, live_hash) = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot { engines, order_client_id_sequence, hash, .. } => {
                Some((orders(engines), *order_client_id_sequence, *hash))
            }
            _ => None,
        })
        .expect("the exit Snapshot");
    let mut want =
        vec![(old.to_string(), OrderStatus::Canceled), (new.to_string(), OrderStatus::Accepted)];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(live, want, "PREMISE: the live run ends on the old order and its one replacement");

    let crash = Scratch::reserved("amend-journal-crash");
    let mut snaps = 0;
    rewrite_journal(
        &crash,
        recs.into_iter()
            .filter(|r| {
                !matches!(r, JournalRecord::Snapshot { .. }) || {
                    snaps += 1;
                    snaps == 1
                }
            })
            .collect(),
    );
    let restored = restore_from_journal(&crash).unwrap().expect("a Snapshot");
    assert_eq!(orders(&restored.engines), live, "the refold places the replacement ONCE");
    assert_eq!(
        restored.order_client_id_sequence, live_sequence,
        "and mints no order client id the live run did not"
    );
    let owner = |c: &str| {
        restored.order_client_id_mounts.iter().find(|(x, _)| x == c).map(|(_, m)| m.clone())
    };
    assert!(owner(old).is_some(), "PREMISE: the strategy's order is the strategy's");
    assert_eq!(owner(new), owner(old), "the replacement keeps its owner with no record of its own");

    let out = replay_offline(dir).expect("the replay fence holds");
    assert_eq!(orders(&out.engines), live);
    assert_eq!(out.final_hash, live_hash);
}

/// Rests a limit buy under the tag `q` on its first bar and moves it to 0.45 on its second.
struct LimitThenMove {
    bars: u32,
}

impl Strategy<LiveBroker> for LimitThenMove {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        self.bars += 1;
        match self.bars {
            1 => broker.submit_limit_tagged("q", 1, 2.0, 0.40),
            2 => broker.modify("q", None, Some(0.45)),
            _ => {}
        }
    }
}

/// A buy fill on polymarket's `TOKEN` at 0.40.
fn polymarket_buy(tid: &str) -> Event {
    let mut event = bare_buy(tid);
    if let Event::Fill(f) = &mut event {
        f.venue = "polymarket".into();
        f.symbol = "TOKEN".into();
        f.last_price = 0.40;
        f.mark_price = Some(0.40);
    }
    event
}

/// Who rests polymarket's limit and moves it.
#[derive(Clone, Copy, Debug)]
enum Mover {
    /// A mounted strategy ([`LimitThenMove`]): its Modify is its `StrategySubmit`.
    Mount,
    /// The operator: a `Command::Order` submit and a `Modify` of the order client id it was given.
    Operator,
}

/// A live run on polymarket's `TOKEN` under the order client id `session`. Polymarket has no amend, so the amend is
/// CANCEL-FIRST: the replacement is placed at the drain-loop boundary once the venue says the old order is
/// gone. A long of 4 (the 4th buy trips the cadence `Snapshot`, the refolds' BASE, before anything else), a buy
/// limit of 2 at 0.40 that `mover` rests and moves to 0.45, the venue's cancel, the replacement, its
/// answer. `filler` repeats of a folded buy (journaled, and the account drops each as a duplicate, so no
/// `Snapshot` grows) ride between the Modify and the venue's cancel, and again after the replacement rests.
/// Returns `(old, new)`.
fn cancel_first_run(dir: &Path, session: &str, mover: Mover, filler: usize) -> (String, String) {
    let strategy = matches!(mover, Mover::Mount)
        .then(|| mount_of("polymarket", "TOKEN", "1m", Box::new(LimitThenMove { bars: 0 })));
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("polymarket", "TOKEN", RecordingClient::default()),
        CoreConfig { strategy, order_client_id_session: Some((session.into(), 0)), ..config(dir) },
    ));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    let bar = |ts: i64| {
        handle
            .bar_sender()
            .close(BarUpdate {
                venue: "polymarket".into(),
                symbol: "TOKEN".into(),
                interval: "1m".into(),
                bar: ohlc_bar(ts, 0.40, 0.41, 0.39, 0.40),
            })
            .unwrap();
    };
    let fill = || {
        for _ in 0..filler {
            events.blocking_send(polymarket_buy("t3")).unwrap();
        }
    };
    for tid in ["t0", "t1", "t2", "t3"] {
        events.blocking_send(polymarket_buy(tid)).unwrap();
    }
    wait_for_snapshot(&cell, 5, "the long of 4", |s| {
        s.portfolio.venues.iter().flat_map(|v| v.positions.iter()).any(|p| p.size == 4.0)
    });
    match mover {
        Mover::Mount => bar(60_000),
        Mover::Operator => {
            handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
                symbol: "TOKEN".into(),
                ..limit_on("polymarket", "", 1, 2.0, 0.40)
            }))))
        }
    }
    wait_for_snapshot(&cell, 5, "the limit", |s| !s.orders.is_empty());
    let old = cell.load_full().orders[0].order_client_id.clone();
    for event in answered(&old) {
        events.blocking_send(event).unwrap();
    }
    wait_for_snapshot(&cell, 5, "the limit rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == old && o.status == OrderStatus::Accepted)
    });

    match mover {
        Mover::Mount => bar(120_000),
        Mover::Operator => handle.send_command(Command::Order(OrderIntent::Modify {
            order_client_id: old.clone(),
            new_qty: None,
            new_price: Some(0.45),
            new_trigger_price: None,
        })),
    }
    wait_for_snapshot(&cell, 5, "the amend began: the old limit is being cancelled", |s| {
        s.recent_events.iter().any(|l| l.contains("AMEND"))
    });
    fill();
    events
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            order_client_id: old.clone(),
            reason: "venue".to_string().into(),
            timestamp: 2,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 30, "the replacement is placed once the old one is gone", |s| {
        s.orders.iter().any(|o| o.order_client_id != old)
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.order_client_id != old)
        .map(|o| o.order_client_id.clone())
        .expect("a replacement");
    for event in answered(&new) {
        events.blocking_send(event).unwrap();
    }
    wait_for_snapshot(&cell, 5, "the new limit rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == new && o.status == OrderStatus::Accepted)
    });
    fill();
    handle.shutdown_and_join();
    (old, new)
}

/// **A MOUNT'S CANCEL-FIRST amend refolds to the live run.** The replacement's one record is its
/// `MintedSubmit` (naming the order it `replaces`, no `StrategySubmit`): the refold places it there, once,
/// under the same order client id, and the restore gives it the strategy as its owner.
#[test]
fn a_mounts_cancel_first_amend_replays_and_restores_one_replacement() {
    let dir = Scratch::reserved("amend-journal-mount-cancel-first");
    let (old, new) = cancel_first_run(&dir, "cafef00d", Mover::Mount, 0);
    assert_one_replacement(&dir, &old, &new);
}

/// The live run at `dir` (its exit `Snapshot`) against both refolds of its records, the replay fence from its
/// first `Snapshot` and a crash restore of the records with only that `Snapshot` kept: the WHOLE first registry
/// (every order's status, terms, stamps and amend lineage), the `order_client_id_sequence` and the state hash. Returns the
/// live registry.
fn assert_refolds_as_live(dir: &Path, case: &str) -> Vec<(String, vike_model::OrderRecord)> {
    let recs = records(dir);
    let (live, live_sequence, live_hash) = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot { engines, order_client_id_sequence, hash, .. } => {
                Some((engines[0].registry.clone(), *order_client_id_sequence, *hash))
            }
            _ => None,
        })
        .expect("the exit Snapshot");

    let out = replay_offline(dir).unwrap_or_else(|e| panic!("{case}: the replay fence: {e:?}"));
    assert_eq!(out.engines[0].registry, live, "{case}: the replay's registry");
    assert_eq!(
        out.order_client_id_sequence, live_sequence,
        "{case}: the replay's order_client_id_sequence"
    );
    assert_eq!(out.final_hash, live_hash, "{case}: the replay's state hash");

    let crash = Scratch::reserved("amend-journal-crash");
    let mut snaps = 0;
    rewrite_journal(
        &crash,
        recs.into_iter()
            .filter(|r| {
                !matches!(r, JournalRecord::Snapshot { .. }) || {
                    snaps += 1;
                    snaps == 1
                }
            })
            .collect(),
    );
    let restored = restore_from_journal(&crash).unwrap().expect("a Snapshot");
    assert_eq!(restored.engines[0].registry, live, "{case}: the restore's registry");
    assert_eq!(
        restored.order_client_id_sequence, live_sequence,
        "{case}: the restore's order_client_id_sequence"
    );
    assert_eq!(
        vike_execution::state_hash(&restored.engines),
        live_hash,
        "{case}: the restore's hash"
    );
    live
}

/// **A cancel-first amend refolds as the live run whatever the REPLAY core's batches are.** A replay core
/// never places a replacement at its own drain-loop boundary and reads no clock there; the journal's
/// `MintedSubmit` naming what it `replaces` places it, at that record's stamp. `FILLER` (more than the
/// replay core's `batch_max` of 1024) FORCES a replay boundary between the Modify and the venue's cancel
/// (where a clock read shifted every later stamp) and another after the replacement rests (where the
/// replay core placed it again, or placed it only after its answer had been folded). For a mount's amend
/// and an operator's, under a v1 and a v2 order client id session, three times each (the four cases of a round side
/// by side: each has its own cores and directory).
#[test]
fn a_cancel_first_amend_refolds_as_live_across_replay_batch_boundaries() {
    const FILLER: usize = 1_100; // > `CoreConfig::batch_max` (1024)
    for rep in 0..3 {
        std::thread::scope(|s| {
            for session in ["cafef00d", "Wk3x9q2m7d1p4"] {
                for mover in [Mover::Mount, Mover::Operator] {
                    s.spawn(move || {
                        let case = format!("rep {rep}, session {session}, {mover:?}");
                        let dir = Scratch::reserved("amend-journal-cancel-first-boundaries");
                        let (old, new) = cancel_first_run(&dir, session, mover, FILLER);
                        let live = assert_refolds_as_live(&dir, &case);
                        let status =
                            |c: &str| live.iter().find(|(x, _)| x == c).map(|(_, mo)| mo.status);
                        assert_eq!(
                            (live.len(), status(&old), status(&new)),
                            (2, Some(OrderStatus::Canceled), Some(OrderStatus::Accepted)),
                            "{case}: PREMISE: the old limit and its one replacement"
                        );
                    });
                }
            }
        });
    }
}

/// **A mount whose only records after the base `Snapshot` are its Modify mints its replacement as live, past
/// the `R` cap too.** A v2 session on a binance perpetual whose 10-character broker code leaves the id no
/// room for `R` (`order_client_id_shape_for`'s fit): every replacement is a FRESH mint, labelled by its owner. The
/// refolds start from a `Snapshot` taken after the strategy's stop rests, every record before it gone (as a prune
/// leaves the journal), so the mount's only records after it are its Modify (an order client id, no venue) and the
/// venue's answers. The replay core holds the mount that `Snapshot` names, owns its stop, and mints the
/// replacement `S` + the mount's code, byte for byte as live (an engine's `E` + `0000` without it).
#[test]
fn a_mount_named_only_by_the_snap_mints_its_replacement_past_the_r_cap_as_live() {
    let dir = Scratch::reserved("amend-journal-snap-only-mount");
    let code = OrderClientIdStrategyCode::parse("00A3");
    let mount = Mount {
        order_client_id_code: code,
        ..mount_of("binance", "BTCUSDT.P", "1m", Box::new(StopThenMove { bars: 0 }))
    };
    let mut engine = engine_on("binance", "BTCUSDT.P", RecordingClient::default());
    engine.broker_code_len = 10;
    let handle = CoreJoinGuard::new(spawn_core(
        engine,
        CoreConfig {
            strategy: Some(mount),
            order_client_id_session: Some(("Wk3x9q2m7d1p4".into(), 0)),
            ..config(&dir)
        },
    ));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    let long_of = |n: f64| {
        wait_for_snapshot(&cell, 5, "the long", |s| {
            s.portfolio.venues.iter().flat_map(|v| v.positions.iter()).any(|p| p.size == n)
        });
    };
    for tid in ["t0", "t1", "t2", "t3"] {
        events.blocking_send(bare_buy(tid)).unwrap();
    }
    long_of(4.0);
    close_binance_bar(&handle, "BTCUSDT.P", ohlc_bar(60_000, 100.0, 101.0, 99.0, 100.0));
    wait_for_snapshot(&cell, 5, "the strategy's stop", |s| !s.orders.is_empty());
    let old = cell.load_full().orders[0].order_client_id.clone();
    for event in answered(&old) {
        events.blocking_send(event).unwrap();
    }
    // four more journaled buys: a cadence `Snapshot` falls after the stop rests and before the Modify
    for tid in ["t4", "t5", "t6", "t7"] {
        events.blocking_send(bare_buy(tid)).unwrap();
    }
    long_of(8.0);
    close_binance_bar(&handle, "BTCUSDT.P", ohlc_bar(120_000, 100.0, 101.0, 99.0, 100.0));
    wait_for_snapshot(&cell, 5, "the replacement is in the book", |s| {
        s.orders.iter().any(|o| o.order_client_id != old)
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.order_client_id != old)
        .map(|o| o.order_client_id.clone())
        .expect("a replacement");
    for event in answered(&new) {
        events.blocking_send(event).unwrap();
    }
    events
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            order_client_id: old.clone(),
            reason: "venue".to_string().into(),
            timestamp: 2,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        s.orders.iter().any(|o| o.order_client_id == old && o.status == OrderStatus::Canceled)
            && s.orders
                .iter()
                .any(|o| o.order_client_id == new && o.status == OrderStatus::Accepted)
    });
    handle.shutdown_and_join();

    let parts = |id: &str| match parse_order_client_id(id) {
        Some(OrderClientIdReading::V2(p)) => (p.src, Some(p.strat), p.generation),
        other => panic!("{id}: not a v2 id: {other:?}"),
    };
    assert_eq!(
        parts(&new),
        (OrderClientIdSource::Strategy, code, 0),
        "PREMISE: a fresh mint, the mount's"
    );

    // The journal from the last `Snapshot` before the Modify on: the mount's other records are gone.
    let recs = records(&dir);
    let is_modify = |r: &JournalRecord| {
        matches!(r, JournalRecord::StrategySubmit { intent: OrderIntent::Modify { .. }, .. })
    };
    let modify = recs.iter().position(is_modify).expect("the mount's Modify");
    let base = recs[..modify]
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("a Snapshot before the Modify");
    let cut_recs: Vec<JournalRecord> = recs.into_iter().skip(base).collect();
    assert!(
        cut_recs.iter().all(|r| !matches!(r, JournalRecord::StrategySubmit { .. }) || is_modify(r)),
        "PREMISE: no record after the base names the mount's venue"
    );
    let cut = Scratch::reserved("amend-journal-snap-only-cut");
    rewrite_journal(&cut, cut_recs);
    let live = assert_refolds_as_live(&cut, "a mount named only by the Snapshot");
    assert!(live.iter().any(|(c, _)| *c == new), "the live registry holds {new}");
}
