//! **This install's order client id sessions at boot** (`live_runtime`'s `order_client_id_sessions_at_boot`, package
//! 2's ownership check): the session the journal restored is recorded at the sequence it reached, never lowered, and
//! every recorded session goes to the core, so a late fill on an id a PAST session minted is ours.

use std::path::Path;

use super::super::live_runtime::{
    order_client_id_session_recorder, order_client_id_sessions_at_boot,
    recorded_order_client_id_sessions,
};

#[test]
fn every_recorded_session_reaches_the_core_and_the_restored_one_is_recorded_first() {
    assert_eq!(
        order_client_id_sessions_at_boot(None, Some(&("W00000000abcd".into(), 7))),
        Vec::new(),
        "no project"
    );

    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(settings.path()))
        .expect("a store");
    let dir = Some(settings.path());
    assert_eq!(
        order_client_id_sessions_at_boot(dir, None),
        Vec::new(),
        "nothing recorded, nothing restored"
    );
    // the first boot on a v1 session; the next switches to a v2 one, which a later boot restores
    assert_eq!(
        order_client_id_sessions_at_boot(dir, Some(&("1f2e3d4c".into(), 40))),
        [("1f2e3d4c".to_string(), 40)]
    );
    assert_eq!(
        order_client_id_sessions_at_boot(dir, Some(&("W00000000abcd".into(), 7))),
        [("1f2e3d4c".to_string(), 40), ("W00000000abcd".to_string(), 7)]
    );
    // a restore that read less (an older checkpoint) never lowers what a session reached
    assert_eq!(
        order_client_id_sessions_at_boot(dir, Some(&("W00000000abcd".into(), 3))),
        [("1f2e3d4c".to_string(), 40), ("W00000000abcd".to_string(), 7)]
    );
}

/// The journal at `dir`, on a small segment (the default reserves a large one).
fn journal_at(dir: &Path) -> vike_core::JournalConfig {
    vike_core::JournalConfig {
        dir: dir.to_path_buf(),
        file: vike_journal::JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 64,
    }
}

/// A journaling core on sim/BTCUSDT over `journal` whose session is recorded in `settings` before it mints.
fn recording_core(journal: &Path, settings: &Path) -> vike_core::CoreHandle {
    vike_core::spawn_core(
        vike_execution::testing::engine_on(
            "sim",
            "BTCUSDT",
            vike_execution::testing::RecordingClient::default(),
        ),
        vike_core::CoreConfig {
            seed_cash: 1_000_000.0,
            journal: Some(journal_at(journal)),
            order_client_id_session_recorder: order_client_id_session_recorder(Some(settings)),
            ..vike_core::CoreConfig::default()
        },
    )
}

/// The journal at `dir` BROKEN past its last frame (a frame whose check holds and whose payload no build reads), so
/// the next boot moves it aside.
fn break_journal(dir: &Path) {
    let segment = std::fs::read_dir(dir)
        .expect("the journal")
        .map(|e| e.expect("entry").path())
        .find(|p| p.extension().is_some_and(|x| x == "vjl"))
        .expect("a segment");
    let mut bytes = std::fs::read(&segment).expect("read");
    let mut at = 16; // the segment header
    while let Some(len) = bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        && len != 0
    {
        at += 8 + len as usize;
    }
    let payload = br#"{"NotARecordOfThisBuild":{}}"#;
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&vike_journal::fnv1a32(payload).to_le_bytes());
    frame.extend_from_slice(payload);
    bytes[at..at + frame.len()].copy_from_slice(&frame);
    std::fs::write(&segment, bytes).expect("write");
}

/// **An id minted right before a journal is moved aside is ours after the move.** Nothing but the boot's recorder
/// wrote the session's row (no Snapshot was ever read into `vike.db`), the journal that named the session is moved
/// aside unread, and the next core starts a new session: a late fill on the id is still OURS, because the row was
/// written before the session minted, with a lease above every id it minted.
#[test]
fn an_id_minted_right_before_a_move_aside_reads_as_ours_after_the_move() {
    use vike_execution::{Command, OrderIntent};
    use vike_journal::{Journal, JournalRecord};
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(settings.path()))
        .expect("a store");
    let root = tempfile::tempdir().expect("temp");
    let journal = root.path().join("journal");

    let first = recording_core(&journal, settings.path());
    first.send_command(Command::Order(OrderIntent::Submit(Box::new(vike_model::OrderRequest {
        order_client_id: String::new(), // the core mints it
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }))));
    first.shutdown_and_join();
    let minted = Journal::read_all(&journal)
        .expect("reads")
        .into_iter()
        .find_map(|r| match r {
            JournalRecord::MintedSubmit { request, .. } => Some(request.order_client_id),
            _ => None,
        })
        .expect("the core minted the order's id");
    break_journal(&journal);

    let recorded = recorded_order_client_id_sessions(Some(settings.path()));
    assert!(!recorded.is_empty(), "the boot's recorder wrote the session's row: {recorded:?}");
    let scratch = tempfile::tempdir().expect("temp");
    let journal_config = journal_at(&journal);
    let restore = vike_core::replay::restore_for_boot(
        Some(&journal_config),
        scratch.path(),
        vike_model::now_ms(),
        &recorded,
    )
    .expect("the boot restores");
    assert!(
        matches!(
            restore.outcome(),
            vike_core::replay::RestoreOutcome::Broken { moved_to: Some(_), .. }
        ),
        "moved aside: {:?}",
        restore.outcome()
    );
    let past =
        order_client_id_sessions_at_boot(Some(settings.path()), restore.order_client_id_session());
    let mut config = vike_core::CoreConfig {
        seed_cash: 1_000_000.0,
        journal: Some(journal_config),
        ..vike_core::CoreConfig::default()
    };
    restore.apply_to(&mut config);
    config.order_client_id_past_sessions = past;
    let second = vike_core::spawn_core(
        vike_execution::testing::engine_on(
            "sim",
            "BTCUSDT",
            vike_execution::testing::RecordingClient::default(),
        ),
        config,
    );
    let late = vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new("late").expect("non-empty"),
        order_client_id: minted.clone(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        timestamp: 7,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    second.event_sender().blocking_send(vike_model::events::Event::Fill(late)).unwrap();
    second.shutdown_and_join();
    let report = Journal::read_all(&journal)
        .expect("the fresh journal reads")
        .into_iter()
        .find_map(|r| match r {
            JournalRecord::ExecutionReport { report, .. } if report.order_client_id == minted => {
                Some(report)
            }
            _ => None,
        })
        .expect("the late fill is reported");
    assert_eq!(
        report.ownership,
        vike_model::orders::ExecutionOwnership::Ours,
        "an id of the session before the move is this install's: {report:?}"
    );
}
