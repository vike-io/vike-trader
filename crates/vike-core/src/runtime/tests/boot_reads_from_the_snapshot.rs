//! **A boot reads its journal from the newest `Snapshot` on, and restores what a read of the WHOLE journal restores**
//! (`docs/decisions/0304-a-boot-restores-from-the-newest-snapshot-and-its-tail-and-old-segments-are-archived-never-deleted.md`).
//! White-box: two coded strategy mounts on one v2 session mint and cancel orders and re-price one no engine holds,
//! over many small segments; the clock then moves past `vike_execution::ENDED_ORDER_KEEP_MS`, so every ended order is
//! left out of the newest `Snapshot`; each mount leaves an order resting and arms a stop; the newest `Snapshot` follows,
//! and a tail fills both resting orders and, LATE, one order that ended long before, then a crash. Every fact the
//! restore used to fold from that history (each order's and each arm's mount, the past sessions) must come from the
//! `Snapshot` and the tail, so the boot's read and a read of every record restore the same core. And a corrupt frame
//! in a segment below the newest `Snapshot` is not read at all: the boot restores in full.

use std::path::Path;

use super::*;
use crate::replay::{RestoreOutcome, restore_for_boot, restore_for_boot_reading};
use crate::runtime::test_support::core_with;
use crate::scratch::Scratch;
use vike_execution::OrderIntent;
use vike_journal::{Journal, JournalFileConfig};
use vike_model::events::{FillEvent, OrderAccepted, OrderCanceled, PositionSide, TradeId};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";
/// The rounds of the history below the newest `Snapshot`: each mount mints, has accepted and cancels one order, and
/// re-prices an order no engine holds [`INERT_PER_ROUND`] times.
const ROUNDS: i64 = 40;
const INERT_PER_ROUND: usize = 50;

fn journal(dir: &Path) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        // small segments, so the history spans several and the newest `Snapshot` is in a late one
        file: JournalFileConfig { segment_bytes: 64 << 10, flush_every: 64 },
        snapshot_every: u64::MAX,
    }
}

/// On its first feed status, one resting limit and one protective stop (a core-held arm, minted at its fire).
struct Rester;

impl Strategy<LiveBroker> for Rester {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_limit_tagged("rest", 1, 1.0, 95.0);
        broker.submit_stop(-1, 1.0, 80.0);
    }
}

fn coded(code: &str, controller: &str) -> Mount {
    Mount {
        order_client_id_code: vike_model::orders::client_id::OrderClientIdStrategyCode::parse(code),
        controller_id: Some(controller.into()),
        ..crate::testkit::mount_of(VENUE, SYMBOL, "1m", Box::new(Rester))
    }
}

fn bid(price: f64) -> OrderIntent {
    OrderIntent::Submit(Box::new(vike_model::OrderRequest {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }))
}

fn fill(order_client_id: &str, at: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t-", order_client_id),
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 0.5,
        last_price: 95.0,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: at,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// The past session the core is handed (as the host hands `vike.db`'s), which its `Snapshot`s carry.
const PAST_SESSION: (&str, u64) = ("Wzzzzzzz00000", 9);

/// What [`session`] leaves for the test to check: the order the tail fills LATE (minted and ended below the newest
/// `Snapshot`, left out of it) and its mount.
struct Session {
    late: String,
    late_mount: String,
}

/// The session at `dir`, ended by a crash (no exit `Snapshot`).
fn session(dir: &Path) -> Session {
    // one clock for the core and the stamps the test hands it, so an ended order AGES out of the newest `Snapshot`
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(10));
    let read = std::sync::Arc::clone(&clock);
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || read.load(std::sync::atomic::Ordering::SeqCst)),
        order_client_id_session: Some(("W00000000abcd".into(), 0)),
        order_client_id_past_sessions: vec![(PAST_SESSION.0.into(), PAST_SESSION.1)],
        journal: Some(journal(dir)),
        strategy: Some(coded("0001", "maker-a")),
        extra_mounts: vec![coded("0002", "maker-b")],
        ..CoreConfig::default()
    });
    // every strategy `Modify` journaled, the inert ones too: the bulk of the history, in no `Snapshot`
    core.journal_every_modify = true;
    core.write_snap(1); // the oldest base
    let tick = || clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let mut late = None;
    for round in 0..ROUNDS {
        for slot in 0..2 {
            let id = core.apply_strategy_intent(bid(90.0 - round as f64 * 0.01), tick(), slot)[0]
                .clone();
            core.dispatch(Ingest::Event(Event::OrderAccepted(OrderAccepted {
                order_client_id: id.clone(),
                venue_order_id: Some(format!("v{round}-{slot}").into()),
                timestamp: tick(),
            })));
            core.apply_strategy_intent(OrderIntent::Cancel(id.clone()), tick(), slot);
            core.dispatch(Ingest::Event(Event::OrderCanceled(OrderCanceled {
                order_client_id: id.clone(),
                reason: "venue".to_string().into(),
                timestamp: tick(),
                post_only_would_cross: false,
                refusal_id: None,
            })));
            if round == 0 && slot == 1 {
                late = Some((id, core.mount_ids[slot].clone()));
            }
            for k in 0..INERT_PER_ROUND {
                let price = 90.0 + k as f64 * 0.01;
                core.apply_strategy_intent(modify("held-by-no-engine", price), tick(), slot);
            }
        }
    }
    // past `ENDED_ORDER_KEEP_MS`, so every ended order above is left out of the newest `Snapshot`
    clock.fetch_add(vike_execution::ENDED_ORDER_KEEP_MS * 2, std::sync::atomic::Ordering::SeqCst);
    // the feed-status drain below stamps its orders on the engine clock, which only a dispatch moves
    core.engine.now_ms = tick();
    // each mount leaves an order resting and arms a stop, all minted below the newest `Snapshot`
    core.drive_strategy_feed_status(VENUE, SYMBOL, FeedStatus::Disconnected);
    let resting: Vec<String> = core
        .engine
        .registry
        .live()
        .filter(|(id, _)| core.order_client_id_mount.contains_key(*id))
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(resting.len(), 2, "premise: one resting order per mount");
    core.write_snap(tick()); // the newest `Snapshot`
    let (late, late_mount) = late.expect("the first round ran");
    // the tail: a fill on each resting order, and a LATE fill on an order that ended long before the `Snapshot`
    for id in resting.iter().chain([&late]) {
        let at = tick();
        core.dispatch(Ingest::Event(Event::Fill(fill(id, at))));
        core.dispatch_applied_fills();
    }
    core.journal.as_mut().expect("journal").flush().expect("flush");
    Session { late, late_mount }
}

