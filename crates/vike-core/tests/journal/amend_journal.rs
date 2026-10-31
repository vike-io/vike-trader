//! A cancel-then-new amend through a REAL core with a journal (decision record on the unified amend): it
//! needs NO record kind of its own. The Modify command is journaled as it always was, the replacement is
//! journaled by the standard minted-submit record (and its lifecycle is the venue's own events), the
//! segment is not raised above the base format, and the replay fence holds: the journal alone re-derives the
//! state the live run held, the replacement included.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderSubmitted, PositionSide, TradeId,
};
use vike_model::{Clock, OrderRequest, OrderStatus, OrderType};

use crate::kit::engines::engine_on;
use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

fn config(dir: &Path) -> CoreConfig {
    // Message `k` dispatches at `now_ms == k`: no wall-clock wait, a distinct stamp per record.
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            // the 4th journaled message trips a cadence `Snap`: the replay's BASE
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}

/// A bare external buy on the binance perpetual: opens the long the reduce-only stop below protects.
fn bare_buy(tid: &str) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("non-empty"),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

fn stop(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Stop,
        trigger_price: Some(90.0),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

fn answered(coid: &str) -> [Event; 2] {
    [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 1 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: Some(format!("v-{coid}").into()),
            ts: 1,
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
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(stop("s1")))));
    for ev in answered("s1") {
        events.blocking_send(ev).unwrap();
    }
    wait_for_snapshot(&cell, 5, "s1 resting", |s| {
        s.orders.iter().any(|o| o.client_order_id == "s1" && o.status == OrderStatus::Accepted)
    });

    handle.send_command(Command::Order(OrderIntent::Modify {
        client_order_id: "s1".into(),
        new_qty: None,
        new_price: Some(91.0),
    }));
    wait_for_snapshot(&cell, 5, "the replacement is in the book", |s| {
        s.orders.iter().any(|o| o.client_order_id != "s1")
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.client_order_id != "s1")
        .map(|o| o.client_order_id.clone())
        .expect("a replacement");
    for ev in answered(&new) {
        events.blocking_send(ev).unwrap();
    }
    events
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            client_order_id: "s1".into(),
            reason: "venue".to_string().into(),
            ts: 2,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        s.orders.iter().any(|o| o.client_order_id == "s1" && o.status == OrderStatus::Canceled)
            && s.orders
                .iter()
                .any(|o| o.client_order_id == new && o.status == OrderStatus::Accepted)
    });
    handle.shutdown_and_join();

    // the replacement is journaled by the standard minted-submit record, with its new trigger
    let recs = CommandJournal::read_all(&dir).unwrap();
    let minted: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MintedSubmit { req, .. } => Some(req.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(minted.len(), 1, "one minted submit: the replacement ({minted:?})");
    assert_eq!(minted[0].client_order_id, new);
    assert_eq!(minted[0].trigger_price, Some(91.0));
    // ...the Modify command is a journaled command, as it always was...
    assert!(
        recs.iter().any(|r| matches!(
            r,
            JournalRecord::Cmd {
                msg: vike_exec::Ingest::Command(Command::Order(OrderIntent::Modify { .. })),
                ..
            }
        )),
        "the Modify is journaled"
    );
    // ...nothing raised the segment above the base format...
    assert_eq!(CommandJournal::latest_segment_version(&dir).unwrap(), Some(18));
    // ...and the journal ALONE re-derives the state the live run held.
    let out = replay_offline(&dir).expect("the replay fence holds");
    assert!(out.records > 0 && out.snaps_compared >= 1);
    let status = |coid: &str| {
        out.engines[0].registry.iter().find(|(c, _)| c == coid).map(|(_, mo)| mo.status)
    };
    assert_eq!(status("s1"), Some(OrderStatus::Canceled));
    assert_eq!(status(&new), Some(OrderStatus::Accepted));
}