fn modify(order_client_id: &str, price: f64) -> OrderIntent {
    OrderIntent::Modify {
        order_client_id: order_client_id.into(),
        new_qty: None,
        new_price: Some(price),
        new_trigger_price: None,
    }
}

/// A copy of every file of `dir` (a restore may move the directory aside, so each read restores its own copy).
fn copy_of(dir: &Path, tag: &str) -> Scratch {
    let copy = Scratch::created(tag);
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
        }
    }
    copy
}

/// Every restore field a core applies, as text (the notes name the directory, so they are compared without it).
fn applied(config: &CoreConfig, dir: &Path) -> String {
    let notes: Vec<String> = config
        .restore_notes
        .iter()
        .map(|n| n.replace(&dir.display().to_string(), "<dir>"))
        .collect();
    format!(
        "{:?}",
        (
            (&config.order_client_id_session, config.arm_sequence, &config.order_emulation_arms),
            (&config.conditional_owners, &config.contingencies, &config.held_releases),
            (&config.emulated, &config.restored_engines, &config.journal_orders),
            (&config.mount_attribution, &config.gateway_cursors, &config.closed_trades),
            (&config.closed_tail, &config.recent_order_client_ids, &config.owed_execution_reports),
            (config.restored_snap_sequence, &config.restored_downtime_ms, notes),
        )
    )
}

#[test]
fn the_newest_snapshot_and_its_tail_restore_what_the_whole_journal_does() {
    let dir = Scratch::reserved("vco-boot-snap-read");
    let Session { late, late_mount } = session(&dir);
    let segments = std::fs::read_dir(&*dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".vjl"))
        .count();
    let (every, _) = Journal::read_all_located(&dir).unwrap();
    let (from_newest, _) = Journal::read_from_latest_snap_located(&dir).unwrap();
    assert!(segments >= 4, "premise: the history spans several segments ({segments})");
    assert!(
        from_newest.len() * 10 < every.len(),
        "premise: the boot reads a small part ({} of {})",
        from_newest.len(),
        every.len()
    );
    let vike_journal::JournalRecord::Snapshot {
        engines,
        order_client_id_mounts,
        order_client_id_past_sessions,
        ..
    } = &from_newest[0]
    else {
        panic!("the boot's read starts at its `Snapshot`")
    };
    assert!(
        engines.iter().all(|e| e.registry.iter().all(|(id, _)| *id != late)),
        "premise: the late fill's order was left out of the newest `Snapshot`"
    );
    assert_eq!(order_client_id_mounts.len(), 2, "the two resting orders' owners ride it");
    assert_eq!(
        order_client_id_past_sessions,
        &vec![(PAST_SESSION.0.to_string(), PAST_SESSION.1)],
        "and the core's past sessions"
    );

    let (whole, newest) = (copy_of(&dir, "vco-boot-whole"), copy_of(&dir, "vco-boot-newest"));
    let (tmp_whole, tmp_newest) =
        (Scratch::created("vco-boot-whole-tmp"), Scratch::created("vco-boot-newest-tmp"));
    let now = 1_000_000;
    let by_every = restore_for_boot_reading(
        Some(&journal(&whole)),
        &tmp_whole,
        now,
        &[],
        Journal::read_all_located,
    )
    .unwrap();
    let by_newest = restore_for_boot(Some(&journal(&newest)), &tmp_newest, now, &[]).unwrap();
    assert_eq!(by_every.outcome(), &RestoreOutcome::Full, "{:?}", by_every.notes());
    assert_eq!(by_newest.outcome(), &RestoreOutcome::Full, "{:?}", by_newest.notes());
    let candidates: Vec<String> =
        by_newest.journal_orders().iter().map(|c| c.mount_id.clone()).collect();

    let (mut a, mut b) = (CoreConfig::default(), CoreConfig::default());
    by_every.apply_to(&mut a);
    by_newest.apply_to(&mut b);
    assert_eq!(
        a.conditional_owners.len(),
        2,
        "premise: each mount's stop has its mount: {:?}",
        a.conditional_owners
    );
    assert_eq!(
        b.conditional_owners, a.conditional_owners,
        "the arms' mounts come from the `Snapshot`"
    );
    assert_eq!(
        candidates.len(),
        2,
        "both resting orders are candidates under their mounts: {candidates:?}"
    );
    assert_eq!(b.closed_tail, a.closed_tail, "the tail's fills book under the same mounts");
    assert!(
        b.closed_tail.iter().any(|t| matches!(
            t,
            crate::replay::TradeClosedTail::Fill { fill, owner: Some(owner), .. }
                if fill.order_client_id == late && *owner == late_mount
        )),
        "the late fill of an order the `Snapshot` left out books under its mount by its id's code: {:?}",
        b.closed_tail
    );
    assert_eq!(applied(&b, &newest), applied(&a, &whole));
}

/// **A restore after the archive pass restores what one before it does** (decision 0304's verdict 2): the pass moves
/// every segment below the newest `Snapshot`'s into `archive/`, and the boot never reads one of them.
#[test]
fn a_restore_after_the_archive_pass_equals_one_before_it() {
    let dir = Scratch::reserved("vco-boot-archived");
    let _ = session(&dir);
    let (kept, archived) = (copy_of(&dir, "vco-archive-kept"), copy_of(&dir, "vco-archive-moved"));
    let everything = vike_journal::JournalArchivePolicy {
        keep_ms: 0,
        max_bytes: u64::MAX,
        max_age_ms: i64::MAX,
        at_limit: vike_model::JournalArchiveFull::Keep,
    };
    let closed = vike_journal::JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };
    let pass = Journal::prune_before_latest_snap(&archived, &everything, i64::MAX, closed).unwrap();
    assert!(pass.moved.len() >= 3, "premise: most of the history moved: {pass:?}");
    for index in &pass.moved {
        let name = format!("journal-{index:08}.vjl");
        assert!(
            archived.join(vike_journal::ARCHIVE_DIR).join(&name).exists()
                && !archived.join(&name).exists()
        );
    }

    let (tmp_kept, tmp_archived) =
        (Scratch::created("vco-archive-kept-tmp"), Scratch::created("vco-archive-moved-tmp"));
    let before = restore_for_boot(Some(&journal(&kept)), &tmp_kept, 1_000_000, &[]).unwrap();
    let after = restore_for_boot(Some(&journal(&archived)), &tmp_archived, 1_000_000, &[]).unwrap();
    assert_eq!(before.outcome(), &RestoreOutcome::Full, "{:?}", before.notes());
    assert_eq!(after.outcome(), &RestoreOutcome::Full, "{:?}", after.notes());
    let (mut a, mut b) = (CoreConfig::default(), CoreConfig::default());
    before.apply_to(&mut a);
    after.apply_to(&mut b);
    assert_eq!(applied(&b, &archived), applied(&a, &kept));
}

#[test]
fn a_corrupt_segment_below_the_newest_snapshot_is_not_read() {
    let dir = Scratch::reserved("vco-boot-snap-corrupt");
    let _ = session(&dir);
    // flip one payload byte of the FIRST frame of segment 0: its check fails, and records follow in later segments
    let first = dir.join("journal-00000000.vjl");
    let mut bytes = std::fs::read(&first).unwrap();
    bytes[16 + 8] ^= 0xff;
    std::fs::write(&first, &bytes).unwrap();

    let (whole, newest) = (copy_of(&dir, "vco-corrupt-whole"), copy_of(&dir, "vco-corrupt-newest"));
    let (tmp_whole, tmp_newest) =
        (Scratch::created("vco-corrupt-whole-tmp"), Scratch::created("vco-corrupt-newest-tmp"));
    let by_every = restore_for_boot_reading(
        Some(&journal(&whole)),
        &tmp_whole,
        1_000_000,
        &[],
        Journal::read_all_located,
    )
    .unwrap();
    assert!(
        matches!(by_every.outcome(), RestoreOutcome::Broken { .. }),
        "a read of every record halts there: {:?}",
        by_every.outcome()
    );
    let by_newest = restore_for_boot(Some(&journal(&newest)), &tmp_newest, 1_000_000, &[]).unwrap();
    assert_eq!(by_newest.outcome(), &RestoreOutcome::Full, "{:?}", by_newest.notes());
    assert_eq!(by_newest.journal_orders().len(), 2);
}
